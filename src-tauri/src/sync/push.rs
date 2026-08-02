//! Push protocol: background sync loop with exponential backoff.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};
use tokio::sync::{watch, Mutex, Notify};

use crate::services::authored_documents::AuthoredDocuments;

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
    let admitted_user_id = crate::database::local::auth::admitted_principal(pool)
        .await
        .map_err(SyncError::Local)?
        .ok_or(SyncError::AuthRequired)?;
    let (token, token_user_id) = get_auth(state_pool).await?;
    if token_user_id != admitted_user_id {
        return Err(SyncError::Local(
            "verified remote session does not match the active app-database principal".into(),
        ));
    }
    let principal_key = crate::database::local::auth::principal_key(Some(&admitted_user_id));
    let ops = pending::fetch_ready_ops(pool, &principal_key).await?;
    let mut flushed = 0;

    for op in &ops {
        eprintln!(
            "[sync] push {} {}.{} (attempt {})",
            op.op_type, op.table_name, op.record_id, op.attempts
        );
        match execute_op(remote, op, &token, &admitted_user_id).await {
            Ok(()) => {
                if op.op_type == "upsert" {
                    // Mark synced first so if remove_op fails the record is
                    // at least marked clean and won't be re-pushed.
                    mark_synced(pool, op, &admitted_user_id).await?;
                }
                pending::remove_op(pool, op).await?;
                flushed += 1;
            }
            Err(SyncError::Api { status: 401, .. }) => {
                eprintln!("[sync] 401 — stopping batch for token refresh");
                return Err(SyncError::Api {
                    status: 401,
                    message: "token expired".into(),
                });
            }
            Err(SyncError::Api {
                status: 409,
                ref message,
            }) => {
                // A conflict is not proof that the remote row is identical to
                // the local payload. Treating arbitrary 409s as success loses
                // data on unique-key or immutable-row violations. Keep the op
                // queued; FK conflicts and content conflicts both need either
                // their dependency or the underlying divergence resolved.
                let kind = if message.contains("23503") {
                    "FK conflict"
                } else {
                    "conflict"
                };
                eprintln!(
                    "[sync] 409 {kind} {}.{} — requeueing for retry: {message}",
                    op.table_name, op.record_id
                );
                pending::record_failure(pool, op, op.attempts + 1, message).await?;
            }
            Err(e @ SyncError::Network(_)) => {
                // Offline — propagate immediately so the loop can back off.
                // Do not touch attempts: network errors never count against MAX_ATTEMPTS.
                return Err(e);
            }
            Err(e) => {
                let msg = format!("{e:?}");
                eprintln!(
                    "[sync] Push failed {}.{}: {msg}",
                    op.table_name, op.record_id
                );
                pending::record_failure(pool, op, op.attempts + 1, &msg).await?;
            }
        }
    }

    Ok(flushed)
}

async fn execute_op(
    remote: &dyn RemoteClient,
    op: &PendingOp,
    token: &str,
    admitted_user_id: &str,
) -> Result<(), SyncError> {
    let table = registry::get_table(&op.table_name).ok_or_else(|| {
        SyncError::Parse(format!(
            "table {:?} is not registered for relational sync",
            op.table_name
        ))
    })?;
    match op.op_type.as_str() {
        "upsert" => {
            if op.conflict_key != table.conflict_key {
                return Err(SyncError::Parse(format!(
                    "queued conflict key {:?} does not match {} for table {:?}",
                    op.conflict_key, table.conflict_key, op.table_name
                )));
            }
            let payload: Value = serde_json::from_str(
                op.payload_json
                    .as_deref()
                    .ok_or_else(|| SyncError::MissingField("payload_json".into()))?,
            )
            .map_err(|e| SyncError::Parse(e.to_string()))?;
            let payload_user_id = payload
                .get("uid")
                .and_then(Value::as_str)
                .ok_or_else(|| SyncError::MissingField("uid".into()))?;
            if payload_user_id != admitted_user_id {
                return Err(SyncError::Local(format!(
                    "queued payload principal {payload_user_id:?} does not match the active app principal"
                )));
            }
            remote
                .upsert_json(&op.table_name, &payload, &op.conflict_key, token)
                .await
        }
        "delete" => {
            // Soft-delete: PATCH deleted_at on the existing remote row.
            // Uses PATCH (not upsert) because upsert's INSERT half fails
            // NOT NULL constraints when sending only PK + deleted_at.
            let pk_cols = table.pk_columns();
            let pk_values = table.decode_record_id(&op.record_id);

            // Build PostgREST filter: "col1=eq.val1&col2=eq.val2"
            let filter: Vec<String> = pk_cols
                .iter()
                .zip(pk_values.iter())
                .map(|(col, val)| format!("{col}=eq.{val}"))
                .collect();
            let filter = filter.join("&");

            let payload = serde_json::json!({
                "deleted_at": chrono::Utc::now().to_rfc3339(),
            });
            remote
                .patch_json(&op.table_name, &filter, &payload, token)
                .await
        }
        other => Err(SyncError::Parse(format!("unknown op_type: {other}"))),
    }
}

