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
    WorktreeMerge,
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
    pub repository_id: String,
    pub branch_commit_id: String,
    pub document: AuthoredProjectedDocument,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct FinalizeAuthoredTurnInput {
    pub thread_id: String,
    pub assistant_message_id: String,
    pub branch_commit_id: String,
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
        repository_id: String,
        /// The commit created for this operation, even if later commits have
        /// since advanced main.
        commit_id: String,
        /// Whether `commit_id` is still the current projection. `document` is
        /// always the current projection and is therefore safe to hydrate.
        applied_to_current_projection: bool,
        changed: bool,
        document: AuthoredProjectedDocument,
    },
    Conflicted {
        repository_id: String,
        branch_commit_id: String,
        conflicts: Vec<AuthoredMergeConflict>,
    },
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredHistoryEntry {
    pub commit_id: String,
    pub parent_ids: Vec<String>,
    pub message: String,
    /// RFC 3339 / ISO-8601.
    pub authored_at: String,
    pub thread_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub kind: AuthoredOperationKind,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredHistoryPage {
    pub entries: Vec<AuthoredHistoryEntry>,
    /// The exact first-parent commit at which the next page begins. Callers
    /// treat this as opaque; the host verifies it is still on `main`.
    pub next_cursor: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct RestoreAuthoredStateInput {
    pub thread_id: String,
    pub target_commit_id: String,
    pub operation_id: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct AuthoredRestoreResult {
    pub repository_id: String,
    /// The restore operation's commit, even when main later advances.
    pub commit_id: String,
    /// Whether `commit_id` is still the current projection. `document` always
    /// represents the current projection and is safe to hydrate.
    pub applied_to_current_projection: bool,
    pub document: AuthoredProjectedDocument,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateAuthoredWorktreeInput {
    pub thread_id: String,
    pub request_id: String,
    /// Exact commit from the document's main history selected by the
    /// orchestrator as this child's immutable starting point.
    pub expected_base_commit_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AuthoredWorktree {
    pub id: String,
    /// Trusted absolute path of the linked checkout for the local
    /// orchestrator/subagent runtime.
    pub path: String,
    pub branch: String,
    /// Immutable `main` commit selected by the orchestrator at allocation.
    pub base_commit_id: String,
    /// Current branch head; initially equal to `base_commit_id`.
    pub head_commit_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AuthoredWorktreeInput {
    pub thread_id: String,
    pub worktree_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AuthoredWorktreeCheck {
    pub id: String,
    pub head_commit_id: String,
    /// Hash of the exact bounded raw files inspected by this check.
    pub snapshot_id: String,
    pub changed: bool,
    pub document: AuthoredProjectedDocument,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CommitAuthoredWorktreeInput {
    pub thread_id: String,
    pub worktree_id: String,
    pub expected_head_commit_id: String,
    pub expected_snapshot_id: String,
    pub operation_id: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MergeAuthoredWorktreeInput {
    pub thread_id: String,
    pub worktree_id: String,
    pub expected_head_commit_id: String,
    pub operation_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AuthoredWorktreeCommit {
    pub id: String,
    pub commit_id: String,
    /// Whether `commit_id` is still this worktree's branch head. `document`
    /// always represents the current head and is safe to hydrate.
    pub applied_to_current_worktree: bool,
    pub changed: bool,
    pub document: AuthoredProjectedDocument,
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

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AuthoredWorktreeMerge {
    Merged {
        repository_id: String,
        /// The commit created for this merge operation, even if main later
        /// advances.
        commit_id: String,
        applied_to_current_projection: bool,
        document: AuthoredProjectedDocument,
    },
    Conflicted {
        conflicts: Vec<AuthoredMergeConflict>,
    },
}

#[derive(Clone, Debug)]
pub struct AppliedAuthoredState {
    pub repository_id: String,
    pub commit_id: String,
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
