//! Tauri commands for durable agent threads.
//!
//! Every command here takes `State<'_, Db>` and nothing else — no `AppHandle` —
//! so the whole thread lifecycle is reachable from the headless harness.

use tauri::State;

use crate::database::local::agent_threads as db;
use crate::database::Db;
use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, CreateAgentThreadInput,
    NewAgentThreadMessage,
};

#[tauri::command]
pub async fn agent_thread_create(
    db: State<'_, Db>,
    input: CreateAgentThreadInput,
) -> Result<AgentThread, String> {
    db::create_thread(&db.0, input).await
}

#[tauri::command]
pub async fn agent_thread_get(
    db: State<'_, Db>,
    thread_id: String,
) -> Result<AgentThreadDetail, String> {
    db::get_thread(&db.0, &thread_id).await
}

#[tauri::command]
pub async fn agent_thread_list(
    db: State<'_, Db>,
    agent_kind: Option<String>,
    subject_kind: Option<String>,
    subject_id: Option<String>,
) -> Result<Vec<AgentThread>, String> {
    db::list_threads(
        &db.0,
        agent_kind.as_deref(),
        subject_kind.as_deref(),
        subject_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn agent_thread_append_messages(
    db: State<'_, Db>,
    thread_id: String,
    messages: Vec<NewAgentThreadMessage>,
) -> Result<Vec<AgentThreadMessage>, String> {
    db::append_messages(&db.0, &thread_id, messages).await
}

#[tauri::command]
pub async fn agent_thread_truncate_from(
    db: State<'_, Db>,
    thread_id: String,
    seq: i64,
) -> Result<u64, String> {
    db::truncate_from_seq(&db.0, &thread_id, seq).await
}

#[tauri::command]
pub async fn agent_thread_reset(db: State<'_, Db>, thread_id: String) -> Result<u64, String> {
    db::reset_thread(&db.0, &thread_id).await
}

#[tauri::command]
pub async fn agent_thread_delete(db: State<'_, Db>, thread_id: String) -> Result<(), String> {
    db::delete_thread(&db.0, &thread_id).await
}

#[tauri::command]
pub async fn agent_thread_rename(
    db: State<'_, Db>,
    thread_id: String,
    title: Option<String>,
) -> Result<AgentThread, String> {
    db::rename_thread(&db.0, &thread_id, title.as_deref()).await
}
