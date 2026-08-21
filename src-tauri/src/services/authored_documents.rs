//! Authored documents are canonical files with an immutable relational
//! revision DAG. SQLite owns history, live projections, operation outcomes,
//! and the compare-and-swap head in one transaction; there is no second
//! filesystem or ref authority to reconcile.

mod catalog;
mod edits;
mod operations;
mod projection;
mod remote_sync;
mod threads;
mod turns;
mod workspaces;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use uuid::Uuid;

use crate::database::local::agent_threads;
use crate::database::local::auth::principal_key;
use crate::database::local::venue_access::{VenueAccess, VenueResource, Write};
use crate::database::local::write_admission;
use crate::models::agent_threads::{AgentThread, AuthoredThreadRoute};
use crate::models::authored_state::{
    AppliedAuthoredState, AuthoredConversationCheckpoint, AuthoredHistoryEntry,
    AuthoredHistoryPage, AuthoredMergeConflict, AuthoredMergeConflictKind,
    AuthoredMergePathSegment, AuthoredMergeValue, AuthoredOperationKind, AuthoredProjectedDocument,
    AuthoredRestoreMode, AuthoredRestoreResult, AuthoredRevisionPosition, AuthoredTurnCommit,
    FinalizeAuthoredTurnInput, PrepareAuthoredTurnInput, PreparedAuthoredTurn,
};
use crate::models::node_graph::{BlendMode, Graph};
use crate::models::patterns::{ForkPatternInput, ForkPatternResult, PatternSummary};
use crate::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, TrackScore, UpdateTrackScoreInput,
};
use crate::services::authored_merge::{merge_graphs, merge_track_documents};
use crate::services::authored_state::{
    AuthoredDocumentId, AuthoredRevisionStore, AuthoredStateError, FileMap, NewAuthoredDocument,
    RevisionId, RevisionInfo, RevisionMetadata,
};
use crate::services::graph_documents::{
    apply_graph_edit_in_transaction, canonicalize_graph, exact_graph_json, graph_from_files,
    graph_layout_json, graph_revision, load_unscoped_graph_document_for_connection,
    semantic_graph_json, GraphDocument, GraphDocumentError, GraphEditPlan, GraphScope,
    GraphValidationIssue,
};
use crate::services::score_dsl::{
    clips_to_canonical_document, compile_draft_track_document, compile_import_track_document,
    load_score_dsl_context, load_score_pattern_names, merge_document_trivia,
    merge_document_trivia_later_wins, parse_canonical, pattern_names_from_document,
    serialize_canonical, Document, ParseResult, PatternNames,
};
use crate::services::track_edits::{
    apply_track_projection_in_transaction, check_track_detached_candidate,
    check_track_projection_candidate, check_track_workspace_candidate, is_valid_track_draft_id,
    plan_track_snapshot_replacement, remap_track_snapshot_result, revision_for_clips,
    track_clips_semantically_equal, validate_track_draft_envelope, TrackClip, TrackDocument,
    TrackEditError, TrackEditPlan, TrackEditResult, TrackProjectionIdentity, TrackScope,
};
use crate::storage::StorageRoot;

const SCORE_PATH: &str = "score.luma";
const GRAPH_PATH: &str = "graph.json";
const LAYOUT_PATH: &str = "layout.json";
const MAX_HISTORY_PAGE: usize = 500;
const PATTERN_SUMMARY_COLUMNS: &str =
    "id, uid, name, description, category_name, created_at, updated_at, is_verified, author_name, forked_from_id";

#[derive(Debug)]
pub enum AuthoredDocumentsError {
    Invalid(String),
    Scope(String),
    Storage(String),
    State(AuthoredStateError),
    Track(TrackEditError),
    Graph(GraphDocumentError),
}

