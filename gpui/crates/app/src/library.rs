//! The app's whole connection to Luma's data: a typed, awaitable client over
//! the command handlers.
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
//! sees a handler, an `AppServices`, or a runtime — which is the point:
//! [`luma_lib::dispatch::handlers`] is the *only* way this binary reaches
//! Luma's behavior, and re-implementing a query above it would fork the app
//! the same way the headless harness once forked it.
//!
//! # Implementation
//!
//! Two runtimes coexist. GPUI owns the main thread and its own executors;
//! `sqlx` needs a Tokio reactor. So every command runs on a Tokio runtime
//! this type owns, and [`Library::call`] hands back a future that a GPUI task
//! can await — a Tokio `JoinHandle` is an ordinary future, so no bridging
//! machinery is needed beyond keeping the runtime alive.
//!
//! Commands are called as the typed handler functions themselves, with the
//! backend's own models in and out. The JSON dispatcher beside them is for
//! hosts that speak JSON; this one has no reason to encode anything.
//!
//! Failures keep the seam's structure, as [`LibraryError`]. Two designs lost.
//! Returning [`CommandError`] itself is the smaller change, but the ways a
//! call fails *without* reaching a command — the reactor dropped the work, a
//! request or fixture was in the wrong shape — are not command errors, and naming
//! them `Internal` with a `{command}: ` prefix would break that type's
//! documented contract that `Display` is a verbatim passthrough. Reading
//! `CommandError::kind()` is the other: it is the escape hatch for a host that
//! serializes the discriminant, and a host linked against the enum that
//! compared strings would throw the conflict's revisions away a second time.
//! [`LibraryError`] is not a per-layer pass-through wrapper — it adds the two
//! host-side failures and the command name, and hands the seam's own error
//! back untouched through [`LibraryError::command`].

#[cfg(feature = "agent")]
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use luma_lib::agent::model::ModelClient;
use luma_lib::agent::tools::ToolRegistry;
use luma_lib::agent_execution::headless_env;
use luma_lib::database::local::auth::{arm_write_admission, bootstrap_headless_admission};
use luma_lib::database::local::database::{init_app_db_at, open_app_db_at};
use luma_lib::database::local::state::init_state_db_at;
use luma_lib::dispatch::{handlers, AppServices, CommandError, EventSink, Events, SharedServices};
#[cfg(feature = "agent")]
use luma_lib::dispatch::{system_track_sources, TrackSources};
use luma_lib::models::tracks::TrackImportProgress;
use luma_lib::storage::StorageRoot;

mod account;
mod agent;
mod audio;
#[cfg(feature = "agent")]
mod fixtures;
mod groups;
mod live;
mod patch;
mod render;
mod scores;
mod settings;
mod sources;
mod stage;
mod tracks;
mod venues;

pub use account::Account;
#[cfg(feature = "agent")]
use fixtures::FixtureTrackSources;
#[cfg(feature = "agent")]
pub use fixtures::{NavigationFixture, SourceAdapterFixture, SourceSearchFixtureResponse};
pub use patch::Patch;
pub(crate) use scores::ScoreContents;
pub use sources::{SourceLibrary, SourcePlaylist, SourceTrack, TrackSource};
pub use stage::Rig;
pub use tracks::TrackImportRequest;

struct LibraryEvents {
    import_progress: tokio::sync::broadcast::Sender<TrackImportProgress>,
    /// The stored session stopped proving anyone — see
    /// [`Library::session_revoked`].
    session_revoked: tokio::sync::broadcast::Sender<()>,
    /// Rows arrived from another device — see [`Library::replica_changed`].
    replica_changed: tokio::sync::broadcast::Sender<Vec<String>>,
}

