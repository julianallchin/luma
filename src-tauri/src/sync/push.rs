//! Push protocol: background sync loop with exponential backoff.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::Notify;

use super::error::SyncError;
use super::pending::{self, PendingOp};
use super::registry;
use super::traits::RemoteClient;

/// Flush ready pending ops to the remote. Returns count flushed.
pub async fn flush_pending(
    pool: &SqlitePool,
    state_pool: &SqlitePool,
    remote: &dyn RemoteClient,
) -> Result<usize, SyncError> {
    let token = get_token(state_pool).await?;
    let ops = pending::fetch_ready_ops(pool).await?;
    let mut flushed = 0;
    let mut pushed_tables: std::collections::HashSet<String> = std::collections::HashSet::new();

    for op in &ops {
        match execute_op(remote, op, &token).await {
            Ok(()) => {
                pending::remove_op(pool, op.id).await?;
                mark_synced(pool, &op.table_name, &op.record_id).await?;
                pushed_tables.insert(op.table_name.clone());
                flushed += 1;
            }
            Err(SyncError::Api { status: 401, .. }) => {
                eprintln!("[sync] 401 — stopping batch for token refresh");
                break;
            }
            Err(SyncError::Api { status: 409, .. }) => {
                pending::remove_op(pool, op.id).await?;
                mark_synced(pool, &op.table_name, &op.record_id).await?;
                pushed_tables.insert(op.table_name.clone());
                flushed += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                eprintln!(
                    "[sync] Push failed {}.{}: {msg}",
                    op.table_name, op.record_id
                );
                pending::record_failure(pool, op.id, op.attempts + 1, &msg).await?;
            }
        }
    }

    // Advance pull timestamps for pushed tables so the next pull
    // doesn't re-fetch records we just pushed.
    if !pushed_tables.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        for table_name in &pushed_tables {
            let _ = super::state::set_last_pulled_at(pool, table_name, &now).await;
        }
    }

    Ok(flushed)
}

async fn execute_op(
    remote: &dyn RemoteClient,
    op: &PendingOp,
    token: &str,
) -> Result<(), SyncError> {
    match op.op_type.as_str() {
        "upsert" => {
            let payload: Value = serde_json::from_str(
                op.payload_json
                    .as_deref()
                    .ok_or_else(|| SyncError::MissingField("payload_json".into()))?,
            )
            .map_err(|e| SyncError::Parse(e.to_string()))?;
            remote
                .upsert_json(&op.table_name, &payload, &op.conflict_key, token)
                .await
        }
        "delete" => remote.delete(&op.table_name, &op.record_id, token).await,
        other => Err(SyncError::Parse(format!("unknown op_type: {other}"))),
    }
}

/// Mark a record as synced using TableMeta-derived SQL.
async fn mark_synced(
    pool: &SqlitePool,
    table_name: &str,
    record_id: &str,
) -> Result<(), SyncError> {
    let Some(table) = registry::get_table(table_name) else {
        return Ok(());
    };
    let sql = table.mark_synced_sql();
    let pk_values = table.decode_record_id(record_id);
    let mut query = sqlx::query(&sql);
    for val in &pk_values {
        query = query.bind(*val);
    }
    query.execute(pool).await?;
    Ok(())
}

/// Background sync loop: push dirty every 10s, pull delta every 60s.
pub async fn run_sync_loop(
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<dyn RemoteClient>,
    notify: Arc<Notify>,
) {
    let mut pull_interval = tokio::time::interval(Duration::from_secs(60));
    pull_interval.tick().await; // skip immediate first tick

    loop {
        tokio::select! {
            _ = notify.notified() => {}
            _ = pull_interval.tick() => {
                run_pull_cycle(&pool, &state_pool, remote.as_ref()).await;
                continue;
            }
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }

        let uid = match get_uid(&state_pool).await {
            Some(uid) => uid,
            None => continue,
        };

        if let Err(e) = super::orchestrator::enqueue_dirty(&pool, &uid).await {
            eprintln!("[sync] Enqueue error: {e}");
        }

        match flush_pending(&pool, &state_pool, remote.as_ref()).await {
            Ok(n) if n > 0 => println!("[sync] Pushed {n} ops"),
            Err(SyncError::AuthRequired) => {}
            Err(e) => eprintln!("[sync] Push error: {e}"),
            _ => {}
        }
    }
}

async fn run_pull_cycle(pool: &SqlitePool, state_pool: &SqlitePool, remote: &dyn RemoteClient) {
    let token = match get_token(state_pool).await {
        Ok(t) => t,
        Err(_) => return,
    };

    match super::pull::pull_all(pool, remote, &token).await {
        Ok(stats) if stats.rows_pulled > 0 => {
            println!(
                "[sync] Pulled {} rows across {} tables",
                stats.rows_pulled, stats.tables_pulled
            );
        }
        Err(e) => eprintln!("[sync] Pull error: {e}"),
        _ => {}
    }
}

async fn get_token(state_pool: &SqlitePool) -> Result<String, SyncError> {
    crate::database::local::auth::get_current_access_token(state_pool)
        .await
        .map_err(SyncError::Local)?
        .ok_or(SyncError::AuthRequired)
}

async fn get_uid(state_pool: &SqlitePool) -> Option<String> {
    crate::database::local::auth::get_current_user_id(state_pool)
        .await
        .ok()?
}
