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
//! **Dispatch calls the underlying service/db functions, never the
//! `#[tauri::command]` wrappers** (those are `State`-injected and unreachable
//! here). Command *names* and *argument shapes* match the Tauri registration
//! exactly — that equality is the whole contract, so the frontend is oblivious.
//! Where a command body was more than a delegation, the body moved into a
//! shared service fn that both callers use; nothing is forked.
//!
//! Deliberately absent: `RenderEngine` (so `run_graph` installs no scene),
//! audio devices, ArtNet, the sync loop.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

use luma_lib::agent_execution::graph_runs::GraphRunStore;
use luma_lib::agent_execution::sandbox;
use luma_lib::agent_execution::workspace::{PythonWorkspaceService, WorkerEnv};
use luma_lib::annotation_preview;
use luma_lib::audio::FftService;
use luma_lib::commands::agent_execution::{cancel_python_cell_inner, run_python_cell_inner};
use luma_lib::database::local::{
    agent_threads as threads_db, auth, categories as categories_db, database::init_app_db_at,
    groups as groups_db, patterns as patterns_db, scores as scores_db, state::init_state_db_at,
    venues as venues_db,
};
use luma_lib::database::local::{database::Db, state::StateDb};
use luma_lib::eval::graph_run::{evaluate_graph, EvaluateOptions};
use luma_lib::models::agent_execution::PythonScopeInput;
use luma_lib::models::agent_threads::{CreateAgentThreadInput, NewAgentThreadMessage};
use luma_lib::models::node_graph::{BeatGrid, Graph, GraphContext};
use luma_lib::models::scores::{CreateTrackScoreInput, TrackScore, UpdateTrackScoreInput};
use luma_lib::services::{fixtures as fixtures_service, groups as groups_service};
use luma_lib::services::{tracks as tracks_service, waveforms as waveforms_service};
use luma_lib::storage::StorageRoot;

/// Everything dispatch needs. The app assembles the same handles into Tauri
/// managed state; here they are plain fields.
struct Harness {
    db: Db,
    state_db: StateDb,
    storage: StorageRoot,
    fixtures_root: PathBuf,
    fft: FftService,
    /// One Python kernel per agent thread, exactly as the app manages it. The
    /// interpreter is resolved on the first cell, so a machine with no venv
    /// only fails the commands that actually need one.
    workspaces: PythonWorkspaceService,
    graph_runs: GraphRunStore,
}

// -----------------------------------------------------------------------------
// Argument extraction
// -----------------------------------------------------------------------------

/// Tauri deserializes command args from the JS object, which is camelCase by
/// convention on both sides. Accept the snake_case spelling too so a Rust-side
/// caller (the smoke driver's raw frames) doesn't have to guess.
fn lookup<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(v) = args.get(key) {
        if !v.is_null() {
            return Some(v);
        }
    }
    let snake = to_snake_case(key);
    args.get(&snake).filter(|v| !v.is_null())
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let v = lookup(args, key).ok_or_else(|| format!("missing required argument `{key}`"))?;
    serde_json::from_value(v.clone()).map_err(|e| format!("bad argument `{key}`: {e}"))
}

fn opt_arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<Option<T>, String> {
    match lookup(args, key) {
        None => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("bad argument `{key}`: {e}")),
    }
}

fn ok<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| format!("failed to serialize result: {e}"))
}

// -----------------------------------------------------------------------------
// Dispatch
// -----------------------------------------------------------------------------

