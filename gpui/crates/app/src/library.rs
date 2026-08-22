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
//!
//! Failures keep the seam's structure, as [`LibraryError`]. Two designs lost.
//! Returning [`CommandError`] itself is the smaller change, but the two ways a
//! call fails *without* reaching a command — the reactor dropped the work, the
//! result decoded into the wrong shape — are not command errors, and naming
//! them `Internal` with a `{command}: ` prefix would break that type's
//! documented contract that `Display` is a verbatim passthrough. Reading
//! `CommandError::kind()` is the other: it is the escape hatch for a host that
//! serializes the discriminant, and a host linked against the enum that
//! compared strings would throw the conflict's revisions away a second time.
//! [`LibraryError`] is not a per-layer pass-through wrapper — it adds the two
//! host-side failures and the command name, and hands the seam's own error
//! back untouched through [`LibraryError::command`].

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "agent")]
use std::sync::Mutex;
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

use luma_lib::agent::model::ModelClient;
use luma_lib::agent::tools::ToolRegistry;
use luma_lib::agent::{AgentService, ThreadScope};
use luma_lib::agent_execution::headless_env;
use luma_lib::database::local::auth::bootstrap_host_admission;
use luma_lib::database::local::database::init_app_db_at;
use luma_lib::database::local::state::init_state_db_at;
use luma_lib::dispatch::{dispatch, AppServices, CommandError, EventSink, Events};
#[cfg(feature = "agent")]
use luma_lib::dispatch::{
    system_track_sources, ImportedEngineDjTrack, ImportedRekordboxTrack, TrackSources,
};
use luma_lib::host_audio::HostAudioSnapshot;
use luma_lib::models::agent_threads::AgentThread;
use luma_lib::models::fixtures::{FixtureDefinition, PatchedFixture};
use luma_lib::models::node_graph::{BeatGrid, Graph, NodeTypeDef};
use luma_lib::models::patterns::PatternSummary;
use luma_lib::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, ScoreSummary, TrackScore,
};
use luma_lib::models::stage::StagePiece;
use luma_lib::models::tracks::{TrackBrowserRow, TrackImportProgress, TrackImportResult};
use luma_lib::models::universe::UniverseState;
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

struct LibraryEvents {
    import_progress: tokio::sync::broadcast::Sender<TrackImportProgress>,
}

impl EventSink for LibraryEvents {
    fn emit(&self, event: &str, payload: Value) {
        if event != "track-import-state" {
            return;
        }
        match serde_json::from_value(payload) {
            Ok(progress) => {
                let _ = self.import_progress.send(progress);
            }
            Err(error) => eprintln!("invalid structured track-import-state event: {error}"),
        }
    }
}

