//! Push protocol: background flush loop with exponential backoff.
//!
//! The flush loop runs as a `tokio::spawn`ed task for the lifetime of the app.
//! It wakes on `Notify` (triggered by `enqueue()`) or every 30 seconds.
//! Pending ops are processed in tier order with idempotent upserts.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::Notify;

use super::error::SyncError;
use super::pending::{self, PendingOp};
use super::traits::RemoteClient;

/// Flush all ready pending ops to the remote.
/// Returns the number of ops successfully flushed.
pub async fn flush_pending(
    pool: &SqlitePool,
    state_pool: &SqlitePool,
    remote: &dyn RemoteClient,
) -> Result<usize, SyncError> {
    let token = crate::database::local::auth::get_current_access_token(state_pool)
        .await
        .map_err(SyncError::Local)?
        .ok_or(SyncError::AuthRequired)?;

    let ops = pending::fetch_ready_ops(pool).await?;
    let mut flushed = 0;

    for op in &ops {
        match execute_op(remote, op, &token).await {
            Ok(()) => {
                pending::remove_op(pool, op.id).await?;
                mark_synced(pool, &op.table_name, &op.record_id).await?;
                flushed += 1;
            }
            Err(SyncError::Api { status: 401, .. }) => {
                // Token expired — stop this batch, let next cycle re-fetch
                eprintln!("[sync/push] 401 — token expired, will retry next cycle");
                break;
            }
            Err(SyncError::Api { status: 409, .. }) => {
                // Conflict — remote already has this or newer. Treat as success.
                pending::remove_op(pool, op.id).await?;
                mark_synced(pool, &op.table_name, &op.record_id).await?;
                flushed += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                eprintln!(
                    "[sync/push] Failed {}.{}: {}",
                    op.table_name, op.record_id, msg
                );
                pending::record_failure(pool, op.id, op.attempts + 1, &msg).await?;
            }
        }
    }

    Ok(flushed)
}

/// Execute a single pending op against the remote.
async fn execute_op(
    remote: &dyn RemoteClient,
    op: &PendingOp,
    token: &str,
) -> Result<(), SyncError> {
    match op.op_type.as_str() {
        "upsert" => {
            let payload_str = op
                .payload_json
                .as_deref()
                .ok_or_else(|| SyncError::MissingField("payload_json".to_string()))?;
            let payload: Value = serde_json::from_str(payload_str)
                .map_err(|e| SyncError::Parse(format!("pending op payload: {e}")))?;
            remote
                .upsert_json(&op.table_name, &payload, &op.conflict_key, token)
                .await
        }
        "delete" => remote.delete(&op.table_name, &op.record_id, token).await,
        other => Err(SyncError::Parse(format!("unknown op_type: {other}"))),
    }
}

