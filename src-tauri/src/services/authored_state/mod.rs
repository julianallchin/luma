//! Git-backed authored-document history.
//!
//! Each bounded document owns one bare repository. Commits contain only
//! caller-supplied canonical files; linked worktrees are disposable projections
//! for isolated agents. This module deliberately knows nothing about tracks,
//! pattern graphs, chat turns, or Tauri. Those adapters can build on these
//! Git primitives without inventing a second history system.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use git2::Delta;
use git2::{
    ObjectType, Oid, Reference, Repository, RepositoryInitOptions, Signature, Sort, Time,
    WorktreeAddOptions, WorktreePruneOptions,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::storage::StorageRoot;

const REPOSITORY_ID_DOMAIN: &[u8] = b"luma.authored-repository.v1\0";
const FILE_SNAPSHOT_DOMAIN: &[u8] = b"luma.authored-file-snapshot.v1\0";
const FILE_CONTENT_DOMAIN: &[u8] = b"luma.authored-file-content.v1\0";
const MAIN_BRANCH: &str = "main";
const INITIAL_MESSAGE: &str = "Initialize authored state";
const MAX_FILES: usize = 4096;
const MAX_PATH_BYTES: usize = 1024;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_TREE_BYTES: usize = MAX_FILES * (MAX_PATH_BYTES + 64);
const MATERIALIZATION_TEMP_PREFIX: &str = ".luma-materialize-";

pub(crate) type FileMap = BTreeMap<String, Vec<u8>>;

pub(crate) fn file_snapshot_id(files: &FileMap) -> Result<String> {
    validate_file_map(files)?;
    let mut hasher = Sha256::new();
    hasher.update(FILE_SNAPSHOT_DOMAIN);
    for (path, bytes) in files {
        hash_field(&mut hasher, path.as_bytes());
        hash_field(&mut hasher, bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Compact, inspectable proof of the exact per-path source bytes consumed by
/// a worktree commit. A commit trailer persists this manifest so response-loss
/// recovery can distinguish a partially materialized canonical tree from
/// a genuinely newer edit without retaining a second authored tree.
pub(crate) fn worktree_source_manifest(files: &FileMap) -> Result<String> {
    validate_file_map(files)?;
    let hashes: BTreeMap<&str, String> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), file_content_id(bytes)))
        .collect();
    serde_json::to_string(&hashes).map_err(|error| {
        AuthoredStateError::InvalidInput(format!(
            "failed to encode worktree source manifest: {error}"
        ))
    })
}

fn file_content_id(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FILE_CONTENT_DOMAIN);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn decode_worktree_source_manifest(value: &str) -> Result<BTreeMap<String, String>> {
    let hashes: BTreeMap<String, String> = serde_json::from_str(value).map_err(|error| {
        AuthoredStateError::InvalidInput(format!("invalid worktree source manifest: {error}"))
    })?;
    if hashes.len() > MAX_FILES {
        return Err(AuthoredStateError::InvalidInput(format!(
            "worktree source manifest may contain at most {MAX_FILES} files"
        )));
    }
    for (path, hash) in &hashes {
        validate_relative_path(path)?;
        let digest = hash.strip_prefix("sha256:").ok_or_else(|| {
            AuthoredStateError::InvalidInput(
                "worktree source manifest contains an invalid content hash".into(),
            )
        })?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(AuthoredStateError::InvalidInput(
                "worktree source manifest contains an invalid content hash".into(),
            ));
        }
    }
    Ok(hashes)
}

#[derive(Debug)]
pub enum AuthoredStateError {
    InvalidInput(String),
    NotFound(String),
    HeadConflict {
        reference: String,
        expected: String,
        actual: String,
    },
    UnsafePath(String),
    Git(git2::Error),
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl AuthoredStateError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for AuthoredStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::NotFound(message) | Self::UnsafePath(message) => {
                f.write_str(message)
            }
            Self::HeadConflict {
                reference,
                expected,
                actual,
            } => write!(
                f,
                "authored ref {reference} moved (expected {expected}, actual {actual})"
            ),
            Self::Git(error) => write!(f, "Git operation failed: {error}"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for AuthoredStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Git(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<git2::Error> for AuthoredStateError {
    fn from(value: git2::Error) -> Self {
        Self::Git(value)
    }
}

pub(crate) type Result<T> = std::result::Result<T, AuthoredStateError>;

/// Stable identity inputs for one bounded authored document. The generic
/// domain/parts representation lets a future project aggregate derive IDs with
/// the same mechanism without changing repository storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthoredRepositoryDescriptor {
    domain: String,
    identity_parts: Vec<String>,
}

impl AuthoredRepositoryDescriptor {
    pub(crate) fn new(
        domain: impl Into<String>,
        identity_parts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let domain = domain.into();
        if domain.is_empty()
            || !domain.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
        {
            return Err(AuthoredStateError::InvalidInput(format!(
                "invalid authored repository domain '{domain}'"
            )));
        }
        let identity_parts: Vec<String> = identity_parts.into_iter().map(Into::into).collect();
        if identity_parts.is_empty() || identity_parts.iter().any(|part| part.is_empty()) {
            return Err(AuthoredStateError::InvalidInput(
                "authored repository identity parts cannot be empty".into(),
            ));
        }
        Ok(Self {
            domain,
            identity_parts,
        })
    }

    pub(crate) fn track_score(
        uid: &str,
        track_id: &str,
        venue_id: &str,
        score_id: &str,
    ) -> Result<Self> {
        Self::new(
            "track_score",
            [uid, track_id, venue_id, score_id].map(str::to_owned),
        )
    }

    pub(crate) fn pattern_graph(
        uid: &str,
        pattern_id: &str,
        implementation_id: &str,
    ) -> Result<Self> {
        Self::new(
            "pattern_graph",
            [uid, pattern_id, implementation_id].map(str::to_owned),
        )
    }
}

/// Filesystem-safe opaque repository identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct AuthoredRepositoryId(String);

impl AuthoredRepositoryId {
    pub(crate) fn derive(descriptor: &AuthoredRepositoryDescriptor) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(REPOSITORY_ID_DOMAIN);
        hash_field(&mut hasher, descriptor.domain.as_bytes());
        for part in &descriptor.identity_parts {
            hash_field(&mut hasher, part.as_bytes());
        }
        Self(format!("r-{:x}", hasher.finalize()))
    }

    #[cfg(test)]
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let digest = value.strip_prefix("r-").ok_or_else(|| {
            AuthoredStateError::InvalidInput("repository id must begin with 'r-'".into())
        })?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AuthoredStateError::InvalidInput(format!(
                "invalid authored repository id '{value}'"
            )));
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthoredRepositoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WorktreeId(String);

impl WorktreeId {
    pub(crate) fn new() -> Self {
        Self(format!("w-{}", Uuid::new_v4()))
    }

    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || value.starts_with('.')
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(AuthoredStateError::InvalidInput(format!(
                "invalid authored worktree id '{value}'"
            )));
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for WorktreeId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CommitId(String);

impl CommitId {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        Oid::from_str(&value).map_err(|_| {
            AuthoredStateError::InvalidInput(format!("invalid Git commit id '{value}'"))
        })?;
        Ok(Self(value))
    }

    fn from_oid(oid: Oid) -> Self {
        Self(oid.to_string())
    }

