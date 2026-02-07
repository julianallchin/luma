use tauri::{AppHandle, State};

use crate::stagelinq_manager::StageLinqManager;

#[tauri::command]
pub async fn stagelinq_connect(
    app: AppHandle,
    manager: State<'_, StageLinqManager>,
) -> Result<(), String> {
    manager.start(app).await
}

#[tauri::command]
pub async fn stagelinq_disconnect(manager: State<'_, StageLinqManager>) -> Result<(), String> {
    manager.stop().await
}