/// Mark a record as synced locally. Uses an `updated_at` guard to avoid
/// marking records that were modified between enqueue and push.
async fn mark_synced(
    pool: &SqlitePool,
    table_name: &str,
    record_id: &str,
) -> Result<(), SyncError> {
    match table_name {
        "track_beats" | "track_roots" | "track_waveforms" => {
            sqlx::query(&format!(
                "UPDATE {table_name} SET synced_at = updated_at, version = version + 1 WHERE track_id = ?"
            ))
            .bind(record_id)
            .execute(pool)
            .await?;
            return Ok(());
        }
        "track_stems" => {
            // record_id encodes "track_id:stem_name"
            if let Some((track_id, stem_name)) = record_id.split_once(':') {
                sqlx::query(
                    "UPDATE track_stems SET synced_at = updated_at, version = version + 1 WHERE track_id = ? AND stem_name = ?"
                )
                .bind(track_id)
                .bind(stem_name)
                .execute(pool)
                .await?;
            }
            return Ok(());
        }
        "venue_implementation_overrides" => {
            if let Some((venue_id, pattern_id)) = record_id.split_once(':') {
                sqlx::query(
                    "UPDATE venue_implementation_overrides SET synced_at = updated_at, version = version + 1 WHERE venue_id = ? AND pattern_id = ?"
                )
                .bind(venue_id)
                .bind(pattern_id)
                .execute(pool)
                .await?;
            }
            return Ok(());
        }
        "fixture_group_members" => {
            if let Some((fixture_id, group_id)) = record_id.split_once(':') {
                sqlx::query(
                    "UPDATE fixture_group_members SET synced_at = updated_at, version = version + 1 WHERE fixture_id = ? AND group_id = ?"
                )
                .bind(fixture_id)
                .bind(group_id)
                .execute(pool)
                .await?;
            }
            return Ok(());
        }
        _ => {}
    }

    // Standard tables with `id` PK
    sqlx::query(&format!(
        "UPDATE {table_name} SET synced_at = updated_at, version = version + 1 WHERE id = ?"
    ))
    .bind(record_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Run the background sync loop. Call via `tauri::async_runtime::spawn`.
/// Every 10s (or on notify): push dirty records.
/// Every 60s: pull remote changes.
pub async fn run_sync_loop(
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<dyn RemoteClient>,
    notify: Arc<Notify>,
) {
    let mut pull_interval = tokio::time::interval(Duration::from_secs(60));
    pull_interval.tick().await; // skip the immediate first tick

    loop {
        // Wait for push notify, pull interval, or 10s push timer
        tokio::select! {
            _ = notify.notified() => {}
            _ = pull_interval.tick() => {
                // Periodic pull
                run_pull_cycle(&pool, &state_pool, remote.as_ref()).await;
                continue;
            }
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }

        // Push cycle
        let uid = match crate::database::local::auth::get_current_user_id(&state_pool).await {
            Ok(Some(uid)) => uid,
            _ => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        };

        if let Err(e) = enqueue_dirty_records(&pool, &uid).await {
            eprintln!("[sync] Enqueue scan error: {e}");
        }

        match flush_pending(&pool, &state_pool, remote.as_ref()).await {
            Ok(n) => {
                if n > 0 {
                    println!("[sync] Pushed {n} ops");
                }
            }
            Err(SyncError::AuthRequired) => {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Err(e) => {
                eprintln!("[sync] Push error: {e}");
            }
        }
    }
}

/// Run a pull cycle: discover venues, delta pull all tables.
async fn run_pull_cycle(pool: &SqlitePool, state_pool: &SqlitePool, remote: &dyn RemoteClient) {
    let (token, _uid) = match get_auth(state_pool).await {
        Some(pair) => pair,
        None => return,
    };

    match super::pull::pull_all(pool, remote, &token).await {
        Ok(stats) => {
            if stats.rows_pulled > 0 {
                println!(
                    "[sync] Pulled {} rows across {} tables",
                    stats.rows_pulled, stats.tables_pulled
                );
            }
        }
        Err(e) => eprintln!("[sync] Pull error: {e}"),
    }
}

async fn get_auth(state_pool: &SqlitePool) -> Option<(String, String)> {
    let token = crate::database::local::auth::get_current_access_token(state_pool)
        .await
        .ok()??;
    let uid = crate::database::local::auth::get_current_user_id(state_pool)
        .await
        .ok()??;
    Some((token, uid))
}

/// Scan all tables for dirty records and enqueue them into pending_ops.
async fn enqueue_dirty_records(pool: &SqlitePool, uid: &str) -> Result<(), SyncError> {
    use super::pending;
    use super::registry;

    for table in registry::TABLES {
        let dirty_ids =
            crate::sync::orchestrator::find_dirty_record_ids(pool, table.name, uid).await?;
        for record_id in &dirty_ids {
            if let Ok(payload) = crate::sync::orchestrator::read_record_as_json(
                pool,
                table.name,
                table.columns,
                table.local_only,
                record_id,
            )
            .await
            {
                let payload_str =
                    serde_json::to_string(&payload).map_err(|e| SyncError::Parse(e.to_string()))?;
                pending::enqueue_upsert(
                    pool,
                    table.name,
                    record_id,
                    &payload_str,
                    table.conflict_key,
                    table.tier,
                )
                .await?;
            }
        }
    }

    Ok(())
}
