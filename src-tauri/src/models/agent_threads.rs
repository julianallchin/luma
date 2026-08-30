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
    /// Source conversation identity for a transcript fork. The source may be
    /// deleted later; this remains immutable audit metadata.
    pub forked_from_thread_id: Option<String>,
    /// Last shared message in the forked prefix, or `None` for an empty fork.
    pub forked_at_message_id: Option<String>,
    /// The thread whose turn spawned this one, for a subagent. A child edits
    /// the same document as its parent, through a workspace of its own, and
    /// does not outlive it.
    pub parent_thread_id: Option<String>,
    /// The parent's tool call that spawned this thread, so the transcript chip
    /// and the child thread name each other.
    pub parent_call_id: Option<String>,
    pub title: Option<String>,
    /// Who this thread's writes are attributed to: the model key the last turn
    /// resolved, or an external MCP client's label. `None` until a turn names
    /// one, and then the host's own session actor answers instead.
    pub actor: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One message of a thread.
///
/// `parts` is a durable JSON schema, read and written by
/// [`crate::agent::transcript`] — the storage layer still does not interpret
/// it, but it is no longer opaque: `AgentChatPart` is the contract, and an
/// unknown part shape round-trips verbatim rather than being dropped.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AgentThreadMessage {
    pub id: String,
    /// The conversation through which this node was read. A shared node can
    /// therefore project into several thread transcripts without duplication.
    pub thread_id: String,
    pub parent_message_id: Option<String>,
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
            parent_message_id: row.try_get("parent_message_id")?,
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
    /// Spawn this thread as a subagent of another. The parent must be an
    /// active thread of the same principal over the same document; creation
    /// then allocates the child's authored workspace from the parent's current
    /// head, so "a child thread has a private head" is a construction
    /// invariant rather than something a caller has to remember.
    pub parent_thread_id: Option<String>,
    pub parent_call_id: Option<String>,
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
    /// Exact transcript tip observed when this tail was planned. `None` means
    /// the caller observed an empty conversation. A mismatch is never rebased
    /// implicitly; the caller must reload or fork.
    pub expected_head_message_id: Option<String>,
    pub messages: Vec<NewAgentThreadMessage>,
}

/// What one agent run against a thread cost.
///
/// Absolute, not incremental: every writer reports the thread's totals so far,
/// and recording the same run twice leaves the same row. A writer that
/// accumulates (the in-app turn loop, one turn at a time) seeds itself from
/// what is already stored rather than asking the database to add.
///
/// The token counts follow [`crate::agent::model::Usage`]'s convention — the
/// four do not overlap, so their sum is the whole spend. `cost_usd` is `None`
/// unless somebody *told* the writer the price: nothing derives it from a rate
/// card, because a rate card in the tree is a second source of truth.
#[derive(TS, Serialize, Deserialize, Clone, Debug, Default, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AgentThreadUsage {
    pub thread_id: String,
    /// The model that ran, in the writer's own vocabulary: a `ModelId` key
    /// from the in-app loop, the CLI's reported model from a harness.
    pub model: Option<String>,
    #[ts(type = "number")]
    pub turns: i64,
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub cache_creation_tokens: i64,
    #[ts(type = "number")]
    pub cache_read_tokens: i64,
    pub cost_usd: Option<f64>,
    #[ts(type = "number")]
    pub duration_ms: i64,
    /// Children the run fanned out to. Zero is "none", not "unknown".
    #[ts(type = "number")]
    pub subagents: i64,
}

/// Current immutable-node tip of one conversation transcript.
#[derive(TS, Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AgentThreadTranscriptHead {
    pub thread_id: String,
    pub head_message_id: Option<String>,
    #[ts(type = "number")]
    pub message_count: i64,
}

/// Explicit compare-and-swap result for a transcript append. A moved head is
/// data, not an overwrite or generic storage failure: the caller must reload,
/// then explicitly discard, re-plan, or fork from the observed prefix. A
/// prepared assistant turn must be prepared again if it is moved to a fork.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub enum AgentThreadAppendOutcome {
    Appended {
        previous_head_message_id: Option<String>,
        head_message_id: String,
        messages: Vec<AgentThreadMessage>,
    },
    HeadMoved {
        expected_head_message_id: Option<String>,
        current_head_message_id: Option<String>,
    },
}

/// What one agent thread is about, and therefore what it revises.
///
/// Keeping the shape here lets command input, durable rows, and authored-state
/// resolution share one invariant instead of accepting a partially routed
/// thread and hoping later initialization can compensate it.
///
/// Two of the three routes revise an *authored document*; a venue thread
/// revises the room's relational rig, which has no revision history of its own.
/// Which of the two a thread is, is the enum's answer rather than each
/// caller's guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadRoute<'a> {
    Authored(AuthoredThreadRoute<'a>),
    /// The room itself: fixtures, stage pieces and their poses. No track, no
    /// score, no authored document — so no assistant row of such a thread
    /// carries a prepared authored turn.
    Venue {
        venue_id: &'a str,
    },
}

/// The two authored-document routes an agent thread may own.
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
    pub(crate) fn route(&self) -> Result<ThreadRoute<'_>, String> {
        route(
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
    pub(crate) fn route(&self) -> Result<ThreadRoute<'_>, String> {
        route(
            &self.agent_kind,
            self.subject_kind.as_deref(),
            self.subject_id.as_deref(),
            self.implementation_id.as_deref(),
            self.venue_id.as_deref(),
            self.score_id.as_deref(),
        )
    }
}

fn route<'a>(
    agent_kind: &str,
    subject_kind: Option<&str>,
    subject_id: Option<&'a str>,
    implementation_id: Option<&'a str>,
    venue_id: Option<&'a str>,
    score_id: Option<&'a str>,
) -> Result<ThreadRoute<'a>, String> {
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
            Ok(ThreadRoute::Authored(AuthoredThreadRoute::Track {
                track_id,
                venue_id,
                score_id,
            }))
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
            Ok(ThreadRoute::Authored(AuthoredThreadRoute::Pattern {
                pattern_id,
                implementation_id,
            }))
        }
        ("venue_rig", Some("venue"), Some(venue_id), None, Some(subject_venue), None)
            if !venue_id.is_empty() && subject_venue == venue_id =>
        {
            Ok(ThreadRoute::Venue { venue_id })
        }
        ("track_copilot", ..) => Err(
            "track agent thread requires non-empty track, venue, and score IDs and no graph implementation"
                .into(),
        ),
        ("pattern_graph", ..) => Err(
            "pattern agent thread requires non-empty pattern and implementation IDs and no score ID"
                .into(),
        ),
        ("venue_rig", ..) => Err(
            "venue agent thread requires a non-empty venue ID as its subject and no track, score, or graph implementation"
                .into(),
        ),
        _ => Err(format!("unsupported agent thread kind '{agent_kind}'")),
    }
}
