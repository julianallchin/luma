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

use std::collections::{BTreeMap, HashMap};
#[cfg(feature = "agent")]
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

use luma_lib::agent::model::ModelClient;
use luma_lib::agent::tools::ToolRegistry;
use luma_lib::agent::{AgentService, ThreadScope};
use luma_lib::agent_execution::headless_env;
use luma_lib::database::local::auth::{arm_write_admission, bootstrap_headless_admission};
use luma_lib::database::local::database::init_app_db_at;
use luma_lib::database::local::state::init_state_db_at;
use luma_lib::dispatch::{dispatch, AppServices, CommandError, EventSink, Events, SharedServices};
#[cfg(feature = "agent")]
use luma_lib::dispatch::{
    system_track_sources, ImportedEngineDjTrack, ImportedRekordboxTrack, TrackSources,
};
use luma_lib::host_audio::HostAudioSnapshot;
use luma_lib::models::agent_threads::AgentThread;
use luma_lib::models::fixtures::{FixtureDefinition, FixtureEntry, PatchedFixture};
use luma_lib::models::groups::FixtureGroup;
use luma_lib::models::node_graph::{
    BeatGrid, Graph, GraphContext, NodeTypeDef, PatternArgDef, RunResult,
};
use luma_lib::models::patterns::{AnnotationPreview, PatternSummary};
use luma_lib::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, ScoreSummary, TrackScore,
};
use luma_lib::models::selection::Selection;
use luma_lib::models::tracks::{TrackBrowserRow, TrackImportProgress, TrackImportResult};
use luma_lib::models::universe::UniverseState;
use luma_lib::models::distribute::{DistributeLayout, DistributeReport};
use luma_lib::models::venue_graph::{PlacementReport, ResolvedVenue, VenueGraphRows};
use luma_lib::models::venues::Venue;
use luma_lib::models::waveforms::{TrackWaveform, WaveformWindow};
use luma_lib::services::fixtures as fixtures_service;
use luma_lib::services::graph_documents::{GraphDocument, GraphEditResult};
use luma_lib::services::track_edits::{TrackClip, TrackEditResult};
use luma_lib::settings::AppSettings;
use luma_lib::storage::StorageRoot;

#[cfg(feature = "agent")]
#[derive(Clone, Debug, Default)]
pub struct NavigationFixture {
    /// Hold the venue catalogue so an outside-in test can observe the real
    /// loading route instead of racing a local SQLite read.
    pub venues_delay: Duration,
    /// Substitute a catalogue failure at the Library boundary. The shipped
    /// app never installs a fixture.
    pub venues_error: Option<String>,
    /// Per-venue delays for forcing two real track reads to land out of order.
    pub track_delays: HashMap<String, Duration>,
    /// Per-call catalogue timing/failure, consumed in request order. This is
    /// the deterministic seam for proving a dismissed picker cannot be
    /// overwritten by its older answer.
    pub catalogue_responses: Vec<(Duration, Option<String>)>,
    /// Per-call venue reads, matched and consumed by venue id. Rows still come
    /// from the real dispatcher; the optional title rewrite makes two reads of
    /// the same venue observably distinct to the outside-in harness.
    pub track_responses: Vec<(String, Duration, Option<String>)>,
    /// Per-write delays consumed by the FIFO session actor. A delayed A then
    /// immediate B must still persist B.
    pub session_write_delays: Vec<Duration>,
}

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
#[derive(Clone, Debug)]
pub struct SourceSearchFixtureResponse {
    pub query: String,
    pub delay: Duration,
    pub rows: Value,
}

#[cfg(feature = "agent")]
struct FixtureTrackSources {
    fixture: Arc<Mutex<Option<SourceAdapterFixture>>>,
    import_delay: Arc<Mutex<Option<Duration>>>,
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
        let delay = *self.import_delay.lock().unwrap();
        if fixture.is_none() {
            return self.fallback.engine_tracks(library_path);
        }
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
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
        let delay = *self.import_delay.lock().unwrap();
        if fixture.is_none() {
            return self.fallback.rekordbox_tracks();
        }
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
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

/// Who the app database admitted, as the app names them.
///
/// Two halves that are not interchangeable: [`Self::id`] is the proven
/// principal every row is stamped with, [`Self::email`] is the address the
/// session was obtained with and exists only to put a name on screen. A
/// session whose blob carries no address is still a perfectly good identity —
/// hence [`Self::label`], which never shows the id.
#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: Option<String>,
}

impl Account {
    /// What to call this account in front of a person.
    ///
    /// The id is deliberately *not* the fallback. A uuid is not a name — it
    /// tells the reader nothing they can act on, and reads as a bug in a row
    /// that is supposed to say who they are. "Signed in" is the honest answer
    /// to a session that proved someone without saying which address it was
    /// issued to.
    #[must_use]
    pub fn label(&self) -> &str {
        match self.email.as_deref() {
            Some(email) if !email.is_empty() => email,
            _ => "Signed in",
        }
    }
}

