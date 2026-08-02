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
    /// The authenticated account that owns this local thread. `None` belongs
    /// exclusively to the signed-out principal.
    pub owner_user_id: Option<String>,
    /// 'track_copilot' | 'pattern_graph'
    pub agent_kind: String,
    /// 'track' | 'pattern' | null
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    /// The exact graph implementation this conversation may author. Required
    /// for pattern-graph threads and absent for every other agent kind.
    pub implementation_id: Option<String>,
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
    /// Caller-owned idempotency key. Retries must reuse this UUID.
    pub request_id: String,
    pub agent_kind: String,
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    pub implementation_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    pub title: Option<String>,
}

/// A message to write into a transcript tail. `id` is the caller's
/// `UIMessage.id`; when omitted a uuid is generated. `seq` is always assigned
/// by the database, never by the caller.
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

/// One replay-safe append to the durable transcript. The message batch and
/// exact response are committed by one SQLite transaction.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AppendAgentThreadMessagesInput {
    pub operation_id: String,
    pub messages: Vec<NewAgentThreadMessage>,
}

/// The only two authored-document routes an agent thread may own. Keeping the
/// shape here lets command input, durable rows, and authored-state resolution
/// share one invariant instead of accepting a partially routed thread and
/// hoping later initialization can compensate it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthoredThreadRoute<'a> {
    Track {
        track_id: &'a str,
        venue_id: &'a str,
        score_id: &'a str,
    },
    Pattern {
        pattern_id: &'a str,
        implementation_id: &'a str,
    },
}

impl CreateAgentThreadInput {
    pub(crate) fn authored_route(&self) -> Result<AuthoredThreadRoute<'_>, String> {
        authored_route(
            &self.agent_kind,
            self.subject_kind.as_deref(),
            self.subject_id.as_deref(),
            self.implementation_id.as_deref(),
            self.venue_id.as_deref(),
            self.score_id.as_deref(),
        )
    }
}

impl AgentThread {
    pub(crate) fn authored_route(&self) -> Result<AuthoredThreadRoute<'_>, String> {
        authored_route(
            &self.agent_kind,
            self.subject_kind.as_deref(),
            self.subject_id.as_deref(),
            self.implementation_id.as_deref(),
            self.venue_id.as_deref(),
            self.score_id.as_deref(),
        )
    }
}

fn authored_route<'a>(
    agent_kind: &str,
    subject_kind: Option<&str>,
    subject_id: Option<&'a str>,
    implementation_id: Option<&'a str>,
    venue_id: Option<&'a str>,
    score_id: Option<&'a str>,
) -> Result<AuthoredThreadRoute<'a>, String> {
    match (
        agent_kind,
        subject_kind,
        subject_id,
        implementation_id,
        venue_id,
        score_id,
    ) {
        (
            "track_copilot",
            Some("track"),
            Some(track_id),
            None,
            Some(venue_id),
            Some(score_id),
        ) if !track_id.is_empty() && !venue_id.is_empty() && !score_id.is_empty() => {
            Ok(AuthoredThreadRoute::Track {
                track_id,
                venue_id,
                score_id,
            })
        }
        (
            "pattern_graph",
            Some("pattern"),
            Some(pattern_id),
            Some(implementation_id),
            venue_id,
            None,
        ) if !pattern_id.is_empty()
            && !implementation_id.is_empty()
            && venue_id.is_none_or(|value| !value.is_empty()) =>
        {
            Ok(AuthoredThreadRoute::Pattern {
                pattern_id,
                implementation_id,
            })
        }
        ("track_copilot", ..) => Err(
            "track agent thread requires non-empty track, venue, and score IDs and no graph implementation"
                .into(),
        ),
        ("pattern_graph", ..) => Err(
            "pattern agent thread requires non-empty pattern and implementation IDs and no score ID"
                .into(),
        ),
        _ => Err(format!("unsupported agent thread kind '{agent_kind}'")),
    }
}
