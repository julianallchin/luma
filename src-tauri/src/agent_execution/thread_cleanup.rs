//! Durable agent-thread deletion recovery.
//!
//! A terminal deletion can arrive from a local command, crash recovery, an
//! identity activation, or row sync. All recovery paths use this one routine
//! so Python processes/directories, published graph runs, authored subagent
//! workspaces, and the mutable thread projection retire in the same order.

use sqlx::SqlitePool;

use crate::database::local::agent_threads;
use crate::services::authored_documents::AuthoredDocuments;

use super::{GraphRunStore, PythonWorkspaceService};

/// Resume every terminal deletion visible to the currently admitted
/// principal. Individual failures remain durable and retryable, while cleanup
/// of unrelated threads continues.
pub async fn recover_deleting_agent_threads(
    pool: &SqlitePool,
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
    let threads = agent_threads::list_deleting_threads(pool, principal.as_deref()).await?;
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
