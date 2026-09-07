//! Relational authored-document revision history.
//!
//! This module is deliberately domain-neutral. It stores exact canonical file
//! bytes, immutable revision metadata, ordered parent edges, and one local
//! compare-and-swap head per bounded document. Score/graph validation and live
//! relational projection belong to `AuthoredDocuments`, which can call these
//! operations on the same SQLite transaction as its domain writes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::Path;

use chrono::DateTime;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqliteConnection};

const DOCUMENT_ID_DOMAIN: &[u8] = b"luma.authored-document.v1\0";
const REVISION_ID_DOMAIN: &[u8] = b"luma.authored-revision.v1\0";
const CONTENT_MANIFEST_DOMAIN: &[u8] = b"luma.authored-content-manifest.v1\0";
const FILE_CONTENT_DOMAIN: &[u8] = b"luma.authored-file-content.v1\0";
const MAX_FILES: usize = 4_096;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_PARENTS: usize = 2;
#[cfg(test)]
const MAX_HISTORY_PAGE: usize = 500;

pub(crate) type FileMap = BTreeMap<String, Vec<u8>>;

#[derive(Debug)]
pub enum AuthoredStateError {
    InvalidInput(String),
    NotFound(String),
    Corrupt(String),
    HeadConflict {
        document_id: String,
        expected: String,
        actual: String,
    },
    AmbiguousMergeBase {
        candidates: Vec<String>,
    },
    Storage(sqlx::Error),
}

impl fmt::Display for AuthoredStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::NotFound(message) | Self::Corrupt(message) => {
                formatter.write_str(message)
            }
            Self::HeadConflict {
                document_id,
                expected,
                actual,
            } => write!(
                formatter,
                "authored document {document_id} head moved (expected {expected}, actual {actual})"
            ),
            Self::AmbiguousMergeBase { candidates } => write!(
                formatter,
                "authored revisions have multiple best merge bases: {}",
                candidates.join(", ")
            ),
            Self::Storage(error) => write!(formatter, "authored revision storage failed: {error}"),
        }
    }
}

impl std::error::Error for AuthoredStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for AuthoredStateError {
    fn from(value: sqlx::Error) -> Self {
        Self::Storage(value)
    }
}

pub(crate) type Result<T> = std::result::Result<T, AuthoredStateError>;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct AuthoredDocumentId(String);