    fn oid(&self) -> Result<Oid> {
        Oid::from_str(&self.0).map_err(|_| {
            AuthoredStateError::InvalidInput(format!("invalid Git commit id '{}'", self.0))
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommitAuthor {
    pub name: String,
    pub email: String,
    pub time_seconds: i64,
    pub offset_minutes: i32,
}

impl CommitAuthor {
    pub(crate) fn new(
        name: impl Into<String>,
        email: impl Into<String>,
        time_seconds: i64,
        offset_minutes: i32,
    ) -> Result<Self> {
        let author = Self {
            name: name.into(),
            email: email.into(),
            time_seconds,
            offset_minutes,
        };
        author.signature()?;
        Ok(author)
    }

    fn signature(&self) -> Result<Signature<'static>> {
        if !(-720..=840).contains(&self.offset_minutes) {
            return Err(AuthoredStateError::InvalidInput(format!(
                "invalid commit timezone offset {}",
                self.offset_minutes
            )));
        }
        Signature::new(
            &self.name,
            &self.email,
            &Time::new(self.time_seconds, self.offset_minutes),
        )
        .map_err(AuthoredStateError::Git)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepositoryInfo {
    pub id: AuthoredRepositoryId,
    pub path: PathBuf,
    pub main_head: CommitId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommitInfo {
    pub id: CommitId,
    pub tree_id: String,
    /// Git parent order is semantically significant for merges.
    pub parents: Vec<CommitId>,
    pub author: CommitAuthor,
    pub message: String,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    TypeChanged,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: FileChangeKind,
    pub old_object_id: Option<String>,
    pub new_object_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorktreeInfo {
    pub id: WorktreeId,
    pub path: PathBuf,
    pub branch: String,
    pub head: CommitId,
    pub locked: bool,
}

/// Process-local serialization complements libgit2's cross-handle ref locks:
/// all mutations of one repository run in order, while unrelated repositories
/// can proceed independently.
#[derive(Clone)]
pub(crate) struct AuthoredStateStore {
    storage: StorageRoot,
    repository_locks: Arc<Mutex<HashMap<AuthoredRepositoryId, Arc<Mutex<()>>>>>,
}

impl AuthoredStateStore {
    pub(crate) fn new(storage: StorageRoot) -> Self {
        Self {
            storage,
            repository_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    fn storage(&self) -> &StorageRoot {
        &self.storage
    }

    pub(crate) fn ensure_repository(
        &self,
        repository_id: &AuthoredRepositoryId,
    ) -> Result<RepositoryInfo> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(|_| {
            AuthoredStateError::InvalidInput("authored repository lock was poisoned".into())
        })?;
        self.ensure_repository_locked(repository_id)
    }

    pub(crate) fn main_head(&self, repository_id: &AuthoredRepositoryId) -> Result<CommitId> {
        let repo = self.open_repository(repository_id)?;
        ref_target(&repo, &branch_ref(MAIN_BRANCH)?)
    }

    pub(crate) fn branch_head(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
    ) -> Result<CommitId> {
        let repo = self.open_repository(repository_id)?;
        ref_target(&repo, &branch_ref(branch)?)
    }

    pub(crate) fn create_branch(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        at: &CommitId,
    ) -> Result<CommitId> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let refname = branch_ref(branch)?;
        let target = at.oid()?;
        repo.find_commit(target)
            .map_err(|_| AuthoredStateError::NotFound(format!("commit {at} does not exist")))?;

        match repo.refname_to_id(&refname) {
            Ok(existing) if existing == target => return Ok(at.clone()),
            Ok(existing) => {
                return Err(AuthoredStateError::HeadConflict {
                    reference: refname,
                    expected: at.to_string(),
                    actual: existing.to_string(),
                });
            }
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let signature = system_signature()?;
        let mut transaction = repo.transaction()?;
        transaction.lock_ref(&refname)?;
        // Re-check after acquiring libgit2's cross-handle reference lock.
        match repo.refname_to_id(&refname) {
            Ok(existing) if existing == target => return Ok(at.clone()),
            Ok(existing) => {
                return Err(AuthoredStateError::HeadConflict {
                    reference: refname,
                    expected: at.to_string(),
                    actual: existing.to_string(),
                });
            }
            Err(error) if error.code() == git2::ErrorCode::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        transaction.set_target(
            &refname,
            target,
            Some(&signature),
            &format!("branch: create {branch}"),
        )?;
        transaction.commit()?;
        Ok(at.clone())
    }

    /// Delete an unneeded branch only if it still points at the caller's exact
    /// expected commit. Used solely to compensate a failed durable thread
    /// creation before any turn can have advanced the branch.
    pub(crate) fn delete_branch_at(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        expected_head: &CommitId,
    ) -> Result<()> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let refname = branch_ref(branch)?;
        let mut transaction = repo.transaction()?;
        transaction.lock_ref(&refname)?;
        let actual = repo.refname_to_id(&refname).map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                AuthoredStateError::NotFound(format!("branch '{branch}' does not exist"))
            } else {
                error.into()
            }
        })?;
        if actual != expected_head.oid()? {
            return Err(AuthoredStateError::HeadConflict {
                reference: refname,
                expected: expected_head.to_string(),
                actual: actual.to_string(),
            });
        }
        transaction.remove(&refname)?;
        transaction.commit()?;
        Ok(())
    }

    /// Write a complete commit object without updating any branch. Authored
    /// histories have one or two ordered parents: ordinary commits name their
    /// predecessor, while merge commits name ours first and theirs second.
    /// Repeating the same preparation is deterministic and returns the same ID.
    pub(crate) fn prepare_commit(
        &self,
        repository_id: &AuthoredRepositoryId,
        parents: &[CommitId],
        files: &FileMap,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<CommitInfo> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        self.prepare_commit_locked(&repo, parents, files, author, message)
    }

    pub(crate) fn commit_files(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        expected_head: &CommitId,
        files: &FileMap,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<CommitInfo> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        self.commit_files_locked(&repo, branch, expected_head, files, author, message, &[])
    }

    #[cfg(test)]
    fn log(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        validate_log_limit(limit)?;
        let repo = self.open_repository(repository_id)?;
        let mut walk = repo.revwalk()?;
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        walk.push_ref(&branch_ref(branch)?)?;
        walk.take(limit)
            .map(|oid| {
                let oid = oid?;
                let commit = repo.find_commit(oid)?;
                commit_info(&commit)
            })
            .collect()
    }

    /// Walk only the first-parent chain from a branch tip. Unlike a revwalk,
    /// this preserves the deterministic mainline chosen by merge parent order.
    #[cfg(test)]
    pub(crate) fn first_parent_log(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        self.first_parent_log_from(repository_id, branch, None, limit)
    }

    /// Page a branch's first-parent history beginning at `cursor`, inclusive.
    /// A cursor is accepted only when it is on the current first-parent chain;
    /// child-worktree and abandoned prepared commits are not history cursors.
    pub(crate) fn first_parent_log_from(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        cursor: Option<&CommitId>,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        validate_log_limit(limit)?;
        let repo = self.open_repository(repository_id)?;
        let refname = branch_ref(branch)?;
        let mut current = ref_target(&repo, &refname)?.oid()?;
        if let Some(cursor) = cursor {
            let target = cursor.oid()?;
            loop {
                if current == target {
                    break;
                }
                let commit = repo.find_commit(current).map_err(|_| {
                    AuthoredStateError::NotFound(format!(
                        "Git ref '{refname}' does not target a commit"
                    ))
                })?;
                if commit.parent_count() == 0 {
                    return Err(AuthoredStateError::NotFound(
                        "history cursor is not on the current mainline".into(),
                    ));
                }
                current = commit.parent_id(0)?;
            }
        }
        let mut commits = Vec::with_capacity(limit);

        while commits.len() < limit {
            let commit = repo.find_commit(current).map_err(|_| {
                AuthoredStateError::NotFound(format!(
                    "Git ref '{refname}' does not target a commit"
                ))
            })?;
            let first_parent = (commit.parent_count() > 0)
                .then(|| commit.parent_id(0))
                .transpose()?;
            commits.push(commit_info(&commit)?);
            match first_parent {
                Some(parent) => current = parent,
                None => break,
            }
        }

        Ok(commits)
    }

    /// Test whether `target` occurs on the branch's first-parent chain. This
    /// intentionally has no presentation limit: ancestry is a correctness
    /// invariant, so an old commit cannot become ineligible merely because a
    /// history view is paginated.
    pub(crate) fn first_parent_contains(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        target: &CommitId,
    ) -> Result<bool> {
        let repo = self.open_repository(repository_id)?;
        let refname = branch_ref(branch)?;
        let mut current = ref_target(&repo, &refname)?.oid()?;
        let target = target.oid()?;

        loop {
            if current == target {
                return Ok(true);
            }
            let commit = repo.find_commit(current).map_err(|_| {
                AuthoredStateError::NotFound(format!(
                    "Git ref '{refname}' does not target a commit"
                ))
            })?;
            if commit.parent_count() == 0 {
                return Ok(false);
            }
            current = commit.parent_id(0)?;
        }
    }

    /// Test ordinary Git ancestry without imposing a log window. Merge retry
    /// detection uses this to recognize a source head already reachable from
    /// main even after later commits have been added.
    pub(crate) fn is_ancestor(
        &self,
        repository_id: &AuthoredRepositoryId,
        ancestor: &CommitId,
        descendant: &CommitId,
    ) -> Result<bool> {
        let repo = self.open_repository(repository_id)?;
        let ancestor = ancestor.oid()?;
        let descendant = descendant.oid()?;
        find_commit(&repo, &CommitId::from_oid(ancestor))?;
        find_commit(&repo, &CommitId::from_oid(descendant))?;
        Ok(ancestor == descendant || repo.graph_descendant_of(descendant, ancestor)?)
    }

    /// Walk every commit reachable from `branch` until `visitor` returns a
    /// value. Unlike [`Self::log`], this is for internal identity/recovery
    /// lookups and therefore has no display-oriented cap.
    pub(crate) fn find_reachable_commit<T>(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        mut visitor: impl FnMut(&CommitInfo) -> Option<T>,
    ) -> Result<Option<T>> {
        let repo = self.open_repository(repository_id)?;
        let mut walk = repo.revwalk()?;
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        walk.push_ref(&branch_ref(branch)?)?;
        for oid in walk {
            let commit = repo.find_commit(oid?)?;
            let info = commit_info(&commit)?;
            if let Some(result) = visitor(&info) {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }

    /// Atomically advance an existing branch to a descendant commit. The ref
    /// update is a compare-and-swap after acquiring Git's reference lock. An
    /// already-at-target result is accepted as an idempotent crash retry.
    pub(crate) fn advance_branch(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        expected_head: &CommitId,
        target: &CommitId,
    ) -> Result<CommitId> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let refname = branch_ref(branch)?;
        let expected = expected_head.oid()?;
        let target = target.oid()?;
        repo.find_commit(expected).map_err(|_| {
            AuthoredStateError::NotFound(format!("commit {expected_head} does not exist"))
        })?;
        repo.find_commit(target).map_err(|_| {
            AuthoredStateError::NotFound(format!("commit {} does not exist", target))
        })?;
        if target != expected && !repo.graph_descendant_of(target, expected)? {
            return Err(AuthoredStateError::InvalidInput(format!(
                "commit {target} is not a descendant of {expected_head}"
            )));
        }

        let signature = system_signature()?;
        let mut transaction = repo.transaction()?;
        transaction.lock_ref(&refname)?;
        let actual = repo.refname_to_id(&refname).map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                AuthoredStateError::NotFound(format!("branch '{branch}' does not exist"))
            } else {
                error.into()
            }
        })?;
        if actual == target {
            return Ok(CommitId::from_oid(target));
        }
        if actual != expected {
            return Err(AuthoredStateError::HeadConflict {
                reference: refname,
                expected: expected_head.to_string(),
                actual: actual.to_string(),
            });
        }
        transaction.set_target(
            &refname,
            target,
            Some(&signature),
            &format!("branch: fast-forward {branch}"),
        )?;
        transaction.commit()?;
        Ok(CommitId::from_oid(target))
    }

    pub(crate) fn read_commit(
        &self,
        repository_id: &AuthoredRepositoryId,
        commit_id: &CommitId,
    ) -> Result<(CommitInfo, FileMap)> {
        let repo = self.open_repository(repository_id)?;
        let commit = find_commit(&repo, commit_id)?;
        let info = commit_info(&commit)?;
        let files = read_tree_files(&repo, commit.tree_id())?;
        Ok((info, files))
    }

    #[cfg(test)]
    fn read_commit_file(
        &self,
        repository_id: &AuthoredRepositoryId,
        commit_id: &CommitId,
        path: &str,
    ) -> Result<Vec<u8>> {
        validate_relative_path(path)?;
        let repo = self.open_repository(repository_id)?;
        let commit = find_commit(&repo, commit_id)?;
        let tree = commit.tree()?;
        let entry = tree.get_path(Path::new(path)).map_err(|_| {
            AuthoredStateError::NotFound(format!("file '{path}' does not exist in {commit_id}"))
        })?;
        if entry.kind() != Some(ObjectType::Blob) || entry.filemode() == 0o120000 {
            return Err(AuthoredStateError::UnsafePath(format!(
                "'{path}' is not a regular tracked file"
            )));
        }
        let contents = repo.find_blob(entry.id())?.content().to_vec();
        Ok(contents)
    }

    #[cfg(test)]
    fn diff(
        &self,
        repository_id: &AuthoredRepositoryId,
        old_commit: &CommitId,
        new_commit: &CommitId,
    ) -> Result<Vec<FileDiff>> {
        let repo = self.open_repository(repository_id)?;
        let old = find_commit(&repo, old_commit)?;
        let new = find_commit(&repo, new_commit)?;
        let old_tree = old.tree()?;
        let new_tree = new.tree()?;
        let diff = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)?;
        let mut changes = Vec::new();
        for delta in diff.deltas() {
            let kind = match delta.status() {
                Delta::Added => FileChangeKind::Added,
                Delta::Deleted => FileChangeKind::Deleted,
                Delta::Modified => FileChangeKind::Modified,
                Delta::Renamed => FileChangeKind::Renamed,
                Delta::Copied => FileChangeKind::Copied,
                Delta::Typechange => FileChangeKind::TypeChanged,
                Delta::Unmodified => continue,
                other => {
                    return Err(AuthoredStateError::InvalidInput(format!(
                        "unsupported Git diff status {other:?}"
                    )));
                }
            };
            let old_file = delta.old_file();
            let new_file = delta.new_file();
            let path = new_file
                .path()
                .or_else(|| old_file.path())
                .and_then(Path::to_str)
                .ok_or_else(|| {
                    AuthoredStateError::UnsafePath("Git diff contains a non-UTF-8 path".into())
                })?
                .replace('\\', "/");
            changes.push(FileDiff {
                path,
                old_path: old_file
                    .path()
                    .and_then(Path::to_str)
                    .map(|path| path.replace('\\', "/")),
                kind,
                old_object_id: old_file.is_valid_id().then(|| old_file.id().to_string()),
                new_object_id: new_file.is_valid_id().then(|| new_file.id().to_string()),
            });
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(changes)
    }

    /// Return all best merge bases in deterministic object-id order.
    fn merge_bases(
        &self,
        repository_id: &AuthoredRepositoryId,
        ours: &CommitId,
        theirs: &CommitId,
    ) -> Result<Vec<CommitId>> {
        let repo = self.open_repository(repository_id)?;
        find_commit(&repo, ours)?;
        find_commit(&repo, theirs)?;
        let bases = repo.merge_bases(ours.oid()?, theirs.oid()?)?;
        let mut bases: Vec<CommitId> = bases.iter().copied().map(CommitId::from_oid).collect();
        bases.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(bases)
    }

    /// Resolve the one merge base admitted by the authored-state topology.
    /// Public mutations merge linear child branches into `main` and therefore
    /// cannot create criss-cross ancestry. Silently choosing one of multiple
    /// best bases would change typed merge meaning, so externally-mutated or
    /// corrupt ancestry fails closed instead.
    pub(crate) fn merge_base(
        &self,
        repository_id: &AuthoredRepositoryId,
        ours: &CommitId,
        theirs: &CommitId,
    ) -> Result<CommitId> {
        let bases = self.merge_bases(repository_id, ours, theirs)?;
        match bases.as_slice() {
            [base] => Ok(base.clone()),
            [] => Err(AuthoredStateError::NotFound(format!(
                "commits {ours} and {theirs} have no common ancestor"
            ))),
            _ => Err(AuthoredStateError::InvalidInput(format!(
                "commits {ours} and {theirs} have multiple best merge bases; authored-state branches violate the single-base topology"
            ))),
        }
    }

    /// Create a merge commit from a domain resolver's complete, already-
    /// validated file map. Parent order is always target/ours first, source/
    /// theirs second.
    #[cfg(test)]
    fn create_merge_commit(
        &self,
        repository_id: &AuthoredRepositoryId,
        target_branch: &str,
        expected_target_head: &CommitId,
        source_commit: &CommitId,
        merged_files: &FileMap,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<CommitInfo> {
        if source_commit == expected_target_head {
            return Err(AuthoredStateError::InvalidInput(
                "a merge commit requires two distinct parents".into(),
            ));
        }
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        find_commit(&repo, source_commit)?;
        self.commit_files_locked(
            &repo,
            target_branch,
            expected_target_head,
            merged_files,
            author,
            message,
            std::slice::from_ref(source_commit),
        )
    }

    /// Restore a historical tree as a new child of the current branch head.
    /// History is never rewritten and the target commit remains untouched.
    #[cfg(test)]
    fn restore_as_commit(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        expected_head: &CommitId,
        target_commit: &CommitId,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<CommitInfo> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let target = find_commit(&repo, target_commit)?;
        let tree = target.tree()?;
        self.commit_tree_locked(
            &repo,
            branch,
            expected_head,
            tree.id(),
            author,
            message,
            &[],
        )
    }

    pub(crate) fn create_worktree(
        &self,
        repository_id: &AuthoredRepositoryId,
        branch: &str,
        worktree_id: &WorktreeId,
    ) -> Result<WorktreeInfo> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let refname = branch_ref(branch)?;
        let reference = repo.find_reference(&refname).map_err(|_| {
            AuthoredStateError::NotFound(format!("branch '{branch}' does not exist"))
        })?;
        let expected_path = self
            .storage
            .authored_worktree_dir(repository_id.as_str(), worktree_id.as_str());
        let parent = self
            .storage
            .authored_repository_worktrees_dir(repository_id.as_str());
        ensure_bounded_directory_tree(self.storage.path(), &parent)?;

        if let Ok(existing) = repo.find_worktree(worktree_id.as_str()) {
            validate_registered_worktree_path(
                self.storage.path(),
                existing.path(),
                &expected_path,
                worktree_id,
            )?;
            let projection_exists =
                validate_bounded_directory_tree(self.storage.path(), &expected_path)?;
            if projection_exists {
                if let Ok(info) = worktree_info(&repo, &existing) {
                    if info.branch == branch {
                        return Ok(info);
                    }
                    return Err(AuthoredStateError::UnsafePath(format!(
                        "worktree '{}' is already registered at {} for branch '{}'",
                        worktree_id.as_str(),
                        info.path.display(),
                        info.branch
                    )));
                }
            }
            validate_registered_worktree_head(&repo, worktree_id, branch)?;
            if projection_exists {
                let expected_files = files_for_reference(&repo, &reference)?;
                validate_recoverable_worktree_projection(&expected_path, &expected_files)?;
            }
            let mut options = WorktreePruneOptions::new();
            options.working_tree(false);
            existing.prune(Some(&mut options))?;
        }

        if validate_bounded_directory_tree(self.storage.path(), &expected_path)? {
            let expected_files = files_for_reference(&repo, &reference)?;
            remove_recoverable_worktree_projection(&expected_path, &expected_files)?;
        }

        let mut options = WorktreeAddOptions::new();
        options.reference(Some(&reference));
        let worktree = repo.worktree(worktree_id.as_str(), &expected_path, Some(&options))?;
        if !validate_bounded_directory_tree(self.storage.path(), &expected_path)? {
            return Err(AuthoredStateError::NotFound(format!(
                "libgit2 did not create worktree projection {}",
                expected_path.display()
            )));
        }
        let info = validate_worktree_binding(
            &repo,
            &worktree,
            self.storage.path(),
            &expected_path,
            branch,
        )?;
        Ok(info)
    }

    #[cfg(test)]
    fn list_worktrees(&self, repository_id: &AuthoredRepositoryId) -> Result<Vec<WorktreeInfo>> {
        let repo = self.open_repository(repository_id)?;
        let mut result = Vec::new();
        let names = repo.worktrees()?;
        for name in &names {
            let name = name?.ok_or_else(|| {
                AuthoredStateError::InvalidInput("worktree name is not UTF-8".into())
            })?;
            let id = WorktreeId::parse(name)?;
            let worktree = repo.find_worktree(name)?;
            let expected = self
                .storage
                .authored_worktree_dir(repository_id.as_str(), id.as_str());
            if !validate_bounded_directory_tree(self.storage.path(), &expected)? {
                return Err(AuthoredStateError::NotFound(format!(
                    "worktree projection '{}' does not exist",
                    id.as_str()
                )));
            }
            let info = worktree_info(&repo, &worktree)?;
            validate_registered_worktree_path(self.storage.path(), &info.path, &expected, &id)?;
            result.push(info);
        }
        result.sort_by(|left, right| left.id.0.cmp(&right.id.0));
        Ok(result)
    }

    pub(crate) fn remove_worktree(
        &self,
        repository_id: &AuthoredRepositoryId,
        worktree_id: &WorktreeId,
        expected_branch: &str,
        force_locked: bool,
    ) -> Result<()> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let expected = self
            .storage
            .authored_worktree_dir(repository_id.as_str(), worktree_id.as_str());
        let projection_exists = validate_bounded_directory_tree(self.storage.path(), &expected)?;
        let worktree = match repo.find_worktree(worktree_id.as_str()) {
            Ok(worktree) => worktree,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                if projection_exists {
                    remove_unregistered_worktree_projection(&repo, expected_branch, &expected)?;
                    return Ok(());
                }
                return Err(AuthoredStateError::NotFound(format!(
                    "worktree '{}' does not exist",
                    worktree_id.as_str()
                )));
            }
            Err(error) => return Err(error.into()),
        };
        if !projection_exists {
            validate_registered_worktree_path(
                self.storage.path(),
                worktree.path(),
                &expected,
                worktree_id,
            )?;
            validate_registered_worktree_head(&repo, worktree_id, expected_branch)?;
            let mut options = WorktreePruneOptions::new();
            options.working_tree(false).locked(force_locked);
            worktree.prune(Some(&mut options))?;
            return Ok(());
        }
        let info = validate_worktree_binding(
            &repo,
            &worktree,
            self.storage.path(),
            &expected,
            expected_branch,
        )?;
        let working_files = read_regular_files(&expected)?;
        let branch_head = find_commit(&repo, &info.head)?;
        let committed_files = read_tree_files(&repo, branch_head.tree_id())?;
        if working_files != committed_files {
            return Err(AuthoredStateError::InvalidInput(format!(
                "refusing to remove worktree '{}': uncommitted or untracked files are present",
                worktree_id.as_str()
            )));
        }
        if read_regular_files(&expected)? != working_files {
            return Err(AuthoredStateError::InvalidInput(
                "worktree files changed while removal was verifying them".into(),
            ));
        }
        let mut options = WorktreePruneOptions::new();
        options.valid(true).working_tree(true).locked(force_locked);
        worktree.prune(Some(&mut options))?;
        Ok(())
    }

    /// Preserve a deleting thread's exact bounded worktree files on its
    /// isolated branch before pruning the disposable checkout. This is the
    /// deletion-only counterpart to `remove_worktree`: ordinary removal still
    /// refuses dirty data, while terminal thread cleanup never discards it.
    pub(crate) fn archive_and_remove_worktree(
        &self,
        repository_id: &AuthoredRepositoryId,
        worktree_id: &WorktreeId,
        expected_branch: &str,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<Option<CommitInfo>> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let expected_path = self
            .storage
            .authored_worktree_dir(repository_id.as_str(), worktree_id.as_str());
        let projection_exists =
            validate_bounded_directory_tree(self.storage.path(), &expected_path)?;
        let worktree = match repo.find_worktree(worktree_id.as_str()) {
            Ok(worktree) => worktree,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                if projection_exists {
                    remove_unregistered_worktree_projection(
                        &repo,
                        expected_branch,
                        &expected_path,
                    )?;
                    return Ok(None);
                }
                return Err(AuthoredStateError::NotFound(format!(
                    "worktree '{}' does not exist",
                    worktree_id.as_str()
                )));
            }
            Err(error) => return Err(error.into()),
        };
        if !projection_exists {
            validate_registered_worktree_path(
                self.storage.path(),
                worktree.path(),
                &expected_path,
                worktree_id,
            )?;
            validate_registered_worktree_head(&repo, worktree_id, expected_branch)?;
            let mut options = WorktreePruneOptions::new();
            options.working_tree(false);
            worktree.prune(Some(&mut options))?;
            return Ok(None);
        }
        let info = validate_worktree_binding(
            &repo,
            &worktree,
            self.storage.path(),
            &expected_path,
            expected_branch,
        )?;
        let files = read_regular_files(&expected_path)?;
        let branch_commit = find_commit(&repo, &info.head)?;
        let committed_files = read_tree_files(&repo, branch_commit.tree_id())?;
        let archived = if files == committed_files {
            None
        } else {
            Some(self.commit_files_locked(
                &repo,
                expected_branch,
                &info.head,
                &files,
                author,
                message,
                &[],
            )?)
        };
        if read_regular_files(&expected_path)? != files {
            return Err(AuthoredStateError::InvalidInput(
                "worktree files changed while deletion was archiving them".into(),
            ));
        }
        let mut options = WorktreePruneOptions::new();
        options.valid(true).working_tree(true);
        worktree.prune(Some(&mut options))?;
        Ok(archived)
    }

