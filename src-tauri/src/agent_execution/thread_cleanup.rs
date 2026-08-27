//! Durable agent-thread recovery: deletions to finish, and workspaces whose
//! turn is gone.
//!
//! A terminal deletion can arrive from a local command, crash recovery, an
//! identity activation, or row sync. All recovery paths use this one routine
//! so Python processes/directories, published graph runs, authored subagent
//! workspaces, and the mutable thread projection retire in the same order.
//!
//! The two sweeps are always run as a pair, by [`recover_threads`], whenever
//! the identity boundary opens or a pull lands rows: at startup, at an
//! identity activation, and after every sync pull. Both are safe to repeat.

use sqlx::SqlitePool;

use crate::agent::subagent::SubagentRegistry;
use crate::database::local::agent_threads;
use crate::services::authored_documents::AuthoredDocuments;

use super::{GraphRunStore, PythonWorkspaceService};

/// What one [`recover_threads`] pass finished.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recovered {
    /// Thread deletions carried to their terminal state.
    pub deletions: usize,
    /// Stranded subagent workspaces retired.
    pub workspaces: usize,
}

/// Carry every durable agent-thread cleanup owed by the admitted principal to
/// its terminal state: deletions finish, and workspaces whose subagent turn is
/// gone retire.
///
/// The two sweeps are independent, so one failing does not skip the other.
/// This is the only entry point — a caller that ran just one of them would
/// leave the other's rows stranded until some unrelated path swept them.
///
/// # Errors
///
/// A message naming every thread that could not be cleaned. Each failure stays
/// durable and retries on the next pass.
pub async fn recover_threads(
    pool: &SqlitePool,
    authored: &AuthoredDocuments,
    workspaces: &PythonWorkspaceService,
    graph_runs: &GraphRunStore,
    subagents: &SubagentRegistry,
) -> Result<Recovered, String> {
    let deletions = recover_deleting_agent_threads(pool, authored, workspaces, graph_runs).await;
    let retired = retire_stranded_subagent_workspaces(pool, authored, subagents).await;
    match (deletions, retired) {
        (Ok(deletions), Ok(workspaces)) => Ok(Recovered {
            deletions,
            workspaces,
        }),
        (Err(one), Err(two)) => Err(format!("{one}; {two}")),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// Resume every terminal deletion visible to the currently admitted
/// principal. Individual failures remain durable and retryable, while cleanup
/// of unrelated threads continues.
async fn recover_deleting_agent_threads(
    pool: &SqlitePool,
    authored: &AuthoredDocuments,
    workspaces: &PythonWorkspaceService,
    graph_runs: &GraphRunStore,
) -> Result<usize, String> {
    let Some(principal) = admitted_principal(pool).await? else {
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
                |workspace_ids| async {
                    for workspace_id in workspace_ids {
                        workspaces.retire_thread(&workspace_id).await?;
                        graph_runs.forget(&workspace_id);
                    }
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
    report(recovered, failures, "recover", "deleting agent thread(s)")
}

/// Retire every authored workspace whose subagent turn is no longer running.
///
/// A workspace is opened with its child thread and retired by the publish at
/// the end of that child's turn. A drop cannot await, so a cancelled
/// delegation — or a process that died mid-run — leaves the row `active` with
/// nothing left to publish it. This is where those rows end, and
/// [`SubagentRegistry::is_running`] is what separates them from the children
/// currently writing: without it a mid-session sweep would retire a workspace
/// out from under a live turn.
///
/// # Errors
///
/// A message naming the threads whose workspace could not be retired. Each
/// failure stays durable and retryable, and unrelated threads still retire.
async fn retire_stranded_subagent_workspaces(
    pool: &SqlitePool,
    authored: &AuthoredDocuments,
    subagents: &SubagentRegistry,
) -> Result<usize, String> {
    let Some(principal) = admitted_principal(pool).await? else {
        return Ok(0);
    };
    let threads =
        agent_threads::list_subagent_threads_with_active_workspaces(pool, principal.as_deref())
            .await?;
    let mut retired = 0usize;
    let mut failures = Vec::new();
    for thread in threads {
        if subagents.is_running(&thread.id) {
            continue;
        }
        match authored
            .discard_subagent(pool, thread.owner_user_id.as_deref(), &thread.id)
            .await
        {
            Ok(()) => retired += 1,
            Err(error) => failures.push(format!("{}: {error}", thread.id)),
        }
    }
    report(
        retired,
        failures,
        "retire",
        "stranded subagent workspace(s)",
    )
}

/// The principal whose threads may be cleaned right now, or `None` when the
/// identity boundary is closed — a state with no authority over either the
/// guest namespace or a previously authenticated user's threads. `Some(None)`
/// is the guest namespace, which is a principal like any other.
async fn admitted_principal(pool: &SqlitePool) -> Result<Option<Option<String>>, String> {
    let admission = sqlx::query_as::<_, (i64, i64, i64, i64, Option<String>)>(
        "SELECT armed, accepting, maintenance, remote_writes, active_uid
         FROM auth_write_admission WHERE singleton = 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to inspect thread-recovery admission: {error}"))?;
    match admission {
        Some((1, 1, 0, 0, principal)) => Ok(Some(principal)),
        _ => Ok(None),
    }
}

/// A sweep succeeds only if nothing in it failed, and names every thread that
/// did so the failure is diagnosable without reading a log stream.
fn report(done: usize, failures: Vec<String>, verb: &str, subject: &str) -> Result<usize, String> {
    if failures.is_empty() {
        Ok(done)
    } else {
        Err(format!(
            "failed to {verb} {} {subject}: {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}