impl AuthoredDocumentId {
    pub(crate) fn derive(domain: &str, identity_parts: &[&str]) -> Result<Self> {
        validate_identity_domain(domain)?;
        if identity_parts.is_empty() {
            return Err(AuthoredStateError::InvalidInput(
                "authored document identity needs at least one part".into(),
            ));
        }
        let mut hash = Sha256::new();
        hash.update(DOCUMENT_ID_DOMAIN);
        hash_field(&mut hash, domain.as_bytes());
        for part in identity_parts {
            validate_required(part, "authored document identity part")?;
            hash_field(&mut hash, part.as_bytes());
        }
        Ok(Self(format!("ad-{:x}", hash.finalize())))
    }

    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_sha_id(&value, "ad-", "authored document id")?;
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthoredDocumentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RevisionId(String);

impl RevisionId {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_sha_id(&value, "rv-", "authored revision id")?;
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RevisionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthoredDocumentKind {
    TrackScore,
    PatternGraph,
}

impl AuthoredDocumentKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TrackScore => "track_score",
            Self::PatternGraph => "pattern_graph",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "track_score" => Ok(Self::TrackScore),
            "pattern_graph" => Ok(Self::PatternGraph),
            _ => Err(AuthoredStateError::Corrupt(format!(
                "unknown authored document kind {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NewAuthoredDocument {
    pub id: AuthoredDocumentId,
    pub kind: AuthoredDocumentKind,
    pub principal_key: String,
    pub subject_id: String,
    pub track_id: Option<String>,
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
    pub implementation_id: Option<String>,
}

impl NewAuthoredDocument {
    pub(crate) fn track_score(
        principal_key: &str,
        track_id: &str,
        venue_id: &str,
        score_id: &str,
    ) -> Result<Self> {
        validate_principal_key(principal_key)?;
        for (value, name) in [
            (track_id, "track id"),
            (venue_id, "venue id"),
            (score_id, "score id"),
        ] {
            validate_required(value, name)?;
        }
        Ok(Self {
            id: AuthoredDocumentId::derive(
                "track_score",
                &[principal_key, track_id, venue_id, score_id],
            )?,
            kind: AuthoredDocumentKind::TrackScore,
            principal_key: principal_key.to_owned(),
            subject_id: track_id.to_owned(),
            track_id: Some(track_id.to_owned()),
            venue_id: Some(venue_id.to_owned()),
            score_id: Some(score_id.to_owned()),
            implementation_id: None,
        })
    }

    pub(crate) fn pattern_graph(
        principal_key: &str,
        pattern_id: &str,
        implementation_id: &str,
    ) -> Result<Self> {
        validate_principal_key(principal_key)?;
        validate_required(pattern_id, "pattern id")?;
        validate_required(implementation_id, "implementation id")?;
        Ok(Self {
            id: AuthoredDocumentId::derive(
                "pattern_graph",
                &[principal_key, pattern_id, implementation_id],
            )?,
            kind: AuthoredDocumentKind::PatternGraph,
            principal_key: principal_key.to_owned(),
            subject_id: pattern_id.to_owned(),
            track_id: None,
            venue_id: None,
            score_id: None,
            implementation_id: Some(implementation_id.to_owned()),
        })
    }

    fn validate(&self) -> Result<()> {
        validate_principal_key(&self.principal_key)?;
        validate_required(&self.subject_id, "authored subject id")?;
        let expected = match self.kind {
            AuthoredDocumentKind::TrackScore => {
                let track_id = required_option(&self.track_id, "track id")?;
                let venue_id = required_option(&self.venue_id, "venue id")?;
                let score_id = required_option(&self.score_id, "score id")?;
                if self.subject_id != track_id || self.implementation_id.is_some() {
                    return Err(AuthoredStateError::InvalidInput(
                        "track score document has inconsistent routing".into(),
                    ));
                }
                AuthoredDocumentId::derive(
                    self.kind.as_str(),
                    &[&self.principal_key, track_id, venue_id, score_id],
                )?
            }
            AuthoredDocumentKind::PatternGraph => {
                let implementation_id =
                    required_option(&self.implementation_id, "implementation id")?;
                if self.track_id.is_some() || self.venue_id.is_some() || self.score_id.is_some() {
                    return Err(AuthoredStateError::InvalidInput(
                        "pattern graph document has inconsistent routing".into(),
                    ));
                }
                AuthoredDocumentId::derive(
                    self.kind.as_str(),
                    &[&self.principal_key, &self.subject_id, implementation_id],
                )?
            }
        };
        if expected != self.id {
            return Err(AuthoredStateError::InvalidInput(
                "authored document id does not match its immutable scope".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthoredDocumentRecord {
    pub spec: NewAuthoredDocument,
    pub archived_at: Option<String>,
    pub created_at: String,
}

/// Who produced a revision: a stable label, kept on the row forever because a
/// thread can be deleted and a model key can retire.
///
/// An open vocabulary on purpose — `user`, a model key from
/// [`crate::agent::model::MODELS`], or `client:<name>/<version>[:<model>]` for
/// an out-of-process MCP client — so a new writer needs no schema change and no
/// second enum to drift from this one. Deliberately *not* part of
/// [`derive_revision_id`]: the id is a content-and-operation hash that the
/// server re-derives (`private.expected_revision_id`), and hashing provenance
/// would invalidate every revision that already exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Actor(String);

impl Actor {
    /// A human editing in the app. Also what a revision with no writing
    /// session of its own falls back to.
    pub(crate) const USER: &'static str = "user";
    /// A revision minted by the sync layer itself while integrating a
    /// server-ordered proposal — no human and no model authored it.
    pub(crate) const SYNC: &'static str = "sync";

    /// The label for the human in the editor.
    pub(crate) fn user() -> Self {
        Self(Self::USER.to_owned())
    }

    /// The label for a device-convergence revision the sync layer minted.
    pub(crate) fn sync() -> Self {
        Self(Self::SYNC.to_owned())
    }

    /// Read a stored or caller-supplied label.
    ///
    /// A label naming a model this build knows is canonicalized to that
    /// model's key, so the several spellings the two agent loops accept
    /// (`claude-opus-5`, `anthropic/claude-opus-5`) land on one actor. Anything
    /// else — `user`, a client label, a retired model — is kept verbatim: an
    /// unrecognized writer is still an honest one.
    ///
    /// # Errors
    ///
    /// [`AuthoredStateError::InvalidInput`] if the label is empty, over 256
    /// bytes, or carries anything but `[A-Za-z0-9-_.:/]`.
    pub(crate) fn parse(label: &str) -> Result<Self> {
        validate_actor(label)?;
        let label = crate::agent::model::ModelId::parse(label).map_or(label, |id| id.key());
        Ok(Self(label.to_owned()))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RevisionMetadata {
    pub operation_kind: String,
    /// Who produced this revision. Unlike `author_name`/`author_email` — two
    /// constants the deterministic id happens to hash — this is the real
    /// answer, and it is not hashed.
    pub actor: Actor,
    pub operation_id: Option<String>,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    /// RFC 3339 timestamp captured by the trusted host.
    pub authored_at: String,
    pub thread_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub restored_revision_id: Option<RevisionId>,
}

impl RevisionMetadata {
    fn validate(&self) -> Result<()> {
        if self.operation_kind.is_empty()
            || self.operation_kind.len() > 64
            || !self
                .operation_kind
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(AuthoredStateError::InvalidInput(
                "revision operation kind must be lower snake case".into(),
            ));
        }
        if let Some(operation_id) = &self.operation_id {
            validate_token(operation_id, "revision operation id")?;
        }
        validate_optional_text(&self.message, "revision message", 8_192)?;
        validate_actor(self.actor.as_str())?;
        validate_optional_text(&self.author_name, "revision author name", 1_024)?;
        validate_optional_text(&self.author_email, "revision author email", 1_024)?;
        validate_rfc3339(&self.authored_at, "revision authored_at")?;
        if let Some(thread_id) = &self.thread_id {
            validate_token(thread_id, "revision thread id")?;
        }
        if let Some(message_id) = &self.assistant_message_id {
            validate_token(message_id, "revision assistant message id")?;
            if self.thread_id.is_none() {
                return Err(AuthoredStateError::InvalidInput(
                    "assistant message revision metadata requires a thread id".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RevisionInfo {
    pub id: RevisionId,
    pub document_id: AuthoredDocumentId,
    pub principal_key: String,
    pub content_hash: String,
    pub parents: Vec<RevisionId>,
    pub metadata: RevisionMetadata,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthoredHead {
    pub document_id: AuthoredDocumentId,
    pub principal_key: String,
    pub revision_id: RevisionId,
    pub generation: i64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthoredFileManifest {
    pub path: String,
    pub content_hash: String,
    pub byte_length: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthoredContentManifest {
    pub content_hash: String,
    pub files: Vec<AuthoredFileManifest>,
    pub byte_length: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileChangeKind {
    Added,
    Deleted,
    Modified,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileDiff {
    pub path: String,
    pub kind: FileChangeKind,
    pub old_content_hash: Option<String>,
    pub new_content_hash: Option<String>,
}

/// Stateless relational revision operations. Every method receives the exact
/// connection whose surrounding transaction owns the larger domain mutation.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AuthoredRevisionStore;

impl AuthoredRevisionStore {
    pub(crate) async fn insert_document(
        &self,
        connection: &mut SqliteConnection,
        document: &NewAuthoredDocument,
    ) -> Result<AuthoredDocumentRecord> {
        document.validate()?;
        sqlx::query(
            "INSERT INTO authored_documents
             (document_id, document_kind, principal_key, subject_id, track_id, venue_id,
              score_id, implementation_id)
             SELECT ?, ?, ?, ?, ?, ?, ?, ?
             WHERE NOT (
                 (? = 'track_score' AND EXISTS (
                     SELECT 1 FROM authored_documents terminal
                     WHERE terminal.principal_key = ?
                       AND terminal.document_kind = 'track_score'
                       AND terminal.score_id IS ?
                       AND terminal.archived_at IS NOT NULL
                 ))
                 OR
                 (? = 'pattern_graph'
                  AND EXISTS (
                      SELECT 1 FROM authored_documents sibling
                      WHERE sibling.principal_key = ?
                        AND sibling.document_kind = 'pattern_graph'
                        AND sibling.subject_id = ?
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM authored_documents live
                      WHERE live.principal_key = ?
                        AND live.document_kind = 'pattern_graph'
                        AND live.subject_id = ?
                        AND live.archived_at IS NULL
                  ))
             )
             ON CONFLICT(document_id) DO NOTHING",
        )
        .bind(document.id.as_str())
        .bind(document.kind.as_str())
        .bind(&document.principal_key)
        .bind(&document.subject_id)
        .bind(&document.track_id)
        .bind(&document.venue_id)
        .bind(&document.score_id)
        .bind(&document.implementation_id)
        .bind(document.kind.as_str())
        .bind(&document.principal_key)
        .bind(&document.score_id)
        .bind(document.kind.as_str())
        .bind(&document.principal_key)
        .bind(&document.subject_id)
        .bind(&document.principal_key)
        .bind(&document.subject_id)
        .execute(&mut *connection)
        .await?;
        let stored = match self.document(connection, &document.id).await {
            Ok(stored) => stored,
            Err(AuthoredStateError::NotFound(_)) => {
                return Err(AuthoredStateError::InvalidInput(format!(
                    "new {} document conflicts with a terminal authored route",
                    document.kind.as_str()
                )));
            }
            Err(error) => return Err(error),
        };
        if stored.spec != *document {
            return Err(AuthoredStateError::Corrupt(format!(
                "authored document {} is already bound to another scope",
                document.id
            )));
        }
        Ok(stored)
    }

    pub(crate) async fn document(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
    ) -> Result<AuthoredDocumentRecord> {
        let row = sqlx::query_as::<_, DocumentRow>(
            "SELECT document_id, document_kind, principal_key, subject_id, track_id,
                    venue_id, score_id, implementation_id, archived_at, created_at
             FROM authored_documents WHERE document_id = ?",
        )
        .bind(document_id.as_str())
        .fetch_optional(&mut *connection)
        .await?
        .ok_or_else(|| {
            AuthoredStateError::NotFound(format!("authored document {document_id} does not exist"))
        })?;
        row.try_into()
    }

    pub(crate) async fn archive_document(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        expected_head: &RevisionId,
        archived_at: &str,
    ) -> Result<AuthoredDocumentRecord> {
        validate_rfc3339(archived_at, "authored archive timestamp")?;
        let updated = sqlx::query(
            "UPDATE authored_documents SET archived_at = ?
             WHERE document_id = ? AND archived_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM authored_document_heads head
                   WHERE head.document_id = authored_documents.document_id
                     AND head.revision_id = ?
               )",
        )
        .bind(archived_at)
        .bind(document_id.as_str())
        .bind(expected_head.as_str())
        .execute(&mut *connection)
        .await?
        .rows_affected();
        let document = self.document(connection, document_id).await?;
        if updated == 1 || document.archived_at.as_deref() == Some(archived_at) {
            return Ok(document);
        }
        if let Some(actual) = document.archived_at {
            return Err(AuthoredStateError::Corrupt(format!(
                "authored document {document_id} was archived by another operation at {actual}"
            )));
        }
        let head = self.head(connection, document_id).await?;
        Err(AuthoredStateError::HeadConflict {
            document_id: document_id.to_string(),
            expected: expected_head.to_string(),
            actual: head.revision_id.to_string(),
        })
    }

    /// Insert one immutable revision. The ID is a deterministic hash of the
    /// document, ordered parents, content manifest, and typed operation
    /// metadata. An exact retry returns the existing row; the same content in
    /// a later restore still receives a distinct ID because its lineage and
    /// operation metadata differ.
    pub(crate) async fn insert_revision(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        parents: &[RevisionId],
        files: &FileMap,
        metadata: &RevisionMetadata,
    ) -> Result<RevisionInfo> {
        metadata.validate()?;
        if parents.len() > MAX_PARENTS {
            return Err(AuthoredStateError::InvalidInput(format!(
                "an authored revision may have at most {MAX_PARENTS} parents"
            )));
        }
        if parents.iter().collect::<HashSet<_>>().len() != parents.len() {
            return Err(AuthoredStateError::InvalidInput(
                "an authored revision cannot repeat a parent".into(),
            ));
        }
        let document = self.document(connection, document_id).await?;
        let manifest = content_manifest(files)?;
        let revision_id = derive_revision_id(document_id, parents, &manifest, metadata);
        // Exact retries are reads, not new writes. In particular, retrying the
        // deterministic initial revision must not fail merely because history
        // now exists, and a response-loss retry remains safe after archival.
        if let Some(existing) = self
            .revision_info_optional(connection, document_id, &revision_id)
            .await?
        {
            let (_, existing_files) = self
                .read_revision(connection, document_id, &revision_id)
                .await?;
            if existing.parents == parents
                && existing.content_hash == manifest.content_hash
                && existing.metadata == *metadata
                && existing_files == *files
            {
                return Ok(existing);
            }
            return Err(AuthoredStateError::Corrupt(format!(
                "deterministic revision id {revision_id} is bound to different content"
            )));
        }
        if document.archived_at.is_some() {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored document {document_id} is archived"
            )));
        }
        let existing_revision_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM authored_revisions WHERE document_id = ?")
                .bind(document_id.as_str())
                .fetch_one(&mut *connection)
                .await?;
        if parents.is_empty() && existing_revision_count != 0 {
            return Err(AuthoredStateError::InvalidInput(
                "only a document's initial revision may have no parent".into(),
            ));
        }
        if !parents.is_empty() && existing_revision_count == 0 {
            return Err(AuthoredStateError::InvalidInput(
                "a document's initial revision cannot name parents".into(),
            ));
        }
        for parent in parents {
            self.require_revision_in_document(connection, document_id, parent)
                .await?;
        }
        if let Some(restored) = &metadata.restored_revision_id {
            self.require_revision_in_document(connection, document_id, restored)
                .await?;
        }

        sqlx::query(
            "INSERT INTO authored_revisions
             (revision_id, document_id, principal_key, parent_count, content_hash,
              operation_kind, operation_id,
              message, actor, author_name, author_email, authored_at, thread_id,
              assistant_message_id, restored_revision_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(revision_id.as_str())
        .bind(document_id.as_str())
        .bind(&document.spec.principal_key)
        .bind(i64::try_from(parents.len()).expect("at most two parents"))
        .bind(&manifest.content_hash)
        .bind(&metadata.operation_kind)
        .bind(&metadata.operation_id)
        .bind(&metadata.message)
        .bind(metadata.actor.as_str())
        .bind(&metadata.author_name)
        .bind(&metadata.author_email)
        .bind(&metadata.authored_at)
        .bind(&metadata.thread_id)
        .bind(&metadata.assistant_message_id)
        .bind(
            metadata
                .restored_revision_id
                .as_ref()
                .map(RevisionId::as_str),
        )
        .execute(&mut *connection)
        .await?;
        for file in &manifest.files {
            sqlx::query(
                "INSERT INTO authored_revision_files
                 (revision_id, principal_key, path, content_hash, content)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(revision_id.as_str())
            .bind(&document.spec.principal_key)
            .bind(&file.path)
            .bind(&file.content_hash)
            .bind(
                files
                    .get(&file.path)
                    .expect("manifest path came from files"),
            )
            .execute(&mut *connection)
            .await?;
        }
        for (parent_order, parent) in parents.iter().enumerate() {
            sqlx::query(
                "INSERT INTO authored_revision_parents
                 (principal_key, document_id, revision_id, parent_order, parent_revision_id)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&document.spec.principal_key)
            .bind(document_id.as_str())
            .bind(revision_id.as_str())
            .bind(i64::try_from(parent_order).expect("at most two parents"))
            .bind(parent.as_str())
            .execute(&mut *connection)
            .await?;
        }
        self.revision_info(connection, document_id, &revision_id)
            .await
    }

    pub(crate) async fn create_head(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        revision_id: &RevisionId,
    ) -> Result<AuthoredHead> {
        let document = self.document(connection, document_id).await?;
        if document.archived_at.is_some() {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored document {document_id} is archived"
            )));
        }
        self.require_revision_in_document(connection, document_id, revision_id)
            .await?;
        let inserted = sqlx::query(
            "INSERT INTO authored_document_heads (document_id, principal_key, revision_id)
             VALUES (?, ?, ?) ON CONFLICT(document_id) DO NOTHING",
        )
        .bind(document_id.as_str())
        .bind(&document.spec.principal_key)
        .bind(revision_id.as_str())
        .execute(&mut *connection)
        .await?
        .rows_affected();
        let head = self.head(connection, document_id).await?;
        if inserted == 1 || head.revision_id == *revision_id {
            Ok(head)
        } else {
            Err(AuthoredStateError::HeadConflict {
                document_id: document_id.to_string(),
                expected: "<missing>".into(),
                actual: head.revision_id.to_string(),
            })
        }
    }

    /// Materialize one exact server head observation. The caller must hold a
    /// trusted-pull admission because server generations can jump, move to a
    /// sibling revision, or repair a stale local clock without changing the
    /// revision. The expected revision keeps the projection update a local
    /// compare-and-swap; `None` is valid only for the first observed head.
    pub(crate) async fn project_server_head(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        expected: Option<&RevisionId>,
        target: &RevisionId,
        generation: i64,
        updated_at: &str,
    ) -> Result<AuthoredHead> {
        let document = self.document(connection, document_id).await?;
        if document.archived_at.is_some() {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored document {document_id} is archived"
            )));
        }
        self.require_revision_in_document(connection, document_id, target)
            .await?;
        if let Some(expected) = expected {
            self.require_revision_in_document(connection, document_id, expected)
                .await?;
        }
        let admitted: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM auth_write_admission AS admission
             WHERE admission.singleton = 1 AND admission.armed = 1
               AND admission.accepting = 1 AND admission.maintenance = 0
               AND admission.remote_writes = 1
               AND ? = 'signed-in:' || admission.active_uid",
        )
        .bind(&document.spec.principal_key)
        .fetch_optional(&mut *connection)
        .await?;
        if admitted.is_none() {
            return Err(AuthoredStateError::InvalidInput(
                "server head projection requires trusted remote-write admission".into(),
            ));
        }

        match expected {
            Some(expected) => {
                sqlx::query(
                    "UPDATE authored_document_heads
                     SET revision_id = ?, generation = ?, updated_at = ?
                     WHERE document_id = ? AND principal_key = ? AND revision_id = ?",
                )
                .bind(target.as_str())
                .bind(generation)
                .bind(updated_at)
                .bind(document_id.as_str())
                .bind(&document.spec.principal_key)
                .bind(expected.as_str())
                .execute(&mut *connection)
                .await?;
            }
            None => {
                sqlx::query(
                    "INSERT INTO authored_document_heads
                     (document_id, principal_key, revision_id, generation, updated_at)
                     VALUES (?, ?, ?, ?, ?) ON CONFLICT(document_id) DO NOTHING",
                )
                .bind(document_id.as_str())
                .bind(&document.spec.principal_key)
                .bind(target.as_str())
                .bind(generation)
                .bind(updated_at)
                .execute(&mut *connection)
                .await?;
            }
        }
        let head = self.head(connection, document_id).await?;
        if head.revision_id == *target
            && head.generation == generation
            && head.updated_at == updated_at
        {
            Ok(head)
        } else {
            Err(AuthoredStateError::HeadConflict {
                document_id: document_id.to_string(),
                expected: expected
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "<missing>".into()),
                actual: head.revision_id.to_string(),
            })
        }
    }

    pub(crate) async fn head(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
    ) -> Result<AuthoredHead> {
        let document = self.document(connection, document_id).await?;
        let row = sqlx::query_as::<_, HeadRow>(
            "SELECT document_id, principal_key, revision_id, generation, updated_at
             FROM authored_document_heads WHERE document_id = ?",
        )
        .bind(document_id.as_str())
        .fetch_optional(&mut *connection)
        .await?
        .ok_or_else(|| {
            AuthoredStateError::NotFound(format!("authored document {document_id} has no head"))
        })?;
        let head: AuthoredHead = row.try_into()?;
        if head.principal_key != document.spec.principal_key {
            return Err(AuthoredStateError::Corrupt(format!(
                "authored document {document_id} head has the wrong principal binding"
            )));
        }
        self.require_revision_in_document(connection, document_id, &head.revision_id)
            .await?;
        Ok(head)
    }

    /// Advance a head only from the exact expected revision. The target must
    /// descend from the expected revision; restore therefore creates a new
    /// child instead of moving the pointer backwards. Already-at-target is an
    /// idempotent response-loss retry.
    pub(crate) async fn compare_and_swap_head(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        expected: &RevisionId,
        target: &RevisionId,
    ) -> Result<AuthoredHead> {
        let document = self.document(connection, document_id).await?;
        if document.archived_at.is_some() {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored document {document_id} is archived"
            )));
        }
        self.require_revision_in_document(connection, document_id, expected)
            .await?;
        self.require_revision_in_document(connection, document_id, target)
            .await?;
        if expected == target {
            let head = self.head(connection, document_id).await?;
            return if head.revision_id == *target {
                Ok(head)
            } else {
                Err(AuthoredStateError::HeadConflict {
                    document_id: document_id.to_string(),
                    expected: expected.to_string(),
                    actual: head.revision_id.to_string(),
                })
            };
        }
        if !self
            .is_ancestor(connection, document_id, expected, target)
            .await?
        {
            return Err(AuthoredStateError::InvalidInput(format!(
                "revision {target} does not descend from expected head {expected}"
            )));
        }
        let changed = sqlx::query(
            "UPDATE authored_document_heads
             SET revision_id = ?, generation = generation + 1
             WHERE document_id = ? AND revision_id = ?",
        )
        .bind(target.as_str())
        .bind(document_id.as_str())
        .bind(expected.as_str())
        .execute(&mut *connection)
        .await?
        .rows_affected();
        let head = self.head(connection, document_id).await?;
        if changed == 1 || head.revision_id == *target {
            Ok(head)
        } else {
            Err(AuthoredStateError::HeadConflict {
                document_id: document_id.to_string(),
                expected: expected.to_string(),
                actual: head.revision_id.to_string(),
            })
        }
    }

    /// Apply the server-authoritative result of the ordered proposal protocol.
    /// Unlike an ordinary authored mutation, a terminal server result may
    /// supersede an optimistic local proposal rather than descend from it.
    /// The pointer update remains a strict local CAS and the target must still
    /// be a closed revision of this exact document; only the ancestry check is
    /// intentionally omitted. The superseded tip stays immutable history.
    pub(crate) async fn compare_and_swap_integrated_head(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        expected: &RevisionId,
        target: &RevisionId,
    ) -> Result<AuthoredHead> {
        let document = self.document(connection, document_id).await?;
        if document.archived_at.is_some() {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored document {document_id} is archived"
            )));
        }
        self.require_revision_in_document(connection, document_id, expected)
            .await?;
        self.require_revision_in_document(connection, document_id, target)
            .await?;
        if expected == target {
            let head = self.head(connection, document_id).await?;
            return if head.revision_id == *target {
                Ok(head)
            } else {
                Err(AuthoredStateError::HeadConflict {
                    document_id: document_id.to_string(),
                    expected: expected.to_string(),
                    actual: head.revision_id.to_string(),
                })
            };
        }
        let changed = sqlx::query(
            "UPDATE authored_document_heads
             SET revision_id = ?, generation = generation + 1
             WHERE document_id = ? AND revision_id = ?",
        )
        .bind(target.as_str())
        .bind(document_id.as_str())
        .bind(expected.as_str())
        .execute(&mut *connection)
        .await?
        .rows_affected();
        let head = self.head(connection, document_id).await?;
        if changed == 1 || head.revision_id == *target {
            Ok(head)
        } else {
            Err(AuthoredStateError::HeadConflict {
                document_id: document_id.to_string(),
                expected: expected.to_string(),
                actual: head.revision_id.to_string(),
            })
        }
    }

    pub(crate) async fn revision_info(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        revision_id: &RevisionId,
    ) -> Result<RevisionInfo> {
        self.revision_info_optional(connection, document_id, revision_id)
            .await?
            .ok_or_else(|| {
                AuthoredStateError::NotFound(format!(
                    "revision {revision_id} does not exist in document {document_id}"
                ))
            })
    }

    pub(crate) async fn read_revision(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        revision_id: &RevisionId,
    ) -> Result<(RevisionInfo, FileMap)> {
        let info = self
            .revision_info(connection, document_id, revision_id)
            .await?;
        let rows = sqlx::query_as::<_, FileRow>(
            "SELECT principal_key, path, content_hash, content FROM authored_revision_files
             WHERE revision_id = ? ORDER BY path",
        )
        .bind(revision_id.as_str())
        .fetch_all(&mut *connection)
        .await?;
        let mut files = FileMap::new();
        for row in rows {
            if row.principal_key != info.principal_key {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {revision_id} file {:?} has the wrong principal binding",
                    row.path
                )));
            }
            validate_relative_path(&row.path).map_err(|error| {
                AuthoredStateError::Corrupt(format!(
                    "revision {revision_id} has an invalid stored path: {error}"
                ))
            })?;
            let actual_hash = file_content_hash(&row.content);
            if actual_hash != row.content_hash {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {revision_id} file {:?} content hash mismatch",
                    row.path
                )));
            }
            if files.insert(row.path.clone(), row.content).is_some() {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {revision_id} repeats path {:?}",
                    row.path
                )));
            }
        }
        let manifest = content_manifest(&files)?;
        if manifest.content_hash != info.content_hash {
            return Err(AuthoredStateError::Corrupt(format!(
                "revision {revision_id} manifest hash mismatch"
            )));
        }
        let expected_id = derive_revision_id(document_id, &info.parents, &manifest, &info.metadata);
        if expected_id != info.id {
            return Err(AuthoredStateError::Corrupt(format!(
                "revision {} does not match its deterministic id {expected_id}",
                info.id
            )));
        }
        Ok((info, files))
    }

    #[cfg(test)]
    pub(crate) async fn diff(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        old_revision: &RevisionId,
        new_revision: &RevisionId,
    ) -> Result<Vec<FileDiff>> {
        let (_, old_files) = self
            .read_revision(connection, document_id, old_revision)
            .await?;
        let (_, new_files) = self
            .read_revision(connection, document_id, new_revision)
            .await?;
        let paths: BTreeSet<&str> = old_files
            .keys()
            .chain(new_files.keys())
            .map(String::as_str)
            .collect();
        let mut changes = Vec::new();
        for path in paths {
            let old = old_files.get(path);
            let new = new_files.get(path);
            let kind = match (old, new) {
                (None, Some(_)) => FileChangeKind::Added,
                (Some(_), None) => FileChangeKind::Deleted,
                (Some(old), Some(new)) if old != new => FileChangeKind::Modified,
                _ => continue,
            };
            changes.push(FileDiff {
                path: path.to_owned(),
                kind,
                old_content_hash: old.map(|bytes| file_content_hash(bytes)),
                new_content_hash: new.map(|bytes| file_content_hash(bytes)),
            });
        }
        Ok(changes)
    }

    pub(crate) async fn is_ancestor(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        ancestor: &RevisionId,
        descendant: &RevisionId,
    ) -> Result<bool> {
        self.require_revision_in_document(connection, document_id, ancestor)
            .await?;
        self.require_revision_in_document(connection, document_id, descendant)
            .await?;
        let parents = self
            .load_reachable_parent_map(connection, document_id, &[descendant])
            .await?;
        require_node(&parents, descendant, document_id)?;
        Ok(ancestor_set(&parents, descendant).contains(ancestor))
    }

    /// Return every best common ancestor in deterministic revision-id order.
    /// A well-formed linear-child merge topology normally has one; criss-cross
    /// histories intentionally expose all best bases instead of silently
    /// choosing a semantically arbitrary one.
    pub(crate) async fn merge_bases(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        ours: &RevisionId,
        theirs: &RevisionId,
    ) -> Result<Vec<RevisionId>> {
        let parents = self
            .load_reachable_parent_map(connection, document_id, &[ours, theirs])
            .await?;
        require_node(&parents, ours, document_id)?;
        require_node(&parents, theirs, document_id)?;
        let ours_ancestors = ancestor_set(&parents, ours);
        let theirs_ancestors = ancestor_set(&parents, theirs);
        let common: BTreeSet<RevisionId> = ours_ancestors
            .intersection(&theirs_ancestors)
            .cloned()
            .collect();
        if common.is_empty() {
            return Err(AuthoredStateError::NotFound(format!(
                "revisions {ours} and {theirs} have no common ancestor"
            )));
        }
        let mut superseded = HashSet::new();
        for revision in &common {
            for parent in parents
                .get(revision)
                .expect("common revision came from parent map")
            {
                if common.contains(parent) {
                    superseded.insert(parent.clone());
                }
            }
        }
        Ok(common
            .into_iter()
            .filter(|candidate| !superseded.contains(candidate))
            .collect())
    }

    pub(crate) async fn merge_base(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        ours: &RevisionId,
        theirs: &RevisionId,
    ) -> Result<RevisionId> {
        let bases = self
            .merge_bases(connection, document_id, ours, theirs)
            .await?;
        match bases.as_slice() {
            [base] => Ok(base.clone()),
            _ => Err(AuthoredStateError::AmbiguousMergeBase {
                candidates: bases.into_iter().map(|base| base.to_string()).collect(),
            }),
        }
    }

    /// Page the current head's first-parent history. `cursor` is inclusive and
    /// accepted only when it remains on that exact mainline.
    #[cfg(test)]
    pub(crate) async fn first_parent_log_from(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        cursor: Option<&RevisionId>,
        limit: usize,
    ) -> Result<Vec<RevisionInfo>> {
        validate_history_limit(limit)?;
        let head = self.head(connection, document_id).await?;
        let parents = self
            .load_reachable_parent_map(connection, document_id, &[&head.revision_id])
            .await?;
        let start = cursor.unwrap_or(&head.revision_id);
        require_node(&parents, start, document_id)?;
        if !first_parent_contains_map(&parents, &head.revision_id, start) {
            return Err(AuthoredStateError::NotFound(
                "history cursor is not on the current first-parent mainline".into(),
            ));
        }
        let mut current = start.clone();
        let mut result = Vec::with_capacity(limit);
        while result.len() < limit {
            result.push(
                self.revision_info(connection, document_id, &current)
                    .await?,
            );
            let Some(parent) = parents
                .get(&current)
                .and_then(|revision_parents| revision_parents.first())
            else {
                break;
            };
            current = parent.clone();
        }
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) async fn first_parent_contains(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        target: &RevisionId,
    ) -> Result<bool> {
        let head = self.head(connection, document_id).await?;
        let parents = self
            .load_reachable_parent_map(connection, document_id, &[&head.revision_id])
            .await?;
        require_node(&parents, target, document_id)?;
        Ok(first_parent_contains_map(
            &parents,
            &head.revision_id,
            target,
        ))
    }

    async fn revision_info_optional(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        revision_id: &RevisionId,
    ) -> Result<Option<RevisionInfo>> {
        let row = sqlx::query_as::<_, RevisionRow>(
            "SELECT revision.revision_id, revision.document_id, revision.principal_key,
                    document.principal_key AS document_principal_key,
                    revision.parent_count,
                    revision.content_hash, revision.operation_kind,
                    revision.operation_id, revision.message, revision.actor,
                    revision.author_name,
                    revision.author_email, revision.authored_at, revision.thread_id,
                    revision.assistant_message_id, revision.restored_revision_id,
                    revision.created_at
             FROM authored_revisions revision
             JOIN authored_documents document ON document.document_id = revision.document_id
             WHERE revision.document_id = ? AND revision.revision_id = ?",
        )
        .bind(document_id.as_str())
        .bind(revision_id.as_str())
        .fetch_optional(&mut *connection)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let parents = self
            .load_revision_parents(connection, document_id, revision_id)
            .await?;
        row.into_info(parents).map(Some)
    }

    async fn load_revision_parents(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        revision_id: &RevisionId,
    ) -> Result<Vec<RevisionId>> {
        let rows = sqlx::query_as::<_, ParentRow>(
            "SELECT principal_key, parent_order, parent_revision_id
             FROM authored_revision_parents
             WHERE document_id = ? AND revision_id = ? ORDER BY parent_order",
        )
        .bind(document_id.as_str())
        .bind(revision_id.as_str())
        .fetch_all(&mut *connection)
        .await?;
        let principal_key: String = sqlx::query_scalar(
            "SELECT principal_key FROM authored_documents WHERE document_id = ?",
        )
        .bind(document_id.as_str())
        .fetch_optional(&mut *connection)
        .await?
        .ok_or_else(|| {
            AuthoredStateError::NotFound(format!("authored document {document_id} does not exist"))
        })?;
        decode_parent_rows(revision_id, &principal_key, rows)
    }

    async fn require_revision_in_document(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        revision_id: &RevisionId,
    ) -> Result<()> {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM authored_revisions WHERE document_id = ? AND revision_id = ?",
        )
        .bind(document_id.as_str())
        .bind(revision_id.as_str())
        .fetch_optional(&mut *connection)
        .await?;
        found.map(|_| ()).ok_or_else(|| {
            AuthoredStateError::NotFound(format!(
                "revision {revision_id} does not exist in document {document_id}"
            ))
        })
    }

    /// Load and validate only the ancestor closure reachable from the named
    /// revisions. Immutable rows may arrive before the proposal that makes
    /// them authoritative; an unrelated partial upload must not poison a
    /// valid head comparison or merge. Every row inside the queried closure
    /// still fails closed on missing parents, malformed ordering, principal
    /// mismatch, or cycles.
    async fn load_reachable_parent_map(
        &self,
        connection: &mut SqliteConnection,
        document_id: &AuthoredDocumentId,
        roots: &[&RevisionId],
    ) -> Result<HashMap<RevisionId, Vec<RevisionId>>> {
        if roots.is_empty() {
            return Err(AuthoredStateError::InvalidInput(
                "authored ancestry requires at least one descendant".into(),
            ));
        }
        let document = self.document(connection, document_id).await?;
        let roots_json = serde_json::to_string(
            &roots
                .iter()
                .map(|revision| revision.as_str())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| {
            AuthoredStateError::Corrupt(format!("encode authored ancestry roots: {error}"))
        })?;
        let revision_values: Vec<(String, String, i64)> = sqlx::query_as(
            "WITH RECURSIVE reachable(revision_id) AS (
                 SELECT CAST(value AS TEXT) FROM json_each(?)
                 UNION
                 SELECT edge.parent_revision_id
                 FROM authored_revision_parents edge
                 JOIN reachable ON reachable.revision_id = edge.revision_id
                 WHERE edge.document_id = ?
             )
             SELECT revision.revision_id, revision.principal_key, revision.parent_count
             FROM authored_revisions revision
             JOIN reachable ON reachable.revision_id = revision.revision_id
             WHERE revision.document_id = ?
             ORDER BY revision.revision_id",
        )
        .bind(&roots_json)
        .bind(document_id.as_str())
        .bind(document_id.as_str())
        .fetch_all(&mut *connection)
        .await?;
        let mut parents = HashMap::with_capacity(revision_values.len());
        let mut declared_parent_counts = HashMap::with_capacity(revision_values.len());
        for (value, principal_key, parent_count) in revision_values {
            if principal_key != document.spec.principal_key {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {value} has the wrong principal binding"
                )));
            }
            if !(0..=i64::try_from(MAX_PARENTS).expect("small bound")).contains(&parent_count) {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {value} has invalid declared parent count {parent_count}"
                )));
            }
            let revision = RevisionId::parse(value)?;
            declared_parent_counts.insert(
                revision.clone(),
                usize::try_from(parent_count).expect("validated non-negative count"),
            );
            parents.insert(revision, Vec::new());
        }
        let rows = sqlx::query_as::<_, FullParentRow>(
            "WITH RECURSIVE reachable(revision_id) AS (
                 SELECT CAST(value AS TEXT) FROM json_each(?)
                 UNION
                 SELECT edge.parent_revision_id
                 FROM authored_revision_parents edge
                 JOIN reachable ON reachable.revision_id = edge.revision_id
                 WHERE edge.document_id = ?
             )
             SELECT edge.principal_key, edge.revision_id, edge.parent_order,
                    edge.parent_revision_id
             FROM authored_revision_parents edge
             JOIN reachable ON reachable.revision_id = edge.revision_id
             WHERE edge.document_id = ?
             ORDER BY edge.revision_id, edge.parent_order",
        )
        .bind(&roots_json)
        .bind(document_id.as_str())
        .bind(document_id.as_str())
        .fetch_all(&mut *connection)
        .await?;
        for row in rows {
            if row.principal_key != document.spec.principal_key {
                return Err(AuthoredStateError::Corrupt(
                    "authored parent edge has the wrong principal binding".into(),
                ));
            }
            let revision = RevisionId::parse(row.revision_id)?;
            let parent = RevisionId::parse(row.parent_revision_id)?;
            if !parents.contains_key(&revision) {
                return Err(AuthoredStateError::Corrupt(format!(
                    "parent edge names missing revision {revision}"
                )));
            }
            if revision == parent || !parents.contains_key(&parent) {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {revision} has an invalid parent {parent}"
                )));
            }
            let target = parents
                .get_mut(&revision)
                .expect("revision membership was checked");
            let expected_order = i64::try_from(target.len()).expect("bounded parent count");
            if row.parent_order != expected_order || target.len() >= MAX_PARENTS {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {revision} has malformed parent ordering"
                )));
            }
            target.push(parent);
        }
        for (revision, declared_count) in declared_parent_counts {
            let actual_count = parents
                .get(&revision)
                .expect("declared revision came from parent map")
                .len();
            if actual_count != declared_count {
                return Err(AuthoredStateError::Corrupt(format!(
                    "revision {revision} declares {declared_count} parents but stores {actual_count}"
                )));
            }
        }
        validate_acyclic(&parents)?;
        Ok(parents)
    }
}

