//! Headless JSON-RPC harness over Luma's backend command surface.
//!
//! The desktop app reaches the Rust core through Tauri's IPC. Agent code — the
//! track copilot, the graph agent — is ordinary TypeScript that calls
//! `invoke("command_name", args)`. To exercise that code outside a window we
//! need the same command surface without an `AppHandle`, a WebView, or an
//! NSApplication. This binary is that surface: one JSON request per line on
//! stdin, one JSON response per line on stdout.
//!
//! ```text
//! ->  {"id": 1, "cmd": "list_patterns", "args": {}}
//! <-  {"id": 1, "ok": [ ... ]}
//! <-  {"id": 1, "err": "message"}
//! ```
//!
//! Paired with `scripts/headless/shim.ts`, which installs
//! `window.__TAURI_INTERNALS__.invoke` on top of this pipe, unmodified frontend
//! modules run under Bun against a real `luma.db`.
//!
//! This binary is a **thin adapter over `luma_lib::dispatch`**, the same seam
//! the desktop app's `#[tauri::command]` wrappers sit on: every command it
//! serves is the shared handler, so command *names* and *argument shapes* match
//! the Tauri registration by construction and the frontend is oblivious.
//!
//! Deliberately absent: ArtNet (its manager needs an `AppHandle`), audio
//! devices, and the loops — nothing spawns a render loop or a sync loop here,
//! so a `RenderEngine` exists but drives nothing. Emitted events go to stderr.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use std::io::{BufRead, Write};

use luma_lib::agent_execution::headless_env;
use luma_lib::agent_execution::workspace::PythonWorkspaceService;
use luma_lib::database::local::{auth, database::init_app_db_at, state::init_state_db_at};
use luma_lib::dispatch::{self, AppServices, EventSink, Events};
use luma_lib::services::fixtures as fixtures_service;
use luma_lib::storage::StorageRoot;

/// Emitted events land on stderr, alongside the harness's other diagnostics.
struct StderrEvents;

impl EventSink for StderrEvents {
    fn emit(&self, event: &str, payload: Value) {
        eprintln!("[event] {event} {payload}");
    }
}

// -----------------------------------------------------------------------------
// Setup
// -----------------------------------------------------------------------------

struct Cli {
    config_dir: Option<PathBuf>,
    fixtures_root: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    fixture_principal: Option<String>,
}

fn parse_cli() -> Result<Cli, String> {
    let mut cli = Cli {
        config_dir: None,
        fixtures_root: None,
        cache_dir: None,
        fixture_principal: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--config-dir" => {
                cli.config_dir = Some(PathBuf::from(
                    it.next()
                        .ok_or_else(|| "--config-dir requires a path".to_string())?,
                ));
            }
            "--fixtures-root" => {
                cli.fixtures_root =
                    Some(PathBuf::from(it.next().ok_or_else(|| {
                        "--fixtures-root requires a path".to_string()
                    })?));
            }
            "--cache-dir" => {
                cli.cache_dir = Some(PathBuf::from(
                    it.next()
                        .ok_or_else(|| "--cache-dir requires a path".to_string())?,
                ));
            }
            "--fixture-principal" => {
                let principal = it
                    .next()
                    .ok_or_else(|| "--fixture-principal requires an id".to_string())?;
                if principal.trim().is_empty() || principal.chars().any(char::is_control) {
                    return Err("--fixture-principal requires a non-empty printable id".into());
                }
                cli.fixture_principal = Some(principal);
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    if cli.fixture_principal.is_some() && cli.config_dir.is_none() {
        return Err("--fixture-principal requires an explicit --config-dir".into());
    }
    Ok(cli)
}

/// Repo-relative fixtures root, resolved against `CARGO_MANIFEST_DIR` rather
/// than the CWD so the harness works no matter where it was launched from.
/// Picks the newest (lexicographically greatest) version directory, matching
/// how `resolve_fixtures_root` hardcodes today's bundle.
fn repo_fixtures_root() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("resources/fixtures");
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .max()
}

fn resolve_config_dir(cli: &Cli) -> Result<StorageRoot, String> {
    if let Some(p) = &cli.config_dir {
        return Ok(StorageRoot::from_path(p.clone()));
    }
    if let Some(p) = std::env::var_os("LUMA_CONFIG_DIR") {
        return Ok(StorageRoot::from_path(PathBuf::from(p)));
    }
    StorageRoot::from_env_default()
}