    /// Read a linked worktree as a canonical map of regular files. The Git
    /// control file and all symlinks are excluded; nested `.git` entries are an
    /// error rather than silently ignored.
    pub(crate) fn read_worktree_files(
        &self,
        repository_id: &AuthoredRepositoryId,
        worktree_id: &WorktreeId,
        expected_branch: &str,
    ) -> Result<FileMap> {
        let repo = self.open_repository(repository_id)?;
        let expected = self
            .storage
            .authored_worktree_dir(repository_id.as_str(), worktree_id.as_str());
        if !validate_bounded_directory_tree(self.storage.path(), &expected)? {
            return Err(AuthoredStateError::NotFound(format!(
                "worktree projection '{}' does not exist",
                worktree_id.as_str()
            )));
        }
        let worktree = repo.find_worktree(worktree_id.as_str()).map_err(|_| {
            AuthoredStateError::NotFound(format!(
                "worktree '{}' does not exist",
                worktree_id.as_str()
            ))
        })?;
        validate_worktree_binding(
            &repo,
            &worktree,
            self.storage.path(),
            &expected,
            expected_branch,
        )?;
        read_regular_files(&expected)
    }

    /// Commit a complete, caller-validated file map to a linked worktree's
    /// branch, then materialize that exact canonical tree back into the
    /// checkout. Git is the single source of truth: a successful commit never
    /// leaves a second, byte-different representation that is also considered
    /// clean.
    pub(crate) fn commit_worktree_files(
        &self,
        repository_id: &AuthoredRepositoryId,
        worktree_id: &WorktreeId,
        expected_branch: &str,
        expected_head: &CommitId,
        expected_working_files: &FileMap,
        files: &FileMap,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<CommitInfo> {
        validate_file_map(files)?;
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let expected_path = self
            .storage
            .authored_worktree_dir(repository_id.as_str(), worktree_id.as_str());
        if !validate_bounded_directory_tree(self.storage.path(), &expected_path)? {
            return Err(AuthoredStateError::NotFound(format!(
                "worktree projection '{}' does not exist",
                worktree_id.as_str()
            )));
        }
        let worktree = repo.find_worktree(worktree_id.as_str()).map_err(|_| {
            AuthoredStateError::NotFound(format!(
                "worktree '{}' does not exist",
                worktree_id.as_str()
            ))
        })?;
        validate_worktree_binding(
            &repo,
            &worktree,
            self.storage.path(),
            &expected_path,
            expected_branch,
        )?;
        let actual_working_files = read_regular_files(&expected_path)?;
        if actual_working_files != *expected_working_files {
            return Err(AuthoredStateError::InvalidInput(
                "worktree files changed while the commit was being prepared".into(),
            ));
        }
        let committed = self.commit_files_locked(
            &repo,
            expected_branch,
            expected_head,
            files,
            author,
            message,
            &[],
        )?;

        self.materialize_canonical_worktree_locked(
            &repo,
            &expected_path,
            &committed.id,
            expected_working_files,
            files,
        )?;

        Ok(committed)
    }

    /// Finish materializing a canonical worktree commit after a response-loss
    /// retry. The source manifest proves each path is still either the exact
    /// consumed source or the exact canonical result, which also recovers an
    /// interrupted multi-file materialization. Any third state is a later edit and is
    /// preserved.
    pub(crate) fn recover_canonical_worktree_materialization(
        &self,
        repository_id: &AuthoredRepositoryId,
        worktree_id: &WorktreeId,
        expected_branch: &str,
        expected_head: &CommitId,
        source_manifest: &str,
    ) -> Result<bool> {
        let lock = self.repository_lock(repository_id)?;
        let _guard = lock.lock().map_err(lock_poisoned)?;
        let repo = self.open_repository(repository_id)?;
        let expected_path = self
            .storage
            .authored_worktree_dir(repository_id.as_str(), worktree_id.as_str());
        if !validate_bounded_directory_tree(self.storage.path(), &expected_path)? {
            return Err(AuthoredStateError::NotFound(format!(
                "worktree projection '{}' does not exist",
                worktree_id.as_str()
            )));
        }
        let worktree = repo.find_worktree(worktree_id.as_str()).map_err(|_| {
            AuthoredStateError::NotFound(format!(
                "worktree '{}' does not exist",
                worktree_id.as_str()
            ))
        })?;
        let info = validate_worktree_binding(
            &repo,
            &worktree,
            self.storage.path(),
            &expected_path,
            expected_branch,
        )?;
        if &info.head != expected_head {
            return Ok(false);
        }
        let commit = find_commit(&repo, expected_head)?;
        let canonical_files = read_tree_files(&repo, commit.tree_id())?;
        remove_materialization_temps(&expected_path)?;
        let working_files = read_regular_files(&expected_path)?;
        if working_files == canonical_files {
            return Ok(true);
        }
        let source_hashes = decode_worktree_source_manifest(source_manifest)?;
        let paths: BTreeSet<&str> = source_hashes
            .keys()
            .chain(canonical_files.keys())
            .chain(working_files.keys())
            .map(String::as_str)
            .collect();
        for path in paths {
            let working = working_files.get(path);
            let matches_source = match (source_hashes.get(path), working) {
                (Some(expected), Some(bytes)) => file_content_id(bytes) == *expected,
                (None, None) => true,
                _ => false,
            };
            let matches_canonical = working == canonical_files.get(path);
            if !matches_source && !matches_canonical {
                return Ok(false);
            }
        }
        self.materialize_canonical_worktree_locked(
            &repo,
            &expected_path,
            expected_head,
            &working_files,
            &canonical_files,
        )?;
        Ok(true)
    }

    fn materialize_canonical_worktree_locked(
        &self,
        repo: &Repository,
        worktree_path: &Path,
        commit_id: &CommitId,
        expected_working_files: &FileMap,
        canonical_files: &FileMap,
    ) -> Result<()> {
        validate_file_map(expected_working_files)?;
        validate_file_map(canonical_files)?;
        let branch_head = find_commit(repo, commit_id)?;
        if read_tree_files(repo, branch_head.tree_id())? != *canonical_files {
            return Err(AuthoredStateError::InvalidInput(
                "canonical files do not match their committed Git tree".into(),
            ));
        }

        remove_materialization_temps(worktree_path)?;
        if read_regular_files(worktree_path)? != *expected_working_files {
            return Err(AuthoredStateError::InvalidInput(
                "worktree files changed before canonical materialization".into(),
            ));
        }
        if expected_working_files == canonical_files {
            return Ok(());
        }

        // Remove paths first so transitions between `a` and `a/b` work in
        // either direction. Every unlink is preceded by an exact byte check;
        // no path is resolved through the mutable checkout `.git` file.
        for (path, expected) in expected_working_files {
            if !canonical_files.contains_key(path) {
                remove_materialized_file(worktree_path, path, expected)?;
            }
        }
        remove_empty_materialization_directories(worktree_path)?;

        for (path, canonical) in canonical_files {
            let expected = expected_working_files.get(path).map(Vec::as_slice);
            if expected != Some(canonical.as_slice()) {
                write_materialized_file(worktree_path, path, expected, canonical)?;
            }
        }

        if read_regular_files(worktree_path)? != *canonical_files {
            return Err(AuthoredStateError::InvalidInput(
                "worktree files changed while the canonical tree was being materialized".into(),
            ));
        }
        if read_tree_files(repo, branch_head.tree_id())? != *canonical_files {
            return Err(AuthoredStateError::InvalidInput(
                "materialized files no longer match their committed Git tree".into(),
            ));
        }
        Ok(())
    }

    fn ensure_repository_locked(
        &self,
        repository_id: &AuthoredRepositoryId,
    ) -> Result<RepositoryInfo> {
        let path = self.storage.authored_repository_dir(repository_id.as_str());
        let parent = self.storage.authored_repositories_dir();
        ensure_bounded_directory_tree(self.storage.path(), &parent)?;
        if validate_bounded_directory_tree(self.storage.path(), &path)? {
            let repo = open_bare_checked(&path)?;
            return repository_info(repository_id, path, &repo);
        }

        let temporary = parent.join(format!(
            ".tmp-{}-{}",
            repository_id.as_str(),
            Uuid::new_v4()
        ));
        let initialized = (|| -> Result<()> {
            let mut options = RepositoryInitOptions::new();
            options
                .bare(true)
                .initial_head(MAIN_BRANCH)
                .external_template(false)
                .no_reinit(true);
            let repo = Repository::init_opts(&temporary, &options)?;
            let tree_id = repo.treebuilder(None)?.write()?;
            let tree = repo.find_tree(tree_id)?;
            let signature = system_signature()?;
            repo.commit(
                Some(&branch_ref(MAIN_BRANCH)?),
                &signature,
                &signature,
                INITIAL_MESSAGE,
                &tree,
                &[],
            )?;
            Ok(())
        })();
        if let Err(error) = initialized {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            // Another process may have atomically published the same repository
            // after our initial existence check. Accept only a valid bare repo.
            match validate_bounded_directory_tree(self.storage.path(), &path) {
                Ok(true) => {
                    let _ = fs::remove_dir_all(&temporary);
                    let repo = open_bare_checked(&path)?;
                    return repository_info(repository_id, path, &repo);
                }
                Ok(false) => {}
                Err(validation_error) => {
                    let _ = fs::remove_dir_all(&temporary);
                    return Err(validation_error);
                }
            }
            let _ = fs::remove_dir_all(&temporary);
            return Err(AuthoredStateError::io(
                format!("failed to publish repository {}", path.display()),
                error,
            ));
        }
        if !validate_bounded_directory_tree(self.storage.path(), &path)? {
            return Err(AuthoredStateError::NotFound(format!(
                "published authored repository {} is missing",
                path.display()
            )));
        }
        let repo = open_bare_checked(&path)?;
        repository_info(repository_id, path, &repo)
    }

    fn commit_files_locked(
        &self,
        repo: &Repository,
        branch: &str,
        expected_head: &CommitId,
        files: &FileMap,
        author: &CommitAuthor,
        message: &str,
        extra_parents: &[CommitId],
    ) -> Result<CommitInfo> {
        let tree_id = prepare_file_tree(repo, files)?;
        self.commit_tree_locked(
            repo,
            branch,
            expected_head,
            tree_id,
            author,
            message,
            extra_parents,
        )
    }

    fn commit_tree_locked(
        &self,
        repo: &Repository,
        branch: &str,
        expected_head: &CommitId,
        tree_id: Oid,
        author: &CommitAuthor,
        message: &str,
        extra_parents: &[CommitId],
    ) -> Result<CommitInfo> {
        let refname = branch_ref(branch)?;
        let mut parents = Vec::with_capacity(1 + extra_parents.len());
        parents.push(expected_head.clone());
        parents.extend_from_slice(extra_parents);
        let candidate =
            self.prepare_commit_tree_locked(repo, &parents, tree_id, author, message)?;
        let candidate_id = candidate.id.oid()?;
        let signature = author.signature()?;

        let mut transaction = repo.transaction()?;
        transaction.lock_ref(&refname)?;
        let actual = repo.refname_to_id(&refname).map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                AuthoredStateError::NotFound(format!("branch '{branch}' does not exist"))
            } else {
                error.into()
            }
        })?;
        if actual == candidate_id {
            return Ok(candidate);
        }
        if actual != expected_head.oid()? {
            return Err(AuthoredStateError::HeadConflict {
                reference: refname,
                expected: expected_head.to_string(),
                actual: actual.to_string(),
            });
        }
        transaction.set_target(
            &refname,
            candidate_id,
            Some(&signature),
            &format!("commit: {message}"),
        )?;
        transaction.commit()?;
        Ok(candidate)
    }

    fn prepare_commit_locked(
        &self,
        repo: &Repository,
        parents: &[CommitId],
        files: &FileMap,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<CommitInfo> {
        let tree_id = prepare_file_tree(repo, files)?;
        self.prepare_commit_tree_locked(repo, parents, tree_id, author, message)
    }

    fn prepare_commit_tree_locked(
        &self,
        repo: &Repository,
        parents: &[CommitId],
        tree_id: Oid,
        author: &CommitAuthor,
        message: &str,
    ) -> Result<CommitInfo> {
        validate_commit_message(message)?;
        if !(1..=2).contains(&parents.len()) {
            return Err(AuthoredStateError::InvalidInput(
                "an authored commit must have one or two parents".into(),
            ));
        }
        if parents.len() == 2 && parents[0] == parents[1] {
            return Err(AuthoredStateError::InvalidInput(
                "a merge commit requires two distinct parents".into(),
            ));
        }

        // Trees copied by restore may not have originated from a FileMap, so
        // enforce the same regular-file and bounded-size invariants here too.
        read_tree_files(repo, tree_id)?;
        let tree = repo.find_tree(tree_id)?;
        let parent_commits = parents
            .iter()
            .map(|parent| find_commit(repo, parent))
            .collect::<Result<Vec<_>>>()?;
        let parent_refs = parent_commits.iter().collect::<Vec<_>>();
        let signature = author.signature()?;
        let id = repo.commit(None, &signature, &signature, message, &tree, &parent_refs)?;
        commit_info(&repo.find_commit(id)?)
    }

    fn open_repository(&self, repository_id: &AuthoredRepositoryId) -> Result<Repository> {
        let path = self.storage.authored_repository_dir(repository_id.as_str());
        if !validate_bounded_directory_tree(self.storage.path(), &path)? {
            return Err(AuthoredStateError::NotFound(format!(
                "authored repository {repository_id} does not exist"
            )));
        }
        open_bare_checked(&path)
    }

    fn repository_lock(&self, repository_id: &AuthoredRepositoryId) -> Result<Arc<Mutex<()>>> {
        let mut locks = self.repository_locks.lock().map_err(lock_poisoned)?;
        Ok(Arc::clone(
            locks
                .entry(repository_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        ))
    }
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn lock_poisoned<T>(_: std::sync::PoisonError<T>) -> AuthoredStateError {
    AuthoredStateError::InvalidInput("authored repository lock was poisoned".into())
}

fn branch_ref(branch: &str) -> Result<String> {
    if branch.is_empty() || branch.len() > 512 || branch.starts_with('-') {
        return Err(AuthoredStateError::InvalidInput(format!(
            "invalid authored branch name '{branch}'"
        )));
    }
    let refname = format!("refs/heads/{branch}");
    if !Reference::is_valid_name(&refname) {
        return Err(AuthoredStateError::InvalidInput(format!(
            "invalid authored branch name '{branch}'"
        )));
    }
    Ok(refname)
}

fn validate_log_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > 10_000 {
        return Err(AuthoredStateError::InvalidInput(
            "log limit must be between 1 and 10000".into(),
        ));
    }
    Ok(())
}

fn system_signature() -> Result<Signature<'static>> {
    Signature::new("Luma", "authored-state@luma.local", &Time::new(0, 0))
        .map_err(AuthoredStateError::Git)
}