pub(crate) fn content_manifest(files: &FileMap) -> Result<AuthoredContentManifest> {
    validate_file_map(files)?;
    let mut manifest_hash = Sha256::new();
    manifest_hash.update(CONTENT_MANIFEST_DOMAIN);
    let mut byte_length = 0usize;
    let mut manifest_files = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        let content_hash = file_content_hash(bytes);
        hash_field(&mut manifest_hash, path.as_bytes());
        hash_field(&mut manifest_hash, bytes);
        byte_length += bytes.len();
        manifest_files.push(AuthoredFileManifest {
            path: path.clone(),
            content_hash,
            byte_length: bytes.len(),
        });
    }
    Ok(AuthoredContentManifest {
        content_hash: format!("sha256:{:x}", manifest_hash.finalize()),
        files: manifest_files,
        byte_length,
    })
}

fn derive_revision_id(
    document_id: &AuthoredDocumentId,
    parents: &[RevisionId],
    manifest: &AuthoredContentManifest,
    metadata: &RevisionMetadata,
) -> RevisionId {
    let mut hash = Sha256::new();
    hash.update(REVISION_ID_DOMAIN);
    hash_field(&mut hash, document_id.as_str().as_bytes());
    hash.update((parents.len() as u64).to_be_bytes());
    for parent in parents {
        hash_field(&mut hash, parent.as_str().as_bytes());
    }
    hash_field(&mut hash, manifest.content_hash.as_bytes());
    hash_field(&mut hash, metadata.operation_kind.as_bytes());
    hash_optional(&mut hash, metadata.operation_id.as_deref());
    hash_field(&mut hash, metadata.message.as_bytes());
    hash_field(&mut hash, metadata.author_name.as_bytes());
    hash_field(&mut hash, metadata.author_email.as_bytes());
    hash_field(&mut hash, metadata.authored_at.as_bytes());
    hash_optional(&mut hash, metadata.thread_id.as_deref());
    hash_optional(&mut hash, metadata.assistant_message_id.as_deref());
    hash_optional(
        &mut hash,
        metadata
            .restored_revision_id
            .as_ref()
            .map(RevisionId::as_str),
    );
    RevisionId(format!("rv-{:x}", hash.finalize()))
}

