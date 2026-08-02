//! Authoritative integration between durable conversations, real Git authored
//! state, and the relational projection consumed by the live application.
//!
//! Git owns history and refs. SQLite owns the live projection plus small
//! routing/recovery records. Every mutation follows the same order:
//! validate a complete document, prepare (but do not publish) a Git commit,
//! project the document and recovery ledger in one SQLite transaction, then
//! compare-and-swap `main`. A crash after projection is recovered by advancing
//! the already-prepared commit; `main` is never published ahead of projection.

mod catalog;
mod edits;
mod ledger;
mod projection;
mod threads;
mod turns;
mod worktrees;

#[cfg(test)]
mod tests;

use catalog::{available_projection_scopes, pattern_projection_scopes, track_scope_from_catalog};
use ledger::{
    archive_projection_ledger, deterministic_creation_id, insert_creation_association,
    load_creation_association, load_ledger, normalized_creation_request_id, operation_association,
    operation_association_for_connection, pending_turn_preparations, record_operation_conflict,
    turn_association, validate_ledger_scope, verify_creation_replay, write_ledger,
    write_operation_association, write_turn_association, write_turn_conflict,
    write_turn_preparation, CommittedOperationReplay, CreationAssociation, LedgerRow,
    MaterializationState, OperationAssociation, OperationOutcome, ProjectionLedger,
    ProjectionLedgerExpectation, TurnOutcome,
};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use uuid::Uuid;

use crate::database::local::agent_threads;
use crate::database::local::auth::principal_key;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, VenueResource, Write};
use crate::database::local::{patterns as pattern_db, scores as score_db, write_admission};
use crate::models::agent_threads::{
    AgentThread, AgentThreadMessage, AuthoredThreadRoute, CreateAgentThreadInput,
};
use crate::models::authored_state::{
    AppliedAuthoredState, AuthoredHistoryEntry, AuthoredHistoryPage, AuthoredMergeConflict,
    AuthoredMergeConflictKind, AuthoredMergePathSegment, AuthoredMergeValue, AuthoredOperationKind,
    AuthoredProjectedDocument, AuthoredRestoreResult, AuthoredTurnCommit, AuthoredWorktree,
    AuthoredWorktreeCheck, AuthoredWorktreeCommit, AuthoredWorktreeMerge,
    CommitAuthoredWorktreeInput, CreateAuthoredWorktreeInput, FinalizeAuthoredTurnInput,
    MergeAuthoredWorktreeInput, PrepareAuthoredTurnInput, PreparedAuthoredTurn,
};
use crate::models::node_graph::{BlendMode, Graph};
use crate::models::patterns::{ForkPatternInput, ForkPatternResult, PatternSummary};
use crate::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, TrackScore, UpdateTrackScoreInput,
};
use crate::services::authored_merge::{merge_graphs, merge_track_documents};
use crate::services::authored_state::{
    file_snapshot_id, worktree_source_manifest, AuthoredRepositoryDescriptor, AuthoredRepositoryId,
    AuthoredStateError, AuthoredStateStore, CommitAuthor, CommitId, CommitInfo, FileMap,
    WorktreeId,
};
use crate::services::graph_documents::{
    apply_graph_edit_in_transaction, canonicalize_graph, exact_graph_json, graph_from_files,
    graph_layout_json, graph_revision, load_graph_document,
    load_unscoped_graph_document_for_connection, semantic_graph_json, GraphDocument,
    GraphDocumentError, GraphEditPlan, GraphScope, GraphValidationIssue,
};
use crate::services::score_dsl::{
    clips_to_canonical_document, compile_draft_track_document, compile_import_track_document,
    decode_canonical_track_document, load_score_dsl_context, load_score_pattern_names,
    merge_document_trivia, parse_canonical, pattern_names_from_document, serialize_canonical,
    Document, ParseResult, PatternNames,
};
use crate::services::track_edits::{
    apply_track_projection_in_transaction, check_track_projection_candidate,
    check_track_worktree_candidate, is_valid_track_draft_id, load_track_document_for_principal,
    plan_track_snapshot_replacement, remap_track_snapshot_result, revision_for_clips,
    track_clips_semantically_equal, validate_track_draft_envelope, TrackClip, TrackDocument,
    TrackEditError, TrackEditPlan, TrackEditResult, TrackProjectionIdentity, TrackScope,
};
use crate::storage::StorageRoot;