impl Harness {
    async fn dispatch(&self, cmd: &str, args: &Value) -> Result<Value, String> {
        let pool = &self.db.0;
        match cmd {
            // -- agent threads (commands/agent_threads.rs) ---------------------
            "agent_thread_create" => {
                let input: CreateAgentThreadInput = arg(args, "input")?;
                ok(threads_db::create_thread(pool, input).await?)
            }
            "agent_thread_get" => {
                let thread_id: String = arg(args, "threadId")?;
                ok(threads_db::get_thread(pool, &thread_id).await?)
            }
            "agent_thread_list" => {
                let agent_kind: Option<String> = opt_arg(args, "agentKind")?;
                let subject_kind: Option<String> = opt_arg(args, "subjectKind")?;
                let subject_id: Option<String> = opt_arg(args, "subjectId")?;
                ok(threads_db::list_threads(
                    pool,
                    agent_kind.as_deref(),
                    subject_kind.as_deref(),
                    subject_id.as_deref(),
                )
                .await?)
            }
            "agent_thread_append_messages" => {
                let thread_id: String = arg(args, "threadId")?;
                let messages: Vec<NewAgentThreadMessage> = arg(args, "messages")?;
                ok(threads_db::append_messages(pool, &thread_id, messages).await?)
            }
            "agent_thread_truncate_from" => {
                let thread_id: String = arg(args, "threadId")?;
                let seq: i64 = arg(args, "seq")?;
                ok(threads_db::truncate_from_seq(pool, &thread_id, seq).await?)
            }
            // Reset and delete own live Python state as well as rows — a reset
            // that left the kernel running would keep invisible state across a
            // conversation the user believes is empty (design §13.5).
            "agent_thread_reset" => {
                let thread_id: String = arg(args, "threadId")?;
                let deleted = threads_db::reset_thread(pool, &thread_id).await?;
                self.graph_runs.forget(&thread_id);
                self.workspaces.workspace_for(&thread_id)?.reset()?;
                ok(deleted)
            }
            "agent_thread_delete" => {
                let thread_id: String = arg(args, "threadId")?;
                threads_db::delete_thread(pool, &thread_id).await?;
                self.graph_runs.forget(&thread_id);
                ok(self.workspaces.shutdown_thread(&thread_id)?)
            }

            // -- agent code execution (commands/agent_execution.rs) ------------
            "run_python_cell" => {
                let thread_id: String = arg(args, "threadId")?;
                let code: String = arg(args, "code")?;
                let scope: PythonScopeInput = arg(args, "scope")?;
                ok(run_python_cell_inner(
                    pool,
                    &self.storage,
                    &self.fixtures_root,
                    &self.workspaces,
                    &self.graph_runs,
                    thread_id,
                    code,
                    scope,
                )
                .await?)
            }
            "cancel_python_cell" => {
                let thread_id: String = arg(args, "threadId")?;
                ok(cancel_python_cell_inner(&thread_id))
            }
            "agent_thread_rename" => {
                let thread_id: String = arg(args, "threadId")?;
                let title: Option<String> = opt_arg(args, "title")?;
                ok(threads_db::rename_thread(pool, &thread_id, title.as_deref()).await?)
            }

            // -- patterns (commands/patterns.rs) -------------------------------
            "list_patterns" => ok(patterns_db::list_patterns_pool(pool).await?),
            "get_pattern" => {
                let id: String = arg(args, "id")?;
                ok(patterns_db::get_pattern_pool(pool, &id).await?)
            }
            "get_pattern_graph" => {
                let id: String = arg(args, "id")?;
                ok(patterns_db::get_pattern_graph_pool(pool, &id).await?)
            }
            "get_pattern_args" => {
                let id: String = arg(args, "id")?;
                ok(patterns_db::get_pattern_args_pool(pool, &id).await?)
            }
            // The command additionally pokes the sync engine; headless has no
            // sync loop, so the DB write is the whole operation.
            "save_pattern_graph" => {
                let id: String = arg(args, "id")?;
                let graph_json: String = arg(args, "graphJson")?;
                ok(patterns_db::save_pattern_graph_pool(pool, &id, graph_json).await?)
            }
            "list_pattern_categories" => {
                ok(categories_db::list_pattern_categories_pool(pool).await?)
            }

            // -- scores (commands/scores.rs) -----------------------------------
            "list_scores_for_track" => {
                let track_id: String = arg(args, "trackId")?;
                let venue_id: String = arg(args, "venueId")?;
                ok(scores_db::list_scores_for_track(pool, &track_id, &venue_id).await?)
            }
            "create_score" => {
                let track_id: String = arg(args, "trackId")?;
                let venue_id: String = arg(args, "venueId")?;
                let uid: String = arg(args, "uid")?;
                let name: Option<String> = opt_arg(args, "name")?;
                ok(
                    scores_db::create_score(pool, &track_id, &venue_id, &uid, name.as_deref())
                        .await?,
                )
            }
            "list_track_scores" => {
                let score_id: String = arg(args, "scoreId")?;
                ok(scores_db::list_track_scores_for_score(pool, &score_id).await?)
            }
            "create_track_score" => {
                let payload: CreateTrackScoreInput = arg(args, "payload")?;
                ok(scores_db::create_track_score(pool, payload).await?)
            }
            "update_track_score" => {
                let payload: UpdateTrackScoreInput = arg(args, "payload")?;
                ok(scores_db::update_track_score(pool, payload).await?)
            }
            "delete_track_score" => {
                let id: String = arg(args, "id")?;
                ok(scores_db::delete_track_score(pool, &id).await?)
            }
            "replace_track_scores" => {
                let score_id: String = arg(args, "scoreId")?;
                let track_id: String = arg(args, "trackId")?;
                let scores: Vec<TrackScore> = arg(args, "scores")?;
                ok(scores_db::replace_track_scores(pool, &score_id, &track_id, scores).await?)
            }

            // -- tracks (commands/tracks.rs, commands/waveforms.rs) ------------
            "list_tracks" => ok(tracks_service::list_tracks(pool).await?),
            "list_tracks_enriched" => {
                let venue_id: Option<String> = opt_arg(args, "venueId")?;
                ok(tracks_service::list_tracks_enriched(pool, venue_id.as_deref()).await?)
            }
            "get_track_beats" => {
                let track_id: String = arg(args, "trackId")?;
                ok(tracks_service::get_track_beats(pool, &track_id).await?)
            }
            "get_track_waveform" => {
                let track_id: String = arg(args, "trackId")?;
                ok(waveforms_service::get_track_waveform(pool, &track_id).await?)
            }
            "get_track_bar_classifications" => {
                let track_id: String = arg(args, "trackId")?;
                ok(tracks_service::get_track_bar_classifications(pool, &track_id).await?)
            }
            "get_track_drum_onsets" => {
                let track_id: String = arg(args, "trackId")?;
                ok(
                    luma_lib::database::local::tracks::get_track_drum_onsets(pool, &track_id)
                        .await?,
                )
            }
            "get_classifier_thresholds" => ok(tracks_service::classifier_thresholds()?),

            // -- venues, fixtures, groups --------------------------------------
            "list_venues" => match auth::get_current_user_id(&self.state_db.0).await? {
                Some(uid) => ok(venues_db::list_venues_for_user(pool, &uid).await?),
                None => ok(venues_db::list_venues(pool).await?),
            },
            "get_venue" => {
                let id: String = arg(args, "id")?;
                ok(venues_db::get_venue(pool, &id).await?)
            }
            // The command also pushes the patch into ArtNet; there is no ArtNet
            // manager headless (the app itself treats it as optional).
            "get_patched_fixtures" => {
                let venue_id: String = arg(args, "venueId")?;
                ok(fixtures_service::get_patched_fixtures(pool, &venue_id).await?)
            }
            "get_grouped_hierarchy" => {
                let venue_id: String = arg(args, "venueId")?;
                ok(groups_service::get_grouped_hierarchy_with_path(
                    &self.fixtures_root,
                    pool,
                    &venue_id,
                )
                .await?)
            }
            "list_groups" => {
                let venue_id: String = arg(args, "venueId")?;
                ok(groups_db::list_groups(pool, &venue_id).await?)
            }

            // -- graph evaluation + previews -----------------------------------
            "get_node_types" => ok(luma_lib::node_graph::nodes::get_node_types()),
            // `run_graph` in the app also installs the result as the live scene.
            // Headless has no RenderEngine — the projection to `RunResult` is
            // the same `evaluate_graph(...).into_run_result()`.
            "run_graph" => {
                let graph: Graph = arg(args, "graph")?;
                let context: GraphContext = arg(args, "context")?;
                let include_mel: Option<bool> = opt_arg(args, "includeMelSpecs")?;
                let agent_thread_id: Option<String> = opt_arg(args, "agentThreadId")?;
                let evaluation = evaluate_graph(
                    pool,
                    &self.storage,
                    &self.fixtures_root,
                    &self.fft,
                    &graph,
                    &context,
                    EvaluateOptions {
                        include_mel: include_mel.unwrap_or(true),
                    },
                )
                .await?;
                if let Some(thread_id) = agent_thread_id {
                    self.graph_runs
                        .publish(&thread_id, std::sync::Arc::new(evaluation.clone()));
                }
                ok(evaluation.into_run_result())
            }
            "preview_pattern_image" => ok(annotation_preview::preview_pattern_image_at(
                pool,
                &self.storage,
                &self.fixtures_root,
                &arg::<String>(args, "patternId")?,
                &arg::<String>(args, "trackId")?,
                &arg::<String>(args, "venueId")?,
                arg(args, "startTime")?,
                arg(args, "endTime")?,
                opt_arg::<BeatGrid>(args, "beatGrid")?,
            )
            .await?),
            "preview_graph_image" => ok(annotation_preview::preview_graph_image_at(
                pool,
                &self.storage,
                &self.fixtures_root,
                &arg::<Graph>(args, "graph")?,
                &arg::<String>(args, "trackId")?,
                &arg::<String>(args, "venueId")?,
                arg(args, "startTime")?,
                arg(args, "endTime")?,
                opt_arg::<BeatGrid>(args, "beatGrid")?,
            )
            .await?),
            "view_composite_image" => ok(annotation_preview::view_composite_image_at(
                pool,
                &self.storage,
                &self.fixtures_root,
                &arg::<String>(args, "trackId")?,
                arg(args, "startTime")?,
                arg(args, "endTime")?,
            )
            .await?),

            other => Err(format!("unknown command `{other}`")),
        }
    }
}