/// The app cache dir, with the CLI's override ahead of the shared default.
fn resolve_cache_dir(cli: &Cli) -> Result<PathBuf, String> {
    match &cli.cache_dir {
        Some(path) => Ok(path.clone()),
        None => headless_env::cache_dir(),
    }
}

fn resolve_fixtures(cli: &Cli) -> Result<PathBuf, String> {
    if let Some(p) = &cli.fixtures_root {
        return Ok(p.clone());
    }
    if let Some(p) = std::env::var_os("LUMA_FIXTURES_ROOT") {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = repo_fixtures_root() {
        return Ok(p);
    }
    fixtures_service::resolve_fixtures_root_from(None)
}

// -----------------------------------------------------------------------------
// Main loop
// -----------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("[agent_harness] fatal: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let cli = parse_cli()?;
    let storage = resolve_config_dir(&cli)?;
    let fixtures_root = resolve_fixtures(&cli)?;

    let cache_dir = resolve_cache_dir(&cli)?;

    let db = init_app_db_at(storage.path()).await?;
    let state_db = init_state_db_at(storage.path()).await?;
    if let Some(principal) = cli.fixture_principal.as_deref() {
        // The caller explicitly owns this disposable fixture. Avoid creating
        // or copying a Supabase token merely to exercise authenticated command
        // plumbing; arm the same app-database admission gate startup normally
        // derives from the verified host session.
        auth::arm_write_admission(&db.0, Some(principal)).await?;
    } else {
        auth::bootstrap_host_admission(&db.0, &state_db.0).await?;
    }

    let workspaces = std::sync::Arc::new(PythonWorkspaceService::new(
        storage.agent_workspaces_dir(),
        std::sync::Arc::new(move || headless_env::resolve_worker_env(&cache_dir)),
    ));

    // Events go to stderr: stdout carries the response protocol and must stay
    // one JSON frame per line.
    let services = AppServices::headless(db, state_db, storage, fixtures_root, workspaces)
        .with_events(Events::new(StderrEvents))
        .with_fixture_principal(cli.fixture_principal);

    if let Err(error) = luma_lib::agent_execution::thread_cleanup::recover_deleting_agent_threads(
        &services.db().0,
        services.authored(),
        services.workspaces(),
        services.graph_runs(),
    )
    .await
    {
        eprintln!("[agent-threads] startup deletion recovery: {error}");
    }

    eprintln!(
        "[agent_harness] ready: config={} fixtures={}",
        services.storage().path().display(),
        services.fixtures_root().display()
    );

    // Requests are dispatched concurrently, one task each, because Tauri's IPC
    // is concurrent and some pairs of commands only make sense that way:
    // `cancel_python_cell` exists precisely to interrupt a `run_python_cell`
    // that is still in flight, and a strictly serial loop could never deliver
    // it. Responses are matched by `id`, so completion order is free.
    let services = std::sync::Arc::new(services);
    let stdout = std::sync::Arc::new(tokio::sync::Mutex::new(std::io::stdout()));

    // stdin is blocking; read it on its own thread and feed the runtime.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    eprintln!("[agent_harness] stdin read failed: {e}");
                    return;
                }
            }
        }
    });

    while let Some(line) = rx.recv().await {
        if line.trim().is_empty() {
            continue;
        }
        let services = std::sync::Arc::clone(&services);
        let stdout = std::sync::Arc::clone(&stdout);
        // A malformed frame must never take the process down — the shim keeps
        // one long-lived child and would lose every in-flight call.
        tokio::spawn(async move {
            let response = match serde_json::from_str::<Value>(&line) {
                Err(e) => {
                    json!({ "id": Value::Null, "err": format!("malformed request JSON: {e}") })
                }
                Ok(req) => {
                    let id = req.get("id").cloned().unwrap_or(Value::Null);
                    match req.get("cmd").and_then(|c| c.as_str()) {
                        None => json!({ "id": id, "err": "request is missing `cmd`" }),
                        Some(cmd) => {
                            let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
                            match dispatch::dispatch(&services, cmd, &args).await {
                                Ok(v) => json!({ "id": id, "ok": v }),
                                Err(e) => json!({ "id": id, "err": String::from(e) }),
                            }
                        }
                    }
                }
            };
            let Ok(mut buf) = serde_json::to_vec(&response) else {
                eprintln!("[agent_harness] response was not serializable");
                return;
            };
            buf.push(b'\n');
            let mut out = stdout.lock().await;
            if let Err(e) = out.write_all(&buf).and_then(|()| out.flush()) {
                eprintln!("[agent_harness] stdout write failed: {e}");
            }
        });
    }

    Ok(())
}
