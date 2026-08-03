//! Thin authenticated IPC surface for relational authored state.

use tauri::State;

use crate::database::local::auth;
use crate::database::Db;
use crate::models::authored_state::{
    AuthoredHistoryPage, AuthoredRestoreResult, AuthoredTurnCommit, FinalizeAuthoredTurnInput,
    PrepareAuthoredTurnInput, PreparedAuthoredTurn, RestoreAuthoredStateInput,
};
use crate::services::authored_documents::AuthoredDocuments;

#[tauri::command]
pub async fn authored_state_prepare_turn(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: PrepareAuthoredTurnInput,
) -> Result<PreparedAuthoredTurn, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .prepare_turn(&db.0, principal.as_deref(), input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_finalize_turn(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: FinalizeAuthoredTurnInput,
) -> Result<AuthoredTurnCommit, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .finalize_turn(&db.0, principal.as_deref(), input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_recover_turns(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    thread_id: String,
) -> Result<Vec<AuthoredTurnCommit>, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .recover_turns(&db.0, principal.as_deref(), &thread_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_list_history(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    thread_id: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<AuthoredHistoryPage, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .list_history(
            &db.0,
            principal.as_deref(),
            &thread_id,
            cursor.as_deref(),
            limit,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn authored_state_restore(
    db: State<'_, Db>,
    authored: State<'_, AuthoredDocuments>,
    input: RestoreAuthoredStateInput,
) -> Result<AuthoredRestoreResult, String> {
    let principal = auth::admitted_principal(&db.0).await?;
    authored
        .restore(
            &db.0,
            principal.as_deref(),
            &input.thread_id,
            &input.target_revision_id,
            &input.operation_id,
            input.mode,
        )
        .await
        .map_err(|error| error.to_string())
}