/// A connection to the real Luma library.
pub struct Library {
    services: SharedServices,
    /// Who the app database admitted. `None` is the guest namespace, which is
    /// also what an un-signed-in host gets.
    ///
    /// Shared and mutable because identity is no longer fixed for the life of
    /// the process: signing in and signing out both move it, and every row a
    /// screen writes afterwards is stamped with what this says. The database's
    /// `auth_write_admission.active_uid` remains the authority — this is the
    /// cache of it, written through at the three moments it can change.
    ///
    /// One cache, not two: the id and the address it is shown under move
    /// together at every one of those moments, and a screen reading them from
    /// separate places would be able to name the previous account.
    account: Arc<Mutex<Option<Account>>>,
    /// Why boot admitted the guest namespace despite finding credentials —
    /// see [`Library::lapsed`]. Fixed at open: it is a fact about how this
    /// process started, and signing in afterwards does not rewrite it.
    lapsed: Option<String>,
    /// Owns the reactor every dispatched command runs on. Dropping it cancels
    /// in-flight work, so it lives exactly as long as the `Library`.
    runtime: tokio::runtime::Runtime,
    import_progress: tokio::sync::broadcast::Sender<TrackImportProgress>,
    /// Session navigation writes are an actor, not detached calls. Its FIFO is
    /// the durable ordering boundary: selecting B after A cannot leave A as
    /// the last-open venue merely because A's SQLite task completed later.
    session_writes: tokio::sync::mpsc::UnboundedSender<SessionWrite>,
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
    #[cfg(feature = "agent")]
    source_fixture_delay: Option<Duration>,
    #[cfg(feature = "agent")]
    source_search_fixture: Arc<Mutex<VecDeque<SourceSearchFixtureResponse>>>,
    #[cfg(feature = "agent")]
    source_import_fixture_delay: Arc<Mutex<Option<Duration>>>,
    #[cfg(feature = "agent")]
    navigation_fixture: Arc<Mutex<NavigationFixture>>,
}

struct SessionWrite {
    command: &'static str,
    args: Value,
    reply: tokio::sync::oneshot::Sender<Result<(), LibraryError>>,
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
        let source_import_fixture_delay = Arc::new(Mutex::new(None));
        #[cfg(feature = "agent")]
        let navigation_fixture = Arc::new(Mutex::new(NavigationFixture::default()));
        #[cfg(feature = "agent")]
        let import_sources: Arc<dyn TrackSources> = Arc::new(FixtureTrackSources {
            fixture: Arc::clone(&source_fixture),
            import_delay: Arc::clone(&source_import_fixture_delay),
            fallback: system_track_sources(),
        });
        let (services, account, lapsed) = runtime.block_on(async {
            let db = init_app_db_at(storage.path()).await?;
            let state_db = init_state_db_at(storage.path()).await?;
            // Admission is host bootstrap, not a command: until it is armed
            // every `auth_visible_*` view is empty, so a host that skipped
            // this would show an empty library and no error to explain it.
            //
            // Offline, and deliberately: `bootstrap_headless_admission`
            // proves the stored session against the host proof beside it and
            // never spends the single-use refresh token. Whether Supabase
            // still honours that token is a question for the network, and a
            // question the network can refuse to answer — asking it here is
            // what made a dead token, or an airport, into a launch failure.
            //
            // So auth is never a reason not to open. Anything short of a
            // proven principal is the guest namespace, which is a *working*
            // library: guest rows carry no `uid` and the admission triggers
            // admit those unconditionally. The shell shows sign-in over it
            // (see `crate::signin`); the user can also just work offline.
            //
            // Arming guest explicitly rather than trusting the bootstrap's
            // own arming covers the one state it deliberately leaves closed —
            // a committed sign-out still waiting for a renderer to consume its
            // journal. That journal outlives this and is consumed by the next
            // sign-in's identity switch; leaving admission shut instead would
            // fail every read with "App database admission is closed".
            let (user_id, lapsed) = match bootstrap_headless_admission(&db.0, &state_db.0).await {
                Ok(Some(user_id)) => (Some(user_id), None),
                // Nothing stored, or a sign-out that already committed. The
                // guest namespace on purpose, so nothing is owed the user.
                Ok(None) => {
                    arm_write_admission(&db.0, None).await?;
                    (None, None)
                }
                // Credentials are on this machine and cannot be turned into a
                // principal. Only signing in again repairs that, which is the
                // one state that owes the user the gate.
                Err(error) => {
                    eprintln!("[luma] the stored session no longer proves anyone: {error}");
                    arm_write_admission(&db.0, None).await?;
                    (None, Some(error))
                }
            };
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
            // The address to name the principal by, out of the same stored
            // snapshot the admission above was proven from. `current_account`
            // makes no request — see its handler — which is the property this
            // line depends on: boot never touches the network, so a launch on
            // a plane still knows whose library this is.
            //
            // A label that cannot be read is not a launch failure and not a
            // different identity: the id is already proven, so it stands alone.
            let email = match command::<Option<Account>>(&services, "current_account", &Value::Null)
                .await
            {
                Ok(Some(account)) if Some(&account.id) == user_id.as_ref() => account.email,
                _ => None,
            };
            let account = user_id.map(|id| Account { id, email });
            Ok::<_, String>((services, account, lapsed))
        })?;

