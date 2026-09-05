//! Readable sync state. Delivery failures stay in SQLite; activity belongs to
//! the engine that owns the sync lock.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub syncing: bool,
    pub errors: Vec<String>,
    pub failures: Vec<SyncFailure>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct SyncFailure {
    pub table_name: String,
    pub record_id: String,
    pub subject: String,
    pub attempts: i64,
    pub permanent: bool,
    pub last_error: Option<String>,
}
