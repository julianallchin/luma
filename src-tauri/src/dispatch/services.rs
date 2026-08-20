//! [`AppServices`] — the single injection point every command body reaches its
//! services through, and the two host capabilities it carries.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use super::CommandError;
use crate::agent_execution::graph_runs::GraphRunStore;
use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::artnet::ArtNetManager;
use crate::audio::FftService;
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::preprocessing::AnalysisTaskGroup;
use crate::render_engine::RenderEngine;
use crate::services::authored_documents::AuthoredDocuments;
use crate::storage::StorageRoot;
use crate::sync::orchestrator::SyncEngine;

// -----------------------------------------------------------------------------
// Event emission
// -----------------------------------------------------------------------------

/// Where a command's push notifications go. The desktop adapter forwards to
/// Tauri's event bus; a headless host records or discards them.
///
/// **Emission cannot fail.** A command's write is committed before it emits, so
/// an undeliverable event cannot mean a failed command; encoding that in the
/// signature is cheaper than asking every call site to drop a `Result`.
///
/// `Send + Sync + 'static` is load-bearing: the import commands clone their
/// emitter into a spawned background task to report progress, so a sink has to
/// outlive the call that created it.
pub trait EventSink: Send + Sync + 'static {
    /// Deliver one event. Implementations must not block and must not panic.
    fn emit(&self, event: &str, payload: Value);
}

/// Cloneable handle to an [`EventSink`].
#[derive(Clone)]
pub struct Events(Arc<dyn EventSink>);

impl Events {
    /// Route events to `sink`.
    pub fn new<S: EventSink>(sink: S) -> Self {
        Self(Arc::new(sink))
    }

    /// Drop every event. The default for a host with no observer.
    #[must_use]
    pub fn discard() -> Self {
        Self::new(DiscardSink)
    }

    /// Emit one event. Infallible by design — see [`EventSink`]. A payload that
    /// cannot serialize is logged and dropped: that is a bug in the caller, not
    /// a condition the caller can handle.
    pub fn emit<T: Serialize>(&self, event: &str, payload: T) {
        match serde_json::to_value(payload) {
            Ok(value) => self.0.emit(event, value),
            Err(error) => log::warn!("[events] `{event}` payload was not serializable: {error}"),
        }
    }
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Events")
    }
}

struct DiscardSink;

impl EventSink for DiscardSink {
    fn emit(&self, _event: &str, _payload: Value) {}
}

// -----------------------------------------------------------------------------
// Host control
// -----------------------------------------------------------------------------

/// The one piece of genuine host control on the command surface: `force_quit`
/// terminates the process. Abstracted rather than left as a Tauri-only shim so
/// the dispatch layer covers the whole surface.
pub trait Host: Send + Sync + 'static {
    /// Terminate the host. Does not return on a host that honours it.
    fn exit(&self, code: i32);
}

/// Cloneable handle to a [`Host`].
#[derive(Clone)]
pub struct HostControl(Arc<dyn Host>);

impl HostControl {
    /// Route host control to `host`.
    pub fn new<H: Host>(host: H) -> Self {
        Self(Arc::new(host))
    }

    /// Terminate the current process directly. The default for a host with no
    /// orderly shutdown of its own.
    #[must_use]
    pub fn process_exit() -> Self {
        Self::new(ProcessExitHost)
    }

    /// Terminate the host with `code`.
    pub fn exit(&self, code: i32) {
        self.0.exit(code);
    }
}

struct ProcessExitHost;

impl Host for ProcessExitHost {
    fn exit(&self, code: i32) {
        std::process::exit(code);
    }
}

// -----------------------------------------------------------------------------
// Services
// -----------------------------------------------------------------------------

/// Every service a command body can reach.
///
/// Each one is a process-global singleton built once at startup and
/// live for the process, so one struct passed by reference covers the whole
/// command surface and no body carries an injection lifetime.
///
/// Three of these fields stand in for a Tauri `AppHandle`, which the surface
/// used for exactly three things: emitting events ([`Events`]), resolving the
/// `storage` and `fixtures_root` paths, and terminating the process
/// ([`HostControl`]). Nothing else about a host is abstracted here.
///
/// Fields are `pub(crate)`: handlers read them directly, and an external host
/// neither builds nor inspects them beyond the accessors below. That keeps
/// `SyncEngine`, `RenderEngine` and friends out of the crate's public API.
pub struct AppServices {
    pub(crate) db: Db,
    pub(crate) state_db: StateDb,
    pub(crate) authored: AuthoredDocuments,
    pub(crate) workspaces: Arc<PythonWorkspaceService>,
    pub(crate) graph_runs: Arc<GraphRunStore>,
    pub(crate) analysis_tasks: AnalysisTaskGroup,
    pub(crate) fft: FftService,
    pub(crate) render_engine: RenderEngine,
    pub(crate) sync: SyncEngine,
    /// Absent on any host that cannot build an `AppHandle`, which is what
    /// `ArtNetManager::new` requires.
    pub(crate) artnet: Option<Arc<ArtNetManager>>,
    pub(crate) storage: StorageRoot,
    pub(crate) fixtures_root: PathBuf,
    pub(crate) events: Events,
    pub(crate) host: HostControl,
    /// Explicit trusted principal for a disposable headless fixture. Unset on
    /// the desktop app, where identity resolves from the verified state
    /// database and the app-database admission gate instead.
    pub(crate) fixture_principal: Option<String>,
}

