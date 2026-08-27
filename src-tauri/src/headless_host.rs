//! Bootstrapping a windowless Luma host.
//!
//! Two binaries serve Luma's backend to an out-of-process client — the JSON-RPC
//! `agent_harness` and the MCP `luma-mcp` server — and neither has an
//! `AppHandle`, a WebView, or an NSApplication. What they share is everything up
//! to the first request: the same flags, the same database migrations, the same
//! write-admission gate, the same managed-venv workspace service, and the same
//! startup recovery of half-deleted agent threads. Only the wire protocol
//! differs, so only the wire protocol lives in the bins.
//!
//! Deliberately absent, exactly as [`AppServices::headless`] describes: ArtNet,
//! audio devices, and the loops. Events go to stderr, because both hosts put
//! their protocol on stdout.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_execution::headless_env;
use crate::database::local::{auth, database::init_app_db_at, state::init_state_db_at};
use crate::dispatch::{AppServices, EventSink, Events};
use crate::services::fixtures as fixtures_service;
use crate::storage::StorageRoot;

/// Emitted events land on stderr, alongside the host's other diagnostics.
struct StderrEvents;

impl EventSink for StderrEvents {
    fn emit(&self, event: &str, payload: Value) {
        eprintln!("[event] {event} {payload}");
    }
}

/// Where a headless host reads its library, fixtures and managed venv from.
///
/// Every field is an override: `None` means "resolve the way the app would".
#[derive(Default, Debug, Clone)]
pub struct HostConfig {
    config_dir: Option<PathBuf>,
    fixtures_root: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    fixture_principal: Option<String>,
}

impl HostConfig {
    /// Parse the shared headless flags: `--config-dir`, `--fixtures-root`,
    /// `--cache-dir`, `--fixture-principal`.
    ///
    /// `--fixture-principal` is a host-only trusted-identity seam. It arms the
    /// app-database write gate without a verified Supabase session, so it is
    /// only legible against a disposable config dir the caller nominated — hence
    /// the `--config-dir` requirement.
    ///
    /// # Errors
    ///
    /// On an unknown flag, a flag missing its value, or a principal given
    /// without an explicit config dir.
    pub fn parse_args(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut config = Self::default();
        let mut args = args;
        while let Some(flag) = args.next() {
            let mut value = |flag: &str| {
                args.next()
                    .ok_or_else(|| format!("{flag} requires a value"))
            };
            match flag.as_str() {
                "--config-dir" => config.config_dir = Some(PathBuf::from(value("--config-dir")?)),
                "--fixtures-root" => {
                    config.fixtures_root = Some(PathBuf::from(value("--fixtures-root")?));
                }
                "--cache-dir" => config.cache_dir = Some(PathBuf::from(value("--cache-dir")?)),
                "--fixture-principal" => {
                    let principal = value("--fixture-principal")?;
                    if principal.trim().is_empty() || principal.chars().any(char::is_control) {
                        return Err("--fixture-principal requires a non-empty printable id".into());
                    }
                    config.fixture_principal = Some(principal);
                }
                other => return Err(format!("unknown flag `{other}`")),
            }
        }
        if config.fixture_principal.is_some() && config.config_dir.is_none() {
            return Err("--fixture-principal requires an explicit --config-dir".into());
        }
        Ok(config)
    }

    /// The library directory this host will open.
    fn storage(&self) -> Result<StorageRoot, String> {
        if let Some(path) = &self.config_dir {
            return Ok(StorageRoot::from_path(path.clone()));
        }
        if let Some(path) = std::env::var_os("LUMA_CONFIG_DIR") {
            return Ok(StorageRoot::from_path(PathBuf::from(path)));
        }
        StorageRoot::from_env_default()
    }

    fn cache_dir(&self) -> Result<PathBuf, String> {
        match &self.cache_dir {
            Some(path) => Ok(path.clone()),
            None => headless_env::cache_dir(),
        }
    }

    fn fixtures_root(&self) -> Result<PathBuf, String> {
        if let Some(path) = &self.fixtures_root {
            return Ok(path.clone());
        }
        if let Some(path) = std::env::var_os("LUMA_FIXTURES_ROOT") {
            return Ok(PathBuf::from(path));
        }
        if let Some(path) = repo_fixtures_root() {
            return Ok(path);
        }
        fixtures_service::resolve_fixtures_root_from(None)
    }
}

/// Repo-relative fixtures root, resolved against `CARGO_MANIFEST_DIR` rather
/// than the CWD so a headless host works no matter where it was launched from.
/// Picks the newest (lexicographically greatest) version directory, matching
/// how `resolve_fixtures_root` hardcodes today's bundle.
fn repo_fixtures_root() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("resources/fixtures");
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .max()
}

/// Migrate the databases, arm write admission, and assemble the service graph.
///
/// Migrations run against whatever config dir this is given, exactly as the app
/// does — pointing a host at an empty directory produces a fresh, fully
/// migrated `luma.db` + `state.db`.
///
/// # Errors
///
/// If a path cannot be resolved, a migration fails, or write admission cannot
/// be armed.
pub async fn boot(config: &HostConfig) -> Result<AppServices, String> {
    let storage = config.storage()?;
    let fixtures_root = config.fixtures_root()?;
    let cache_dir = config.cache_dir()?;

    // No `AppHandle` here, so the bundled binary is found from the executable's
    // own location instead of a Tauri resource dir. Without this a headless
    // host falls through to system PATH and fails at spawn time.
    crate::ffmpeg_env::init_headless();

    let db = init_app_db_at(storage.path()).await?;
    let state_db = init_state_db_at(storage.path()).await?;
    if let Some(principal) = config.fixture_principal.as_deref() {
        // The caller explicitly owns this disposable fixture. Avoid creating
        // or copying a Supabase token merely to exercise authenticated command
        // plumbing; arm the same app-database admission gate startup normally
        // derives from the verified host session.
        auth::arm_write_admission(&db.0, Some(principal)).await?;
    } else {
        auth::bootstrap_host_admission(&db.0, &state_db.0).await?;
    }

    let workspaces = std::sync::Arc::new(headless_env::workspace_service(&storage, cache_dir));

    let services = AppServices::headless(db, state_db, storage, fixtures_root, workspaces)
        .with_events(Events::new(StderrEvents))
        .with_fixture_principal(config.fixture_principal.clone());

    if let Err(error) = crate::agent_execution::thread_cleanup::recover_threads(
        &services.db().0,
        services.authored(),
        services.workspaces(),
        services.graph_runs(),
        &services.subagents,
    )
    .await
    {
        eprintln!("[agent-threads] startup recovery: {error}");
    }

    Ok(services)
}