fn open_bare_checked(path: &Path) -> Result<Repository> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AuthoredStateError::io(
            format!("failed to inspect repository {}", path.display()),
            error,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AuthoredStateError::UnsafePath(format!(
            "authored repository path {} is not a real directory",
            path.display()
        )));
    }
    let repo = Repository::open_bare(path)?;
    if !repo.is_bare() || repo.is_worktree() {
        return Err(AuthoredStateError::UnsafePath(format!(
            "authored repository {} is not bare",
            path.display()
        )));
    }
    Ok(repo)
}

fn repository_info(
    id: &AuthoredRepositoryId,
    path: PathBuf,
    repo: &Repository,
) -> Result<RepositoryInfo> {
    Ok(RepositoryInfo {
        id: id.clone(),
        path,
        main_head: ref_target(repo, &branch_ref(MAIN_BRANCH)?)?,
    })
}

fn ref_target(repo: &Repository, refname: &str) -> Result<CommitId> {
    repo.refname_to_id(refname)
        .map(CommitId::from_oid)
        .map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                AuthoredStateError::NotFound(format!("Git ref '{refname}' does not exist"))
            } else {
                error.into()
            }
        })
}

fn find_commit<'repo>(repo: &'repo Repository, id: &CommitId) -> Result<git2::Commit<'repo>> {
    repo.find_commit(id.oid()?)
        .map_err(|_| AuthoredStateError::NotFound(format!("commit {id} does not exist")))
}