fn file_content_hash(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(FILE_CONTENT_DOMAIN);
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    format!("sha256:{:x}", hash.finalize())
}

fn hash_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

fn hash_optional(hash: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash.update([1]);
            hash_field(hash, value.as_bytes());
        }
        None => hash.update([0]),
    }
}

fn validate_identity_domain(domain: &str) -> Result<()> {
    if domain.is_empty()
        || !domain.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
    {
        return Err(AuthoredStateError::InvalidInput(format!(
            "invalid authored document identity domain {domain:?}"
        )));
    }
    Ok(())
}

fn validate_sha_id(value: &str, prefix: &str, name: &str) -> Result<()> {
    let digest = value.strip_prefix(prefix).ok_or_else(|| {
        AuthoredStateError::InvalidInput(format!("{name} must begin with {prefix:?}"))
    })?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(AuthoredStateError::InvalidInput(format!(
            "invalid {name} {value:?}"
        )));
    }
    Ok(())
}

fn validate_principal_key(value: &str) -> Result<()> {
    if value == "signed-out"
        || value
            .strip_prefix("signed-in:")
            .is_some_and(|uid| !uid.is_empty() && !uid.contains('\0'))
    {
        Ok(())
    } else {
        Err(AuthoredStateError::InvalidInput(format!(
            "invalid authored principal key {value:?}"
        )))
    }
}

