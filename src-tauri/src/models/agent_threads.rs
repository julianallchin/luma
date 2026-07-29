use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row};
use ts_rs::TS;

/// A durable agent conversation. The subject association (track, pattern, ...)
/// is metadata, not identity — several threads may exist for one subject, and
/// each owns its own Python workspace.
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AgentThread {
    pub id: String,
    /// 'track_copilot' | 'pattern_graph'
    pub agent_kind: String,
    /// 'track' | 'pattern' | null
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One message of a thread. `parts` is the AI SDK `UIMessage.parts` array
/// verbatim — the backend never interprets it.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AgentThreadMessage {
    pub id: String,
    pub thread_id: String,
    #[ts(type = "number")]
    pub seq: i64,
    pub role: String,
    #[ts(type = "unknown[]")]
    pub parts: Value,
    pub created_at: String,
}

impl<'r> FromRow<'r, SqliteRow> for AgentThreadMessage {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let parts_json: String = row.try_get("parts_json")?;
        let parts: Value =
            serde_json::from_str(&parts_json).map_err(|e| sqlx::Error::ColumnDecode {
                index: "parts_json".into(),
                source: Box::new(e),
            })?;

        Ok(Self {
            id: row.try_get("id")?,
            thread_id: row.try_get("thread_id")?,
            seq: row.try_get("seq")?,
            role: row.try_get("role")?,
            parts,
            created_at: row.try_get("created_at")?,
        })
    }
}

/// A thread plus its full ordered message history.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AgentThreadDetail {
    pub thread: AgentThread,
    pub messages: Vec<AgentThreadMessage>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateAgentThreadInput {
    pub agent_kind: String,
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    pub title: Option<String>,
}

/// A message to append. `id` is the caller's `UIMessage.id`; when omitted a
/// uuid is generated. `seq` is always assigned by the database, never by the
/// caller.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct NewAgentThreadMessage {
    pub id: Option<String>,
    pub role: String,
    #[ts(type = "unknown[]")]
    pub parts: Value,
}