fn commit_info(commit: &git2::Commit<'_>) -> Result<CommitInfo> {
    let author = commit.author();
    let time = author.when();
    let name = author.name()?;
    let email = author.email()?;
    let message = commit.message()?;
    Ok(CommitInfo {
        id: CommitId::from_oid(commit.id()),
        tree_id: commit.tree_id().to_string(),
        parents: commit.parent_ids().map(CommitId::from_oid).collect(),
        author: CommitAuthor {
            name: name.to_string(),
            email: email.to_string(),
            time_seconds: time.seconds(),
            offset_minutes: time.offset_minutes(),
        },
        message: message.to_string(),
    })
}

fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.contains('\0')
        || path.contains('\\')
        || path.ends_with('/')
    {
        return Err(AuthoredStateError::UnsafePath(format!(
            "unsafe tracked path '{path}'"
        )));
    }
    let parsed = Path::new(path);
    if parsed.is_absolute() {
        return Err(AuthoredStateError::UnsafePath(format!(
            "tracked path '{path}' must be relative"
        )));
    }
    for component in path.split('/') {
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.eq_ignore_ascii_case(".git")
            || is_materialization_temp_component(component)
            || component.ends_with(' ')
            || component.ends_with('.')
            || component
                .chars()
                .any(|character| character.is_control() || r#"<>:\"|?*"#.contains(character))
            || is_windows_reserved_component(component)
        {
            return Err(AuthoredStateError::UnsafePath(format!(
                "tracked path '{path}' contains a reserved component"
            )));
        }
    }
    for component in parsed.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    AuthoredStateError::UnsafePath("tracked paths must be UTF-8".into())
                })?;
                if value.eq_ignore_ascii_case(".git") || value.is_empty() {
                    return Err(AuthoredStateError::UnsafePath(format!(
                        "tracked path '{path}' contains a reserved component"
                    )));
                }
            }
            _ => {
                return Err(AuthoredStateError::UnsafePath(format!(
                    "tracked path '{path}' contains traversal"
                )));
            }
        }
    }
    Ok(())
}

