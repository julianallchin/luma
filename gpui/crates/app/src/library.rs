//! The app's whole connection to Luma's data: a typed, awaitable client over
//! the command dispatcher.
//!
//! # Interface
//!
//! ```ignore
//! let library = Library::open()?;
//! let venues: Vec<Venue> = library.venues().await?;
//! let tracks: Vec<TrackBrowserRow> = library.tracks(&venue.id).await?;
//! library.set_setting("audio_output_enabled", "false").await?;
//! ```
//!
//! Everything a screen knows about data access is those methods. It never
//! sees a command name, a JSON frame, an `AppServices`, or a runtime — which
//! is the point: [`luma_lib::dispatch`] is the *only* way this binary reaches
//! Luma's behavior, and re-implementing a query above it would fork the app
//! the same way the headless harness once forked it.
//!
//! # Implementation
//!
//! Two runtimes coexist. GPUI owns the main thread and its own executors;
//! `sqlx` needs a Tokio reactor. So the dispatcher runs on a Tokio runtime
//! this type owns, and [`Library::call`] hands back a future that a GPUI task
//! can await — a Tokio `JoinHandle` is an ordinary future, so no bridging
//! machinery is needed beyond keeping the runtime alive.
//!
//! Results cross that boundary as JSON, because that is the dispatcher's
//! host-facing shape: `dispatch` returns `serde_json::Value` and this module
//! deserializes into the same models the frontend's bindings are generated
//! from. It costs one round-trip through serde per call, which for a screenful
//! of rows is not worth a second, typed entry point into the seam.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use luma_lib::agent::model::ModelClient;
use luma_lib::agent::tools::ToolRegistry;
use luma_lib::agent::AgentService;
use luma_lib::agent_execution::workspace::PythonWorkspaceService;
use luma_lib::database::local::auth::bootstrap_host_admission;
use luma_lib::database::local::database::init_app_db_at;
use luma_lib::database::local::state::init_state_db_at;
use luma_lib::dispatch::{dispatch, AppServices};
use luma_lib::host_audio::HostAudioSnapshot;
use luma_lib::models::node_graph::{BeatGrid, Graph, NodeTypeDef};
use luma_lib::models::patterns::PatternSummary;
use luma_lib::models::scores::{ScoreSummary, TrackScore};
use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::venues::Venue;
use luma_lib::models::waveforms::{TrackWaveform, WaveformWindow};
use luma_lib::services::fixtures as fixtures_service;
use luma_lib::services::graph_documents::{GraphDocument, GraphEditResult};
use luma_lib::services::track_edits::TrackEditResult;
use luma_lib::settings::AppSettings;
use luma_lib::storage::StorageRoot;

/// How long a fine waveform window waits before it is sent.
///
/// A wheel notch changes the zoom every few milliseconds and each one wants a
/// different window; this is the pause that says the gesture has stopped. Short
/// enough that a deliberate zoom is answered within a frame or two of settling,
/// long enough that a flick asks once instead of forty times.
const WINDOW_DEBOUNCE: Duration = Duration::from_millis(40);

/// A connection to the real Luma library.
pub struct Library {
    services: Arc<AppServices>,
    /// Who the app database admitted at startup. `None` is the guest
    /// namespace, which is also what an un-signed-in host gets.
    user_id: Option<String>,
    /// Owns the reactor every dispatched command runs on. Dropping it cancels
    /// in-flight work, so it lives exactly as long as the `Library`.
    runtime: tokio::runtime::Runtime,
    /// Drives agent turns with this client instead of a configured provider.
    /// The one injection seam the harness needs, and the shipped app never
    /// sets: without it the loop resolves its model and key the way it does
    /// headless.
    model: Option<Arc<dyn ModelClient>>,
    /// Exposes these tools instead of the agent kind's own set. The seam a
    /// subagent will use too, which is why it is the registry and not a flag:
    /// a surface built by a second path could drift from the parent's.
    tools: Option<ToolRegistry>,
}