const MAIN_BRANCH: &str = "main";
const SCORE_PATH: &str = "score.luma";
const GRAPH_PATH: &str = "graph.json";
const LAYOUT_PATH: &str = "layout.json";
const MAX_HISTORY_PAGE: usize = 500;
const PATTERN_SUMMARY_COLUMNS: &str =
    "id, uid, name, description, category_name, created_at, updated_at, is_verified, author_name, forked_from_id";

const TRAILER_OPERATION: &str = "Luma-Operation";
const TRAILER_THREAD: &str = "Luma-Thread-ID";
const TRAILER_MESSAGE: &str = "Luma-Assistant-Message-ID";
const TRAILER_WORKTREE: &str = "Luma-Worktree-ID";
const TRAILER_RESTORE: &str = "Luma-Restore-Commit";
const TRAILER_OPERATION_ID: &str = "Luma-Operation-ID";
const TRAILER_WORKTREE_HEAD: &str = "Luma-Worktree-Head";
const TRAILER_WORKTREE_SOURCE_MANIFEST: &str = "Luma-Worktree-Source-Manifest";
const TRAILER_REQUEST_FINGERPRINT: &str = "Luma-Request-Fingerprint";
const TRAILER_FORK_SOURCE_PATTERN: &str = "Luma-Fork-Source-Pattern";
const TRAILER_FORK_SOURCE_IMPLEMENTATION: &str = "Luma-Fork-Source-Implementation";

#[derive(Debug)]
pub enum AuthoredDocumentsError {
    Invalid(String),
    Scope(String),
    Storage(String),
    Git(AuthoredStateError),
    Track(TrackEditError),
    Graph(GraphDocumentError),
}