fn is_materialization_temp_component(component: &str) -> bool {
    component
        .get(..MATERIALIZATION_TEMP_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(MATERIALIZATION_TEMP_PREFIX))
}

fn is_windows_reserved_component(component: &str) -> bool {
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                number.len() == 1 && matches!(number.as_bytes().first(), Some(b'1'..=b'9'))
            })
}

fn validate_file_map(files: &FileMap) -> Result<()> {
    if files.len() > MAX_FILES {
        return Err(AuthoredStateError::InvalidInput(format!(
            "an authored commit may contain at most {MAX_FILES} files"
        )));
    }
    let mut total = 0usize;
    for (path, bytes) in files {
        validate_relative_path(path)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "tracked file '{path}' exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        total = total.checked_add(bytes.len()).ok_or_else(|| {
            AuthoredStateError::InvalidInput("authored file bytes overflow".into())
        })?;
        if total > MAX_TOTAL_BYTES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored commit exceeds {MAX_TOTAL_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

fn validate_commit_message(message: &str) -> Result<()> {
    if message.is_empty() || message.contains('\0') {
        return Err(AuthoredStateError::InvalidInput(
            "commit message cannot be empty or contain NUL".into(),
        ));
    }
    Ok(())
}

fn prepare_file_tree(repo: &Repository, files: &FileMap) -> Result<Oid> {
    validate_file_map(files)?;
    let mut root = PendingTree::default();
    for (path, data) in files {
        let blob = repo.blob(data)?;
        let components = path.split('/').collect::<Vec<_>>();
        root.insert(&components, blob, path)?;
    }
    root.write(repo)
}

#[derive(Default)]
struct PendingTree {
    entries: BTreeMap<String, PendingTreeEntry>,
}

enum PendingTreeEntry {
    Blob(Oid),
    Tree(PendingTree),
}

impl PendingTree {
    fn insert(&mut self, components: &[&str], blob: Oid, path: &str) -> Result<()> {
        let (name, remainder) = components.split_first().ok_or_else(|| {
            AuthoredStateError::UnsafePath(format!("tracked path '{path}' is empty"))
        })?;
        if remainder.is_empty() {
            if self
                .entries
                .insert((*name).to_string(), PendingTreeEntry::Blob(blob))
                .is_some()
            {
                return Err(AuthoredStateError::InvalidInput(format!(
                    "tracked path '{path}' conflicts with another entry"
                )));
            }
            return Ok(());
        }

        let entry = self
            .entries
            .entry((*name).to_string())
            .or_insert_with(|| PendingTreeEntry::Tree(Self::default()));
        match entry {
            PendingTreeEntry::Tree(tree) => tree.insert(remainder, blob, path),
            PendingTreeEntry::Blob(_) => Err(AuthoredStateError::InvalidInput(format!(
                "tracked path '{path}' has a file as an ancestor"
            ))),
        }
    }

    fn write(&self, repo: &Repository) -> Result<Oid> {
        let mut builder = repo.treebuilder(None)?;
        for (name, entry) in &self.entries {
            match entry {
                PendingTreeEntry::Blob(blob) => {
                    builder.insert(name, *blob, 0o100644)?;
                }
                PendingTreeEntry::Tree(tree) => {
                    builder.insert(name, tree.write(repo)?, 0o040000)?;
                }
            }
        }
        builder.write().map_err(Into::into)
    }
}

#[derive(Default)]
struct ReadBudget {
    files: usize,
    content_bytes: usize,
    tree_bytes: usize,
}

impl ReadBudget {
    fn reserve_file(&mut self, path: &str, bytes: usize) -> Result<()> {
        self.files = self.files.checked_add(1).ok_or_else(|| {
            AuthoredStateError::InvalidInput("authored file count overflow".into())
        })?;
        if self.files > MAX_FILES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "an authored commit may contain at most {MAX_FILES} files"
            )));
        }
        if bytes > MAX_FILE_BYTES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "tracked file '{path}' exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        self.content_bytes = self.content_bytes.checked_add(bytes).ok_or_else(|| {
            AuthoredStateError::InvalidInput("authored file bytes overflow".into())
        })?;
        if self.content_bytes > MAX_TOTAL_BYTES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored commit exceeds {MAX_TOTAL_BYTES} bytes"
            )));
        }
        Ok(())
    }

    fn reserve_tree(&mut self, bytes: usize) -> Result<()> {
        self.tree_bytes = self.tree_bytes.checked_add(bytes).ok_or_else(|| {
            AuthoredStateError::InvalidInput("authored tree bytes overflow".into())
        })?;
        if self.tree_bytes > MAX_TREE_BYTES {
            return Err(AuthoredStateError::InvalidInput(format!(
                "authored Git trees exceed {MAX_TREE_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

fn read_tree_files(repo: &Repository, tree_id: Oid) -> Result<FileMap> {
    let mut files = FileMap::new();
    let mut budget = ReadBudget::default();
    collect_tree(repo, tree_id, "", &mut files, &mut budget)?;
    Ok(files)
}

fn collect_tree(
    repo: &Repository,
    tree_id: Oid,
    prefix: &str,
    files: &mut FileMap,
    budget: &mut ReadBudget,
) -> Result<()> {
    let object_database = repo.odb()?;
    let (tree_bytes, kind) = object_database.read_header(tree_id)?;
    if kind != ObjectType::Tree {
        return Err(AuthoredStateError::UnsafePath(format!(
            "Git object {tree_id} is not a tree"
        )));
    }
    budget.reserve_tree(tree_bytes)?;
    let tree = repo.find_tree(tree_id)?;
    for entry in tree.iter() {
        let name = entry.name()?;
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        validate_relative_path(&path)?;
        match entry.kind() {
            Some(ObjectType::Tree) => {
                collect_tree(repo, entry.id(), &path, files, budget)?;
            }
            Some(ObjectType::Blob) if entry.filemode() != 0o120000 => {
                let (blob_bytes, kind) = object_database.read_header(entry.id())?;
                if kind != ObjectType::Blob {
                    return Err(AuthoredStateError::UnsafePath(format!(
                        "Git object {} at '{path}' is not a blob",
                        entry.id()
                    )));
                }
                budget.reserve_file(&path, blob_bytes)?;
                let blob = repo.find_blob(entry.id())?;
                if blob.size() != blob_bytes {
                    return Err(AuthoredStateError::InvalidInput(format!(
                        "Git blob size changed while reading '{path}'"
                    )));
                }
                if files
                    .insert(path.clone(), blob.content().to_vec())
                    .is_some()
                {
                    return Err(AuthoredStateError::InvalidInput(format!(
                        "Git tree contains duplicate path '{path}'"
                    )));
                }
            }
            Some(ObjectType::Blob) => {
                return Err(AuthoredStateError::UnsafePath(format!(
                    "Git tree contains symlink '{path}'"
                )));
            }
            other => {
                return Err(AuthoredStateError::UnsafePath(format!(
                    "Git tree contains unsupported entry '{path}' ({other:?})"
                )));
            }
        }
    }
    Ok(())
}

fn worktree_info(repo: &Repository, worktree: &git2::Worktree) -> Result<WorktreeInfo> {
    let name = worktree
        .name()?
        .ok_or_else(|| AuthoredStateError::InvalidInput("worktree name is not UTF-8".into()))?;
    let id = WorktreeId::parse(name)?;
    let branch = registered_worktree_branch(repo, &id)?;
    let head = ref_target(repo, &branch_ref(&branch)?)?;
    Ok(WorktreeInfo {
        id,
        path: worktree.path().to_path_buf(),
        branch,
        head,
        locked: !matches!(worktree.is_locked()?, git2::WorktreeLockStatus::Unlocked),
    })
}

/// Prove that a linked checkout still represents the immutable worktree
/// binding recorded by the orchestration layer. Linked worktrees are ordinary
/// Git checkouts, so an agent can run `git switch` inside one; no read, commit,
/// refresh, or removal may trust that mutable HEAD as authority.
fn validate_worktree_binding(
    repo: &Repository,
    worktree: &git2::Worktree,
    trusted_root: &Path,
    expected_path: &Path,
    expected_branch: &str,
) -> Result<WorktreeInfo> {
    branch_ref(expected_branch)?;
    let name = worktree
        .name()?
        .ok_or_else(|| AuthoredStateError::InvalidInput("worktree name is not UTF-8".into()))?;
    let id = WorktreeId::parse(name)?;
    validate_registered_worktree_path(trusted_root, worktree.path(), expected_path, &id)?;
    let info = worktree_info(repo, worktree)?;
    if info.branch != expected_branch {
        return Err(AuthoredStateError::InvalidInput(format!(
            "worktree '{}' is attached to branch '{}', expected immutable branch '{}'",
            info.id.as_str(),
            info.branch,
            expected_branch
        )));
    }
    Ok(info)
}

/// Canonical equality alone is insufficient: an externally registered path
/// can symlink back to the expected checkout and compare equal while still
/// granting Git a destructive path outside Luma's storage root. Validate the
/// registered spelling and each existing component before comparing targets.
fn validate_registered_worktree_path(
    trusted_root: &Path,
    registered_path: &Path,
    expected_path: &Path,
    worktree_id: &WorktreeId,
) -> Result<()> {
    // libgit canonicalizes the checkout location it records. On macOS that
    // legitimately changes `/var/...` into `/private/var/...`, so admit the
    // configured root and exactly its canonical spelling—not any arbitrary
    // path that happens to symlink back to the expected checkout.
    let canonical_root = trusted_root.canonicalize().map_err(|error| {
        AuthoredStateError::io(
            format!(
                "failed to canonicalize trusted storage root {}",
                trusted_root.display()
            ),
            error,
        )
    })?;
    let registered_root = if registered_path.strip_prefix(trusted_root).is_ok() {
        trusted_root
    } else if registered_path.strip_prefix(&canonical_root).is_ok() {
        canonical_root.as_path()
    } else {
        return Err(AuthoredStateError::UnsafePath(format!(
            "worktree '{}' is registered outside its authored-state root at {}",
            worktree_id.as_str(),
            registered_path.display()
        )));
    };
    let _ = validate_bounded_directory_tree(registered_root, registered_path)?;
    if !paths_equal(registered_path, expected_path) {
        return Err(AuthoredStateError::UnsafePath(format!(
            "worktree '{}' is registered outside its authored-state root at {}",
            worktree_id.as_str(),
            registered_path.display()
        )));
    }
    Ok(())
}

fn validate_registered_worktree_head(
    repo: &Repository,
    worktree_id: &WorktreeId,
    expected_branch: &str,
) -> Result<()> {
    branch_ref(expected_branch)?;
    let actual_branch = registered_worktree_branch(repo, worktree_id)?;
    if actual_branch != expected_branch {
        return Err(AuthoredStateError::InvalidInput(format!(
            "worktree '{}' is not registered to expected immutable branch '{}'",
            worktree_id.as_str(),
            expected_branch
        )));
    }
    ref_target(repo, &branch_ref(expected_branch)?)?;
    Ok(())
}

/// Read the linked worktree's immutable branch binding exclusively from the
/// host-owned bare repository. The checkout's `.git` file is agent-writable
/// projection data and is never an authority for branch identity or object
/// lookup.
fn registered_worktree_branch(repo: &Repository, worktree_id: &WorktreeId) -> Result<String> {
    let administrative_dir = repo.path().join("worktrees").join(worktree_id.as_str());
    if !validate_bounded_directory_tree(repo.path(), &administrative_dir)? {
        return Err(AuthoredStateError::NotFound(format!(
            "worktree '{}' has no administrative state",
            worktree_id.as_str()
        )));
    }
    let head_path = repo
        .path()
        .join("worktrees")
        .join(worktree_id.as_str())
        .join("HEAD");
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&head_path).map_err(|error| {
        AuthoredStateError::io(
            format!("failed to open worktree HEAD {}", head_path.display()),
            error,
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        AuthoredStateError::io(
            format!("failed to inspect worktree HEAD {}", head_path.display()),
            error,
        )
    })?;
    if !metadata.is_file() || metadata.len() > 1024 {
        return Err(AuthoredStateError::UnsafePath(format!(
            "worktree '{}' has an invalid administrative HEAD",
            worktree_id.as_str()
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(1025).read_to_end(&mut bytes).map_err(|error| {
        AuthoredStateError::io(
            format!("failed to read worktree HEAD {}", head_path.display()),
            error,
        )
    })?;
    if bytes.len() != metadata.len() as usize {
        return Err(AuthoredStateError::InvalidInput(format!(
            "worktree '{}' administrative HEAD changed while it was read",
            worktree_id.as_str()
        )));
    }
    let raw = std::str::from_utf8(&bytes).map_err(|_| {
        AuthoredStateError::UnsafePath(format!(
            "worktree '{}' administrative HEAD is not UTF-8",
            worktree_id.as_str()
        ))
    })?;
    let raw = raw.strip_suffix('\n').unwrap_or(raw);
    let raw = raw.strip_suffix('\r').unwrap_or(raw);
    if raw.contains(['\r', '\n']) {
        return Err(AuthoredStateError::UnsafePath(format!(
            "worktree '{}' has an invalid administrative HEAD",
            worktree_id.as_str()
        )));
    }
    let reference = raw.strip_prefix("ref: ").ok_or_else(|| {
        AuthoredStateError::InvalidInput(format!(
            "worktree '{}' has a detached HEAD",
            worktree_id.as_str()
        ))
    })?;
    let branch = reference.strip_prefix("refs/heads/").ok_or_else(|| {
        AuthoredStateError::UnsafePath(format!(
            "worktree '{}' administrative HEAD is not a branch",
            worktree_id.as_str()
        ))
    })?;
    if branch_ref(branch)? != reference {
        return Err(AuthoredStateError::UnsafePath(format!(
            "worktree '{}' has an invalid administrative branch",
            worktree_id.as_str()
        )));
    }
    Ok(branch.to_string())
}

fn files_for_reference(repo: &Repository, reference: &Reference<'_>) -> Result<FileMap> {
    let commit = reference.peel_to_commit()?;
    read_tree_files(repo, commit.tree_id())
}

fn remove_unregistered_worktree_projection(
    repo: &Repository,
    expected_branch: &str,
    path: &Path,
) -> Result<()> {
    let reference = repo
        .find_reference(&branch_ref(expected_branch)?)
        .map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                AuthoredStateError::NotFound(format!("branch '{expected_branch}' does not exist"))
            } else {
                error.into()
            }
        })?;
    let expected_files = files_for_reference(repo, &reference)?;
    remove_recoverable_worktree_projection(path, &expected_files)
}

/// A worktree reservation is never exposed to an agent until creation returns.
/// Therefore a pre-existing projection at its unique internal path can only be
/// an interrupted host checkout. Recover it iff every visible authored file is
/// an exact subset of the requested branch tree; unknown or modified data is
/// preserved and reported instead of being overwritten.
fn validate_recoverable_worktree_projection(path: &Path, expected: &FileMap) -> Result<()> {
    let actual = read_regular_files(path)?;
    let safe = actual
        .iter()
        .all(|(name, contents)| expected.get(name) == Some(contents));
    if !safe {
        return Err(AuthoredStateError::UnsafePath(format!(
            "refusing to overwrite non-checkout data at interrupted worktree path {}",
            path.display()
        )));
    }
    Ok(())
}

fn remove_recoverable_worktree_projection(path: &Path, expected: &FileMap) -> Result<()> {
    validate_recoverable_worktree_projection(path, expected)?;
    let captured = read_regular_files(path)?;
    validate_recoverable_worktree_projection(path, expected)?;
    if read_regular_files(path)? != captured {
        return Err(AuthoredStateError::InvalidInput(format!(
            "interrupted worktree projection changed during recovery at {}",
            path.display()
        )));
    }
    fs::remove_dir_all(path).map_err(|error| {
        AuthoredStateError::io(
            format!(
                "failed to remove interrupted worktree projection {}",
                path.display()
            ),
            error,
        )
    })
}

fn ensure_directory_not_symlink(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AuthoredStateError::io(format!("failed to inspect {}", path.display()), error)
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AuthoredStateError::UnsafePath(format!(
            "{} must be a real directory",
            path.display()
        )));
    }
    Ok(())
}

/// Validate every directory component below a trusted storage root without
/// following symlinks. When `create_missing` is true, components below the root
/// are created one at a time so `create_dir_all` can never traverse a hostile
/// authored-state ancestor. The storage root itself is the trust anchor; it may
/// be created for a new profile, but must resolve to a real directory.
fn bounded_directory_tree(
    trusted_root: &Path,
    target: &Path,
    create_missing: bool,
) -> Result<bool> {
    let relative = target.strip_prefix(trusted_root).map_err(|_| {
        AuthoredStateError::UnsafePath(format!(
            "authored path {} escapes trusted storage root {}",
            target.display(),
            trusted_root.display()
        ))
    })?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AuthoredStateError::UnsafePath(format!(
            "authored path {} is not a normalized child of {}",
            target.display(),
            trusted_root.display()
        )));
    }

    if create_missing {
        match fs::symlink_metadata(trusted_root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(trusted_root).map_err(|error| {
                    AuthoredStateError::io(
                        format!(
                            "failed to create trusted storage root {}",
                            trusted_root.display()
                        ),
                        error,
                    )
                })?;
            }
            Err(error) => {
                return Err(AuthoredStateError::io(
                    format!(
                        "failed to inspect trusted storage root {}",
                        trusted_root.display()
                    ),
                    error,
                ));
            }
        }
    }
    match fs::symlink_metadata(trusted_root) {
        Ok(_) => ensure_directory_not_symlink(trusted_root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(AuthoredStateError::io(
                format!(
                    "failed to inspect trusted storage root {}",
                    trusted_root.display()
                ),
                error,
            ));
        }
    }

    let mut current = trusted_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            unreachable!("relative authored path was validated above")
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(_) => ensure_directory_not_symlink(&current)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_missing => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(AuthoredStateError::io(
                            format!("failed to create authored directory {}", current.display()),
                            error,
                        ));
                    }
                }
                ensure_directory_not_symlink(&current)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AuthoredStateError::io(
                    format!("failed to inspect authored directory {}", current.display()),
                    error,
                ));
            }
        }
    }
    Ok(true)
}

