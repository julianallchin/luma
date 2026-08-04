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

use luma_lib::agent_execution::graph_runs::{authorize_publish_target, GraphRunStore};
use luma_lib::agent_execution::sandbox;
use luma_lib::agent_execution::workspace::{PythonWorkspaceService, WorkerEnv};
use luma_lib::annotation_preview;
use luma_lib::audio::FftService;
use luma_lib::commands::agent_execution::{
    cancel_python_cell_inner, run_python_cell_inner_as_scoped,
};
use luma_lib::database::local::venue_access::{Read, VenueAccess, VenueResource};
use luma_lib::database::local::{
    agent_threads as threads_db, auth, categories as categories_db, database::init_app_db_at,
    groups as groups_db, patterns as patterns_db, scores as scores_db, state::init_state_db_at,
    venues as venues_db,
};
use luma_lib::database::local::{database::Db, state::StateDb};
use luma_lib::eval::graph_run::{evaluate_graph, EvaluateOptions};
use luma_lib::models::agent_execution::PythonScopeInput;
use luma_lib::models::agent_threads::{AppendAgentThreadMessagesInput, CreateAgentThreadInput};
use luma_lib::models::authored_state::{
    AuthoredProjectedDocument, AuthoredWorkspaceHandle, AuthoredWorkspaceInput,
    CommitAuthoredWorkspaceInput, CreateAuthoredWorkspaceInput, FinalizeAuthoredTurnInput,
    ForkAuthoredWorkspaceInput, MergeAuthoredWorkspaceInput,
    MergeAuthoredWorkspaceIntoWorkspaceInput, PrepareAuthoredTurnInput, RestoreAuthoredStateInput,
    WriteAuthoredWorkspaceGraphInput,
};
use luma_lib::models::node_graph::{BeatGrid, Graph, GraphContext};
use luma_lib::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, TrackScore, UpdateTrackScoreInput,
};
use luma_lib::services::authored_documents::AuthoredDocuments;
use luma_lib::services::graph_documents::{load_visible_graph_document, GraphEditResult};
use luma_lib::services::score_mutations;
use luma_lib::services::{fixtures as fixtures_service, groups as groups_service};
use luma_lib::services::{tracks as tracks_service, waveforms as waveforms_service};
use luma_lib::storage::StorageRoot;
use luma_lib::AnalysisTaskGroup;

/// Everything dispatch needs. The app assembles the same handles into Tauri
/// managed state; here they are plain fields.
struct Harness {
    db: Db,
    state_db: StateDb,
    /// Explicit trusted principal for a disposable headless fixture. The
    /// production app never has this seam; without it the harness resolves
    /// identity from the same verified state database as the desktop app.
    fixture_principal: Option<String>,
    storage: StorageRoot,
    authored: AuthoredDocuments,
    fixtures_root: PathBuf,
    fft: FftService,
    /// One Python kernel per agent thread, exactly as the app manages it. The
    /// interpreter is resolved on the first cell, so a machine with no venv
    /// only fails the commands that actually need one.
    workspaces: PythonWorkspaceService,
    graph_runs: GraphRunStore,
    analysis_tasks: AnalysisTaskGroup,
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
    async fn current_user_id(&self) -> Result<Option<String>, String> {
        match self.fixture_principal.as_ref() {
            Some(principal) => Ok(Some(principal.clone())),
            None => auth::get_current_user_id(&self.state_db.0).await,
        }
    }