impl Library {
    /// Open the library in the app's real config directory — the same
    /// `$APPCONFIG` the Tauri app and the headless harness use, honouring the
    /// `LUMA_CONFIG_DIR` / `LUMA_FIXTURES_ROOT` overrides so a disposable
    /// fixture directory can be pointed at without touching the real one.
    ///
    /// # Errors
    ///
    /// If the config directory cannot be located or either database fails to
    /// open or migrate.
    pub fn open() -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("failed to start the async runtime: {error}"))?;

        let storage = config_dir()?;
        let fixtures_root = fixtures_root()?;

        let (services, user_id) = runtime.block_on(async {
            let db = init_app_db_at(storage.path()).await?;
            let state_db = init_state_db_at(storage.path()).await?;
            // Admission is host bootstrap, not a command: until it is armed
            // every `auth_visible_*` view is empty, so a host that skipped
            // this would show an empty library and no error to explain it.
            let user_id = bootstrap_host_admission(&db.0, &state_db.0).await?;
            // Nothing in this host runs Python, and a workspace service
            // resolves its worker environment lazily, so refusing to build one
            // is both honest and harmless. The day this app runs an agent, it
            // grows the same `resolve_worker_env` the headless harness has.
            let workspaces = Arc::new(PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("the GPUI app does not run Python workspaces".to_string())),
            ));
            let services = AppServices::headless(db, state_db, storage, fixtures_root, workspaces);
            // The audio host keeps its own copy of the settings it reads, and
            // is only ever told by a write. A host that skipped this would run
            // the whole session on the defaults and ignore what the user last
            // chose — audible the first time a track is loaded.
            services
                .apply_persisted_settings()
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((services, user_id))
        })?;

        Ok(Self {
            services: Arc::new(services),
            user_id,
            runtime,
            model: None,
            tools: None,
        })
    }

    /// The agent loop, on the reactor its database needs.
    ///
    /// A turn outlives the call that starts it, so it cannot borrow this
    /// `Library`; [`AgentService`] owns a shared handle on the same services
    /// every dispatched command runs on, which is what keeps the agent's
    /// writes and the app's reads inside one transaction boundary.
    pub fn agent(&self) -> luma_chat::Agent {
        let mut service = AgentService::new(Arc::clone(&self.services));
        if let Some(model) = &self.model {
            service = service.with_model(Arc::clone(model));
        }
        if let Some(tools) = &self.tools {
            service = service.with_tools(tools.clone());
        }
        luma_chat::Agent::new(service, self.runtime.handle().clone())
    }

    /// Drive agent turns with `model`. Call before the first turn — an
    /// [`AgentService`] is built per turn, so a later change simply takes
    /// effect from the next one.
    pub fn set_agent_model(&mut self, model: Arc<dyn ModelClient>) {
        self.model = Some(model);
    }

    /// Expose `tools` to the agent instead of its kind's own set.
    pub fn set_agent_tools(&mut self, tools: ToolRegistry) {
        self.tools = Some(tools);
    }

    /// The signed-in principal, or `None` for the guest namespace. Identity is
    /// fixed for the life of the process here — this host has no sign-in — so
    /// it is a field rather than a query.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    /// Every venue in the library, newest activity first.
    pub fn venues(&self) -> impl Future<Output = Result<Vec<Venue>, String>> + use<> {
        self.call("list_venues", json!({}))
    }

    /// The track browser's rows for `venue_id`, carrying that venue's
    /// annotation counts.
    pub fn tracks(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<TrackBrowserRow>, String>> + use<> {
        self.call("list_tracks_enriched", json!({ "venueId": venue_id }))
    }

    /// Display names for other people's uids, as `uid -> name`. Sparse: a uid
    /// the directory does not know is simply absent.
    pub fn display_names(
        &self,
        uids: Vec<String>,
    ) -> impl Future<Output = Result<HashMap<String, String>, String>> + use<> {
        self.call("get_display_names", json!({ "uids": uids }))
    }

    /// Every pattern in the library.
    pub fn patterns(&self) -> impl Future<Output = Result<Vec<PatternSummary>, String>> + use<> {
        self.call("list_patterns", json!({}))
    }

    /// The node catalogue: what every `typeId` in a graph means. Static for
    /// the life of the process, so a screen reads it once.
    pub fn node_types(&self) -> impl Future<Output = Result<Vec<NodeTypeDef>, String>> + use<> {
        self.call("get_node_types", json!({}))
    }

    /// One pattern's graph, with the implementation and revision it came from.
    /// Both are the write token: [`Self::save_pattern_graph`] needs them, and
    /// they are only valid for the document this returned.
    ///
    /// Resolves the implementation venue-agnostically, as the web editor does
    /// — `get_pattern_args` is the one that resolves against a venue, and the
    /// two can disagree.
    pub fn pattern_graph(
        &self,
        pattern_id: &str,
    ) -> impl Future<Output = Result<GraphDocument, String>> + use<> {
        self.call(
            "get_pattern_graph_document",
            json!({ "id": pattern_id, "implementationId": null }),
        )
    }

    /// Write a whole graph back, optimistically.
    ///
    /// `base_revision` is the revision the edited graph was read at; the seam
    /// refuses the write if the document has moved since, which is the only
    /// thing keeping two editors from silently overwriting each other.
    /// `operation_id` makes a retry of *this* write idempotent — reuse it
    /// verbatim across attempts, never mint a second one.
    pub fn save_pattern_graph(
        &self,
        pattern_id: &str,
        implementation_id: &str,
        operation_id: &str,
        base_revision: &str,
        graph: &Graph,
    ) -> impl Future<Output = Result<GraphEditResult, String>> + use<> {
        self.call(
            "save_pattern_graph_document",
            json!({
                "id": pattern_id,
                "implementationId": implementation_id,
                "operationId": operation_id,
                "baseRevision": base_revision,
                "graph": graph,
            }),
        )
    }

    // -- the track editor -----------------------------------------------------

    /// This track's scores in `venue_id`, newest first. A track with none has
    /// never been annotated here, and there is no timeline to open.
    pub fn scores_for_track(
        &self,
        track_id: &str,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<ScoreSummary>, String>> + use<> {
        self.call(
            "list_scores_for_track",
            json!({ "trackId": track_id, "venueId": venue_id }),
        )
    }

    /// One score's clips.
    pub fn track_scores(
        &self,
        score_id: &str,
    ) -> impl Future<Output = Result<Vec<TrackScore>, String>> + use<> {
        self.call("list_track_scores", json!({ "scoreId": score_id }))
    }

    /// The rendered envelope of a track, at both resolutions. Large — a full
    /// track is tens of thousands of floats — so a screen reads it once.
    pub fn track_waveform(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<TrackWaveform, String>> + use<> {
        self.call("get_track_waveform", json!({ "trackId": track_id }))
    }

    /// One visible range of a track's audio, measured into `buckets` min/max/RMS
    /// buckets — the fine half of the pair, and the only source with a bucket
    /// per pixel once the zoom outruns the stored envelope's fixed resolution.
    ///
    /// Waited before it is sent, because it is asked for while a zoom gesture
    /// is still moving and only the window that gesture settles on is worth
    /// measuring.
    pub fn track_waveform_window(
        &self,
        track_id: &str,
        start_seconds: f64,
        end_seconds: f64,
        buckets: u32,
    ) -> impl Future<Output = Result<WaveformWindow, String>> + use<> {
        self.call_after(
            "get_track_waveform_window",
            json!({
                "trackId": track_id,
                "startSeconds": start_seconds,
                "endSeconds": end_seconds,
                "buckets": buckets,
            }),
            WINDOW_DEBOUNCE,
        )
    }

    /// A track's analyzed beats, or `None` when it has not been analyzed.
    pub fn track_beats(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<Option<BeatGrid>, String>> + use<> {
        self.call("get_track_beats", json!({ "trackId": track_id }))
    }

    /// Move one clip's bounds.
    ///
    /// `operation_id` makes a retry of *this* edit idempotent — reuse it
    /// verbatim across attempts, never mint a second one. Unlike a graph
    /// document there is no `base_revision` here: the authored score's edit
    /// protocol resolves a partial update against whatever is current, so two
    /// people moving different clips do not fight.
    pub fn move_clip(
        &self,
        score_id: &str,
        track_id: &str,
        clip_id: &str,
        operation_id: &str,
        start_time: f64,
        end_time: f64,
    ) -> impl Future<Output = Result<TrackEditResult, String>> + use<> {
        self.call(
            "update_track_score",
            json!({ "payload": {
                "operationId": operation_id,
                "scoreId": score_id,
                "trackId": track_id,
                "id": clip_id,
                "startTime": start_time,
                "endTime": end_time,
            }}),
        )
    }

    /// Decode a track and hand it to the audio host, from 0.0. Slow — this is
    /// the decode — and it must land before the transport does anything.
    pub fn load_audio(&self, track_id: &str) -> impl Future<Output = Result<(), String>> + use<> {
        self.call("host_load_track", json!({ "trackId": track_id }))
    }

    /// Start playback from wherever the transport currently is.
    pub fn play(&self) -> impl Future<Output = Result<(), String>> + use<> {
        self.call("host_play", json!({}))
    }

    pub fn pause(&self) -> impl Future<Output = Result<(), String>> + use<> {
        self.call("host_pause", json!({}))
    }

    /// Move the transport to `seconds` from the loaded segment's start, which
    /// for a whole track is absolute track time.
    pub fn seek(&self, seconds: f32) -> impl Future<Output = Result<(), String>> + use<> {
        self.call("host_seek", json!({ "seconds": seconds }))
    }

    /// The transport, `after` a wait.
    ///
    /// The desktop app learns the playhead from a `host-audio://state` event
    /// that a Tauri broadcaster emits; nothing emits it here, so this host
    /// polls instead — and the pacing belongs on the Tokio runtime because
    /// that is the one this process has real timers on. GPUI's own timers are
    /// driven by a test clock under the harness, so a poll paced there would
    /// never tick in a test.
    pub fn transport_after(
        &self,
        after: Duration,
    ) -> impl Future<Output = Result<HostAudioSnapshot, String>> + use<> {
        self.call_after("host_snapshot", json!({}), after)
    }

    /// Every app setting, typed and defaulted.
    pub fn settings(&self) -> impl Future<Output = Result<AppSettings, String>> + use<> {
        self.call("get_settings", json!({}))
    }

    /// Write one setting. The seam owns what a key means and what a write
    /// costs (Art-Net and audio both reload on one), so the caller's only job
    /// afterwards is to read the settings back — which is also the only honest
    /// proof the write landed.
    pub fn set_setting(&self, key: &str, value: &str) -> impl Future<Output = Result<(), String>> {
        self.call("set_setting", json!({ "key": key, "value": value }))
    }

    /// Run one command on the Tokio runtime and decode its result.
    ///
    /// The returned future is detached from `&self` so a caller can hold it
    /// across a GPUI task boundary without borrowing the `Library`.
    fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        name: &'static str,
        args: Value,
    ) -> impl Future<Output = Result<T, String>> + use<T> {
        self.call_after(name, args, Duration::ZERO)
    }

    /// [`Self::call`], `after` a wait on the Tokio runtime. The wait is here
    /// rather than at the call site because this runtime is the one this
    /// process has real timers on — see [`Self::transport_after`].
    fn call_after<T: DeserializeOwned + Send + 'static>(
        &self,
        name: &'static str,
        args: Value,
        after: Duration,
    ) -> impl Future<Output = Result<T, String>> + use<T> {
        let services = Arc::clone(&self.services);
        let task = self.runtime.spawn(async move {
            if !after.is_zero() {
                tokio::time::sleep(after).await;
            }
            let value: Value = dispatch(&services, name, &args)
                .await
                .map_err(|error| format!("{name}: {error}"))?;
            serde_json::from_value(value)
                .map_err(|error| format!("{name}: unexpected result shape: {error}"))
        });
        async move {
            task.await
                .map_err(|error| format!("{name} did not complete: {error}"))?
        }
    }
}

/// The app config directory: `$APPCONFIG` as the Tauri app resolves it, with
/// the same `LUMA_CONFIG_DIR` escape hatch the headless harness has.
fn config_dir() -> Result<StorageRoot, String> {
    match std::env::var_os("LUMA_CONFIG_DIR") {
        Some(path) => Ok(StorageRoot::from_path(PathBuf::from(path))),
        None => StorageRoot::from_env_default(),
    }
}

/// Root of the bundled fixture definitions. Prefers the repo's newest bundle
/// so a dev build sees today's fixtures, exactly as the headless harness does.
fn fixtures_root() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("LUMA_FIXTURES_ROOT") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = repo_fixtures_root() {
        return Ok(path);
    }
    fixtures_service::resolve_fixtures_root_from(None)
}

fn repo_fixtures_root() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)?
        .join("resources/fixtures");
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .max()
}