/// Mark a record as synced using TableMeta-derived SQL.
async fn mark_synced(
    pool: &SqlitePool,
    op: &PendingOp,
    admitted_user_id: &str,
) -> Result<(), SyncError> {
    let Some(table) = registry::get_table(&op.table_name) else {
        return Err(SyncError::Parse(format!(
            "table {:?} is not registered for relational sync",
            op.table_name
        )));
    };
    if !table.columns.contains(&"uid") {
        return Err(SyncError::Local(format!(
            "sync table {:?} has no principal column",
            op.table_name
        )));
    }
    let sql = format!(
        "{} AND uid = ? AND EXISTS (
             SELECT 1 FROM auth_write_admission AS admission
             WHERE admission.singleton = 1
               AND admission.armed = 1
               AND admission.accepting = 1
               AND admission.maintenance = 0
               AND admission.remote_writes = 0
               AND admission.active_uid = ?
         )",
        table.mark_synced_sql()
    );
    let pk_values = table.decode_record_id(&op.record_id);
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for val in &pk_values {
        query = query.bind(*val);
    }
    query = query.bind(admitted_user_id).bind(admitted_user_id);
    let result = query.execute(pool).await?;
    if result.rows_affected() != 1 {
        return Err(SyncError::Local(format!(
            "refusing to mark {}.{} synced: the row is not owned by the active app principal",
            op.table_name, op.record_id
        )));
    }
    Ok(())
}

/// Background sync loop: push dirty every 10s, full pull+files every 60s.
/// Accepts a shutdown receiver for graceful termination.
pub async fn run_sync_loop(
    pool: SqlitePool,
    state_pool: SqlitePool,
    remote: Arc<dyn RemoteClient>,
    notify: Arc<Notify>,
    sync_lock: Arc<Mutex<()>>,
    authored: AuthoredDocuments,
    app_handle: AppHandle,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut pull_interval = tokio::time::interval(Duration::from_secs(60));
    pull_interval.tick().await; // skip immediate first tick
    let mut auth_backoff: Option<tokio::time::Instant> = None;
    let mut offline_until: Option<tokio::time::Instant> = None;

    loop {
        let is_pull_tick;
        tokio::select! {
            _ = shutdown.changed() => {
                println!("[sync] Shutting down sync loop");
                return;
            }
            _ = notify.notified() => { is_pull_tick = false; }
            _ = pull_interval.tick() => { is_pull_tick = true; }
            _ = tokio::time::sleep(Duration::from_secs(10)) => { is_pull_tick = false; }
        }

        // If we recently got a 401, back off before retrying.
        if let Some(until) = auth_backoff {
            if tokio::time::Instant::now() < until {
                continue;
            }
            auth_backoff = None;
        }

        // If we recently hit a network error, pause until the check window expires.
        if let Some(until) = offline_until {
            if tokio::time::Instant::now() < until {
                continue;
            }
            offline_until = None;
        }

        // Acquire the sync lock so we don't collide with sync_full.
        let _guard = sync_lock.lock().await;

        if is_pull_tick {
            if let Err(SyncError::Network(msg)) =
                run_pull_cycle(&pool, &state_pool, remote.as_ref(), &authored, &app_handle).await
            {
                eprintln!("[sync] Offline — retrying in 30s ({msg})");
                offline_until = Some(tokio::time::Instant::now() + Duration::from_secs(30));
                continue;
            }
        }

        let uid = match crate::database::local::auth::admitted_principal(&pool).await {
            Ok(Some(uid)) => uid,
            Ok(None) | Err(_) => continue,
        };

        if let Err(e) = super::orchestrator::enqueue_dirty(&pool, &uid).await {
            eprintln!("[sync] Enqueue error: {e}");
        }

        match flush_pending(&pool, &state_pool, remote.as_ref()).await {
            Ok(n) if n > 0 => println!("[sync] Pushed {n} ops"),
            Err(SyncError::AuthRequired) => {}
            Err(SyncError::Api { status: 401, .. }) => {
                auth_backoff = Some(tokio::time::Instant::now() + Duration::from_secs(30));
            }
            Err(SyncError::Network(msg)) => {
                eprintln!("[sync] Offline — retrying in 30s ({msg})");
                offline_until = Some(tokio::time::Instant::now() + Duration::from_secs(30));
            }
            Err(e) => eprintln!("[sync] Push error: {e}"),
            _ => {}
        }
    }
}