impl EventSink for LibraryEvents {
    fn emit(&self, event: &str, payload: Value) {
        if event == "session-revoked" {
            let _ = self.session_revoked.send(());
            return;
        }
        if event == "replica-changed" {
            let tables = payload
                .get("tables")
                .and_then(Value::as_array)
                .map(|tables| {
                    tables
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let _ = self.replica_changed.send(tables);
            return;
        }
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
    /// The request never reached a command because it, or the fixture
    /// standing in for the command, was not in the shape the method needs.
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
            Cause::Shape(error) => write!(formatter, "{command}: unexpected shape: {error}"),
            Cause::Cancelled(error) => write!(formatter, "{command} did not complete: {error}"),
        }
    }
}

impl std::error::Error for LibraryError {}

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
    /// Whether the QLC+ definition index has been built this process. See
    /// [`Library::search_fixtures`], which is the only reader and the only
    /// writer.
    fixtures_indexed: Arc<std::sync::atomic::AtomicBool>,
    /// Why boot admitted the guest namespace despite finding credentials —
    /// see [`Library::lapsed`]. Fixed at open: it is a fact about how this
    /// process started, and signing in afterwards does not rewrite it.
    lapsed: Option<String>,
    /// Owns the reactor every command runs on. Dropping it cancels
    /// in-flight work, so it lives exactly as long as the `Library`.
    ///
    /// An `Option` only so [`Drop`] can take it and shut it down with a time
    /// limit. It is `Some` for the whole life of the `Library`.
    runtime: Option<tokio::runtime::Runtime>,
    import_progress: tokio::sync::broadcast::Sender<TrackImportProgress>,
    /// Whether this library talks to the cloud — see `Runtime::cloud`.
    cloud: bool,
    #[cfg(feature = "agent")]
    sync_status_fixture: Option<Arc<Mutex<luma_lib::models::sync::SyncStatus>>>,
    /// Fires when the backend learns the session is revoked; see
    /// [`Library::session_revoked`].
    session_revoked: tokio::sync::broadcast::Sender<()>,
    /// Rows arrived from another device — see [`Library::replica_changed`].
    replica_changed: tokio::sync::broadcast::Sender<Vec<String>>,
    /// Tells the background sync loop to stop. Sent on drop; the reactor
    /// going with it is what makes the stop take.
    sync_shutdown: tokio::sync::watch::Sender<bool>,
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
    /// The write itself, not yet polled: it runs when the actor reaches it.
    write: std::pin::Pin<Box<dyn Future<Output = Result<(), LibraryError>> + Send>>,
    reply: tokio::sync::oneshot::Sender<Result<(), LibraryError>>,
}