impl fmt::Display for AuthoredDocumentsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Scope(message) | Self::Storage(message) => {
                formatter.write_str(message)
            }
            Self::State(error) => error.fmt(formatter),
            Self::Track(error) => error.fmt(formatter),
            Self::Graph(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AuthoredDocumentsError {}

impl From<AuthoredStateError> for AuthoredDocumentsError {
    fn from(value: AuthoredStateError) -> Self {
        Self::State(value)
    }
}

impl From<TrackEditError> for AuthoredDocumentsError {
    fn from(value: TrackEditError) -> Self {
        Self::Track(value)
    }
}

impl From<GraphDocumentError> for AuthoredDocumentsError {
    fn from(value: GraphDocumentError) -> Self {
        Self::Graph(value)
    }
}

pub type Result<T> = std::result::Result<T, AuthoredDocumentsError>;

pub struct AppliedAuthoredTrackEdit {
    pub authored: AppliedAuthoredState,
    pub edit: TrackEditResult,
}

/// Authenticated detached score state exposed to a workspace-scoped domain
/// tool. The workspace id remains an opaque capability selected by the host,
/// while the semantic document supplies the Python binding revision and clips.
#[derive(Clone)]
pub(crate) struct AuthoredTrackWorkspace {
    pub scope: TrackScope,
    pub document: TrackDocument,
}

/// Managed Tauri state. Per-document locks keep expensive decoding and merge
/// work ordered locally; correctness still comes from the SQLite head CAS.
#[derive(Clone)]
pub struct AuthoredDocuments {
    store: AuthoredRevisionStore,
    storage: StorageRoot,
    document_locks: Arc<Mutex<HashMap<AuthoredDocumentId, Arc<Mutex<()>>>>>,
    lifecycle_lock: Arc<RwLock<()>>,
}

struct AuthoredDocumentGuard {
    _lifecycle: OwnedRwLockReadGuard<()>,
    _document: OwnedMutexGuard<()>,
}

pub(crate) struct PreparedAuthoredSignOut {
    _lifecycle: OwnedRwLockWriteGuard<()>,
}

pub(crate) struct AuthoredIdentitySwitch {
    _lifecycle: OwnedRwLockWriteGuard<()>,
}

impl AuthoredDocuments {
    pub fn new(storage: StorageRoot) -> Self {
        Self {
            store: AuthoredRevisionStore,
            storage,
            document_locks: Arc::new(Mutex::new(HashMap::new())),
            lifecycle_lock: Arc::new(RwLock::new(())),
        }
    }

    async fn document_guard(&self, document_id: &AuthoredDocumentId) -> AuthoredDocumentGuard {
        let lifecycle = Arc::clone(&self.lifecycle_lock).read_owned().await;
        let document = self.document_guard_inside_lifecycle(document_id).await;
        AuthoredDocumentGuard {
            _lifecycle: lifecycle,
            _document: document,
        }
    }

    async fn document_guard_inside_lifecycle(
        &self,
        document_id: &AuthoredDocumentId,
    ) -> OwnedMutexGuard<()> {
        let lock: Arc<Mutex<()>> = {
            let mut locks = self.document_locks.lock().await;
            Arc::clone(
                locks
                    .entry(document_id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        lock.lock_owned().await
    }

    /// Sign-out needs only a lifecycle barrier. Authored rows already live in
    /// the same database transaction domain as their projections, so there is
    /// no external repository to flush or reconcile.
    pub(crate) async fn prepare_sign_out(
        &self,
        _pool: &SqlitePool,
    ) -> Result<PreparedAuthoredSignOut> {
        Ok(PreparedAuthoredSignOut {
            _lifecycle: Arc::clone(&self.lifecycle_lock).write_owned().await,
        })
    }

    pub(crate) async fn begin_identity_switch(&self) -> AuthoredIdentitySwitch {
        AuthoredIdentitySwitch {
            _lifecycle: Arc::clone(&self.lifecycle_lock).write_owned().await,
        }
    }
}

#[derive(Clone)]
enum DocumentScope {
    Track(TrackScope),
    Pattern(GraphScope),
}

#[derive(Clone)]
struct ResolvedScope {
    document_id: AuthoredDocumentId,
    principal_key: String,
    owner_user_id: Option<String>,
    thread_id: Option<String>,
    document: DocumentScope,
}

impl ResolvedScope {
    fn from_thread(thread: &AgentThread, principal: Option<&str>) -> Result<Self> {
        ensure_thread_owned(thread, principal)?;
        let mut scope = match thread
            .authored_route()
            .map_err(AuthoredDocumentsError::Scope)?
        {
            AuthoredThreadRoute::Track {
                track_id,
                venue_id,
                score_id,
            } => Self::track(
                principal,
                TrackScope {
                    score_id: score_id.to_owned(),
                    track_id: track_id.to_owned(),
                    venue_id: venue_id.to_owned(),
                },
            )?,
            AuthoredThreadRoute::Pattern {
                pattern_id,
                implementation_id,
            } => Self::pattern(principal, pattern_id, implementation_id)?,
        };
        scope.thread_id = Some(thread.id.clone());
        Ok(scope)
    }

    fn track(principal: Option<&str>, track_scope: TrackScope) -> Result<Self> {
        let principal_key = principal_key(principal);
        let spec = NewAuthoredDocument::track_score(
            &principal_key,
            &track_scope.track_id,
            &track_scope.venue_id,
            &track_scope.score_id,
        )?;
        Ok(Self {
            document_id: spec.id,
            principal_key,
            owner_user_id: principal.map(str::to_owned),
            thread_id: None,
            document: DocumentScope::Track(track_scope),
        })
    }

    fn pattern(principal: Option<&str>, pattern_id: &str, implementation_id: &str) -> Result<Self> {
        let principal_key = principal_key(principal);
        let spec =
            NewAuthoredDocument::pattern_graph(&principal_key, pattern_id, implementation_id)?;
        Ok(Self {
            document_id: spec.id,
            principal_key,
            owner_user_id: principal.map(str::to_owned),
            thread_id: None,
            document: DocumentScope::Pattern(GraphScope {
                pattern_id: pattern_id.to_owned(),
                implementation_id: implementation_id.to_owned(),
                owner_user_id: principal.map(str::to_owned),
            }),
        })
    }

    fn specification(&self) -> Result<NewAuthoredDocument> {
        match &self.document {
            DocumentScope::Track(scope) => NewAuthoredDocument::track_score(
                &self.principal_key,
                &scope.track_id,
                &scope.venue_id,
                &scope.score_id,
            )
            .map_err(Into::into),
            DocumentScope::Pattern(scope) => NewAuthoredDocument::pattern_graph(
                &self.principal_key,
                &scope.pattern_id,
                &scope.implementation_id,
            )
            .map_err(Into::into),
        }
    }

    fn track_scope(&self) -> Option<&TrackScope> {
        match &self.document {
            DocumentScope::Track(scope) => Some(scope),
            DocumentScope::Pattern(_) => None,
        }
    }
}

#[derive(Clone)]
enum AuthoredDocument {
    Track(TrackDocument),
    Graph(GraphDocument),
}

impl AuthoredDocument {
    fn revision(&self) -> &str {
        match self {
            Self::Track(track) => &track.revision,
            Self::Graph(graph) => &graph.revision,
        }
    }

    fn projected(&self) -> AuthoredProjectedDocument {
        match self {
            Self::Track(track) => AuthoredProjectedDocument::TrackScore {
                revision: track.revision.clone(),
            },
            Self::Graph(graph) => AuthoredProjectedDocument::PatternGraph {
                implementation_id: graph.implementation_id.clone(),
                revision: graph.revision.clone(),
                graph: graph.graph.clone(),
            },
        }
    }
}

#[derive(Clone)]
struct AuthoredSnapshot {
    files: FileMap,
    document: AuthoredDocument,
}

struct MainState {
    head: RevisionId,
    files: FileMap,
    document: AuthoredDocument,
}

enum TrackProjectionAuthority {
    ExistingOnly,
    HostAllocated(BTreeSet<String>),
    Allowed {
        lineage_ids: BTreeSet<String>,
        host_allocated_ids: BTreeSet<String>,
    },
    TrustedRevision,
}

impl TrackProjectionAuthority {
    fn identity(&self) -> TrackProjectionIdentity<'_> {
        match self {
            Self::ExistingOnly => TrackProjectionIdentity::ExistingOnly,
            Self::HostAllocated(ids) => TrackProjectionIdentity::HostAllocated(ids),
            Self::Allowed {
                lineage_ids,
                host_allocated_ids,
            } => TrackProjectionIdentity::Allowed {
                lineage_ids,
                host_allocated_ids,
            },
            Self::TrustedRevision => TrackProjectionIdentity::TrustedRevision,
        }
    }
}

fn ensure_thread_owned(thread: &AgentThread, principal: Option<&str>) -> Result<()> {
    if thread.owner_user_id.as_deref() != principal {
        return Err(AuthoredDocumentsError::Scope(
            "thread does not belong to the current principal".into(),
        ));
    }
    validate_token(&thread.id, "thread id")
}

fn validate_token(value: &str, name: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AuthoredDocumentsError::Invalid(format!(
            "{name} must be a bounded alphanumeric token"
        )));
    }
    Ok(())
}

fn normalized_subject(subject: &str) -> Result<&str> {
    let subject = subject.trim();
    if subject.is_empty() || subject.len() > 240 || subject.contains(['\0', '\n', '\r']) {
        return Err(AuthoredDocumentsError::Invalid(
            "revision subject must be one non-empty line of at most 240 bytes".into(),
        ));
    }
    Ok(subject)
}

fn revision_metadata(
    operation_kind: &str,
    operation_id: Option<&str>,
    subject: &str,
    thread_id: Option<&str>,
    assistant_message_id: Option<&str>,
    restored_revision_id: Option<RevisionId>,
) -> Result<RevisionMetadata> {
    Ok(RevisionMetadata {
        operation_kind: operation_kind.to_owned(),
        operation_id: operation_id.map(str::to_owned),
        message: normalized_subject(subject)?.to_owned(),
        author_name: "Luma".into(),
        author_email: "authored-state@luma.local".into(),
        authored_at: Utc::now().to_rfc3339(),
        thread_id: thread_id.map(str::to_owned),
        assistant_message_id: assistant_message_id.map(str::to_owned),
        restored_revision_id,
    })
}

fn operation_request_fingerprint(kind: &str, fields: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"luma.authored-operation.v2\0");
    for field in std::iter::once(kind).chain(fields.iter().copied()) {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    format!("sha256:{:x}", hash.finalize())
}

fn file_snapshot_id(files: &FileMap) -> String {
    let mut hash = Sha256::new();
    hash.update(b"luma.authored-workspace-snapshot.v1\0");
    for (path, bytes) in files {
        hash.update((path.len() as u64).to_be_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    format!("sha256:{:x}", hash.finalize())
}

fn normalized_creation_request_id(request_id: &str) -> Result<String> {
    Uuid::parse_str(request_id)
        .map(|id| id.to_string())
        .map_err(|_| {
            AuthoredDocumentsError::Invalid("authored creation request_id must be a UUID".into())
        })
}

fn deterministic_creation_id(
    principal_key: &str,
    creation_kind: &str,
    request_id: &str,
    role: &str,
) -> String {
    let mut hash = Sha256::new();
    for field in [
        "luma.authored-creation-id.v1",
        principal_key,
        creation_kind,
        request_id,
        role,
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn graph_files(graph: &Graph) -> Result<FileMap> {
    Ok(FileMap::from([
        (
            GRAPH_PATH.to_owned(),
            semantic_graph_json(graph)?.into_bytes(),
        ),
        (
            LAYOUT_PATH.to_owned(),
            graph_layout_json(graph)?.into_bytes(),
        ),
    ]))
}

fn require_exact_paths(files: &FileMap, expected: &[&str]) -> Result<()> {
    let actual: HashSet<&str> = files.keys().map(String::as_str).collect();
    let expected: HashSet<&str> = expected.iter().copied().collect();
    if actual != expected {
        return Err(AuthoredDocumentsError::Invalid(format!(
            "authored revision has unexpected paths: {actual:?}"
        )));
    }
    Ok(())
}

fn utf8_file<'a>(files: &'a FileMap, path: &str) -> Result<&'a str> {
    let bytes = files
        .get(path)
        .ok_or_else(|| AuthoredDocumentsError::Invalid(format!("missing {path}")))?;
    std::str::from_utf8(bytes)
        .map_err(|_| AuthoredDocumentsError::Invalid(format!("{path} is not UTF-8")))
}

fn serialize_track(
    track: &TrackDocument,
    pattern_names: &PatternNames,
    prior_source: Option<&str>,
) -> Result<String> {
    let prior = prior_source.map(parse_score_document).transpose()?;
    let mut available_names = prior
        .as_ref()
        .map(pattern_names_from_document)
        .unwrap_or_default();
    available_names.extend(pattern_names.clone());
    let mut document = clips_to_canonical_document(&track.clips, &available_names)
        .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
    if let Some(prior) = prior {
        document = merge_document_trivia(&prior, &prior, &prior, document)
            .into_result()
            .map_err(|conflicts| {
                AuthoredDocumentsError::Invalid(format!(
                    "cannot preserve canonical score comments: {} structured conflict(s)",
                    conflicts.len()
                ))
            })?;
    }
    serialize_canonical(&document)
        .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))
}

fn parse_score_document(source: &str) -> Result<Document> {
    match parse_canonical(source) {
        ParseResult::Success { document, .. } => Ok(document),
        ParseResult::Failure { errors, .. } => Err(AuthoredDocumentsError::Invalid(format!(
            "canonical score contains {} syntax error(s)",
            errors.len()
        ))),
    }
}