/// DJ catalog selected by the import flow. Source-specific identifiers remain
/// opaque strings above this seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackSource {
    EngineDj { library_path: String },
    Rekordbox,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackImportRequest {
    Files(Vec<PathBuf>),
    Source {
        source: TrackSource,
        track_ids: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceLibrary {
    pub identity: Option<String>,
    pub track_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePlaylist {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub track_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceTrack {
    pub id: String,
    pub file_path: Option<String>,
    pub filename: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bpm: Option<f64>,
    pub duration_seconds: Option<f64>,
}

/// Raw adapter answers used only by the GPUI agent harness. Keeping the
/// fixture at the JSON boundary proves the public Library methods perform the
/// same source-specific decoding and normalization as production dispatch.
#[cfg(feature = "agent")]
#[derive(Clone, Debug)]
pub struct SourceAdapterFixture {
    pub library: Value,
    pub playlists: Value,
    pub tracks: Value,
    pub playlist_tracks: HashMap<String, Value>,
    pub searches: HashMap<String, Value>,
}

#[cfg(feature = "agent")]
struct FixtureTrackSources {
    fixture: Arc<Mutex<Option<SourceAdapterFixture>>>,
    fallback: Arc<dyn TrackSources>,
}

#[cfg(feature = "agent")]
impl TrackSources for FixtureTrackSources {
    fn engine_tracks<'a>(
        &'a self,
        library_path: &'a str,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<(String, Vec<ImportedEngineDjTrack>), String>> + Send + 'a>,
    > {
        let fixture = self.fixture.lock().unwrap().clone();
        if fixture.is_none() {
            return self.fallback.engine_tracks(library_path);
        }
        Box::pin(async move {
            let fixture =
                fixture.ok_or_else(|| "Engine DJ source fixture is not installed".to_string())?;
            let library: EngineDjLibraryWire = serde_json::from_value(fixture.library)
                .map_err(|error| format!("invalid Engine DJ library fixture: {error}"))?;
            let tracks = serde_json::from_value(fixture.tracks)
                .map_err(|error| format!("invalid Engine DJ track fixture: {error}"))?;
            Ok((library.database_uuid, tracks))
        })
    }

    fn rekordbox_tracks(
        &self,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<Vec<ImportedRekordboxTrack>, String>> + Send + '_>,
    > {
        let fixture = self.fixture.lock().unwrap().clone();
        if fixture.is_none() {
            return self.fallback.rekordbox_tracks();
        }
        Box::pin(async move {
            let fixture =
                fixture.ok_or_else(|| "Rekordbox source fixture is not installed".to_string())?;
            serde_json::from_value(fixture.tracks)
                .map_err(|error| format!("invalid Rekordbox track fixture: {error}"))
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineDjLibraryWire {
    database_uuid: String,
    track_count: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineDjPlaylistWire {
    id: i64,
    title: String,
    parent_id: Option<i64>,
    track_count: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineDjTrackWire {
    id: i64,
    path: String,
    filename: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    bpm_analyzed: Option<f64>,
    length: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RekordboxLibraryWire {
    track_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RekordboxPlaylistWire {
    id: String,
    name: String,
    parent_id: Option<String>,
    track_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RekordboxTrackWire {
    uuid: String,
    file_path: Option<String>,
    filename: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    bpm: Option<f64>,
    duration_seconds: Option<f64>,
}

impl From<EngineDjPlaylistWire> for SourcePlaylist {
    fn from(row: EngineDjPlaylistWire) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.title,
            parent_id: row.parent_id.map(|id| id.to_string()),
            track_count: row.track_count.try_into().unwrap_or(0),
        }
    }
}

impl From<RekordboxPlaylistWire> for SourcePlaylist {
    fn from(row: RekordboxPlaylistWire) -> Self {
        Self {
            id: row.id,
            name: row.name,
            parent_id: row.parent_id,
            track_count: row.track_count,
        }
    }
}

impl From<EngineDjTrackWire> for SourceTrack {
    fn from(row: EngineDjTrackWire) -> Self {
        Self {
            id: row.id.to_string(),
            file_path: Some(row.path),
            filename: Some(row.filename),
            title: row.title,
            artist: row.artist,
            album: row.album,
            bpm: row.bpm_analyzed,
            duration_seconds: row.length,
        }
    }
}

impl From<RekordboxTrackWire> for SourceTrack {
    fn from(row: RekordboxTrackWire) -> Self {
        Self {
            id: row.uuid,
            file_path: row.file_path,
            filename: row.filename,
            title: row.title,
            artist: row.artist,
            album: row.album,
            bpm: row.bpm,
            duration_seconds: row.duration_seconds,
        }
    }
}

#[cfg(feature = "agent")]
fn normalize_source_library(
    source: &TrackSource,
    value: Value,
) -> Result<SourceLibrary, LibraryError> {
    match source {
        TrackSource::EngineDj { .. } => {
            let opened: EngineDjLibraryWire =
                decode_source_fixture("engine_dj_open_library", value)?;
            Ok(SourceLibrary {
                identity: Some(opened.database_uuid),
                track_count: opened.track_count.try_into().unwrap_or(0),
            })
        }
        TrackSource::Rekordbox => {
            let opened: RekordboxLibraryWire =
                decode_source_fixture("rekordbox_open_library", value)?;
            Ok(SourceLibrary {
                identity: None,
                track_count: opened.track_count,
            })
        }
    }
}

#[cfg(feature = "agent")]
fn normalize_source_playlists(
    source: &TrackSource,
    value: Value,
) -> Result<Vec<SourcePlaylist>, LibraryError> {
    match source {
        TrackSource::EngineDj { .. } => {
            let rows: Vec<EngineDjPlaylistWire> =
                decode_source_fixture("engine_dj_list_playlists", value)?;
            Ok(rows.into_iter().map(SourcePlaylist::from).collect())
        }
        TrackSource::Rekordbox => {
            let rows: Vec<RekordboxPlaylistWire> =
                decode_source_fixture("rekordbox_list_playlists", value)?;
            Ok(rows.into_iter().map(SourcePlaylist::from).collect())
        }
    }
}

#[cfg(feature = "agent")]
fn normalize_source_tracks(
    source: &TrackSource,
    command: &'static str,
    value: Value,
) -> Result<Vec<SourceTrack>, LibraryError> {
    match source {
        TrackSource::EngineDj { .. } => {
            let rows: Vec<EngineDjTrackWire> = decode_source_fixture(command, value)?;
            Ok(rows.into_iter().map(SourceTrack::from).collect())
        }
        TrackSource::Rekordbox => {
            let rows: Vec<RekordboxTrackWire> = decode_source_fixture(command, value)?;
            Ok(rows.into_iter().map(SourceTrack::from).collect())
        }
    }
}

#[cfg(feature = "agent")]
fn decode_source_fixture<T: DeserializeOwned>(
    command: &'static str,
    value: Value,
) -> Result<T, LibraryError> {
    serde_json::from_value(value)
        .map_err(|error| LibraryError::at(command, Cause::Shape(error.to_string())))
}

/// Why a [`Library`] call did not produce a value.
///
/// `Display` names the command, because a screen shows one message for a load
/// made of several calls and "not found" alone would not say which one.
#[derive(Debug)]
pub struct LibraryError {
    /// The wire name of the command that was asked for.
    command: &'static str,
    cause: Cause,
}

#[derive(Debug)]
enum Cause {
    /// The command ran and refused, with the seam's own structure intact.
    Command(CommandError),
    /// The command answered, but not in the shape the method promised — this
    /// host's model and the seam's have drifted.
    Shape(String),
    /// The reactor never delivered the answer: the `Library` was dropped
    /// mid-call, or the task panicked.
    Cancelled(String),
}

impl LibraryError {
    /// The seam's refusal, or `None` when the call never reached one.
    ///
    /// This is where a caller tells a lost race from a failure worth
    /// reporting: match [`CommandError::Conflict`], whose `expected` and
    /// `found` are the two revisions that disagreed.
    pub fn command(&self) -> Option<&CommandError> {
        match &self.cause {
            Cause::Command(error) => Some(error),
            Cause::Shape(_) | Cause::Cancelled(_) => None,
        }
    }

    fn at(command: &'static str, cause: Cause) -> Self {
        Self { command, cause }
    }
}

impl fmt::Display for LibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let command = self.command;
        match &self.cause {
            Cause::Command(error) => write!(formatter, "{command}: {error}"),
            Cause::Shape(error) => write!(formatter, "{command}: unexpected result shape: {error}"),
            Cause::Cancelled(error) => write!(formatter, "{command} did not complete: {error}"),
        }
    }
}

impl std::error::Error for LibraryError {}

/// A connection to the real Luma library.
pub struct Library {
    services: Arc<AppServices>,
    /// Who the app database admitted at startup. `None` is the guest
    /// namespace, which is also what an un-signed-in host gets.
    user_id: Option<String>,
    /// Owns the reactor every dispatched command runs on. Dropping it cancels
    /// in-flight work, so it lives exactly as long as the `Library`.
    runtime: tokio::runtime::Runtime,
    import_progress: tokio::sync::broadcast::Sender<TrackImportProgress>,
    /// Drives agent turns with this client instead of a configured provider.
    /// The one injection seam the harness needs, and the shipped app never
    /// sets: without it the loop resolves its model and key the way it does
    /// headless.
    model: Option<Arc<dyn ModelClient>>,
    /// Exposes these tools instead of the agent kind's own set. The seam a
    /// subagent will use too, which is why it is the registry and not a flag:
    /// a surface built by a second path could drift from the parent's.
    tools: Option<ToolRegistry>,
    #[cfg(feature = "agent")]
    source_fixture: Arc<Mutex<Option<SourceAdapterFixture>>>,
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

        let (progress_tx, _) = tokio::sync::broadcast::channel(256);
        let events = Events::new(LibraryEvents {
            import_progress: progress_tx.clone(),
        });
        #[cfg(feature = "agent")]
        let source_fixture = Arc::new(Mutex::new(None));
        #[cfg(feature = "agent")]
        let import_sources: Arc<dyn TrackSources> = Arc::new(FixtureTrackSources {
            fixture: Arc::clone(&source_fixture),
            fallback: system_track_sources(),
        });
        let (services, user_id) = runtime.block_on(async {
            let db = init_app_db_at(storage.path()).await?;
            let state_db = init_state_db_at(storage.path()).await?;
            // Admission is host bootstrap, not a command: until it is armed
            // every `auth_visible_*` view is empty, so a host that skipped
            // this would show an empty library and no error to explain it.
            let user_id = bootstrap_host_admission(&db.0, &state_db.0).await?;
            // This host runs the agent, and the agent's only tool is Python —
            // so it resolves the same managed environment the Tauri app
            // created, through the one resolver every non-Tauri host shares.
            // Resolution is lazy: a machine with no venv yet still opens.
            let workspaces = Arc::new(headless_env::workspace_service(
                &storage,
                headless_env::cache_dir()?,
            ));
            let services = AppServices::headless(db, state_db, storage, fixtures_root, workspaces)
                .with_events(events);
            #[cfg(feature = "agent")]
            let services = services.with_track_sources(import_sources);
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
            import_progress: progress_tx,
            model: None,
            tools: None,
            #[cfg(feature = "agent")]
            source_fixture,
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

    /// Replace DJ source I/O with raw adapter answers in the agent harness.
    /// Normalization remains production code; only external database reads
    /// are substituted.
    #[cfg(feature = "agent")]
    pub fn set_source_adapter_fixture(&mut self, fixture: SourceAdapterFixture) {
        *self.source_fixture.lock().unwrap() = Some(fixture);
    }

    #[cfg(feature = "agent")]
    fn source_adapter_fixture(&self) -> Option<SourceAdapterFixture> {
        self.source_fixture.lock().unwrap().clone()
    }

    /// The signed-in principal, or `None` for the guest namespace. Identity is
    /// fixed for the life of the process here — this host has no sign-in — so
    /// it is a field rather than a query.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    /// Every venue in the library, newest activity first.
    pub fn venues(&self) -> impl Future<Output = Result<Vec<Venue>, LibraryError>> + use<> {
        self.call("list_venues", json!({}))
    }

    /// Create a venue and return the durable row written by the catalog.
    pub fn create_venue(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> impl Future<Output = Result<Venue, LibraryError>> + use<> {
        self.call(
            "create_venue",
            json!({ "name": name, "description": description }),
        )
    }

    /// The track browser's rows for `venue_id`, carrying that venue's
    /// annotation counts.
    pub fn tracks(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<TrackBrowserRow>, LibraryError>> + use<> {
        self.call("list_tracks_enriched", json!({ "venueId": venue_id }))
    }

    /// Every visible track in Luma, newest first, without a venue decoration.
    pub fn all_tracks(
        &self,
    ) -> impl Future<Output = Result<Vec<TrackBrowserRow>, LibraryError>> + use<> {
        self.call("list_tracks_enriched", json!({ "venueId": null }))
    }

    /// Import from disk or either DJ source through one typed request/result
    /// contract. The returned rows are already durable; analysis continues on
    /// the service-owned task group after this future and its dialog are gone.
    pub fn import_tracks(
        &self,
        request: TrackImportRequest,
    ) -> impl Future<Output = Result<TrackImportResult, LibraryError>> + use<> {
        let files = match &request {
            TrackImportRequest::Files(paths) => Some(self.call(
                "import_tracks",
                json!({
                    "filePaths": paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>()
                }),
            )),
            _ => None,
        };
        let mut request_error = None;
        let engine = match &request {
            TrackImportRequest::Source {
                source: TrackSource::EngineDj { library_path },
                track_ids,
            } => {
                match track_ids
                    .iter()
                    .map(|id| id.parse::<i64>())
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(track_ids) => Some(self.call(
                        "engine_dj_import_tracks",
                        json!({ "libraryPath": library_path, "trackIds": track_ids }),
                    )),
                    Err(error) => {
                        request_error = Some(LibraryError::at(
                            "engine_dj_import_tracks",
                            Cause::Shape(format!("Engine DJ track id is not an integer: {error}")),
                        ));
                        None
                    }
                }
            }
            _ => None,
        };
        let rekordbox = match &request {
            TrackImportRequest::Source {
                source: TrackSource::Rekordbox,
                track_ids,
            } => Some(self.call(
                "rekordbox_import_tracks",
                json!({ "trackUuids": track_ids }),
            )),
            _ => None,
        };
        async move {
            if let Some(error) = request_error {
                return Err(error);
            }
            if let Some(files) = files {
                return files.await;
            }
            if let Some(engine) = engine {
                return engine.await;
            }
            if let Some(rekordbox) = rekordbox {
                return rekordbox.await;
            }
            unreachable!("every import request has one dispatch path")
        }
    }

    /// Subscribe to host-neutral import progress. Lag is explicit rather than
    /// silently replaying stale state; callers reconcile from `all_tracks`.
    pub fn import_progress(&self) -> tokio::sync::broadcast::Receiver<TrackImportProgress> {
        self.import_progress.subscribe()
    }

    /// Open either supported DJ catalog through one GPUI-owned shape.
    pub fn source_library(
        &self,
        source: TrackSource,
    ) -> impl Future<Output = Result<SourceLibrary, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self
            .source_adapter_fixture()
            .map(|fixture| normalize_source_library(&source, fixture.library));
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<SourceLibrary, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                Some(self.call::<EngineDjLibraryWire>(
                    "engine_dj_open_library",
                    json!({ "libraryPath": library_path }),
                ))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => {
                Some(self.call::<RekordboxLibraryWire>("rekordbox_open_library", json!({})))
            }
            _ => None,
        };
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(opened), None) => {
                    let opened = opened.await?;
                    Ok(SourceLibrary {
                        identity: Some(opened.database_uuid),
                        track_count: opened.track_count.try_into().unwrap_or(0),
                    })
                }
                (None, Some(opened)) => {
                    let opened = opened.await?;
                    Ok(SourceLibrary {
                        identity: None,
                        track_count: opened.track_count,
                    })
                }
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// Flat source playlists; `parent_id` is enough for the dialog to build a
    /// shared tree without knowing which DJ product produced it.
    pub fn source_playlists(
        &self,
        source: TrackSource,
    ) -> impl Future<Output = Result<Vec<SourcePlaylist>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self
            .source_adapter_fixture()
            .map(|fixture| normalize_source_playlists(&source, fixture.playlists));
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourcePlaylist>, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                Some(self.call::<Vec<EngineDjPlaylistWire>>(
                    "engine_dj_list_playlists",
                    json!({ "libraryPath": library_path }),
                ))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => {
                Some(self.call::<Vec<RekordboxPlaylistWire>>("rekordbox_list_playlists", json!({})))
            }
            _ => None,
        };
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(rows), None) => {
                    Ok(rows.await?.into_iter().map(SourcePlaylist::from).collect())
                }
                (None, Some(rows)) => {
                    Ok(rows.await?.into_iter().map(SourcePlaylist::from).collect())
                }
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// The whole selected source library in its native stable ordering.
    pub fn source_tracks(
        &self,
        source: TrackSource,
    ) -> impl Future<Output = Result<Vec<SourceTrack>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self
            .source_adapter_fixture()
            .map(|fixture| normalize_source_tracks(&source, "source_list_tracks", fixture.tracks));
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourceTrack>, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                Some(self.call::<Vec<EngineDjTrackWire>>(
                    "engine_dj_list_tracks",
                    json!({ "libraryPath": library_path }),
                ))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => {
                Some(self.call::<Vec<RekordboxTrackWire>>("rekordbox_list_tracks", json!({})))
            }
            _ => None,
        };
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(rows), None) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                (None, Some(rows)) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// One playlist/crate through the same row shape as [`Self::source_tracks`].
    pub fn source_playlist_tracks(
        &self,
        source: TrackSource,
        playlist_id: &str,
    ) -> impl Future<Output = Result<Vec<SourceTrack>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self.source_adapter_fixture().map(|fixture| {
            fixture
                .playlist_tracks
                .get(playlist_id)
                .cloned()
                .ok_or_else(|| {
                    LibraryError::at(
                        "source_playlist_tracks",
                        Cause::Shape(format!("fixture has no playlist {playlist_id}")),
                    )
                })
                .and_then(|value| normalize_source_tracks(&source, "source_playlist_tracks", value))
        });
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourceTrack>, LibraryError>> = None;
        let engine_id = match &source {
            TrackSource::EngineDj { .. } => playlist_id.parse::<i64>().ok(),
            TrackSource::Rekordbox => None,
        };
        let invalid_engine_id = fixture.is_none()
            && matches!(&source, TrackSource::EngineDj { .. })
            && engine_id.is_none();
        let engine = match (&source, engine_id, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, Some(playlist_id), true) => {
                Some(self.call::<Vec<EngineDjTrackWire>>(
                    "engine_dj_get_playlist_tracks",
                    json!({ "libraryPath": library_path, "playlistId": playlist_id }),
                ))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => Some(self.call::<Vec<RekordboxTrackWire>>(
                "rekordbox_get_playlist_tracks",
                json!({ "playlistId": playlist_id }),
            )),
            _ => None,
        };
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            if invalid_engine_id {
                return Err(LibraryError::at(
                    "engine_dj_get_playlist_tracks",
                    Cause::Shape("playlist id was not an Engine DJ integer".into()),
                ));
            }
            match (engine, rekordbox) {
                (Some(rows), None) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                (None, Some(rows)) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// Search either source and return the same row contract.
    pub fn search_source_tracks(
        &self,
        source: TrackSource,
        query: &str,
    ) -> impl Future<Output = Result<Vec<SourceTrack>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self.source_adapter_fixture().map(|fixture| {
            fixture
                .searches
                .get(query)
                .cloned()
                .ok_or_else(|| {
                    LibraryError::at(
                        "source_search_tracks",
                        Cause::Shape(format!("fixture has no search {query:?}")),
                    )
                })
                .and_then(|value| normalize_source_tracks(&source, "source_search_tracks", value))
        });
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourceTrack>, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                Some(self.call::<Vec<EngineDjTrackWire>>(
                    "engine_dj_search_tracks",
                    json!({ "libraryPath": library_path, "query": query }),
                ))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => Some(self.call::<Vec<RekordboxTrackWire>>(
                "rekordbox_search_tracks",
                json!({ "query": query }),
            )),
            _ => None,
        };
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(rows), None) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                (None, Some(rows)) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// Display names for other people's uids, as `uid -> name`. Sparse: a uid
    /// the directory does not know is simply absent.
    pub fn display_names(
        &self,
        uids: Vec<String>,
    ) -> impl Future<Output = Result<HashMap<String, String>, LibraryError>> + use<> {
        self.call("get_display_names", json!({ "uids": uids }))
    }

    /// Every pattern in the library.
    pub fn patterns(
        &self,
    ) -> impl Future<Output = Result<Vec<PatternSummary>, LibraryError>> + use<> {
        self.call("list_patterns", json!({}))
    }

    /// The node catalogue: what every `typeId` in a graph means. Static for
    /// the life of the process, so a screen reads it once.
    pub fn node_types(
        &self,
    ) -> impl Future<Output = Result<Vec<NodeTypeDef>, LibraryError>> + use<> {
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
    ) -> impl Future<Output = Result<GraphDocument, LibraryError>> + use<> {
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
    ) -> impl Future<Output = Result<GraphEditResult, LibraryError>> + use<> {
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
    ) -> impl Future<Output = Result<Vec<ScoreSummary>, LibraryError>> + use<> {
        self.call(
            "list_scores_for_track",
            json!({ "trackId": track_id, "venueId": venue_id }),
        )
    }

    /// Create another score for a track. Multiple scores per track/venue are
    /// intentional; use [`Self::ensure_track_in_venue`] for the Add action.
    /// `request_id` only makes a retry of this exact creation idempotent.
    pub fn create_score(
        &self,
        request_id: &str,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
    ) -> impl Future<Output = Result<Score, LibraryError>> + use<> {
        self.call(
            "create_score",
            json!({
                "requestId": request_id,
                "trackId": track_id,
                "venueId": venue_id,
                "name": name,
            }),
        )
    }

    /// Atomically add a track to a venue. Repeated Add actions return the
    /// existing score even when they carry different request ids.
    pub fn ensure_track_in_venue(
        &self,
        request_id: &str,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
    ) -> impl Future<Output = Result<Score, LibraryError>> + use<> {
        self.call(
            "ensure_venue_score",
            json!({
                "requestId": request_id,
                "trackId": track_id,
                "venueId": venue_id,
                "name": name,
            }),
        )
    }

    /// One score's clips.
    pub fn track_scores(
        &self,
        score_id: &str,
    ) -> impl Future<Output = Result<Vec<TrackScore>, LibraryError>> + use<> {
        self.call("list_track_scores", json!({ "scoreId": score_id }))
    }

    /// The rendered envelope of a track, at both resolutions. Large — a full
    /// track is tens of thousands of floats — so a screen reads it once.
    pub fn track_waveform(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<TrackWaveform, LibraryError>> + use<> {
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
    ) -> impl Future<Output = Result<WaveformWindow, LibraryError>> + use<> {
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
    ) -> impl Future<Output = Result<Option<BeatGrid>, LibraryError>> + use<> {
        self.call("get_track_beats", json!({ "trackId": track_id }))
    }

    /// Create one clip.
    ///
    /// `request_id` is the idempotency key and the clip's id is derived from
    /// it, so a replay returns the clip the first attempt made instead of
    /// minting a second one at the same spot. Read the id back from
    /// [`TrackEditResult::created_clip_id`]; do not assume it.
    pub fn create_clip(
        &self,
        input: CreateTrackScoreInput,
    ) -> impl Future<Output = Result<TrackEditResult, LibraryError>> + use<> {
        self.call("create_track_score", json!({ "payload": input }))
    }

    /// Delete one clip.
    ///
    /// `operation_id` is the idempotency key, and the containing score is
    /// named explicitly so a replay stays resolvable *after* the clip row is
    /// gone — which is the case that matters, because the response a client
    /// lost is the one it will retry.
    pub fn delete_clip(
        &self,
        input: DeleteTrackScoreInput,
    ) -> impl Future<Output = Result<TrackEditResult, LibraryError>> + use<> {
        self.call("delete_track_score", json!({ "payload": input }))
    }

    /// Publish one complete clip list as a single compare-and-swap.
    ///
    /// **This is the only way to express a gesture that touches more than one
    /// clip** — duplicate, split, delete-selection, paste, a group drag's
    /// z-index change. Fanning such a gesture out into per-clip calls would
    /// let it half-land, and the intermediate states are ones the editor's own
    /// rules forbid (a clip gone before its replacement lands, a lane briefly
    /// empty).
    ///
    /// `base` is the list the caller edited and `candidate` is what it should
    /// become; the seam refuses the write if the stored list has moved on
    /// since `base`. A candidate clip whose id is not in `base` is a create,
    /// an id in `base` and not in `candidate` is a delete, and
    /// [`TrackEditResult::id_map`] carries the ids the seam allocated for the
    /// creates. `operation_id` is the idempotency key for the whole batch.
    ///
    /// The result's `clips` are authoritative and may be *newer* than the
    /// candidate on a replay, so adopt them rather than the candidate.
    pub fn replace_clips(
        &self,
        score_id: &str,
        track_id: &str,
        base: &[TrackScore],
        candidate: &[TrackScore],
        operation_id: &str,
    ) -> impl Future<Output = Result<TrackEditResult, LibraryError>> + use<> {
        self.call(
            "replace_track_scores",
            json!({
                "scoreId": score_id,
                "trackId": track_id,
                "baseScores": base,
                "scores": candidate,
                "operationId": operation_id,
            }),
        )
    }

    /// Decode a track and hand it to the audio host, from 0.0. Slow — this is
    /// the decode — and it must land before the transport does anything.
    pub fn load_audio(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_load_track", json!({ "trackId": track_id }))
    }

    /// Start playback from wherever the transport currently is.
    pub fn play(&self) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_play", json!({}))
    }

    pub fn pause(&self) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_pause", json!({}))
    }

    /// Move the transport to `seconds` from the loaded segment's start, which
    /// for a whole track is absolute track time.
    pub fn seek(&self, seconds: f32) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_seek", json!({ "seconds": seconds }))
    }

    /// Loop `region`, or stop looping when it is `None`.
    ///
    /// One argument for what the seam takes as two bounds, because the host
    /// turns looping *on* exactly when both are given — so a pair of
    /// independent `Option`s could spell a state that means nothing, and this
    /// one cannot.
    pub fn set_loop_region(
        &self,
        region: Option<(f32, f32)>,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call(
            "host_set_loop_region",
            json!({
                "startSeconds": region.map(|(start, _)| start),
                "endSeconds": region.map(|(_, end)| end),
            }),
        )
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
    ) -> impl Future<Output = Result<HostAudioSnapshot, LibraryError>> + use<> {
        self.call_after("host_snapshot", json!({}), after)
    }

    /// Every app setting, typed and defaulted.
    pub fn settings(&self) -> impl Future<Output = Result<AppSettings, LibraryError>> + use<> {
        self.call("get_settings", json!({}))
    }

    /// Read one host-session value. App navigation uses this for last-open
    /// state; it is intentionally not an app setting with subsystem effects.
    pub fn get_session_item(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<String>, LibraryError>> + use<> {
        self.call("get_session_item", json!({ "key": key }))
    }

    /// Persist one host-session value.
    pub fn set_session_item(
        &self,
        key: &str,
        value: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("set_session_item", json!({ "key": key, "value": value }))
    }

    /// Remove one host-session value.
    pub fn remove_session_item(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("remove_session_item", json!({ "key": key }))
    }

    /// Write one setting. The seam owns what a key means and what a write
    /// costs (Art-Net and audio both reload on one), so the caller's only job
    /// afterwards is to read the settings back — which is also the only honest
    /// proof the write landed.
    pub fn set_setting(
        &self,
        key: &str,
        value: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> {
        self.call("set_setting", json!({ "key": key, "value": value }))
    }

    /// Every conversation `scope` names, newest first.
    ///
    /// `agent_thread_list` filters on three of a scope's six fields, so the
    /// other three are narrowed here — through [`ThreadScope::matches`], which
    /// is the one statement of what "this thread is about that subject" means.
    /// A caller that filtered its own rows would be a second one.
    pub fn threads(
        &self,
        scope: &ThreadScope,
    ) -> impl Future<Output = Result<Vec<AgentThread>, LibraryError>> + use<> {
        let scope = scope.clone();
        let listed = self.call::<Vec<AgentThread>>(
            "agent_thread_list",
            json!({
                "agentKind": scope.agent_kind.as_str(),
                "subjectKind": scope.subject_kind.as_str(),
                "subjectId": scope.subject_id,
            }),
        );
        async move {
            let threads = listed.await?;
            Ok(threads
                .into_iter()
                .filter(|thread| scope.matches(thread))
                .collect())
        }
    }

    /// Retitle a conversation. `None` clears the title back to whatever the
    /// transcript implies, which is what the durable model does with it.
    pub fn rename_thread(
        &self,
        thread_id: &str,
        title: Option<&str>,
    ) -> impl Future<Output = Result<AgentThread, LibraryError>> + use<> {
        self.call(
            "agent_thread_rename",
            json!({ "threadId": thread_id, "title": title }),
        )
    }

    /// Delete a conversation and its transcript.
    pub fn delete_thread(
        &self,
        thread_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("agent_thread_delete", json!({ "threadId": thread_id }))
    }

    /// Install `track_id`'s persisted score as the render engine's active
    /// scene, so [`Self::sample_universe`] has something to sample.
    ///
    /// Omitting the annotation list is what says "use the score rows" — an
    /// empty list would be an authoritative empty document that clears the
    /// scene instead (see `dispatch::handlers::compositor`).
    pub fn composite_track(
        &self,
        track_id: &str,
        venue_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call(
            "composite_track",
            json!({ "trackId": track_id, "venueId": venue_id }),
        )
    }

    /// Everything the 3D view needs to draw a venue: its patched fixtures, its
    /// stage pieces, and the definition behind every distinct fixture path.
    ///
    /// One method rather than three because they are only meaningful together
    /// — a rig with its definitions missing is not a partial rig, it is fixtures
    /// with no geometry — and because the definitions cannot even be *asked*
    /// for until the fixture list has landed. A screen that orchestrated that
    /// would be a screen that knew the second call depends on the first.
    pub fn venue_rig(&self, venue_id: &str) -> impl Future<Output = Result<Rig, LibraryError>> {
        let services = Arc::clone(&self.services);
        let venue = json!({ "venueId": venue_id });
        let task = self.runtime.spawn(async move {
            let fixtures: Vec<PatchedFixture> =
                command(&services, "get_patched_fixtures", &venue).await?;
            let pieces: Vec<StagePiece> = command(&services, "list_stage_pieces", &venue).await?;
            let mut definitions = HashMap::new();
            for path in fixtures
                .iter()
                .map(|f| f.fixture_path.clone())
                .collect::<std::collections::BTreeSet<_>>()
            {
                let args = json!({ "path": path });
                // A venue can outlive a fixture bundle: a definition that no
                // longer resolves leaves that fixture undrawn rather than
                // taking the whole rig down with it.
                if let Ok(def) = command(&services, "get_fixture_definition", &args).await {
                    definitions.insert(path, def);
                }
            }
            Ok::<_, LibraryError>(Rig {
                fixtures,
                pieces,
                definitions,
            })
        });
        async move {
            task.await.map_err(|e| {
                LibraryError::at("get_patched_fixtures", Cause::Cancelled(e.to_string()))
            })?
        }
    }

    /// The installed scene evaluated at one absolute track time, or `None`
    /// when no scene is installed.
    ///
    /// Synchronous, unlike everything else here, and deliberately: this is read
    /// at paint time, where an awaited answer is a frame late and a frame late
    /// is a beam that lags the music. `Scene::render` is pure in `t`, so there
    /// is no ordering or warmup contract to violate by calling it from a
    /// painter — see `eval::scene`.
    pub fn sample_universe(&self, t: f32) -> Option<UniverseState> {
        self.services.render_engine().sample(t)
    }

    /// Where the transport is, right now, in track seconds. Synchronous for
    /// the same reason as [`Self::sample_universe`].
    pub fn render_time(&self) -> f32 {
        self.services.host_audio().render_time()
    }

    /// What the transport is doing, right now — whether it is running, and how
    /// long what it is running is.
    ///
    /// Separate from [`Self::render_time`] because they answer different
    /// questions: that one is the absolute instant a frame is *of*, this one is
    /// the state a transport control is a picture of. The snapshot's own
    /// `current_time` is relative to the loaded segment and is deliberately not
    /// used as a frame's `t`.
    pub fn transport(&self) -> HostAudioSnapshot {
        self.services.host_audio().snapshot()
    }

    /// Run one command on the Tokio runtime and decode its result.
    ///
    /// The returned future is detached from `&self` so a caller can hold it
    /// across a GPUI task boundary without borrowing the `Library`.
    fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        name: &'static str,
        args: Value,
    ) -> impl Future<Output = Result<T, LibraryError>> + use<T> {
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
    ) -> impl Future<Output = Result<T, LibraryError>> + use<T> {
        let services = Arc::clone(&self.services);
        let task = self.runtime.spawn(async move {
            if !after.is_zero() {
                tokio::time::sleep(after).await;
            }
            command(&services, name, &args).await
        });
        async move {
            task.await
                .map_err(|error| LibraryError::at(name, Cause::Cancelled(error.to_string())))?
        }
    }
}

/// One dispatched command, decoded. The step [`Library::call_after`] and
/// [`Library::venue_rig`] share — the latter runs several of these in one task
/// because each depends on the last.
async fn command<T: DeserializeOwned>(
    services: &AppServices,
    name: &'static str,
    args: &Value,
) -> Result<T, LibraryError> {
    let value: Value = dispatch(services, name, args)
        .await
        .map_err(|error| LibraryError::at(name, Cause::Command(error)))?;
    serde_json::from_value(value)
        .map_err(|error| LibraryError::at(name, Cause::Shape(error.to_string())))
}

/// One venue's 3D contents, as [`Library::venue_rig`] resolves them.
///
/// Rebuilt on venue change, never per frame: geometry is what a venue *is*,
/// and only the light state on top of it moves (spec §2.2).
pub struct Rig {
    /// Every patched fixture, in patch order.
    pub fixtures: Vec<PatchedFixture>,
    /// Every stage piece, parent links not yet resolved.
    pub pieces: Vec<StagePiece>,
    /// Keyed by `fixture_path`; a path whose bundle no longer resolves is
    /// absent rather than an error.
    pub definitions: HashMap<String, FixtureDefinition>,
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