// -----------------------------------------------------------------------------
// Setup
// -----------------------------------------------------------------------------

struct Cli {
    config_dir: Option<PathBuf>,
    fixtures_root: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
}

fn parse_cli() -> Result<Cli, String> {
    let mut cli = Cli {
        config_dir: None,
        fixtures_root: None,
        cache_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut take = |name: &str| {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} requires a path"))
        };
        match flag.as_str() {
            "--config-dir" => cli.config_dir = Some(take("--config-dir")?),
            "--fixtures-root" => cli.fixtures_root = Some(take("--fixtures-root")?),
            "--cache-dir" => cli.cache_dir = Some(take("--cache-dir")?),
            other => return Err(format!("unknown flag `{other}`")),
        }
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

/// The app cache dir: where the managed venv and the deployed `luma_exec`
/// package live. The app derives it from Tauri's identifier; headless we
/// reconstruct the same path.
fn resolve_cache_dir(cli: &Cli) -> Result<PathBuf, String> {
    if let Some(p) = &cli.cache_dir {
        return Ok(p.clone());
    }
    if let Some(p) = std::env::var_os("LUMA_CACHE_DIR") {
        return Ok(PathBuf::from(p));
    }
    dirs::cache_dir()
        .map(|p| p.join("com.luma.luma"))
        .ok_or_else(|| "could not locate a cache directory".to_string())
}

/// The worker environment without an `AppHandle`: the venv must already exist
/// (headless never creates one — that is minutes of work the app does at
/// startup), and the worker script comes from the repo, falling back to the
/// copy the app deploys into its cache.
fn resolve_worker_env(cache_dir: &Path) -> Result<WorkerEnv, String> {
    let python_bin =
        luma_lib::python_env::find_existing_venv_python(cache_dir).ok_or_else(|| {
            format!(
                "no managed python environment under {} — run the app once to create it",
                cache_dir.display()
            )
        })?;

    let repo_script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("python")
        .join("luma_exec")
        .join("worker.py");
    let worker_script = if repo_script.exists() {
        repo_script
    } else {
        cache_dir.join("luma_exec").join("worker.py")
    };
    if !worker_script.exists() {
        return Err(format!(
            "agent python worker missing at {}",
            worker_script.display()
        ));
    }
    Ok(WorkerEnv::new(
        python_bin,
        worker_script,
        std::sync::Arc::new(sandbox::default_launcher),
    ))
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

    let workspaces = PythonWorkspaceService::new(
        storage.agent_workspaces_dir(),
        std::sync::Arc::new(move || resolve_worker_env(&cache_dir)),
    );

    let harness = Harness {
        db,
        state_db,
        storage,
        fixtures_root,
        fft: FftService::new(),
        workspaces,
        graph_runs: GraphRunStore::new(),
    };

    eprintln!(
        "[agent_harness] ready: config={} fixtures={}",
        harness.storage.path().display(),
        harness.fixtures_root.display()
    );

    // Requests are dispatched concurrently, one task each, because Tauri's IPC
    // is concurrent and some pairs of commands only make sense that way:
    // `cancel_python_cell` exists precisely to interrupt a `run_python_cell`
    // that is still in flight, and a strictly serial loop could never deliver
    // it. Responses are matched by `id`, so completion order is free.
    let harness = std::sync::Arc::new(harness);
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
        let harness = std::sync::Arc::clone(&harness);
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
                            match harness.dispatch(cmd, &args).await {
                                Ok(v) => json!({ "id": id, "ok": v }),
                                Err(e) => json!({ "id": id, "err": e }),
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