impl Library {
    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("the runtime lives as long as the library")
    }

    /// Open the library in the app's real config directory — the same
    /// platform config directory that backend tools use, honouring the
    /// `LUMA_CONFIG_DIR` / `LUMA_FIXTURES_ROOT` overrides so a disposable
    /// fixture directory can be pointed at without touching the real one.
    ///
    /// # Errors
    ///
    /// If the config directory cannot be located or either database fails to
    /// open or migrate.
    pub fn open() -> Result<Self, String> {
        Self::open_host(false)
    }

    /// Open the native desktop library with physical output and Python setup.
    pub fn open_desktop() -> Result<Self, String> {
        Self::open_host(true)
    }

    fn open_host(desktop: bool) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("failed to start the async runtime: {error}"))?;

        let storage = config_dir()?;
        let fixtures_root = fixtures_root()?;

        // An import's follower reads nothing until phase one hands back the
        // songs, and every song reports each analysis stage twice; a lagged
        // receiver drops the oldest events, and a row whose stage events were
        // dropped can never settle. Sized for a few hundred songs in flight.
        let (progress_tx, _) = tokio::sync::broadcast::channel(8192);
        let (session_revoked, _) = tokio::sync::broadcast::channel(4);
        let (replica_changed, _) = tokio::sync::broadcast::channel(64);
        let events = Events::new(LibraryEvents {
            import_progress: progress_tx.clone(),
            session_revoked: session_revoked.clone(),
            replica_changed: replica_changed.clone(),
        });
        // Whether this process may talk to the cloud at all — see
        // `Runtime::cloud`. Unasked is a launched app, which may.
        let cloud = luma_ui::runtime::Runtime::with(|runtime| runtime.cloud);
        // …and whether there is anywhere to sync to. A process that will not
        // sync opens the database without the SDK entirely: loading its core
        // extension is not free, and a library nobody is replicating has no
        // use for an upload queue.
        let syncing = cloud && !luma_lib::config::powersync_url().is_empty();
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
            let (db, connections) = if syncing {
                let (db, connections) = open_app_db_at(storage.path()).await?;
                (db, Some(connections))
            } else {
                (init_app_db_at(storage.path()).await?, None)
            };
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
            // so it resolves the managed environment through the same
            // path-based resolver as backend tools.
            // Resolution is lazy: a machine with no venv yet still opens.
            let workspaces = Arc::new(headless_env::workspace_service(
                &storage,
                luma_lib::storage::cache_dir()?,
            ));
            let state_pool = state_db.0.clone();
            let services = AppServices::headless(db, state_db, storage, fixtures_root, workspaces)
                .with_events(events.clone());
            // Record replication, on this library's reactor. Off when the
            // process may not talk to the cloud (a harness) or when no
            // PowerSync instance is configured — the app is then exactly what
            // it was before sync existed, a local SQLite library.
            let services = match connections {
                Some(connections) => {
                    match luma_lib::sync::service::Service::open(
                        connections,
                        state_pool.clone(),
                        events.clone(),
                    )
                    .await
                    {
                        Ok(sync) => services.with_sync(sync),
                        // Not a launch failure: a library that cannot reach the
                        // cloud is still a library.
                        Err(error) => {
                            eprintln!("[luma] record sync did not start: {error}");
                            services
                        }
                    }
                }
                None => services,
            };
            let services = if desktop {
                services.with_output().await?
            } else {
                services
            };
            #[cfg(feature = "agent")]
            let services = services.with_track_sources(import_sources);
            // The harness has no server, so a code is redeemed against this
            // library's own rows, which the fixture seeds.
            #[cfg(feature = "agent")]
            let services = {
                let pool = services.db().0.clone();
                services.with_share_codes(Arc::new(
                    luma_lib::services::venue_sharing::LocalShareCodes::new(pool),
                ))
            };
            // The audio host keeps its own copy of the settings it reads, and
            // is only ever told by a write. A host that skipped this would run
            // the whole session on the defaults and ignore what the user last
            // chose — audible the first time a track is loaded.
            services
                .apply_persisted_settings()
                .await
                .map_err(|error| error.to_string())?;
            // Before any turn runs: a turn cut off by a quit left its last
            // row local, and the thread reads unfinished once it is written.
            // Not a launch failure: the rows wait for the next launch.
            if let Err(error) = services.recover_open_agent_messages().await {
                eprintln!("[luma] agent rows in progress were not recovered: {error}");
            }
            // The address to name the principal by, out of the same stored
            // snapshot the admission above was proven from. `current_account`
            // makes no request — see its handler — which is the property this
            // line depends on: boot never touches the network, so a launch on
            // a plane still knows whose library this is.
            //
            // A label that cannot be read is not a launch failure and not a
            // different identity: the id is already proven, so it stands alone.
            let email = match handlers::auth::current_account(&services).await {
                Ok(Some(account)) if Some(&account.id) == user_id.as_ref() => account.email,
                _ => None,
            };
            let account = user_id.map(|id| Account { id, email });
            Ok::<_, String>((services, account, lapsed))
        })?;

        let services = services.into_shared();
        // The loop that keeps the library current after the sync that opened
        // it, on this library's reactor, for
        // as long as the library lives. It handles its own sign-in state —
        // a 401 backs it off, and the next sign-in's push wakes it.
        if desktop {
            let ready = luma_lib::runtime::python::setup_python_env_background(
                luma_lib::storage::cache_dir()?,
                luma_lib::resources::resource_dir(),
                events.clone(),
            );
            let analysis_services = services.clone();
            runtime.spawn(async move {
                match ready.await {
                    Ok(Ok(())) => {
                        if let Err(error) = analysis_services.reconcile_analysis().await {
                            eprintln!("startup analysis reconciliation failed: {error}");
                        }
                    }
                    Ok(Err(error)) => eprintln!("Python environment is unavailable: {error}"),
                    Err(error) => {
                        eprintln!("Python setup ended before reporting readiness: {error}")
                    }
                }
            });
        }
        let (sync_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        if desktop {
            let output_services = services.clone();
            let output_shutdown = shutdown_rx.clone();
            runtime.spawn(async move {
                output_services.run_output(output_shutdown).await;
            });
        }

        // Bytes move on their own clock: audio, stems and album art go through
        // Supabase Storage, not through the row protocol, and a 40 MB stem has
        // nothing to do with a checkpoint.
        if syncing {
            let host = services.sync_host();
            let media = luma_lib::sync::media::Media::new(
                services.db().0.clone(),
                services.state_pool().clone(),
            );
            let media_shutdown = shutdown_rx.clone();
            runtime.spawn(async move { media.run(host, media_shutdown).await });
        }
        drop(shutdown_rx);
        let (session_writes, mut session_write_rx) =
            tokio::sync::mpsc::unbounded_channel::<SessionWrite>();
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
                let result = write.write.await;
                let _ = write.reply.send(result);
            }
        });

        Ok(Self {
            services,
            account: Arc::new(Mutex::new(account)),
            fixtures_indexed: Arc::default(),
            lapsed,
            runtime: Some(runtime),
            import_progress: progress_tx,
            cloud,
            #[cfg(feature = "agent")]
            sync_status_fixture: None,
            session_revoked,
            replica_changed,
            sync_shutdown,
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

    /// A wall-clock delay, on the library's own runtime — what a UI debounce
    /// awaits. Not `gpui`'s `background_executor().timer`: the automation
    /// harness's headless dispatcher virtualizes gpui time and never advances
    /// it, while this runtime keeps real time on both hosts, exactly as the
    /// transport poll's `after` does.
    pub fn debounce(&self, wait: Duration) -> impl Future<Output = ()> + use<> {
        let task = self.runtime().spawn(async move {
            tokio::time::sleep(wait).await;
        });
        async move {
            task.await.ok();
        }
    }

    /// Run one command on the Tokio runtime: `run` calls its handler against
    /// the services it is handed, and `command` is the wire name a failure
    /// is reported under.
    ///
    /// The returned future is detached from `&self` so a caller can hold it
    /// across a GPUI task boundary without borrowing the `Library`.
    fn call<T, F, Fut>(
        &self,
        command: &'static str,
        run: F,
    ) -> impl Future<Output = Result<T, LibraryError>> + use<T, F, Fut>
    where
        T: Send + 'static,
        F: FnOnce(SharedServices) -> Fut,
        Fut: Future<Output = Result<T, CommandError>> + Send + 'static,
    {
        self.call_after(command, Duration::ZERO, run)
    }

    /// [`Self::call`], `after` a wait on the Tokio runtime. The wait is here
    /// rather than at the call site because this runtime is the one this
    /// process has real timers on — see [`Self::transport_after`].
    fn call_after<T, F, Fut>(
        &self,
        command: &'static str,
        after: Duration,
        run: F,
    ) -> impl Future<Output = Result<T, LibraryError>> + use<T, F, Fut>
    where
        T: Send + 'static,
        F: FnOnce(SharedServices) -> Fut,
        Fut: Future<Output = Result<T, CommandError>> + Send + 'static,
    {
        self.task(command, move |services| {
            let pending = run(services);
            async move {
                if !after.is_zero() {
                    tokio::time::sleep(after).await;
                }
                pending.await.map_err(refused(command))
            }
        })
    }

    /// Work on the Tokio runtime, started before this returns. What
    /// [`Self::call`] runs on, and what a method whose commands depend on
    /// each other uses directly: every command inside names its own failure
    /// with [`refused`], and `name` is what a cancelled task reports.
    fn task<T, F, Fut>(
        &self,
        name: &'static str,
        work: F,
    ) -> impl Future<Output = Result<T, LibraryError>> + use<T, F, Fut>
    where
        T: Send + 'static,
        F: FnOnce(SharedServices) -> Fut,
        Fut: Future<Output = Result<T, LibraryError>> + Send + 'static,
    {
        let task = self.runtime().spawn(work(self.services.clone()));
        async move {
            task.await
                .map_err(|error| LibraryError::at(name, Cause::Cancelled(error.to_string())))?
        }
    }
}

/// A command's refusal, as the [`LibraryError`] that names it.
fn refused(command: &'static str) -> impl FnOnce(CommandError) -> LibraryError {
    move |error| LibraryError::at(command, Cause::Command(error))
}

/// The app config directory: the harness's, else the backend's resolution.
pub(crate) fn config_dir() -> Result<StorageRoot, String> {
    match luma_ui::runtime::Runtime::with(|runtime| runtime.config_dir.clone()) {
        Some(path) => Ok(StorageRoot::from_path(path)),
        None => StorageRoot::from_env_default(),
    }
}

/// Root of the bundled fixture definitions: the harness's, else the backend's
/// resolution.
pub(crate) fn fixtures_root() -> Result<PathBuf, String> {
    match luma_ui::runtime::Runtime::with(|runtime| runtime.fixtures_root.clone()) {
        Some(path) => Ok(path),
        None => luma_lib::resources::fixtures_root(),
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        let _ = self.sync_shutdown.send(true);
        self.services.stop_sync();
        // A plain drop waits with no limit for every blocking task. PowerSync
        // runs its actors as blocking tasks that stop only when the last
        // handle to its database goes, and a handle that outlives the library
        // made quitting hang. Give them a moment, then let the process go.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
        }
    }
}

/// How long quitting waits for background work before it leaves it behind.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(2);