        let services = services.into_shared();
        let (session_writes, mut session_write_rx) =
            tokio::sync::mpsc::unbounded_channel::<SessionWrite>();
        let session_services = services.clone();
        #[cfg(feature = "agent")]
        let session_navigation_fixture = Arc::clone(&navigation_fixture);
        runtime.spawn(async move {
            while let Some(write) = session_write_rx.recv().await {
                #[cfg(feature = "agent")]
                let delay = {
                    let mut fixture = session_navigation_fixture.lock().unwrap();
                    if fixture.session_write_delays.is_empty() {
                        Duration::ZERO
                    } else {
                        fixture.session_write_delays.remove(0)
                    }
                };
                #[cfg(not(feature = "agent"))]
                let delay = Duration::ZERO;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let result = command(&session_services, write.command, &write.args).await;
                let _ = write.reply.send(result);
            }
        });

        Ok(Self {
            services,
            account: Arc::new(Mutex::new(account)),
            lapsed,
            runtime,
            import_progress: progress_tx,
            session_writes,
            model: None,
            tools: None,
            #[cfg(feature = "agent")]
            source_fixture,
            #[cfg(feature = "agent")]
            source_fixture_delay: None,
            #[cfg(feature = "agent")]
            source_search_fixture: Arc::new(Mutex::new(VecDeque::new())),
            #[cfg(feature = "agent")]
            source_import_fixture_delay,
            #[cfg(feature = "agent")]
            navigation_fixture,
        })
    }

    /// The agent loop, on the reactor its database needs.
    ///
    /// A turn outlives the call that starts it, so it cannot borrow this
    /// `Library`; [`AgentService`] owns a shared handle on the same services
    /// every dispatched command runs on, which is what keeps the agent's
    /// writes and the app's reads inside one transaction boundary.
    pub fn agent(&self) -> luma_chat::Agent {
        let mut service = AgentService::new(self.services.clone());
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

    /// Keep a fixture read pending long enough for an outside-in loading-state
    /// assertion. Production adapters and their timing are untouched.
    #[cfg(feature = "agent")]
    pub fn set_source_adapter_fixture_delay(&mut self, delay: Duration) {
        self.source_fixture_delay = Some(delay);
    }

    #[cfg(feature = "agent")]
    pub fn set_source_search_fixture_responses(
        &mut self,
        responses: Vec<SourceSearchFixtureResponse>,
    ) {
        *self.source_search_fixture.lock().unwrap() = responses.into();
    }

    #[cfg(feature = "agent")]
    pub fn set_source_import_fixture_delay(&mut self, delay: Duration) {
        *self.source_import_fixture_delay.lock().unwrap() = Some(delay);
    }

    #[cfg(feature = "agent")]
    pub fn set_navigation_fixture(&mut self, fixture: NavigationFixture) {
        *self.navigation_fixture.lock().unwrap() = fixture;
    }

    #[cfg(feature = "agent")]
    fn source_adapter_fixture(&self) -> Option<SourceAdapterFixture> {
        self.source_fixture.lock().unwrap().clone()
    }

    /// Why the stored session could not be verified at launch, if there was
    /// one to verify.
    ///
    /// Distinct from `user_id().is_none()`, and the distinction is the whole
    /// point: a library nobody has signed into is a guest library, which is a
    /// working library and owes the user nothing. A library holding
    /// credentials that no longer prove anyone has lost something the user
    /// had, and only signing in again gets it back — so that is the one state
    /// that raises the sign-in gate unasked.
    pub fn lapsed(&self) -> Option<&str> {
        self.lapsed.as_deref()
    }

    /// The signed-in principal, or `None` for the guest namespace.
    pub fn user_id(&self) -> Option<String> {
        self.account.lock().unwrap().as_ref().map(|a| a.id.clone())
    }

    /// [`Self::user_id`] with the address to show it under — what a screen
    /// naming the account reads.
    pub fn account(&self) -> Option<Account> {
        self.account.lock().unwrap().clone()
    }

    /// Email a six-digit login code to `email`.
    pub fn send_login_code(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("send_login_code", json!({ "email": email }))
    }

    /// Exchange an emailed code for a session and adopt the principal it
    /// proves. The host performs the identity switch; this only refreshes the
    /// cached answer to [`Library::account`] so rows written next carry it.
    ///
    /// The address needs no second read: a code is only ever verified against
    /// the address it was mailed to, so this call already holds the label the
    /// new session will be shown under.
    pub fn verify_login_code(
        &self,
        email: &str,
        code: &str,
    ) -> impl Future<Output = Result<String, LibraryError>> + use<> {
        let pending =
            self.call::<String>("verify_login_code", json!({ "email": email, "code": code }));
        let cache = Arc::clone(&self.account);
        let email = email.trim().to_string();
        async move {
            let user_id = pending.await?;
            *cache.lock().unwrap() = Some(Account {
                id: user_id.clone(),
                email: Some(email),
            });
            Ok(user_id)
        }
    }

    /// Sign out: make the signed-in projection durable, wipe it, then release
    /// the session.
    ///
    /// Two commands in this order because they are two commit points and only
    /// this order is recoverable — `wipe_database` is the durability boundary
    /// and leaves a journal that `remove_session_item` consumes. The web store
    /// sequences them identically; a host that reversed them would delete the
    /// credential it still needs to flush with.
    ///
    /// Both run inside *one* spawned task, not two: [`Self::call`] dispatches
    /// the moment it is called, so holding two of its futures and awaiting
    /// them in order sequences the awaits and nothing else — the commands
    /// themselves race for the sync lock, and a release that won would clear
    /// the credential the wipe is still flushing with. That is the reversal
    /// this comment says is unrecoverable, arrived at by accident.
    pub fn sign_out(&self) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let services = self.services.clone();
        let cache = Arc::clone(&self.account);
        let task = self.runtime.spawn(async move {
            command::<Value>(&services, "wipe_database", &json!({})).await?;
            command::<Value>(
                &services,
                "remove_session_item",
                &json!({ "key": luma_lib::database::local::auth::SUPABASE_SESSION_KEY }),
            )
            .await?;
            Ok::<_, LibraryError>(())
        });
        async move {
            task.await.map_err(|error| {
                LibraryError::at("sign_out", Cause::Cancelled(error.to_string()))
            })??;
            *cache.lock().unwrap() = None;
            Ok(())
        }
    }

    /// Every venue in the library, newest activity first.
    pub fn venues(&self) -> impl Future<Output = Result<Vec<Venue>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let (delay, error) = {
            let mut fixture = self.navigation_fixture.lock().unwrap();
            if fixture.catalogue_responses.is_empty() {
                (fixture.venues_delay, fixture.venues_error.clone())
            } else {
                fixture.catalogue_responses.remove(0)
            }
        };
        #[cfg(not(feature = "agent"))]
        let (delay, error): (Duration, Option<String>) = (Duration::ZERO, None);
        let pending = self.call_after("list_venues", json!({}), delay);
        async move {
            let result = pending.await;
            if let Some(error) = error {
                return Err(LibraryError::at("list_venues", Cause::Shape(error)));
            }
            result
        }
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
        #[cfg(feature = "agent")]
        let (delay, first_title) = {
            let mut fixture = self.navigation_fixture.lock().unwrap();
            if let Some(index) = fixture
                .track_responses
                .iter()
                .position(|response| response.0 == venue_id)
            {
                let (_, delay, first_title) = fixture.track_responses.remove(index);
                (delay, first_title)
            } else {
                (
                    fixture
                        .track_delays
                        .get(venue_id)
                        .copied()
                        .unwrap_or_default(),
                    None,
                )
            }
        };
        #[cfg(not(feature = "agent"))]
        let (delay, first_title): (Duration, Option<String>) = (Duration::ZERO, None);
        let pending = self.call_after(
            "list_tracks_enriched",
            json!({ "venueId": venue_id }),
            delay,
        );
        async move {
            let mut rows: Vec<TrackBrowserRow> = pending.await?;
            if let (Some(title), Some(first)) = (first_title, rows.first_mut()) {
                first.title = Some(title);
            }
            Ok(rows)
        }
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

    /// Resolve the installed Engine DJ library root. The source picker owns no
    /// product-specific path rules; it asks the adapter for its default and
    /// carries the opaque result in [`TrackSource`].
    pub fn default_engine_dj_source(
        &self,
    ) -> impl Future<Output = Result<TrackSource, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self.source_adapter_fixture().is_some();
        #[cfg(not(feature = "agent"))]
        let fixture = false;
        let path =
            (!fixture).then(|| self.call::<String>("engine_dj_default_library_path", json!({})));
        async move {
            let library_path = match path {
                Some(path) => path.await?,
                None => "/fixture/engine-dj".to_string(),
            };
            Ok(TrackSource::EngineDj { library_path })
        }
    }

    /// Open either supported DJ catalog through one GPUI-owned shape.
    pub fn source_library(
        &self,
        source: TrackSource,
    ) -> impl Future<Output = Result<SourceLibrary, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture_delay = self.source_fixture_delay.map(|delay| {
            self.runtime.spawn(async move {
                tokio::time::sleep(delay).await;
            })
        });
        #[cfg(not(feature = "agent"))]
        let fixture_delay: Option<tokio::task::JoinHandle<()>> = None;
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
                if let Some(delay) = fixture_delay {
                    let _ = delay.await;
                }
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
        let sequenced = {
            let mut responses = self.source_search_fixture.lock().unwrap();
            responses
                .front()
                .is_some_and(|response| response.query == query)
                .then(|| responses.pop_front().unwrap())
        };
        #[cfg(feature = "agent")]
        let sequenced = sequenced.map(|response| {
            let delay = self.runtime.spawn(async move {
                tokio::time::sleep(response.delay).await;
            });
            (
                normalize_source_tracks(&source, "source_search_tracks", response.rows),
                delay,
            )
        });
        #[cfg(feature = "agent")]
        let fixture = sequenced
            .is_none()
            .then(|| self.source_adapter_fixture())
            .flatten()
            .map(|fixture| {
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
                    .and_then(|value| {
                        normalize_source_tracks(&source, "source_search_tracks", value)
                    })
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
            #[cfg(feature = "agent")]
            if let Some((rows, delay)) = sequenced {
                let _ = delay.await;
                return rows;
            }
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

    /// One pattern's arg definitions, resolved against a venue — the schema
    /// the args sheet renders from. Venue-resolved on purpose, unlike
    /// [`Self::pattern_graph`]: a venue can pin a different implementation of
    /// the same pattern (`pattern-args-venue-divergence` in the IPC manifest),
    /// and the strip must show the args of the graph that will actually run.
    pub fn pattern_args(
        &self,
        pattern_id: &str,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<PatternArgDef>, LibraryError>> + use<> {
        self.call(
            "get_pattern_args",
            json!({ "id": pattern_id, "venueId": venue_id, "implementationId": null }),
        )
    }

    /// A venue's fixture groups — the vocabulary a selection expression is
    /// written over, which is what seeds the strip's autocomplete.
    pub fn venue_groups(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<FixtureGroup>, LibraryError>> + use<> {
        self.call("list_groups", json!({ "venueId": venue_id }))
    }

    /// The frame that answers "which heads does this selection light?" — every
    /// matched head open and white, the rest of the rig dark.
    ///
    /// The picker draws this over the venue's own geometry, so the picture a
    /// person points at and the fixtures a clip will drive are resolved by one
    /// evaluator. Head-accurate, which is why it is not `preview_selection_query`.
    pub fn highlight_selection(
        &self,
        venue_id: &str,
        selection: &Selection,
    ) -> impl Future<Output = Result<UniverseState, LibraryError>> + use<> {
        self.call(
            "highlight_selection",
            json!({ "venueId": venue_id, "selection": selection.to_value() }),
        )
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

    /// Run a graph against a resolved context and hand back everything the
    /// run produced — the per-view signals a plot draws, and optionally the
    /// mel spectrograms (`include_mel_specs`, expensive, wanted only when a
    /// spectrogram node is actually on screen).
    ///
    /// Takes the graph by value, not by id: a live preview runs what the
    /// editor *holds*, which is routinely ahead of what the seam has saved.
    pub fn run_graph(
        &self,
        graph: &Graph,
        context: &GraphContext,
        include_mel_specs: bool,
    ) -> impl Future<Output = Result<RunResult, LibraryError>> + use<> {
        self.call(
            "run_graph",
            json!({
                "graph": graph,
                "context": context,
                "includeMelSpecs": include_mel_specs,
                "agentThreadId": null,
                "agentExecutionId": null,
                "driveLivePreview": null,
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

    /// Every score for this track the current admission may see, in any
    /// venue, newest first. What the editor's score rail lists: the venue in
    /// hand is one grouping of this, not a separate read.
    pub fn scores_across_venues(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<Vec<ScoreSummary>, LibraryError>> + use<> {
        self.call("list_scores_across_venues", json!({ "trackId": track_id }))
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

    /// Archive a score. The authored document keeps its history — the seam
    /// tombstones the row rather than rewriting it — so this is "take it off
    /// the list", not "unmake it".
    pub fn delete_score(&self, id: &str) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("delete_score", json!({ "id": id }))
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

    /// Every persisted clip's heatmap preview for `(track_id, venue_id)`, in
    /// z-index order. Slow — the seam evaluates every clip's pattern over its
    /// span — so a screen asks once per open and patches single clips with
    /// [`Self::preview_clip`] afterwards.
    pub fn annotation_previews(
        &self,
        track_id: &str,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<AnnotationPreview>, LibraryError>> + use<> {
        self.call(
            "generate_annotation_previews",
            json!({ "trackId": track_id, "venueId": venue_id }),
        )
    }

    /// One clip's heatmap preview, rendered from the state the editor holds
    /// rather than the persisted row — which is what lets an edited clip's
    /// thumbnail be redrawn before (or without) the edit being written. The
    /// clip's id is echoed back as `annotation_id`, so an uncommitted clip is
    /// previewable too.
    pub fn preview_clip(
        &self,
        track_id: &str,
        venue_id: &str,
        clip_id: &str,
        pattern_id: &str,
        start: f64,
        end: f64,
        args: &serde_json::Value,
    ) -> impl Future<Output = Result<AnnotationPreview, LibraryError>> + use<> {
        self.call(
            "preview_annotation",
            json!({
                "trackId": track_id,
                "venueId": venue_id,
                "annotation": {
                    "id": clip_id,
                    "patternId": pattern_id,
                    "startTime": start,
                    "endTime": end,
                    "args": args,
                },
            }),
        )
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

    /// A wall-clock delay, on the library's own runtime — what a UI debounce
    /// awaits. Not `gpui`'s `background_executor().timer`: the automation
    /// harness's headless dispatcher virtualizes gpui time and never advances
    /// it, while this runtime keeps real time on both hosts, exactly as the
    /// transport poll's `after` does.
    pub fn debounce(&self, wait: Duration) -> impl Future<Output = ()> + use<> {
        let task = self.runtime.spawn(async move {
            tokio::time::sleep(wait).await;
        });
        async move {
            task.await.ok();
        }
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
        self.ordered_write("set_session_item", json!({ "key": key, "value": value }))
    }

    /// Remove one host-session value.
    pub fn remove_session_item(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.ordered_write("remove_session_item", json!({ "key": key }))
    }

    /// Write one setting. The seam owns what a key means and what a write
    /// costs (Art-Net and audio both reload on one), so the caller's only job
    /// afterwards is to read the settings back — which is also the only honest
    /// proof the write landed.
    ///
    /// Through the same FIFO the session writes use, and for the same reason
    /// stated there: a detached call finishes whenever SQLite gets to it, so
    /// the *last* value a control sent is not necessarily the one left in the
    /// database. That was a rarity while every settings control was a checkbox
    /// or a picker; a dragged slider sends a value per pointer move, which
    /// makes it routine.
    pub fn set_setting(
        &self,
        key: &str,
        value: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.ordered_write("set_setting", json!({ "key": key, "value": value }))
    }

    /// One durable write, queued behind the last one.
    ///
    /// Named for the ordering it provides rather than for the session items it
    /// was first written for — settings share the queue, and a name that said
    /// "session" would make that read as a mistake.
    fn ordered_write(
        &self,
        command: &'static str,
        args: Value,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let queued = self.session_writes.send(SessionWrite {
            command,
            args,
            reply,
        });
        async move {
            queued
                .map_err(|error| LibraryError::at(command, Cause::Cancelled(error.to_string())))?;
            answer
                .await
                .map_err(|error| LibraryError::at(command, Cause::Cancelled(error.to_string())))?
        }
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

    /// Install one score as the render engine's active scene, so
    /// [`Self::sample_universe`] has something to sample.
    ///
    /// **The score is the subject, not the `(track, venue)` pair.** A pair
    /// carries as many scores as there are people who annotated it, and the rig
    /// shows the one that is open rather than a blend of all of them — which is
    /// why this takes the id the editor holds and does not look one up.
    ///
    /// `clips` is load-bearing three ways. `None` says "use the score's
    /// persisted rows" — the right answer for a screen that is only *watching*
    /// it, which has no working copy to offer. `Some(list)` composites that
    /// list instead, which is how an editor's live edits reach the rig before
    /// the trailing write does. An empty `Some` is an authoritative empty
    /// document and clears the scene; `None` and `Some(vec![])` are therefore
    /// *not* the same request (see `dispatch::handlers::compositor`).
    pub fn composite_score(
        &self,
        score_id: &str,
        clips: Option<Vec<TrackClip>>,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call(
            "composite_track",
            json!({
                "scoreId": score_id,
                "annotations": clips,
            }),
        )
    }

    /// Everything the 3D view needs to draw a venue: its patched fixtures, its
    /// venue graph solved, and the definition behind every distinct fixture
    /// path.
    ///
    /// One method rather than three because they are only meaningful together
    /// — a rig with its definitions missing is not a partial rig, it is fixtures
    /// with no geometry — and because the definitions cannot even be *asked*
    /// for until the fixture list has landed. A screen that orchestrated that
    /// would be a screen that knew the second call depends on the first.
    pub fn venue_rig(&self, venue_id: &str) -> impl Future<Output = Result<Rig, LibraryError>> {
        let services = self.services.clone();
        let venue = json!({ "venueId": venue_id });
        let task = self.runtime.spawn(async move {
            let fixtures: Vec<PatchedFixture> =
                command(&services, "get_patched_fixtures", &venue).await?;
            let venue_graph: ResolvedVenue =
                command(&services, "get_resolved_venue", &venue).await?;
            let rows: VenueGraphRows = command(&services, "get_venue_graph", &venue).await?;
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
                venue: venue_graph,
                definitions,
                rows,
            })
        });
        async move {
            task.await.map_err(|e| {
                LibraryError::at("get_patched_fixtures", Cause::Cancelled(e.to_string()))
            })?
        }
    }

    // -----------------------------------------------------------------
    // The venue graph — the verbs. The two reads arrive together in
    // `venue_rig`, because a pose and the relation that produced it are one
    // answer.
    //
    // Thin on purpose. Each is one dispatch command with the argument names
    // `src-tauri/src/dispatch/mod.rs` declares, and every mutating one hands
    // back the whole solved venue inside its `PlacementReport`: a graph edit
    // moves everything bolted to what it touched, so a caller that re-fetched
    // would be asking for a second solve of a graph it was just given.
    // -----------------------------------------------------------------

    /// Every QLC+ definition matching `query`, for the distribution popup and
    /// the patch page's add dialog.
    pub fn search_fixtures(
        &self,
        query: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<FixtureEntry>, LibraryError>> + use<> {
        self.call(
            "search_fixtures",
            json!({ "query": query, "offset": 0, "limit": limit }),
        )
    }

    /// One QLC+ definition, for its mode list.
    pub fn fixture_definition(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<FixtureDefinition, LibraryError>> + use<> {
        self.call("get_fixture_definition", json!({ "path": path }))
    }

    /// Place a new node by mating two sockets.
    #[allow(clippy::too_many_arguments)]
    pub fn attach(
        &self,
        venue_id: &str,
        kind: &str,
        catalog_ref: Option<&str>,
        label: Option<&str>,
        parent_id: &str,
        my_socket: &str,
        their_socket: &str,
        yaw: f64,
        params: BTreeMap<String, f64>,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        self.call(
            "attach",
            json!({
                "venueId": venue_id,
                "kind": kind,
                "catalogRef": catalog_ref,
                "label": label,
                "parentId": parent_id,
                "mySocket": my_socket,
                "theirSocket": their_socket,
                "yaw": yaw,
                "params": params,
            }),
        )
    }

    /// Place a node that already exists — a re-attach, or a fixture dragged
    /// out of the tray.
    #[allow(clippy::too_many_arguments)]
    pub fn reattach(
        &self,
        venue_id: &str,
        node_id: &str,
        parent_id: &str,
        my_socket: &str,
        their_socket: &str,
        yaw: f64,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        self.call(
            "reattach",
            json!({
                "venueId": venue_id,
                "nodeId": node_id,
                "parentId": parent_id,
                "mySocket": my_socket,
                "theirSocket": their_socket,
                "yaw": yaw,
            }),
        )
    }

    /// Free placement: seat a new node on a surface at `(u, v, yaw, trim)`.
    #[allow(clippy::too_many_arguments)]
    pub fn place_free(
        &self,
        venue_id: &str,
        kind: &str,
        catalog_ref: Option<&str>,
        label: Option<&str>,
        surface: Option<(&str, &str)>,
        my_socket: &str,
        seat: luma_scene::venue::SurfacePlacement,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        let (surface_node_id, surface_socket) = match surface {
            Some((node, socket)) => (Some(node), Some(socket)),
            None => (None, None),
        };
        self.call(
            "place_free",
            json!({
                "venueId": venue_id,
                "kind": kind,
                "catalogRef": catalog_ref,
                "label": label,
                "surfaceNodeId": surface_node_id,
                "surfaceSocket": surface_socket,
                "mySocket": my_socket,
                "u": seat.u,
                "v": seat.v,
                "yaw": seat.yaw,
                "trim": seat.trim,
            }),
        )
    }

    /// Unplace a node. Its rows stay, so re-attaching restores the branch.
    pub fn detach(
        &self,
        venue_id: &str,
        node_id: &str,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        self.call(
            "detach",
            json!({ "venueId": venue_id, "nodeId": node_id }),
        )
    }

    /// Write down a far end: this socket meets that one. A check, never an
    /// edge.
    pub fn constrain(
        &self,
        venue_id: &str,
        node_id: &str,
        my_socket: &str,
        target_node: &str,
        target_socket: &str,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        self.call(
            "constrain",
            json!({
                "venueId": venue_id,
                "nodeId": node_id,
                "mySocket": my_socket,
                "targetNode": target_node,
                "targetSocket": target_socket,
            }),
        )
    }

    /// Merge parameters into a node, and optionally rename it. `yaw` is
    /// spelled as itself and lands on the edge.
    pub fn set_params(
        &self,
        venue_id: &str,
        node_id: &str,
        params: BTreeMap<String, f64>,
        label: Option<&str>,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        self.call(
            "set_params",
            json!({
                "venueId": venue_id,
                "nodeId": node_id,
                "params": params,
                "label": label,
            }),
        )
    }

    /// Delete a node and everything hanging off it.
    pub fn delete_subtree(
        &self,
        venue_id: &str,
        node_id: &str,
    ) -> impl Future<Output = Result<ResolvedVenue, LibraryError>> + use<> {
        self.call(
            "delete_subtree",
            json!({ "venueId": venue_id, "nodeId": node_id }),
        )
    }

    /// One command, one transaction: place, name, group and patch a row of
    /// fixtures along a host feature.
    #[allow(clippy::too_many_arguments)]
    pub fn distribute(
        &self,
        venue_id: &str,
        host: Option<(&str, &str)>,
        fixture_path: &str,
        mode_name: &str,
        count: usize,
        layout: DistributeLayout,
        label_prefix: Option<&str>,
    ) -> impl Future<Output = Result<DistributeReport, LibraryError>> + use<> {
        let (host_node_id, host_socket) = match host {
            Some((node, socket)) => (Some(node), Some(socket)),
            None => (None, None),
        };
        self.call(
            "distribute",
            json!({
                "venueId": venue_id,
                "hostNodeId": host_node_id,
                "hostSocket": host_socket,
                "fixturePath": fixture_path,
                "modeName": mode_name,
                "count": count,
                "layout": layout,
                "labelPrefix": label_prefix,
            }),
        )
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

    /// Append one entry to the render telemetry log, and forget about it.
    ///
    /// Returns nothing, deliberately. This is called from the stage when a
    /// frame took too long, and a report that made the next frame wait on a
    /// file write would be reporting on a hitch it had just widened. The
    /// command is spawned on the Tokio runtime before this returns (see
    /// [`Self::call_after`]), so dropping the future still runs it — and a
    /// telemetry write that fails is not a fact any screen should act on.
    pub fn record_telemetry(&self, entry: Value) {
        drop(self.call::<()>("append_render_telemetry", json!({ "entry": entry })));
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
        let services = self.services.clone();
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
    /// Every patched fixture, in patch order. The **patch**: what exists and
    /// how it is addressed. Where each one *is* comes from `venue`.
    pub fixtures: Vec<PatchedFixture>,
    /// The venue graph, solved: every pose in the room, derived. A node's id
    /// is the thing it stands for, so a fixture node's id is its `fixtures`
    /// row id and a fixture nobody has placed simply has no node here.
    pub venue: ResolvedVenue,
    /// Keyed by `fixture_path`; a path whose bundle no longer resolves is
    /// absent rather than an error.
    pub definitions: HashMap<String, FixtureDefinition>,
    /// The graph as rows — `(parent, my_socket, their_socket, params)`.
    ///
    /// [`Self::venue`] is the same graph *solved*, and a solve throws the
    /// relations away: it answers where a node is, never which socket met
    /// which. The builder needs both — the pose to draw and the socket to
    /// click — so the two arrive together rather than as a second fetch that
    /// could disagree with the first.
    pub rows: VenueGraphRows,
}

impl Rig {
    /// Whether there is anything at all to draw. The root node is the room
    /// itself, so a venue with only that is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fixtures.is_empty() && self.venue.nodes.len() <= 1
    }
}

/// The app config directory: `$APPCONFIG` as the Tauri app resolves it, with
/// the same escape hatch the headless harness has.
fn config_dir() -> Result<StorageRoot, String> {
    match luma_ui::runtime::Runtime::with(|runtime| runtime.config_dir.clone()) {
        Some(path) => Ok(StorageRoot::from_path(path)),
        None => StorageRoot::from_env_default(),
    }
}

/// Root of the bundled fixture definitions. Prefers the repo's newest bundle
/// so a dev build sees today's fixtures, exactly as the headless harness does.
fn fixtures_root() -> Result<PathBuf, String> {
    if let Some(path) = luma_ui::runtime::Runtime::with(|runtime| runtime.fixtures_root.clone()) {
        return Ok(path);
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