    async fn dispatch(&self, cmd: &str, args: &Value) -> Result<Value, String> {
        let pool = &self.db.0;
        match cmd {
            // -- agent threads (commands/agent_threads.rs) ---------------------
            "agent_thread_create" => {
                let input: CreateAgentThreadInput = arg(args, "input")?;
                let owner_user_id = self.current_user_id().await?;
                ok(self
                    .authored
                    .create_thread_with_authored_state(pool, input, owner_user_id.as_deref())
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "agent_thread_get" => {
                let thread_id: String = arg(args, "threadId")?;
                let owner_user_id = self.current_user_id().await?;
                ok(threads_db::get_thread(pool, &thread_id, owner_user_id.as_deref()).await?)
            }
            "agent_thread_list" => {
                let agent_kind: Option<String> = opt_arg(args, "agentKind")?;
                let subject_kind: Option<String> = opt_arg(args, "subjectKind")?;
                let subject_id: Option<String> = opt_arg(args, "subjectId")?;
                let owner_user_id = self.current_user_id().await?;
                ok(threads_db::list_threads(
                    pool,
                    agent_kind.as_deref(),
                    subject_kind.as_deref(),
                    subject_id.as_deref(),
                    owner_user_id.as_deref(),
                )
                .await?)
            }
            "agent_thread_append_messages" => {
                let thread_id: String = arg(args, "threadId")?;
                let input: AppendAgentThreadMessagesInput = arg(args, "input")?;
                let owner_user_id = self.current_user_id().await?;
                ok(
                    threads_db::append_messages(pool, &thread_id, input, owner_user_id.as_deref())
                        .await?,
                )
            }
            "agent_thread_delete" => {
                let thread_id: String = arg(args, "threadId")?;
                let owner_user_id = self.current_user_id().await?;
                self.authored
                    .delete_thread_with_authored_state(
                        pool,
                        owner_user_id.as_deref(),
                        &thread_id,
                        |workspace_ids| async {
                            for workspace_id in workspace_ids {
                                self.workspaces.retire_thread(&workspace_id).await?;
                                self.graph_runs.forget(&workspace_id);
                            }
                            self.workspaces.retire_thread(&thread_id).await?;
                            self.graph_runs.forget(&thread_id);
                            Ok(())
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                ok(())
            }

            // -- agent code execution (commands/agent_execution.rs) ------------
            "run_python_cell" => {
                let thread_id: String = arg(args, "threadId")?;
                let turn_message_id: String = arg(args, "turnMessageId")?;
                let code: String = arg(args, "code")?;
                let scope: PythonScopeInput = arg(args, "scope")?;
                let execution_id: Option<String> = opt_arg(args, "executionId")?;
                let authored_workspace_id: Option<String> = opt_arg(args, "authoredWorkspaceId")?;
                let current_user_id = self.current_user_id().await?;
                ok(run_python_cell_inner_as_scoped(
                    pool,
                    &self.storage,
                    &self.fixtures_root,
                    &self.workspaces,
                    &self.graph_runs,
                    &self.authored,
                    thread_id,
                    code,
                    scope,
                    Some(turn_message_id),
                    current_user_id,
                    execution_id,
                    authored_workspace_id,
                )
                .await?)
            }
            "cancel_python_cell" => {
                let thread_id: String = arg(args, "threadId")?;
                let execution_id: Option<String> = opt_arg(args, "executionId")?;
                let authored_workspace_id: Option<String> = opt_arg(args, "authoredWorkspaceId")?;
                let owner_user_id = self.current_user_id().await?;
                threads_db::get_thread_row(pool, &thread_id, owner_user_id.as_deref()).await?;
                let execution_id = match (execution_id, authored_workspace_id.as_deref()) {
                    (None, None) => thread_id.clone(),
                    (Some(execution_id), Some(workspace_id)) if execution_id == workspace_id => {
                        self.authored
                            .authorize_workspace(
                                pool,
                                owner_user_id.as_deref(),
                                &thread_id,
                                workspace_id,
                            )
                            .await
                            .map_err(|error| error.to_string())?;
                        execution_id
                    }
                    (Some(_), Some(_)) => {
                        return Err(
                            "child Python execution id must match its authored workspace id".into(),
                        )
                    }
                    _ => return Err(
                        "child Python execution requires both execution and authored workspace ids"
                            .into(),
                    ),
                };
                ok(cancel_python_cell_inner(&self.workspaces, &execution_id))
            }
            "agent_thread_rename" => {
                let thread_id: String = arg(args, "threadId")?;
                let title: Option<String> = opt_arg(args, "title")?;
                let owner_user_id = self.current_user_id().await?;
                ok(threads_db::rename_thread(
                    pool,
                    &thread_id,
                    title.as_deref(),
                    owner_user_id.as_deref(),
                )
                .await?)
            }

            // -- relational authored state (commands/authored_state.rs) -------
            "authored_state_prepare_turn" => {
                let input: PrepareAuthoredTurnInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .prepare_turn(pool, principal.as_deref(), input)
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_finalize_turn" => {
                let input: FinalizeAuthoredTurnInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .finalize_turn(pool, principal.as_deref(), input)
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_recover_turns" => {
                let thread_id: String = arg(args, "threadId")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .recover_turns(pool, principal.as_deref(), &thread_id)
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_list_history" => {
                let thread_id: String = arg(args, "threadId")?;
                let cursor: Option<String> = opt_arg(args, "cursor")?;
                let limit: Option<usize> = opt_arg(args, "limit")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .list_history(
                        pool,
                        principal.as_deref(),
                        &thread_id,
                        cursor.as_deref(),
                        limit,
                    )
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_restore" => {
                let input: RestoreAuthoredStateInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .restore(
                        pool,
                        principal.as_deref(),
                        &input.thread_id,
                        &input.target_revision_id,
                        &input.operation_id,
                        input.mode,
                    )
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_create_workspace" => {
                let input: CreateAuthoredWorkspaceInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                let workspace = self
                    .authored
                    .create_workspace(pool, principal.as_deref(), input)
                    .await
                    .map_err(|error| error.to_string())?;
                ok(AuthoredWorkspaceHandle {
                    id: workspace.id,
                    base_revision_id: workspace.base_revision_id,
                    head_revision_id: workspace.head_revision_id,
                })
            }
            "authored_state_fork_workspace" => {
                let input: ForkAuthoredWorkspaceInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                let workspace = self
                    .authored
                    .fork_workspace(pool, principal.as_deref(), input)
                    .await
                    .map_err(|error| error.to_string())?;
                ok(AuthoredWorkspaceHandle {
                    id: workspace.id,
                    base_revision_id: workspace.base_revision_id,
                    head_revision_id: workspace.head_revision_id,
                })
            }
            "authored_state_check_workspace" => {
                let input: AuthoredWorkspaceInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .check_workspace(
                        pool,
                        principal.as_deref(),
                        &input.thread_id,
                        &input.workspace_id,
                    )
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_write_workspace_graph" => {
                let input: WriteAuthoredWorkspaceGraphInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .write_workspace_graph(
                        pool,
                        principal.as_deref(),
                        &input.thread_id,
                        &input.workspace_id,
                        &input.graph,
                    )
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_commit_workspace" => {
                let input: CommitAuthoredWorkspaceInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .commit_workspace(pool, principal.as_deref(), input)
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_merge_workspace" => {
                let input: MergeAuthoredWorkspaceInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .merge_workspace(pool, principal.as_deref(), input)
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_merge_workspace_into_workspace" => {
                let input: MergeAuthoredWorkspaceIntoWorkspaceInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                ok(self
                    .authored
                    .merge_workspace_into_workspace(pool, principal.as_deref(), input)
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "authored_state_remove_workspace" => {
                let input: AuthoredWorkspaceInput = arg(args, "input")?;
                let principal = self.current_user_id().await?;
                self.authored
                    .authorize_workspace_removal(
                        pool,
                        principal.as_deref(),
                        &input.thread_id,
                        &input.workspace_id,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                self.workspaces.retire_thread(&input.workspace_id).await?;
                self.authored
                    .remove_workspace(
                        pool,
                        principal.as_deref(),
                        &input.thread_id,
                        &input.workspace_id,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                self.graph_runs.forget(&input.workspace_id);
                ok(())
            }

            // -- patterns (commands/patterns.rs) -------------------------------
            "list_patterns" => ok(patterns_db::list_patterns_pool(pool).await?),
            "get_pattern" => {
                let id: String = arg(args, "id")?;
                ok(patterns_db::get_pattern_pool(pool, &id).await?)
            }
            "get_pattern_graph_document" => {
                let id: String = arg(args, "id")?;
                let requested: Option<String> = opt_arg(args, "implementationId")?;
                patterns_db::get_pattern_pool(pool, &id).await?;
                ok(
                    load_visible_graph_document(pool, &id, None, requested.as_deref())
                        .await
                        .map_err(|error| error.to_string())?,
                )
            }
            "get_pattern_args" => {
                let id: String = arg(args, "id")?;
                let venue_id: Option<String> = opt_arg(args, "venueId")?;
                let requested: Option<String> = opt_arg(args, "implementationId")?;
                patterns_db::get_pattern_pool(pool, &id).await?;
                ok(load_visible_graph_document(
                    pool,
                    &id,
                    venue_id.as_deref(),
                    requested.as_deref(),
                )
                .await
                .map_err(|error| error.to_string())?
                .graph
                .args)
            }
            "save_pattern_graph_document" => {
                let id: String = arg(args, "id")?;
                let implementation_id: String = arg(args, "implementationId")?;
                let operation_id: String = arg(args, "operationId")?;
                let base_revision: String = arg(args, "baseRevision")?;
                let graph: Graph = arg(args, "graph")?;
                let owner_user_id = self.current_user_id().await?;
                let result = self
                    .authored
                    .apply_graph_for_scope(
                        pool,
                        owner_user_id.as_deref(),
                        &id,
                        &implementation_id,
                        &operation_id,
                        graph,
                        &base_revision,
                        "Save pattern graph",
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let AuthoredProjectedDocument::PatternGraph {
                    implementation_id: projected_implementation_id,
                    revision,
                    graph,
                } = result.document
                else {
                    return Err("authored graph save returned a track projection".into());
                };
                if projected_implementation_id != implementation_id {
                    return Err("authored graph save returned another implementation".into());
                }
                ok(GraphEditResult {
                    revision,
                    graph,
                    changed: result.changed,
                })
            }
            "list_pattern_categories" => {
                ok(categories_db::list_pattern_categories_pool(pool).await?)
            }

            // -- scores (commands/scores.rs) -----------------------------------
            "list_scores_for_track" => {
                let track_id: String = arg(args, "trackId")?;
                let venue_id: String = arg(args, "venueId")?;
                if venue_id.is_empty() {
                    ok(scores_db::list_accessible_scores_for_track(pool, &track_id).await?)
                } else {
                    VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
                    let mut access =
                        VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
                    ok(scores_db::list_scores_for_track(&mut access, &track_id).await?)
                }
            }
            "create_score" => {
                let request_id: String = arg(args, "requestId")?;
                let track_id: String = arg(args, "trackId")?;
                let venue_id: String = arg(args, "venueId")?;
                let name: Option<String> = opt_arg(args, "name")?;
                ok(self
                    .authored
                    .create_score(pool, &request_id, &track_id, &venue_id, name.as_deref())
                    .await
                    .map_err(|error| error.to_string())?)
            }
            "list_track_scores" => {
                let score_id: String = arg(args, "scoreId")?;
                VenueAccess::<Read>::read(pool, VenueResource::Score(&score_id)).await?;
                let mut access =
                    VenueAccess::<Read>::read(pool, VenueResource::Score(&score_id)).await?;
                ok(scores_db::list_track_scores_for_score(&mut access, &score_id).await?)
            }
            "create_track_score" => {
                let payload: CreateTrackScoreInput = arg(args, "payload")?;
                ok(
                    score_mutations::create_track_score(&self.authored, pool, payload)
                        .await
                        .map_err(|error| error.to_string())?,
                )
            }
            "update_track_score" => {
                let payload: UpdateTrackScoreInput = arg(args, "payload")?;
                ok(
                    score_mutations::update_track_score(&self.authored, pool, payload)
                        .await
                        .map_err(|error| error.to_string())?,
                )
            }
            "delete_track_score" => {
                let payload: DeleteTrackScoreInput = arg(args, "payload")?;
                ok(
                    score_mutations::delete_track_score(&self.authored, pool, payload)
                        .await
                        .map_err(|error| error.to_string())?,
                )
            }
            "replace_track_scores" => {
                let score_id: String = arg(args, "scoreId")?;
                let track_id: String = arg(args, "trackId")?;
                let base_scores: Vec<TrackScore> = arg(args, "baseScores")?;
                let scores: Vec<TrackScore> = arg(args, "scores")?;
                let operation_id: String = arg(args, "operationId")?;
                ok(score_mutations::replace_track_scores(
                    &self.authored,
                    pool,
                    &score_id,
                    &track_id,
                    &base_scores,
                    &scores,
                    &operation_id,
                )
                .await
                .map_err(|error| error.to_string())?)
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
                ok(
                    waveforms_service::get_track_waveform(pool, &self.analysis_tasks, &track_id)
                        .await?,
                )
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
            "list_venues" => ok(venues_db::list_venues(pool).await?),
            "get_venue" => {
                let id: String = arg(args, "id")?;
                let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&id)).await?;
                ok(venues_db::get_venue(&mut access).await?)
            }
            // The command also pushes the patch into ArtNet; there is no ArtNet
            // manager headless (the app itself treats it as optional).
            "get_patched_fixtures" => {
                let venue_id: String = arg(args, "venueId")?;
                let mut access =
                    VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
                ok(fixtures_service::get_patched_fixtures(&mut access).await?)
            }
            "get_grouped_hierarchy" => {
                let venue_id: String = arg(args, "venueId")?;
                let mut access =
                    VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
                ok(groups_service::get_grouped_hierarchy_with_path(
                    &self.fixtures_root,
                    &mut access,
                )
                .await?)
            }
            "list_groups" => {
                let venue_id: String = arg(args, "venueId")?;
                let mut access =
                    VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
                ok(groups_db::list_groups(&mut access).await?)
            }

            // -- graph evaluation + previews -----------------------------------
            "get_node_types" => ok(luma_lib::node_graph::nodes::get_node_types()),
            // `run_graph` in the app also installs the result as the live scene.
            // Headless has no RenderEngine — the projection to `RunResult` is
            // the same `evaluate_graph(...).into_run_result()`.
            "run_graph" => {
                let graph: Graph = arg(args, "graph")?;
                let context: GraphContext = arg(args, "context")?;
                let _venue_access =
                    VenueAccess::<Read>::read(pool, VenueResource::Venue(&context.venue_id))
                        .await?;
                let include_mel: Option<bool> = opt_arg(args, "includeMelSpecs")?;
                let agent_thread_id: Option<String> = opt_arg(args, "agentThreadId")?;
                let agent_execution_id: Option<String> = opt_arg(args, "agentExecutionId")?;
                let owner_user_id = if let Some(thread_id) = agent_thread_id.as_deref() {
                    let owner_user_id = self.current_user_id().await?;
                    authorize_publish_target(pool, thread_id, owner_user_id.as_deref()).await?;
                    if let Some(execution_id) = agent_execution_id.as_deref() {
                        self.authored
                            .authorize_workspace(
                                pool,
                                owner_user_id.as_deref(),
                                thread_id,
                                execution_id,
                            )
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    owner_user_id
                } else {
                    None
                };
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
                    let execution_id = agent_execution_id.as_deref().unwrap_or(&thread_id);
                    self.graph_runs
                        .commit_evaluation(
                            pool,
                            &self.authored,
                            &thread_id,
                            owner_user_id.as_deref(),
                            execution_id,
                            std::sync::Arc::new(evaluation.clone()),
                            || {},
                        )
                        .await?;
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
    if let Some(principal) = cli.fixture_principal.as_deref() {
        // The caller explicitly owns this disposable fixture. Avoid creating
        // or copying a Supabase token merely to exercise authenticated command
        // plumbing; arm the same app-database admission gate startup normally
        // derives from the verified host session.
        auth::arm_write_admission(&db.0, Some(principal)).await?;
    } else {
        let recovered = {
            let mut state_connection = state_db
                .0
                .acquire()
                .await
                .map_err(|error| format!("Failed to lock harness auth state: {error}"))?;
            auth::recover_committed_signout(&db.0, &mut state_connection).await?
        };
        if !recovered {
            auth::get_session_item(&state_db.0, auth::SUPABASE_SESSION_KEY).await?;
            let mut state_connection = state_db
                .0
                .acquire()
                .await
                .map_err(|error| format!("Failed to lock harness auth state: {error}"))?;
            let recovered = auth::recover_committed_signout(&db.0, &mut state_connection).await?;
            if !recovered {
                let principal =
                    auth::load_verified_principal_for_connection(&mut state_connection).await?;
                auth::arm_write_admission(
                    &db.0,
                    principal
                        .as_ref()
                        .map(|principal| principal.user_id.as_str()),
                )
                .await?;
            }
        }
    }

    let workspaces = PythonWorkspaceService::new(
        storage.agent_workspaces_dir(),
        std::sync::Arc::new(move || resolve_worker_env(&cache_dir)),
    );

    let authored = AuthoredDocuments::new(storage.clone());

    let harness = Harness {
        db,
        state_db,
        fixture_principal: cli.fixture_principal,
        authored,
        storage,
        fixtures_root,
        fft: FftService::new(),
        workspaces,
        graph_runs: GraphRunStore::new(),
        analysis_tasks: AnalysisTaskGroup::new(),
    };

    if let Err(error) = luma_lib::agent_execution::thread_cleanup::recover_deleting_agent_threads(
        &harness.db.0,
        &harness.authored,
        &harness.workspaces,
        &harness.graph_runs,
    )
    .await
    {
        eprintln!("[agent-threads] startup deletion recovery: {error}");
    }

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
