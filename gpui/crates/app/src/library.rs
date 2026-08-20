//! The app's whole connection to Luma's data: a typed, awaitable client over
//! the command dispatcher.
//!
//! # Interface
//!
//! ```ignore
//! let library = Library::open()?;
//! let venues: Vec<Venue> = library.venues().await?;
//! let tracks: Vec<TrackBrowserRow> = library.tracks(&venue.id).await?;
//! ```
//!
//! Everything a screen knows about data access is those two methods. It never
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

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use luma_lib::agent_execution::workspace::PythonWorkspaceService;
use luma_lib::database::local::database::init_app_db_at;
use luma_lib::database::local::state::init_state_db_at;
use luma_lib::dispatch::{dispatch, AppServices};
use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::venues::Venue;
use luma_lib::services::fixtures as fixtures_service;
use luma_lib::storage::StorageRoot;

/// A connection to the real Luma library.
pub struct Library {
    services: Arc<AppServices>,
    /// Owns the reactor every dispatched command runs on. Dropping it cancels
    /// in-flight work, so it lives exactly as long as the `Library`.
    runtime: tokio::runtime::Runtime,
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

        let services = runtime.block_on(async {
            let db = init_app_db_at(storage.path()).await?;
            let state_db = init_state_db_at(storage.path()).await?;
            // Read-only v0: nothing here runs Python, and a workspace service
            // resolves its worker environment lazily, so refusing to build one
            // is both honest and harmless. The day this app runs an agent, it
            // grows the same `resolve_worker_env` the headless harness has.
            let workspaces = Arc::new(PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("the GPUI app does not run Python workspaces".to_string())),
            ));
            Ok::<_, String>(AppServices::headless(
                db,
                state_db,
                storage,
                fixtures_root,
                workspaces,
            ))
        })?;

        Ok(Self {
            services: Arc::new(services),
            runtime,
        })
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

    /// Run one command on the Tokio runtime and decode its result.
    ///
    /// The returned future is detached from `&self` so a caller can hold it
    /// across a GPUI task boundary without borrowing the `Library`.
    fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        name: &'static str,
        args: Value,
    ) -> impl Future<Output = Result<T, String>> + use<T> {
        let services = Arc::clone(&self.services);
        let task = self.runtime.spawn(async move {
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