impl fmt::Display for AuthoredDocumentsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Scope(message) | Self::Storage(message) => {
                formatter.write_str(message)
            }
            Self::Git(error) => error.fmt(formatter),
            Self::Track(error) => error.fmt(formatter),
            Self::Graph(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AuthoredDocumentsError {}

impl From<AuthoredStateError> for AuthoredDocumentsError {
    fn from(value: AuthoredStateError) -> Self {
        Self::Git(value)
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

/// One authoritative score mutation result. `edit.id_map` is the bridge from
/// caller draft IDs to host-owned UUIDs; `authored` identifies the Git commit
/// and current projection produced by the same transaction protocol.
pub struct AppliedAuthoredTrackEdit {
    pub authored: AppliedAuthoredState,
    pub edit: TrackEditResult,
}

/// Managed Tauri state. The async lock is the integration-layer transaction
/// boundary: it spans Git preparation, SQLite projection, and ref CAS. The
/// lower-level store still owns its short synchronous libgit2 locks.
#[derive(Clone)]
pub struct AuthoredDocuments {
    store: AuthoredStateStore,
    repository_locks: Arc<Mutex<HashMap<AuthoredRepositoryId, Arc<Mutex<()>>>>>,
    lifecycle_lock: Arc<RwLock<()>>,
}

struct AuthoredDocumentGuard {
    _lifecycle: OwnedRwLockReadGuard<()>,
    _repository: OwnedMutexGuard<()>,
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
            store: AuthoredStateStore::new(storage),
            repository_locks: Arc::new(Mutex::new(HashMap::new())),
            lifecycle_lock: Arc::new(RwLock::new(())),
        }
    }

    async fn repository_guard(
        &self,
        repository_id: &AuthoredRepositoryId,
    ) -> AuthoredDocumentGuard {
        let lifecycle = Arc::clone(&self.lifecycle_lock).read_owned().await;
        let repository = self.repository_guard_inside_lifecycle(repository_id).await;
        AuthoredDocumentGuard {
            _lifecycle: lifecycle,
            _repository: repository,
        }
    }

    async fn repository_guard_inside_lifecycle(
        &self,
        repository_id: &AuthoredRepositoryId,
    ) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.repository_locks.lock().await;
            Arc::clone(
                locks
                    .entry(repository_id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        lock.lock_owned().await
    }

    /// Establish the destructive sign-out barrier. Every live authored
    /// document is durably reconciled into Git while holding the global write
    /// side of the lifecycle gate. The returned guard must remain alive until
    /// the relational wipe commits, so no new document can appear in between.
    pub(crate) async fn prepare_sign_out(
        &self,
        pool: &SqlitePool,
    ) -> Result<PreparedAuthoredSignOut> {
        let lifecycle = Arc::clone(&self.lifecycle_lock).write_owned().await;
        self.reconcile_available_inside_lifecycle(pool).await?;
        Ok(PreparedAuthoredSignOut {
            _lifecycle: lifecycle,
        })
    }

    /// Drain every authored/thread operation and prevent new ones from
    /// entering while the host replaces the credential identity and app-DB
    /// admission. Unlike sign-out, account replacement does not reconcile or
    /// wipe either principal's retained state.
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
    repository_id: AuthoredRepositoryId,
    principal_key: String,
    owner_user_id: Option<String>,
    subject_id: String,
    thread_id: Option<String>,
    document: DocumentScope,
}

impl ResolvedScope {
    fn from_thread(thread: &AgentThread, principal: Option<&str>) -> Result<Self> {
        ensure_thread_owned(thread, principal)?;
        match thread
            .authored_route()
            .map_err(AuthoredDocumentsError::Scope)?
        {
            AuthoredThreadRoute::Track {
                track_id,
                venue_id,
                score_id,
            } => {
                let mut scope = Self::track(
                    principal,
                    TrackScope {
                        score_id: score_id.to_owned(),
                        track_id: track_id.to_owned(),
                        venue_id: venue_id.to_owned(),
                    },
                )?;
                scope.thread_id = Some(thread.id.clone());
                Ok(scope)
            }
            AuthoredThreadRoute::Pattern {
                pattern_id,
                implementation_id,
            } => {
                let mut scope = Self::pattern(principal, pattern_id, implementation_id)?;
                scope.thread_id = Some(thread.id.clone());
                Ok(scope)
            }
        }
    }

    fn track(principal: Option<&str>, track_scope: TrackScope) -> Result<Self> {
        let principal_key = principal_key(principal);
        let descriptor = AuthoredRepositoryDescriptor::track_score(
            &principal_key,
            &track_scope.track_id,
            &track_scope.venue_id,
            &track_scope.score_id,
        )?;
        Ok(Self {
            repository_id: AuthoredRepositoryId::derive(&descriptor),
            principal_key,
            owner_user_id: principal.map(str::to_owned),
            subject_id: track_scope.track_id.clone(),
            thread_id: None,
            document: DocumentScope::Track(track_scope),
        })
    }

    fn pattern(principal: Option<&str>, pattern_id: &str, implementation_id: &str) -> Result<Self> {
        let principal_key = principal_key(principal);
        let descriptor = AuthoredRepositoryDescriptor::pattern_graph(
            &principal_key,
            pattern_id,
            implementation_id,
        )?;
        Ok(Self {
            repository_id: AuthoredRepositoryId::derive(&descriptor),
            principal_key,
            owner_user_id: principal.map(str::to_owned),
            subject_id: pattern_id.to_owned(),
            thread_id: None,
            document: DocumentScope::Pattern(GraphScope {
                pattern_id: pattern_id.to_owned(),
                implementation_id: implementation_id.to_owned(),
                owner_user_id: principal.map(str::to_owned),
            }),
        })
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

enum TrackProjectionAuthority {
    ExistingOnly,
    HostAllocated(BTreeSet<String>),
    Allowed {
        lineage_ids: BTreeSet<String>,
        host_allocated_ids: BTreeSet<String>,
    },
    TrustedRepositoryTree,
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
            Self::TrustedRepositoryTree => TrackProjectionIdentity::TrustedRepositoryTree,
        }
    }
}

impl AuthoredDocument {
    fn revision(&self) -> &str {
        match self {
            Self::Track(track) => &track.revision,
            Self::Graph(graph) => &graph.revision,
        }
    }

    fn semantic_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Track(left), Self::Track(right)) => left.revision == right.revision,
            (Self::Graph(left), Self::Graph(right)) => left.revision == right.revision,
            _ => false,
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
    head: CommitId,
    files: FileMap,
    document: AuthoredDocument,
}

#[derive(Default)]
struct ProjectionMetadata {
    turn: Option<TurnProjection>,
    operation: Option<OperationProjection>,
    creation: Option<CreationProjection>,
}

struct TurnProjection {
    thread_id: String,
    assistant_message_id: String,
    branch_commit: CommitId,
}

struct OperationProjection {
    kind: &'static str,
    operation_id: String,
    request_fingerprint: String,
    base_main_commit: CommitId,
    result_json: Option<String>,
}

struct CreationProjection {
    kind: &'static str,
    request_id: String,
    request_fingerprint: String,
    /// The host-created entity, never its containing authored document.
    subject_id: String,
    auxiliary_id: Option<String>,
}

fn ensure_thread_owned(thread: &AgentThread, principal: Option<&str>) -> Result<()> {
    if thread.owner_user_id.as_deref() != principal {
        return Err(AuthoredDocumentsError::Scope(
            "thread does not belong to the current principal".into(),
        ));
    }
    validate_ref_component(&thread.id, "thread id")
}

fn validate_ref_component(value: &str, name: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AuthoredDocumentsError::Invalid(format!(
            "{name} is not safe for authored Git routing"
        )));
    }
    Ok(())
}

