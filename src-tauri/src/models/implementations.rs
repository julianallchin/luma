use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

/// An implementation is a specific node graph for a pattern
/// Patterns can have multiple implementations (e.g., "default", "minimal", "club mode")
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct Implementation {
    pub id: String,
    pub uid: Option<String>,
    #[sqlx(rename = "pattern_id")]
    pub pattern_id: String,
    pub name: Option<String>,
    #[sqlx(rename = "graph_json")]
    pub graph_json: String,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}