impl AppServices {
    /// Assemble the service set for a host that is not the Tauri app.
    ///
    /// Everything constructible without a window is constructed here rather
    /// than by the caller, so the two adapters cannot drift on how a singleton
    /// is configured. Events are discarded and process control is a direct
    /// `exit` unless overridden with [`AppServices::with_events`] and
    /// [`AppServices::with_host`].
    ///
    /// The deliberate absences are ArtNet — its manager needs an `AppHandle` —
    /// and the loops: nothing spawns a render loop, a sync loop, or an audio
    /// broadcaster, so a `RenderEngine` exists here but drives nothing.
    #[must_use]
    pub fn headless(
        db: Db,
        state_db: StateDb,
        storage: StorageRoot,
        fixtures_root: PathBuf,
        workspaces: Arc<PythonWorkspaceService>,
    ) -> Self {
        let authored = AuthoredDocuments::new(storage.clone());
        let sync = SyncEngine::new(
            db.0.clone(),
            state_db.0.clone(),
            Arc::new(crate::database::remote::common::SupabaseClient::new(
                crate::config::SUPABASE_URL.to_string(),
                crate::config::SUPABASE_ANON_KEY.to_string(),
            )),
            authored.clone(),
        );
        Self {
            db,
            state_db,
            authored,
            workspaces,
            graph_runs: Arc::new(GraphRunStore::new()),
            analysis_tasks: AnalysisTaskGroup::new(),
            fft: FftService::new(),
            render_engine: RenderEngine::default(),
            sync,
            artnet: None,
            storage,
            fixtures_root,
            events: Events::discard(),
            host: HostControl::process_exit(),
            fixture_principal: None,
        }
    }

    /// Observe the events commands emit.
    #[must_use]
    pub fn with_events(mut self, events: Events) -> Self {
        self.events = events;
        self
    }

    /// Take over process termination from the default direct `exit`.
    #[must_use]
    pub fn with_host(mut self, host: HostControl) -> Self {
        self.host = host;
        self
    }

    /// Trust `principal` as the caller's identity instead of resolving it from
    /// the verified session. For a disposable headless fixture only.
    #[must_use]
    pub fn with_fixture_principal(mut self, principal: Option<String>) -> Self {
        self.fixture_principal = principal;
        self
    }

    /// The principal of the verified host session in the state database.
    ///
    /// Not interchangeable with the app database's signed-write admission gate:
    /// this one can refresh a Supabase session, that one reads a local gate.
    /// Which of the two a command wants is per-command — see the port guide.
    ///
    /// # Errors
    ///
    /// [`CommandError::Internal`] if the session cannot be read or verified.
    pub async fn session_user_id(&self) -> Result<Option<String>, CommandError> {
        match &self.fixture_principal {
            Some(principal) => Ok(Some(principal.clone())),
            None => Ok(auth::get_current_user_id(&self.state_db.0).await?),
        }
    }

    /// The principal admitted for signed writes in the app database.
    ///
    /// # Errors
    ///
    /// [`CommandError::Internal`] if the admission gate cannot be read.
    pub(crate) async fn admitted_principal(&self) -> Result<Option<String>, CommandError> {
        match &self.fixture_principal {
            Some(principal) => Ok(Some(principal.clone())),
            None => Ok(auth::admitted_principal(&self.db.0).await?),
        }
    }
}

/// Transitional accessors.
///
/// A host that implements no commands of its own needs none of these — it calls
/// [`super::dispatch`] and nothing else. They exist so a host that does can
/// reach these singletons rather than keep a second, forkable copy.
impl AppServices {
    /// The app database.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Git-backed authored document store.
    pub fn authored(&self) -> &AuthoredDocuments {
        &self.authored
    }

    /// One Python kernel per agent thread.
    pub fn workspaces(&self) -> &PythonWorkspaceService {
        &self.workspaces
    }

    /// Published graph evaluations, keyed by execution id.
    pub fn graph_runs(&self) -> &GraphRunStore {
        &self.graph_runs
    }

    /// Shared FFT service for audio analysis.
    pub fn fft(&self) -> &FftService {
        &self.fft
    }

    /// Root of Luma's durable on-disk data.
    pub fn storage(&self) -> &StorageRoot {
        &self.storage
    }

    /// Root of the bundled fixture definitions.
    pub fn fixtures_root(&self) -> &Path {
        &self.fixtures_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Recorder(Arc<Mutex<Vec<(String, Value)>>>);

    impl EventSink for Recorder {
        fn emit(&self, event: &str, payload: Value) {
            self.0
                .lock()
                .expect("poisoned")
                .push((event.into(), payload));
        }
    }

    /// The import commands clone their emitter into a spawned task; the sink
    /// has to survive the call that made it.
    #[tokio::test]
    async fn events_survive_a_spawned_task() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let events = Events::new(Recorder(Arc::clone(&log)));

        let background = events.clone();
        tokio::spawn(async move {
            background.emit("file-import-progress", serde_json::json!({ "done": 1 }));
        })
        .await
        .expect("spawned emit panicked");

        let recorded = log.lock().expect("poisoned");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "file-import-progress");
    }

    /// A payload that cannot serialize must not take the caller down. A map
    /// with non-string keys is the cheapest way to make `to_value` fail.
    #[test]
    fn unserializable_payload_is_dropped_not_propagated() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let events = Events::new(Recorder(Arc::clone(&log)));
        events.emit("bad", std::collections::HashMap::from([((1, 2), "v")]));
        assert!(log.lock().expect("poisoned").is_empty());
    }
}