fn system_author() -> Result<CommitAuthor> {
    CommitAuthor::new(
        "Luma",
        "authored-state@luma.local",
        Utc::now().timestamp(),
        0,
    )
    .map_err(Into::into)
}

fn normalized_subject(subject: &str) -> Result<&str> {
    let subject = subject.trim();
    if subject.is_empty()
        || subject.len() > 240
        || subject.contains('\0')
        || subject.contains('\n')
        || subject.contains('\r')
    {
        return Err(AuthoredDocumentsError::Invalid(
            "commit subject must be one non-empty line of at most 240 bytes".into(),
        ));
    }
    Ok(subject)
}

fn operation_request_fingerprint(kind: &str, fields: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"luma.authored-operation.v1\0");
    for field in std::iter::once(kind).chain(fields.iter().copied()) {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    format!("sha256:{:x}", hash.finalize())
}

fn commit_message(subject: &str, trailers: &[(&str, &str)]) -> Result<String> {
    let subject = normalized_subject(subject)?;
    let mut message = format!("{subject}\n\n");
    let mut seen = HashSet::new();
    for (key, value) in trailers {
        if !seen.insert(*key)
            || value.is_empty()
            || value.contains('\0')
            || value.contains('\n')
            || value.contains('\r')
        {
            return Err(AuthoredDocumentsError::Invalid(
                "invalid authored commit trailer".into(),
            ));
        }
        message.push_str(key);
        message.push_str(": ");
        message.push_str(value);
        message.push('\n');
    }
    Ok(message)
}

fn find_commit_with_trailers(
    store: &AuthoredStateStore,
    repository_id: &AuthoredRepositoryId,
    branch: &str,
    expected: &[(&str, &str)],
) -> Result<Option<CommitInfo>> {
    store
        .find_reachable_commit(repository_id, branch, |commit| {
            let trailers = parse_trailers(&commit.message);
            expected
                .iter()
                .all(|(key, value)| trailers.get(*key).is_some_and(|found| found == value))
                .then(|| commit.clone())
        })
        .map_err(Into::into)
}

fn commit_changed(
    store: &AuthoredStateStore,
    repository_id: &AuthoredRepositoryId,
    commit: &CommitInfo,
) -> Result<bool> {
    let Some(parent) = commit.parents.first() else {
        return Ok(true);
    };
    let (_, files) = store.read_commit(repository_id, &commit.id)?;
    let (_, parent_files) = store.read_commit(repository_id, parent)?;
    Ok(files != parent_files)
}

fn parse_trailers(message: &str) -> HashMap<String, String> {
    let mut trailers = HashMap::new();
    for line in message.lines().rev() {
        if line.trim().is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(": ") else {
            break;
        };
        trailers.insert(key.to_owned(), value.to_owned());
    }
    trailers
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
            "authored tree has unexpected paths: {:?}",
            actual
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
