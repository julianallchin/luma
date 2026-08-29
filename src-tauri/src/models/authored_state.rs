use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::models::node_graph::Graph;
use crate::services::authored_merge::{
    MergeConflict, MergeConflictKind, MergeInput, MergePathSegment, MergeValue,
};
use crate::services::score_dsl::{
    TriviaField, TriviaMergeConflict, TriviaMergeConflictKind, TriviaMergeInput,
    TriviaMergePathSegment, TriviaMergeValue,
};

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredOperationKind {
    InitialImport,
    Edit,
    AgentTurn,
    Restore,
    PatternFork,
    WorkspaceMerge,
    SyncIntegration,
    /// A valid operation introduced by a newer producer. History remains
    /// listable/restorable even when this client has no specialized label.
    Revision,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredProjectedDocument {
    TrackScore {
        revision: String,
    },
    PatternGraph {
        implementation_id: String,
        revision: String,
        graph: Graph,
    },
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PrepareAuthoredTurnInput {
    pub thread_id: String,
    pub assistant_message_id: String,
    pub graph: Option<Graph>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PreparedAuthoredTurn {
    pub document_id: String,
    pub prepared_revision_id: String,
    pub document: AuthoredProjectedDocument,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct FinalizeAuthoredTurnInput {
    pub thread_id: String,
    pub assistant_message_id: String,
    pub prepared_revision_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub enum AuthoredTurnCommit {
    Committed {
        document_id: String,
        /// The revision created for this operation, even if later revisions
        /// have since advanced the document head.
        revision_id: String,
        /// Whether `revision_id` is still the current projection. `document` is
        /// always the current projection and is therefore safe to hydrate.
        applied_to_current_projection: bool,
        changed: bool,
        document: AuthoredProjectedDocument,
    },
    Conflicted {
        document_id: String,
        prepared_revision_id: String,
        conflicts: Vec<AuthoredMergeConflict>,
    },
}

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredRevisionPosition {
    Current,
    Ancestor,
    Superseded,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredConversationCheckpoint {
    pub thread_id: String,
    pub assistant_message_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredHistoryEntry {
    pub revision_id: String,
    pub parent_ids: Vec<String>,
    pub message: String,
    /// Who produced this revision: `user`, a model key, or
    /// `client:<name>/<version>[:<model>]` for an out-of-process MCP client.
    /// An open vocabulary — the surface shows it, it does not switch on it.
    pub actor: String,
    /// RFC 3339 / ISO-8601.
    pub authored_at: String,
    pub thread_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub kind: AuthoredOperationKind,
    /// Current document head, one of its ancestors, or a proposal tip that
    /// lost deterministic sync integration. Superseded tips remain first-class
    /// history entries and can be restored like any other revision.
    pub position: AuthoredRevisionPosition,
    /// Server-assigned ordering for a cross-device proposal, when applicable.
    #[ts(type = "number | null")]
    pub proposal_sequence: Option<i64>,
    /// Present only when this state corresponds exactly to an immutable
    /// assistant-turn transcript boundary and can therefore be rewound by
    /// forking that transcript prefix.
    pub conversation_checkpoint: Option<AuthoredConversationCheckpoint>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredHistoryPage {
    pub entries: Vec<AuthoredHistoryEntry>,
    /// Opaque stable cursor over the union of current-lineage revisions and
    /// superseded proposal tips.
    pub next_cursor: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredRestoreMode {
    StateOnly,
    StateAndConversation,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct RestoreAuthoredStateInput {
    pub thread_id: String,
    pub target_revision_id: String,
    pub operation_id: String,
    pub mode: AuthoredRestoreMode,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredRestoreResult {
    pub document_id: String,
    /// The restore operation's new forward revision, even when the head later
    /// advances.
    pub revision_id: String,
    /// Whether `revision_id` is still the current projection. `document` always
    /// represents the current projection and is safe to hydrate.
    pub applied_to_current_projection: bool,
    pub document: AuthoredProjectedDocument,
    /// Set only for `state_and_conversation`. The original thread remains
    /// intact; this thread shares its immutable transcript prefix.
    pub forked_thread_id: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateAuthoredWorkspaceInput {
    pub thread_id: String,
    pub request_id: String,
    /// Exact revision selected from document history by the
    /// orchestrator as this child's immutable starting point.
    pub expected_base_revision_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct ForkAuthoredWorkspaceInput {
    pub thread_id: String,
    pub request_id: String,
    pub source_workspace_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AuthoredWorkspace {
    pub id: String,
    /// Trusted absolute path of the bounded plain-directory snapshot for the
    /// local orchestrator/subagent runtime.
    pub path: String,
    /// Immutable document revision selected at allocation.
    pub base_revision_id: String,
    /// Current detached workspace revision; initially the base revision.
    pub head_revision_id: String,
}

/// Path-free workspace identity safe to return across Tauri IPC. The absolute
/// workspace path remains an implementation detail of the local supervisor.
#[derive(TS, Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredWorkspaceHandle {
    pub id: String,
    pub base_revision_id: String,
    pub head_revision_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredWorkspaceInput {
    pub thread_id: String,
    pub workspace_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredWorkspaceCheck {
    pub id: String,
    pub head_revision_id: String,
    /// Hash of the exact bounded raw files inspected by this check.
    pub snapshot_id: String,
    pub changed: bool,
    pub document: AuthoredProjectedDocument,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct CommitAuthoredWorkspaceInput {
    pub thread_id: String,
    pub workspace_id: String,
    pub expected_head_revision_id: String,
    pub expected_snapshot_id: String,
    pub operation_id: String,
    pub message: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct MergeAuthoredWorkspaceInput {
    pub thread_id: String,
    pub workspace_id: String,
    pub expected_head_revision_id: String,
    pub operation_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct MergeAuthoredWorkspaceIntoWorkspaceInput {
    pub thread_id: String,
    pub workspace_id: String,
    pub target_workspace_id: String,
    pub expected_head_revision_id: String,
    pub operation_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredWorkspaceCommit {
    pub id: String,
    pub revision_id: String,
    /// Whether `revision_id` is still this workspace's detached head. `document`
    /// always represents the current head and is safe to hydrate.
    pub applied_to_current_workspace: bool,
    pub changed: bool,
    pub document: AuthoredProjectedDocument,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredCurrentRevision {
    pub document_id: String,
    pub revision_id: String,
    pub document: AuthoredProjectedDocument,
}

/// Replace the complete graph in a pattern subagent workspace without exposing
/// its canonical source files to the model. The host owns serialization of the
/// semantic graph and layout as one bounded workspace update.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct WriteAuthoredWorkspaceGraphInput {
    pub thread_id: String,
    pub workspace_id: String,
    pub graph: Graph,
}

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredMergeInput {
    Base,
    Ours,
    Theirs,
    Semantic,
}

impl From<MergeInput> for AuthoredMergeInput {
    fn from(value: MergeInput) -> Self {
        match value {
            MergeInput::Base => Self::Base,
            MergeInput::Ours => Self::Ours,
            MergeInput::Theirs => Self::Theirs,
        }
    }
}

impl From<TriviaMergeInput> for AuthoredMergeInput {
    fn from(value: TriviaMergeInput) -> Self {
        match value {
            TriviaMergeInput::Base => Self::Base,
            TriviaMergeInput::Ours => Self::Ours,
            TriviaMergeInput::Theirs => Self::Theirs,
            TriviaMergeInput::Semantic => Self::Semantic,
        }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredMergePathSegment {
    Input(AuthoredMergeInput),
    TrackClip(String),
    TrackArgument(String),
    GraphNode(String),
    NodeParameter(String),
    GraphEdge { to_node: String, to_port: String },
    PatternArgument(String),
    ScoreDocument,
    ScoreLayer(#[ts(type = "number")] i64),
    ScoreAnnotation(String),
    ScoreTriviaField(String),
    Field(String),
}

impl From<TriviaMergePathSegment> for AuthoredMergePathSegment {
    fn from(value: TriviaMergePathSegment) -> Self {
        match value {
            TriviaMergePathSegment::Input(input) => Self::Input(input.into()),
            TriviaMergePathSegment::Document => Self::ScoreDocument,
            TriviaMergePathSegment::Layer(z_index) => Self::ScoreLayer(z_index),
            TriviaMergePathSegment::Annotation(id) => Self::ScoreAnnotation(id),
            TriviaMergePathSegment::Field(field) => Self::ScoreTriviaField(
                match field {
                    TriviaField::LeadingComments => "leading_comments",
                    TriviaField::TrailingComment => "trailing_comment",
                    TriviaField::DocumentTrailingComments => "document_trailing_comments",
                }
                .to_owned(),
            ),
        }
    }
}

impl From<MergePathSegment> for AuthoredMergePathSegment {
    fn from(value: MergePathSegment) -> Self {
        match value {
            MergePathSegment::Input(input) => Self::Input(input.into()),
            MergePathSegment::TrackClip(id) => Self::TrackClip(id),
            MergePathSegment::TrackArgument(id) => Self::TrackArgument(id),
            MergePathSegment::GraphNode(id) => Self::GraphNode(id),
            MergePathSegment::NodeParameter(id) => Self::NodeParameter(id),
            MergePathSegment::GraphEdge { to_node, to_port } => {
                Self::GraphEdge { to_node, to_port }
            }
            MergePathSegment::PatternArgument(id) => Self::PatternArgument(id),
            MergePathSegment::Field(field) => Self::Field(field),
        }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredMergeConflictKind {
    DuplicateKey,
    AddAdd,
    DeleteModify,
    ConcurrentEdit,
    SemanticDependency,
    DanglingEndpoint,
    InvalidInput,
}

impl From<TriviaMergeConflictKind> for AuthoredMergeConflictKind {
    fn from(value: TriviaMergeConflictKind) -> Self {
        match value {
            TriviaMergeConflictKind::ConcurrentEdit => Self::ConcurrentEdit,
            TriviaMergeConflictKind::DeleteModify => Self::DeleteModify,
            TriviaMergeConflictKind::DuplicateKey => Self::DuplicateKey,
            TriviaMergeConflictKind::InvalidInput => Self::InvalidInput,
        }
    }
}

impl From<MergeConflictKind> for AuthoredMergeConflictKind {
    fn from(value: MergeConflictKind) -> Self {
        match value {
            MergeConflictKind::DuplicateKey => Self::DuplicateKey,
            MergeConflictKind::AddAdd => Self::AddAdd,
            MergeConflictKind::DeleteModify => Self::DeleteModify,
            MergeConflictKind::ConcurrentEdit => Self::ConcurrentEdit,
            MergeConflictKind::SemanticDependency => Self::SemanticDependency,
            MergeConflictKind::DanglingEndpoint => Self::DanglingEndpoint,
            MergeConflictKind::InvalidInput => Self::InvalidInput,
        }
    }
}

impl From<TriviaMergeValue> for AuthoredMergeValue {
    fn from(value: TriviaMergeValue) -> Self {
        match value {
            TriviaMergeValue::Missing => Self::Missing,
            TriviaMergeValue::Present(comments) => Self::Present(serde_json::json!(comments)),
        }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum AuthoredMergeValue {
    Missing,
    Present(#[ts(type = "unknown")] serde_json::Value),
}

impl From<MergeValue> for AuthoredMergeValue {
    fn from(value: MergeValue) -> Self {
        match value {
            MergeValue::Missing => Self::Missing,
            MergeValue::Present(value) => Self::Present(value),
        }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredMergeConflict {
    pub path: Vec<AuthoredMergePathSegment>,
    pub kind: AuthoredMergeConflictKind,
    pub base: AuthoredMergeValue,
    pub ours: AuthoredMergeValue,
    pub theirs: AuthoredMergeValue,
    pub detail: Option<String>,
}

impl From<MergeConflict> for AuthoredMergeConflict {
    fn from(value: MergeConflict) -> Self {
        Self {
            path: value.path.0.into_iter().map(Into::into).collect(),
            kind: value.kind.into(),
            base: value.base.into(),
            ours: value.ours.into(),
            theirs: value.theirs.into(),
            detail: value.detail,
        }
    }
}

impl From<TriviaMergeConflict> for AuthoredMergeConflict {
    fn from(value: TriviaMergeConflict) -> Self {
        Self {
            path: value.path.0.into_iter().map(Into::into).collect(),
            kind: value.kind.into(),
            base: value.base.into(),
            ours: value.ours.into(),
            theirs: value.theirs.into(),
            detail: value.detail,
        }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub enum AuthoredWorkspaceMerge {
    Merged {
        document_id: String,
        /// The revision created for this merge operation, even if the head later
        /// advances.
        revision_id: String,
        applied_to_current_projection: bool,
        document: AuthoredProjectedDocument,
    },
    Conflicted {
        conflicts: Vec<AuthoredMergeConflict>,
    },
}

#[derive(Clone, Debug)]
pub struct AppliedAuthoredState {
    pub document_id: String,
    pub revision_id: String,
    pub changed: bool,
    pub document: AuthoredProjectedDocument,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_path_layer_is_a_typescript_number() {
        let declaration = AuthoredMergePathSegment::decl();
        assert!(declaration.contains(r#""kind": "score_layer""#));
        assert!(declaration.contains(r#""value": number"#));
        assert!(!declaration.contains("bigint"));
    }
}

/// A revision's actor, split into the parts a surface shows.
///
/// The stored vocabulary is open (see [`AuthoredHistoryEntry::actor`]) and the
/// display side is here so it stays one mapping: the sidebar's score rows, the
/// history list and settings all read a label the same way rather than each
/// slicing the string.
///
/// Parsing never fails. An actor this build has no reading for is
/// [`Self::Named`] and shows verbatim — an unrecognized writer is still an
/// honest one, and inventing "unknown" for it would lose the only fact the row
/// has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorLabel<'a> {
    /// `user` — the human at the keyboard. Deliberately *nameless*: which
    /// human is the document's principal, which the actor does not record, so
    /// the surface names them ("You", a short uid) and this does not.
    User,
    /// `client:<name>/<version>[:<model>]` — an out-of-process MCP client.
    /// The version is parsed and dropped: it identifies a build of the client,
    /// not a writer, and no surface has room for it.
    Client {
        name: &'a str,
        model: Option<&'a str>,
    },
    /// Everything else: a model key the in-app loop wrote under, or a label
    /// from a producer this build does not know.
    Named(&'a str),
}

impl<'a> ActorLabel<'a> {
    #[must_use]
    pub fn parse(actor: &'a str) -> Self {
        if actor == "user" {
            return Self::User;
        }
        let Some(client) = actor.strip_prefix("client:") else {
            return Self::Named(actor);
        };
        // `name/version` then an optional `:model`. A client label missing its
        // version is still a client, and reading it as one beats falling back
        // to the raw string with the prefix still on it.
        let (name, rest) = client.split_once('/').unwrap_or((client, ""));
        let model = rest.split_once(':').map(|(_, model)| model);
        Self::Client { name, model }
    }
}

impl std::fmt::Display for ActorLabel<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => formatter.write_str("user"),
            Self::Client {
                name,
                model: Some(model),
            } => write!(formatter, "{name} · {model}"),
            Self::Client { name, model: None } => formatter.write_str(name),
            Self::Named(actor) => formatter.write_str(actor),
        }
    }
}

#[cfg(test)]
mod actor_label_tests {
    use super::ActorLabel;

    #[test]
    fn every_shape_of_the_open_actor_vocabulary_reads() {
        assert_eq!(ActorLabel::parse("user"), ActorLabel::User);
        assert_eq!(
            ActorLabel::parse("client:claude-code/2.1.247:opus").to_string(),
            "claude-code · opus"
        );
        assert_eq!(
            ActorLabel::parse("client:author_score/0").to_string(),
            "author_score"
        );
        assert_eq!(
            ActorLabel::parse("claude-opus-5").to_string(),
            "claude-opus-5"
        );
        assert_eq!(ActorLabel::parse("agent").to_string(), "agent");
    }
}
