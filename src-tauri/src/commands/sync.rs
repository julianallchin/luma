//! Tauri commands for the sync engine.

use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::sync::orchestrator::{SyncEngine, SyncReport};
use crate::sync::pending;

#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/sync.ts")]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub pending_count: i64,
}

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

#[tauri::command]
pub async fn sync_full(
    app: tauri::AppHandle,
    engine: State<'_, SyncEngine>,
) -> Result<SyncReport, String> {
    engine.sync_full(&app).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sync_pull(engine: State<'_, SyncEngine>) -> Result<(), String> {
    engine.pull().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn sync_files_v2(
    app: tauri::AppHandle,
    engine: State<'_, SyncEngine>,
) -> Result<(), String> {
    engine.sync_files(&app).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn get_sync_status(engine: State<'_, SyncEngine>) -> Result<SyncStatus, String> {
    let count = pending::count_pending(engine.pool())
        .await
        .map_err(|e| e.to_string())?;
    Ok(SyncStatus {
        pending_count: count,
    })
}

#[tauri::command]
pub async fn get_pending_errors(
    engine: State<'_, SyncEngine>,
) -> Result<Vec<PendingOpError>, String> {
    let ops = pending::list_failed(engine.pool())
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

#[tauri::command]
pub async fn retry_pending_op(engine: State<'_, SyncEngine>, op_id: i64) -> Result<(), String> {
    pending::reset_retry(engine.pool(), op_id)
        .await
        .map_err(|e| e.to_string())?;
    engine.push_notify.notify_one();
    Ok(())
}

#[tauri::command]
pub async fn force_quit(app: tauri::AppHandle) {
    app.exit(0);
}
