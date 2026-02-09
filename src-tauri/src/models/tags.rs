use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

/// A tag that can be assigned to fixtures
#[derive(Debug, Serialize, Deserialize, Clone, TS, FromRow)]
#[ts(export, export_to = "../../src/bindings/tags.ts")]
#[serde(rename_all = "camelCase")]
pub struct FixtureTag {
    #[ts(type = "number")]
    pub id: i64,
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[ts(type = "number")]
    pub venue_id: i64,
    pub name: String,
    pub category: String,
    pub is_auto_generated: bool,
    pub created_at: String,
    pub updated_at: String,
}