fn validate_required(value: &str, name: &str) -> Result<()> {
    validate_optional_text(value, name, 4_096)?;
    if value.is_empty() {
        return Err(AuthoredStateError::InvalidInput(format!(
            "{name} cannot be empty"
        )));
    }
    Ok(())
}

fn validate_optional_text(value: &str, name: &str, max_bytes: usize) -> Result<()> {
    if value.len() > max_bytes || value.contains('\0') {
        return Err(AuthoredStateError::InvalidInput(format!(
            "{name} is too large or contains NUL"
        )));
    }
    Ok(())
}

/// A revision actor label. Wider than [`validate_token`] by `/`, which
/// separates an MCP client's name from its version.
fn validate_actor(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(AuthoredStateError::InvalidInput(format!(
            "invalid revision actor {value:?}"
        )));
    }
    Ok(())
}

fn validate_token(value: &str, name: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(AuthoredStateError::InvalidInput(format!(
            "invalid {name} {value:?}"
        )));
    }
    Ok(())
}

fn validate_rfc3339(value: &str, name: &str) -> Result<()> {
    DateTime::parse_from_rfc3339(value).map_err(|_| {
        AuthoredStateError::InvalidInput(format!("{name} must be an RFC 3339 timestamp"))
    })?;
    Ok(())
}