/// Full pull cycle: discovery → pull → files → emit library-changed.
/// Returns `Err(SyncError::Network(_))` when the remote is unreachable so the
/// caller can engage offline backoff.
async fn run_pull_cycle(
    pool: &SqlitePool,
    state_pool: &SqlitePool,
    remote: &dyn RemoteClient,
    authored: &AuthoredDocuments,
    app_handle: &AppHandle,
) -> Result<(), SyncError> {
    let (token, uid) = match get_auth(state_pool).await {
        Ok(auth) => auth,
        Err(_) => return Ok(()),
    };

    // Discovery — find new/removed venues
    if let Err(e) = super::pull::discover_venues(pool, remote, &uid, &token).await {
        if matches!(&e, SyncError::Network(_)) {
            eprintln!("[sync] Discovery error (offline): {e}");
            return Err(e);
        }
        eprintln!("[sync] Discovery error: {e}");
    }

    // Delta pull
    let mut data_changed = false;
    match super::pull::pull_all(pool, authored, remote, &token, Some(&uid)).await {
        Ok(stats) => {
            authored
                .reconcile_available_projections(pool)
                .await
                .map_err(|error| {
                    SyncError::Local(format!(
                        "authored projection reconciliation after pull failed: {error}"
                    ))
                })?;
            if stats.rows_pulled > 0 {
                println!(
                    "[sync] Pulled {} rows across {} tables",
                    stats.rows_pulled, stats.tables_pulled
                );
                data_changed = true;
            }
        }
        Err(e) if matches!(&e, SyncError::Network(_)) => {
            eprintln!("[sync] Pull error (offline): {e}");
            return Err(e);
        }
        Err(e) => eprintln!("[sync] Pull error: {e}"),
    }

    // Emit early so the UI sees pulled data before file downloads.
    if data_changed {
        let _ = app_handle.emit("library-changed", ());
    }

    // File sync (upload pending, download stubs)
    let engine_auth = async {
        let auth = crate::database::local::auth::get_current_auth(state_pool)
            .await
            .map_err(SyncError::Local)?
            .ok_or(SyncError::AuthRequired)?;
        Ok::<_, SyncError>((auth.access_token, auth.principal.user_id))
    };

    if let Ok((token, uid)) = engine_auth.await {
        let mut stats = super::files::FileSyncStats::default();
        let _ =
            super::files::upload_pending_audio(pool, remote, &uid, &token, &mut stats, app_handle)
                .await;
        let _ =
            super::files::upload_pending_stems(pool, remote, &uid, &token, &mut stats, app_handle)
                .await;
        let _ = super::files::upload_pending_album_art(
            pool, remote, &uid, &token, &mut stats, app_handle,
        )
        .await;
        let _ = super::files::download_pending_audio(pool, remote, app_handle, &token, &mut stats)
            .await;
        let _ = super::files::download_pending_stems(pool, remote, app_handle, &token, &mut stats)
            .await;
        let _ =
            super::files::download_pending_album_art(pool, remote, app_handle, &token, &mut stats)
                .await;

        let files_changed = stats.audio_downloaded + stats.stems_downloaded + stats.art_downloaded;
        if files_changed > 0 {
            println!(
                "[sync] Files: {}↑ {}↓ audio, {}↑ {}↓ stems, {}↑ {}↓ art",
                stats.audio_uploaded,
                stats.audio_downloaded,
                stats.stems_uploaded,
                stats.stems_downloaded,
                stats.art_uploaded,
                stats.art_downloaded,
            );
            let _ = app_handle.emit("library-changed", ());
        }
    }

    Ok(())
}

async fn get_auth(state_pool: &SqlitePool) -> Result<(String, String), SyncError> {
    let auth = crate::database::local::auth::get_current_auth(state_pool)
        .await
        .map_err(SyncError::Local)?
        .ok_or(SyncError::AuthRequired)?;
    Ok((auth.access_token, auth.principal.user_id))
}