fn ensure_bounded_directory_tree(trusted_root: &Path, target: &Path) -> Result<()> {
    if !bounded_directory_tree(trusted_root, target, true)? {
        return Err(AuthoredStateError::NotFound(format!(
            "authored directory {} was not created",
            target.display()
        )));
    }
    Ok(())
}

fn validate_bounded_directory_tree(trusted_root: &Path, target: &Path) -> Result<bool> {
    bounded_directory_tree(trusted_root, target, false)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (
        canonicalize_allow_missing(left),
        canonicalize_allow_missing(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn canonicalize_allow_missing(path: &Path) -> std::io::Result<PathBuf> {
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else {
                    return Err(error);
                };
                missing.push(name.to_os_string());
                let Some(parent) = cursor.parent() else {
                    return Err(error);
                };
                cursor = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Remove only host-reserved scratch files left by an interrupted canonical
/// materialization. The prefix is forbidden in authored paths, so recovery can
/// distinguish these files without consulting Git metadata in the checkout.
fn remove_materialization_temps(root: &Path) -> Result<()> {
    ensure_directory_not_symlink(root)?;
    let mut temporary_files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            AuthoredStateError::InvalidInput(format!(
                "failed to inspect worktree {} for interrupted materialization: {error}",
                root.display()
            ))
        })?;
        if entry.path() == root {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            return Err(AuthoredStateError::UnsafePath(
                "worktree paths must be UTF-8".into(),
            ));
        };
        if !is_materialization_temp_component(name) {
            continue;
        }
        if !entry.file_type().is_file() || entry.file_type().is_symlink() {
            return Err(AuthoredStateError::UnsafePath(format!(
                "reserved materialization path {} is not a regular file",
                entry.path().display()
            )));
        }
        temporary_files.push(entry.path().to_path_buf());
    }
    for path in temporary_files {
        fs::remove_file(&path).map_err(|error| {
            AuthoredStateError::io(
                format!(
                    "failed to remove interrupted materialization file {}",
                    path.display()
                ),
                error,
            )
        })?;
    }
    Ok(())
}

fn read_materialized_file(root: &Path, relative: &str) -> Result<Option<Vec<u8>>> {
    validate_relative_path(relative)?;
    let path = root.join(relative);
    let parent = path.parent().ok_or_else(|| {
        AuthoredStateError::UnsafePath(format!("tracked path '{relative}' has no parent"))
    })?;
    if !validate_bounded_directory_tree(root, parent)? {
        return Ok(None);
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(AuthoredStateError::UnsafePath(format!(
                    "materialization target '{relative}' is not a regular file"
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AuthoredStateError::io(
                format!("failed to inspect materialization target '{relative}'"),
                error,
            ));
        }
    }

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&path).map_err(|error| {
        AuthoredStateError::io(
            format!("failed to open materialization target '{relative}'"),
            error,
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        AuthoredStateError::io(
            format!("failed to inspect materialization target '{relative}'"),
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(AuthoredStateError::UnsafePath(format!(
            "materialization target '{relative}' changed while it was read"
        )));
    }
    let expected_bytes = usize::try_from(metadata.len()).map_err(|_| {
        AuthoredStateError::InvalidInput(format!(
            "materialization target '{relative}' is too large for this platform"
        ))
    })?;
    let mut budget = ReadBudget::default();
    budget.reserve_file(relative, expected_bytes)?;
    let mut bytes = Vec::with_capacity(expected_bytes);
    file.take((expected_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            AuthoredStateError::io(
                format!("failed to read materialization target '{relative}'"),
                error,
            )
        })?;
    if bytes.len() != expected_bytes {
        return Err(AuthoredStateError::InvalidInput(format!(
            "materialization target '{relative}' changed while it was read"
        )));
    }
    Ok(Some(bytes))
}

fn remove_materialized_file(root: &Path, relative: &str, expected: &[u8]) -> Result<()> {
    if read_materialized_file(root, relative)?.as_deref() != Some(expected) {
        return Err(AuthoredStateError::InvalidInput(format!(
            "materialization target '{relative}' changed before deletion"
        )));
    }
    let path = root.join(relative);
    fs::remove_file(&path).map_err(|error| {
        AuthoredStateError::io(
            format!("failed to remove materialization target '{relative}'"),
            error,
        )
    })?;
    if read_materialized_file(root, relative)?.is_some() {
        return Err(AuthoredStateError::InvalidInput(format!(
            "materialization target '{relative}' reappeared after deletion"
        )));
    }
    Ok(())
}

fn write_materialized_file(
    root: &Path,
    relative: &str,
    expected: Option<&[u8]>,
    canonical: &[u8],
) -> Result<()> {
    validate_relative_path(relative)?;
    let path = root.join(relative);
    let parent = path.parent().ok_or_else(|| {
        AuthoredStateError::UnsafePath(format!("tracked path '{relative}' has no parent"))
    })?;
    ensure_bounded_directory_tree(root, parent)?;
    if read_materialized_file(root, relative)?.as_deref() != expected {
        return Err(AuthoredStateError::InvalidInput(format!(
            "materialization target '{relative}' changed before replacement"
        )));
    }

    let mut temporary = tempfile::Builder::new()
        .prefix(MATERIALIZATION_TEMP_PREFIX)
        .tempfile_in(parent)
        .map_err(|error| {
            AuthoredStateError::io(
                format!("failed to create a temporary file beside '{relative}'"),
                error,
            )
        })?;
    temporary.write_all(canonical).map_err(|error| {
        AuthoredStateError::io(
            format!("failed to write canonical bytes for '{relative}'"),
            error,
        )
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        AuthoredStateError::io(
            format!("failed to sync canonical bytes for '{relative}'"),
            error,
        )
    })?;

    if read_materialized_file(root, relative)?.as_deref() != expected {
        return Err(AuthoredStateError::InvalidInput(format!(
            "materialization target '{relative}' changed before atomic replacement"
        )));
    }
    temporary.persist(&path).map_err(|error| {
        AuthoredStateError::io(
            format!("failed to atomically replace materialization target '{relative}'"),
            error.error,
        )
    })?;
    if read_materialized_file(root, relative)?.as_deref() != Some(canonical) {
        return Err(AuthoredStateError::InvalidInput(format!(
            "materialization target '{relative}' changed after atomic replacement"
        )));
    }
    Ok(())
}

fn remove_empty_materialization_directories(root: &Path) -> Result<()> {
    ensure_directory_not_symlink(root)?;
    let mut directories = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            AuthoredStateError::InvalidInput(format!(
                "failed to inspect worktree directories {}: {error}",
                root.display()
            ))
        })?;
        if entry.path() == root {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(AuthoredStateError::UnsafePath(format!(
                "worktree contains symlink {}",
                entry.path().display()
            )));
        }
        if entry.file_type().is_dir() {
            directories.push(entry.path().to_path_buf());
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        match fs::remove_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(error) => {
                return Err(AuthoredStateError::io(
                    format!(
                        "failed to remove empty materialization directory {}",
                        directory.display()
                    ),
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn read_regular_files(root: &Path) -> Result<FileMap> {
    ensure_directory_not_symlink(root)?;
    let mut files = FileMap::new();
    let mut budget = ReadBudget::default();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            AuthoredStateError::InvalidInput(format!(
                "failed to walk worktree {}: {error}",
                root.display()
            ))
        })?;
        if entry.path() == root {
            continue;
        }
        let relative = entry.path().strip_prefix(root).map_err(|_| {
            AuthoredStateError::UnsafePath("worktree entry escaped its root".into())
        })?;
        let relative = relative
            .to_str()
            .ok_or_else(|| AuthoredStateError::UnsafePath("worktree paths must be UTF-8".into()))?;
        let relative = relative.replace('\\', "/");
        if relative == ".git" {
            if !entry.file_type().is_file() {
                return Err(AuthoredStateError::UnsafePath(
                    "linked worktree .git control path is not a regular file".into(),
                ));
            }
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(AuthoredStateError::UnsafePath(format!(
                "worktree contains symlink '{relative}'"
            )));
        }
        if entry.file_type().is_dir() {
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
            {
                return Err(AuthoredStateError::UnsafePath(format!(
                    "worktree contains nested Git directory '{relative}'"
                )));
            }
            continue;
        }
        if !entry.file_type().is_file() {
            return Err(AuthoredStateError::UnsafePath(format!(
                "worktree contains unsupported entry '{relative}'"
            )));
        }
        validate_relative_path(&relative)?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(entry.path()).map_err(|error| {
            AuthoredStateError::io(
                format!("failed to open worktree file {}", entry.path().display()),
                error,
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            AuthoredStateError::io(
                format!("failed to inspect worktree file {}", entry.path().display()),
                error,
            )
        })?;
        if !metadata.is_file() {
            return Err(AuthoredStateError::UnsafePath(format!(
                "worktree entry '{relative}' changed while reading"
            )));
        }
        let expected_bytes = usize::try_from(metadata.len()).map_err(|_| {
            AuthoredStateError::InvalidInput(format!(
                "tracked file '{relative}' is too large for this platform"
            ))
        })?;
        budget.reserve_file(&relative, expected_bytes)?;
        let read_limit = expected_bytes.checked_add(1).ok_or_else(|| {
            AuthoredStateError::InvalidInput("worktree read limit overflow".into())
        })?;
        let mut data = Vec::with_capacity(expected_bytes);
        file.take(read_limit as u64)
            .read_to_end(&mut data)
            .map_err(|error| {
                AuthoredStateError::io(
                    format!("failed to read worktree file {}", entry.path().display()),
                    error,
                )
            })?;
        if data.len() != expected_bytes {
            return Err(AuthoredStateError::InvalidInput(format!(
                "worktree file '{relative}' changed while reading"
            )));
        }
        if files.insert(relative.clone(), data).is_some() {
            return Err(AuthoredStateError::InvalidInput(format!(
                "worktree contains duplicate path '{relative}'"
            )));
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests;
