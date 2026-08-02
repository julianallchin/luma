//! Tauri commands for durable agent threads.
//!
//! No `AppHandle` anywhere — the lifecycle commands take the database and, where
//! a thread owns live execution state, the workspace service and the graph-run
//! store, so the whole surface stays reachable from the headless harness.

use tauri::State;

use crate::agent_execution::graph_runs::GraphRunStore;
use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::database::local::{agent_threads as db, auth};
use crate::database::Db;
use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, AppendAgentThreadMessagesInput,
    CreateAgentThreadInput,
};
use crate::services::authored_documents::AuthoredDocuments;

/// Resume every terminal deletion discovered in durable storage. This runs at
/// startup in both the desktop app and headless harness; individual failures
/// do not prevent unrelated rows from being cleaned and remain retryable on
/// the next startup.
pub async fn recover_deleting_agent_threads(
    pool: &sqlx::SqlitePool,
    authored: &AuthoredDocuments,
    workspaces: &PythonWorkspaceService,
    graph_runs: &GraphRunStore,
) -> Result<usize, String> {
    let admission = sqlx::query_as::<_, (i64, i64, i64, i64, Option<String>)>(
        "SELECT armed, accepting, maintenance, remote_writes, active_uid
         FROM auth_write_admission WHERE singleton = 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to inspect thread-recovery admission: {error}"))?;
    let Some((1, 1, 0, 0, principal)) = admission else {
        // A closed identity boundary has no authority to clean either the
        // guest namespace or a previously authenticated user's threads.
        return Ok(0);
    };
    let threads = db::list_deleting_threads(pool, principal.as_deref()).await?;
    let mut recovered = 0usize;
    let mut failures = Vec::new();
    for thread in threads {
        let thread_id = thread.id.clone();
        let result = authored
            .delete_thread_with_authored_state(
                pool,
                thread.owner_user_id.as_deref(),
                &thread_id,
                || async {
                    workspaces.retire_thread(&thread_id).await?;
                    graph_runs.forget(&thread_id);
                    Ok(())
                },
            )
            .await;
        match result {
            Ok(_) => recovered += 1,
            Err(error) => failures.push(format!("{thread_id}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(recovered)
    } else {
        Err(format!(
            "failed to recover {} deleting agent thread(s): {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

#[tauri::command]
pub async fn agent_thread_create(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: CreateAgentThreadInput,
) -> Result<AgentThread, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    authored
        .create_thread_with_authored_state(&db.0, input, owner_user_id.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn agent_thread_get(
    db: State<'_, Db>,
    thread_id: String,
) -> Result<AgentThreadDetail, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    db::get_thread(&db.0, &thread_id, owner_user_id.as_deref()).await
}

#[tauri::command]
pub async fn agent_thread_list(
    db: State<'_, Db>,
    agent_kind: Option<String>,
    subject_kind: Option<String>,
    subject_id: Option<String>,
) -> Result<Vec<AgentThread>, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    db::list_threads(
        &db.0,
        agent_kind.as_deref(),
        subject_kind.as_deref(),
        subject_id.as_deref(),
        owner_user_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn agent_thread_append_messages(
    db: State<'_, Db>,
    thread_id: String,
    input: AppendAgentThreadMessagesInput,
) -> Result<Vec<AgentThreadMessage>, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    db::append_messages(&db.0, &thread_id, input, owner_user_id.as_deref()).await
}

/// Delete the thread after its child worktrees, kernel workspace, and
/// published graph run have been retired. Git history remains restorable.
#[tauri::command]
pub async fn agent_thread_delete(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    workspaces: State<'_, PythonWorkspaceService>,
    graph_runs: State<'_, GraphRunStore>,
    thread_id: String,
) -> Result<(), String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    authored
        .delete_thread_with_authored_state(&db.0, owner_user_id.as_deref(), &thread_id, || async {
            workspaces.retire_thread(&thread_id).await?;
            graph_runs.forget(&thread_id);
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_thread_rename(
    db: State<'_, Db>,
    thread_id: String,
    title: Option<String>,
) -> Result<AgentThread, String> {
    let owner_user_id = auth::admitted_principal(&db.0).await?;
    db::rename_thread(
        &db.0,
        &thread_id,
        title.as_deref(),
        owner_user_id.as_deref(),
    )
    .await
}
