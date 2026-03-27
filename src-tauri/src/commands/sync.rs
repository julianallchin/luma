//! Tauri commands for the sync engine.

use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::sync::orchestrator::{SyncEngine, SyncReport};
use crate::sync::pending;

/// Sync status for the frontend.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/sync.ts")]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub pending_count: i64,
}

/// A failed pending op for UI display.
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/sync.ts")]
#[serde(rename_all = "camelCase")]
pub struct PendingOpError {
    pub id: i64,
    pub op_type: String,
    pub table_name: String,
    pub record_id: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

/// Full sync: discovery → pull → push → files.
#[tauri::command]
pub async fn sync_full(
    app: tauri::AppHandle,
    engine: State<'_, SyncEngine>,
) -> Result<SyncReport, String> {
    engine.full_sync(&app).await.map_err(|e| e.to_string())
}

/// Pull only (manual refresh).
#[tauri::command]
pub async fn sync_pull(engine: State<'_, SyncEngine>) -> Result<(), String> {
    engine.pull().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// File sync (upload + download).
#[tauri::command]
pub async fn sync_files_v2(
    app: tauri::AppHandle,
    engine: State<'_, SyncEngine>,
) -> Result<(), String> {
    engine.sync_files(&app).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Get current sync status.
#[tauri::command]
pub async fn get_sync_status(engine: State<'_, SyncEngine>) -> Result<SyncStatus, String> {
    let pending_count = pending::count_pending(&engine.pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(SyncStatus { pending_count })
}

/// Get list of failed pending ops.
#[tauri::command]
pub async fn get_pending_errors(
    engine: State<'_, SyncEngine>,
) -> Result<Vec<PendingOpError>, String> {
    let ops = pending::list_failed(&engine.pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ops
        .into_iter()
        .map(|op| PendingOpError {
            id: op.id,
            op_type: op.op_type,
            table_name: op.table_name,
            record_id: op.record_id,
            attempts: op.attempts,
            last_error: op.last_error,
        })
        .collect())
}

/// Retry a specific failed op.
#[tauri::command]
pub async fn retry_pending_op(engine: State<'_, SyncEngine>, op_id: i64) -> Result<(), String> {
    pending::reset_retry(&engine.pool, op_id)
        .await
        .map_err(|e| e.to_string())?;
    engine.push_notify.notify_one();
    Ok(())
}