fn required_option<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str> {
    let value = value.as_deref().ok_or_else(|| {
        AuthoredStateError::InvalidInput(format!("authored document is missing {name}"))
    })?;
    validate_required(value, name)?;
    Ok(value)
}

fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.contains('\0')
        || path.contains('\\')
        || Path::new(path).is_absolute()
    {
        return Err(AuthoredStateError::InvalidInput(format!(
            "unsafe authored file path {path:?}"
        )));
    }
    let mut count = 0usize;
    for component in path.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with(['.', ' '])
            || component.contains(':')
        {
            return Err(AuthoredStateError::InvalidInput(format!(
                "unsafe authored file path {path:?}"
            )));
        }
        count += 1;
    }
    if count == 0 {
        return Err(AuthoredStateError::InvalidInput(format!(
            "unsafe authored file path {path:?}"
        )));
    }
    Ok(())
}

fn validate_file_map(files: &FileMap) -> Result<()> {
    if files.is_empty() {
        return Err(AuthoredStateError::InvalidInput(
            "an authored revision must contain at least one canonical file".into(),
        ));
    }
    if files.len() > MAX_FILES {
        return Err(AuthoredStateError::InvalidInput(format!(
            "an authored revision may contain at most {MAX_FILES} files"
        )));
    }
    let mut total = 0usize;
    for (path, bytes) in files {
        validate_relative_path(path)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored file {path:?} exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        total = total.checked_add(bytes.len()).ok_or_else(|| {
            AuthoredStateError::InvalidInput("authored file byte count overflow".into())
        })?;
        if total > MAX_TOTAL_BYTES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored revision exceeds {MAX_TOTAL_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
fn validate_history_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > MAX_HISTORY_PAGE {
        return Err(AuthoredStateError::InvalidInput(format!(
            "history limit must be between 1 and {MAX_HISTORY_PAGE}"
        )));
    }
    Ok(())
}

fn decode_parent_rows(
    revision: &RevisionId,
    principal_key: &str,
    rows: Vec<ParentRow>,
) -> Result<Vec<RevisionId>> {
    if rows.len() > MAX_PARENTS {
        return Err(AuthoredStateError::Corrupt(format!(
            "revision {revision} has too many parents"
        )));
    }
    let mut parents = Vec::with_capacity(rows.len());
    for row in rows {
        if row.principal_key != principal_key {
            return Err(AuthoredStateError::Corrupt(format!(
                "revision {revision} parent has the wrong principal binding"
            )));
        }
        if row.parent_order != i64::try_from(parents.len()).expect("at most two parents") {
            return Err(AuthoredStateError::Corrupt(format!(
                "revision {revision} has malformed parent ordering"
            )));
        }
        let parent = RevisionId::parse(row.parent_revision_id)?;
        if parent == *revision || parents.contains(&parent) {
            return Err(AuthoredStateError::Corrupt(format!(
                "revision {revision} has an invalid parent {parent}"
            )));
        }
        parents.push(parent);
    }
    Ok(parents)
}

fn validate_acyclic(parents: &HashMap<RevisionId, Vec<RevisionId>>) -> Result<()> {
    // 0 = unseen, 1 = visiting, 2 = complete.
    let mut state = HashMap::<RevisionId, u8>::new();
    for start in parents.keys() {
        if state.get(start) == Some(&2) {
            continue;
        }
        state.insert(start.clone(), 1);
        let mut stack = vec![(start.clone(), 0usize)];
        while let Some((node, next_parent)) = stack.last_mut() {
            let node_parents = parents.get(node).expect("stack nodes came from parent map");
            if *next_parent == node_parents.len() {
                state.insert(node.clone(), 2);
                stack.pop();
                continue;
            }
            let parent = node_parents[*next_parent].clone();
            *next_parent += 1;
            match state.get(&parent).copied().unwrap_or(0) {
                0 => {
                    state.insert(parent.clone(), 1);
                    stack.push((parent, 0));
                }
                1 => {
                    return Err(AuthoredStateError::Corrupt(format!(
                        "authored revision ancestry contains a cycle through {parent}"
                    )));
                }
                2 => {}
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

fn ancestor_set(
    parents: &HashMap<RevisionId, Vec<RevisionId>>,
    start: &RevisionId,
) -> HashSet<RevisionId> {
    let mut found = HashSet::new();
    let mut stack = vec![start.clone()];
    while let Some(revision) = stack.pop() {
        if !found.insert(revision.clone()) {
            continue;
        }
        stack.extend(
            parents
                .get(&revision)
                .expect("ancestry was validated")
                .iter()
                .cloned(),
        );
    }
    found
}

fn require_node(
    parents: &HashMap<RevisionId, Vec<RevisionId>>,
    revision: &RevisionId,
    document_id: &AuthoredDocumentId,
) -> Result<()> {
    if parents.contains_key(revision) {
        Ok(())
    } else {
        Err(AuthoredStateError::NotFound(format!(
            "revision {revision} does not exist in document {document_id}"
        )))
    }
}

#[cfg(test)]
fn first_parent_contains_map(
    parents: &HashMap<RevisionId, Vec<RevisionId>>,
    head: &RevisionId,
    target: &RevisionId,
) -> bool {
    let mut current = head;
    loop {
        if current == target {
            return true;
        }
        let Some(parent) = parents.get(current).and_then(|values| values.first()) else {
            return false;
        };
        current = parent;
    }
}

#[derive(FromRow)]
struct DocumentRow {
    document_id: String,
    document_kind: String,
    principal_key: String,
    subject_id: String,
    track_id: Option<String>,
    venue_id: Option<String>,
    score_id: Option<String>,
    implementation_id: Option<String>,
    archived_at: Option<String>,
    created_at: String,
}

impl TryFrom<DocumentRow> for AuthoredDocumentRecord {
    type Error = AuthoredStateError;

    fn try_from(row: DocumentRow) -> Result<Self> {
        let spec = NewAuthoredDocument {
            id: AuthoredDocumentId::parse(row.document_id)?,
            kind: AuthoredDocumentKind::parse(&row.document_kind)?,
            principal_key: row.principal_key,
            subject_id: row.subject_id,
            track_id: row.track_id,
            venue_id: row.venue_id,
            score_id: row.score_id,
            implementation_id: row.implementation_id,
        };
        spec.validate().map_err(|error| {
            AuthoredStateError::Corrupt(format!("stored authored document is invalid: {error}"))
        })?;
        Ok(Self {
            spec,
            archived_at: row.archived_at,
            created_at: row.created_at,
        })
    }
}

#[derive(FromRow)]
struct RevisionRow {
    revision_id: String,
    document_id: String,
    principal_key: String,
    document_principal_key: String,
    parent_count: i64,
    content_hash: String,
    operation_kind: String,
    operation_id: Option<String>,
    message: String,
    actor: String,
    author_name: String,
    author_email: String,
    authored_at: String,
    thread_id: Option<String>,
    assistant_message_id: Option<String>,
    restored_revision_id: Option<String>,
    created_at: String,
}

impl RevisionRow {
    fn into_info(self, parents: Vec<RevisionId>) -> Result<RevisionInfo> {
        if self.principal_key != self.document_principal_key {
            return Err(AuthoredStateError::Corrupt(format!(
                "revision {} has the wrong principal binding",
                self.revision_id
            )));
        }
        if self.parent_count != i64::try_from(parents.len()).expect("at most two parents") {
            return Err(AuthoredStateError::Corrupt(format!(
                "revision {} declares {} parents but stores {}",
                self.revision_id,
                self.parent_count,
                parents.len()
            )));
        }
        validate_content_hash(&self.content_hash)?;
        let metadata = RevisionMetadata {
            operation_kind: self.operation_kind,
            operation_id: self.operation_id,
            message: self.message,
            actor: Actor::parse(&self.actor)?,
            author_name: self.author_name,
            author_email: self.author_email,
            authored_at: self.authored_at,
            thread_id: self.thread_id,
            assistant_message_id: self.assistant_message_id,
            restored_revision_id: self
                .restored_revision_id
                .map(RevisionId::parse)
                .transpose()?,
        };
        metadata.validate().map_err(|error| {
            AuthoredStateError::Corrupt(format!("stored revision metadata is invalid: {error}"))
        })?;
        Ok(RevisionInfo {
            id: RevisionId::parse(self.revision_id)?,
            document_id: AuthoredDocumentId::parse(self.document_id)?,
            principal_key: self.principal_key,
            content_hash: self.content_hash,
            parents,
            metadata,
            created_at: self.created_at,
        })
    }
}

fn validate_content_hash(value: &str) -> Result<()> {
    let digest = value.strip_prefix("sha256:").ok_or_else(|| {
        AuthoredStateError::Corrupt(format!("invalid authored content hash {value:?}"))
    })?;
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(AuthoredStateError::Corrupt(format!(
            "invalid authored content hash {value:?}"
        )))
    }
}

#[derive(FromRow)]
struct FileRow {
    principal_key: String,
    path: String,
    content_hash: String,
    content: Vec<u8>,
}

#[derive(FromRow)]
struct ParentRow {
    principal_key: String,
    parent_order: i64,
    parent_revision_id: String,
}

#[derive(FromRow)]
struct FullParentRow {
    principal_key: String,
    revision_id: String,
    parent_order: i64,
    parent_revision_id: String,
}

#[derive(FromRow)]
struct HeadRow {
    document_id: String,
    principal_key: String,
    revision_id: String,
    generation: i64,
    updated_at: String,
}

impl TryFrom<HeadRow> for AuthoredHead {
    type Error = AuthoredStateError;

    fn try_from(row: HeadRow) -> Result<Self> {
        if row.generation < 0 {
            return Err(AuthoredStateError::Corrupt(
                "authored document head has a negative generation".into(),
            ));
        }
        Ok(Self {
            document_id: AuthoredDocumentId::parse(row.document_id)?,
            principal_key: row.principal_key,
            revision_id: RevisionId::parse(row.revision_id)?,
            generation: row.generation,
            updated_at: row.updated_at,
        })
    }
}

#[cfg(test)]
mod tests;
