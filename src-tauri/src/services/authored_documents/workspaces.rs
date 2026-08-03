use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use sqlx::{FromRow, SqliteConnection, SqlitePool};
use tokio::fs;
use uuid::Uuid;

use crate::models::authored_state::{
    AuthoredCurrentRevision, AuthoredWorkspace, AuthoredWorkspaceCheck, AuthoredWorkspaceCommit,
    AuthoredWorkspaceEdit, AuthoredWorkspaceFile, AuthoredWorkspaceMerge,
    CommitAuthoredWorkspaceInput, CreateAuthoredWorkspaceInput, MergeAuthoredWorkspaceInput,
};

use super::operations::{
    insert_committed_operation, operation_outcome_on, OperationOutcomeRow, OperationSpec,
};
use super::{
    canonicalize_graph, check_track_projection_candidate, check_track_workspace_candidate,
    compile_draft_track_document, compile_import_track_document, file_snapshot_id, graph_files,
    is_valid_track_draft_id, load_score_dsl_context, operation_request_fingerprint,
    require_exact_paths, revision_for_clips, revision_metadata, utf8_file, AuthoredDocument,
    AuthoredDocuments, AuthoredDocumentsError, AuthoredMergeConflict, AuthoredSnapshot,
    DocumentScope, FileMap, ResolvedScope, Result, RevisionId, TrackDocument,
    TrackProjectionAuthority, TrackProjectionIdentity, GRAPH_PATH, LAYOUT_PATH, SCORE_PATH,
};

const MAX_WORKSPACE_FILES: usize = 8;
const MAX_WORKSPACE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORKSPACE_BYTES: u64 = 64 * 1024 * 1024;

impl AuthoredDocuments {
    /// Resolve the live relational head for an authenticated agent thread. The
    /// revision id is the only valid base for a newly supervised workspace.
    pub async fn current_revision(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
    ) -> Result<AuthoredCurrentRevision> {
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        let current = self.load_current_locked(pool, &scope).await?;
        Ok(AuthoredCurrentRevision {
            document_id: scope.document_id.to_string(),
            revision_id: current.head.to_string(),
            document: current.document.projected(),
        })
    }

    pub async fn create_workspace(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: CreateAuthoredWorkspaceInput,
    ) -> Result<AuthoredWorkspace> {
        super::validate_token(&input.request_id, "workspace request id")?;
        let expected_base = RevisionId::parse(&input.expected_base_revision_id)?;
        let (thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        let request_fingerprint = operation_request_fingerprint(
            "workspace_create",
            &[
                scope.document_id.as_str(),
                &thread.id,
                expected_base.as_str(),
            ],
        );

        let mut write = self.scope_write(pool, &scope).await?;
        let connection = write.connection();
        self.ensure_current_on_connection(connection, &scope)
            .await?;
        self.store
            .revision_info(connection, &scope.document_id, &expected_base)
            .await?;
        let proposed_id = Uuid::new_v4().to_string();
        let inserted = sqlx::query(
            "INSERT INTO authored_subagent_workspaces
             (workspace_id, request_id, request_fingerprint, document_id,
              owner_thread_id, base_revision_id, head_revision_id)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(owner_thread_id, request_id) DO NOTHING",
        )
        .bind(&proposed_id)
        .bind(&input.request_id)
        .bind(&request_fingerprint)
        .bind(scope.document_id.as_str())
        .bind(&thread.id)
        .bind(expected_base.as_str())
        .bind(expected_base.as_str())
        .execute(&mut *connection)
        .await
        .map_err(storage("reserve authored workspace"))?
        .rows_affected()
            == 1;
        let row = load_workspace_row(
            connection,
            &scope,
            &thread.id,
            None,
            Some(&input.request_id),
        )
        .await?;
        if row.request_fingerprint != request_fingerprint
            || row.base_revision_id != expected_base.as_str()
        {
            return Err(AuthoredDocumentsError::Scope(
                "workspace request id is already bound to another base or document".into(),
            ));
        }
        if row.status != "active" {
            return Err(AuthoredDocumentsError::Scope(
                "workspace request has been retired".into(),
            ));
        }
        let head = RevisionId::parse(&row.head_revision_id)?;
        let (_, files) = self
            .store
            .read_revision(connection, &scope.document_id, &head)
            .await?;
        write.commit().await?;

        let path = self.workspace_path(&scope, &row.workspace_id)?;
        let materialize = if inserted {
            true
        } else {
            match fs::symlink_metadata(&path).await {
                Ok(_) => {
                    // Idempotent allocation replay must never reset edits that
                    // have not yet been committed into the detached head.
                    read_workspace_files(&path, required_paths(&scope)).await?;
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => return Err(io_error("inspect authored workspace replay")(error)),
            }
        };
        if materialize {
            if let Err(error) = replace_workspace_files(&path, &files).await {
                if inserted {
                    let cleanup = self
                        .remove_failed_workspace_allocation(pool, &scope, &row)
                        .await;
                    let _ = remove_workspace_path(&path).await;
                    if let Err(cleanup_error) = cleanup {
                        return Err(AuthoredDocumentsError::Storage(format!(
                            "{error}; remove failed workspace allocation: {cleanup_error}"
                        )));
                    }
                }
                return Err(error);
            }
        }
        Ok(AuthoredWorkspace {
            id: row.workspace_id,
            path: path.to_string_lossy().into_owned(),
            base_revision_id: expected_base.to_string(),
            head_revision_id: head.to_string(),
        })
    }

    pub async fn check_workspace(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        workspace_id: &str,
    ) -> Result<AuthoredWorkspaceCheck> {
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        let row = self
            .active_workspace_row(pool, &scope, thread_id, workspace_id)
            .await?;
        let head = RevisionId::parse(&row.head_revision_id)?;
        let path = self.workspace_path(&scope, workspace_id)?;
        let raw_files = read_workspace_files(&path, required_paths(&scope)).await?;
        let snapshot_id = file_snapshot_id(&raw_files);
        let main = self.load_current_locked(pool, &scope).await?;
        let candidate = self
            .check_workspace_candidate(pool, &scope, &main, &head, &raw_files)
            .await?;
        let mut connection = pool
            .acquire()
            .await
            .map_err(storage("open workspace revision read"))?;
        let (_, head_files) = self
            .store
            .read_revision(&mut connection, &scope.document_id, &head)
            .await?;
        Ok(AuthoredWorkspaceCheck {
            id: row.workspace_id,
            head_revision_id: head.to_string(),
            snapshot_id,
            changed: raw_files != head_files,
            document: candidate.projected(),
        })
    }

    /// Read one UTF-8 authored source file without revealing the workspace's
    /// host path. The complete directory is validated before any content is
    /// returned, so an unexpected file, symlink, or oversized sibling fails
    /// the operation as a whole.
    pub async fn read_workspace_file(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        workspace_id: &str,
        file_name: &str,
    ) -> Result<AuthoredWorkspaceFile> {
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        self.active_workspace_row(pool, &scope, thread_id, workspace_id)
            .await?;
        require_workspace_file_name(&scope, file_name)?;
        let path = self.workspace_path(&scope, workspace_id)?;
        let files = read_workspace_files(&path, required_paths(&scope)).await?;
        Ok(AuthoredWorkspaceFile {
            file_name: file_name.to_owned(),
            content: utf8_file(&files, file_name)?.to_owned(),
        })
    }

    /// Atomically replace one scope-defined UTF-8 source file. Callers cannot
    /// name nested paths or introduce extra files. A subsequent `check` is the
    /// semantic-validation boundary and supplies the snapshot token required
    /// by `commit`.
    pub async fn write_workspace_file(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        workspace_id: &str,
        file_name: &str,
        content: &str,
    ) -> Result<()> {
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        self.active_workspace_row(pool, &scope, thread_id, workspace_id)
            .await?;
        require_workspace_file_name(&scope, file_name)?;
        let content_len = u64::try_from(content.len()).unwrap_or(u64::MAX);
        if content_len > MAX_WORKSPACE_FILE_BYTES {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace file {file_name} is too large"
            )));
        }

        let path = self.workspace_path(&scope, workspace_id)?;
        let mut files = read_workspace_files(&path, required_paths(&scope)).await?;
        files.insert(file_name.to_owned(), content.as_bytes().to_vec());
        validate_workspace_limits(&files)?;
        atomic_write_workspace_file(&path, file_name, content.as_bytes()).await
    }

    /// Apply one exact replacement while holding the document lock across the
    /// read-modify-write cycle. This prevents parallel child tool calls from
    /// silently overwriting each other's edits.
    #[allow(clippy::too_many_arguments)]
    pub async fn edit_workspace_file(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        workspace_id: &str,
        file_name: &str,
        old_text: &str,
        new_text: &str,
        replace_all: bool,
    ) -> Result<AuthoredWorkspaceEdit> {
        if old_text.is_empty() {
            return Err(AuthoredDocumentsError::Invalid(
                "workspace edit old text must not be empty".into(),
            ));
        }
        if old_text.len() as u64 > MAX_WORKSPACE_FILE_BYTES
            || new_text.len() as u64 > MAX_WORKSPACE_FILE_BYTES
        {
            return Err(AuthoredDocumentsError::Invalid(
                "workspace edit text is too large".into(),
            ));
        }

        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        self.active_workspace_row(pool, &scope, thread_id, workspace_id)
            .await?;
        require_workspace_file_name(&scope, file_name)?;
        let path = self.workspace_path(&scope, workspace_id)?;
        let mut files = read_workspace_files(&path, required_paths(&scope)).await?;
        let content = utf8_file(&files, file_name)?;
        let occurrences = content.match_indices(old_text).count();
        if occurrences == 0 {
            return Err(AuthoredDocumentsError::Invalid(
                "workspace edit old text was not found".into(),
            ));
        }
        if !replace_all && occurrences != 1 {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace edit old text occurs {occurrences} times"
            )));
        }
        let replacements = if replace_all { occurrences } else { 1 };
        let removed = old_text.len().checked_mul(replacements).ok_or_else(|| {
            AuthoredDocumentsError::Invalid("workspace edit size overflow".into())
        })?;
        let added = new_text.len().checked_mul(replacements).ok_or_else(|| {
            AuthoredDocumentsError::Invalid("workspace edit size overflow".into())
        })?;
        let next_len = content
            .len()
            .checked_sub(removed)
            .and_then(|length| length.checked_add(added))
            .ok_or_else(|| {
                AuthoredDocumentsError::Invalid("workspace edit size overflow".into())
            })?;
        if u64::try_from(next_len).unwrap_or(u64::MAX) > MAX_WORKSPACE_FILE_BYTES {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace file {file_name} is too large"
            )));
        }
        let next = if replace_all {
            content.replace(old_text, new_text)
        } else {
            content.replacen(old_text, new_text, 1)
        };
        files.insert(file_name.to_owned(), next.as_bytes().to_vec());
        validate_workspace_limits(&files)?;
        atomic_write_workspace_file(&path, file_name, next.as_bytes()).await?;
        Ok(AuthoredWorkspaceEdit {
            file_name: file_name.to_owned(),
            replacements,
        })
    }

    pub async fn commit_workspace(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: CommitAuthoredWorkspaceInput,
    ) -> Result<AuthoredWorkspaceCommit> {
        super::validate_token(&input.operation_id, "workspace commit operation id")?;
        let expected = RevisionId::parse(&input.expected_head_revision_id)?;
        let subject = super::normalized_subject(&input.message)?;
        let fingerprint = operation_request_fingerprint(
            "workspace_commit",
            &[
                &input.workspace_id,
                expected.as_str(),
                &input.expected_snapshot_id,
                subject,
            ],
        );
        let (_thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        let row = self
            .active_workspace_row(pool, &scope, &input.thread_id, &input.workspace_id)
            .await?;
        let operation = OperationSpec {
            kind: "workspace_commit",
            id: &input.operation_id,
            fingerprint: &fingerprint,
            result_json: None,
        };
        if let Some(outcome) = self
            .operation_outcome(pool, &scope, operation.kind, operation.id)
            .await?
        {
            return self
                .replay_workspace_commit(pool, &scope, &row, operation, outcome)
                .await;
        }
        if row.head_revision_id != expected.as_str() {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace head changed (expected {expected}, current {})",
                row.head_revision_id
            )));
        }

        let path = self.workspace_path(&scope, &row.workspace_id)?;
        let raw_files = read_workspace_files(&path, required_paths(&scope)).await?;
        let actual_snapshot = file_snapshot_id(&raw_files);
        if actual_snapshot != input.expected_snapshot_id {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace files changed (expected snapshot {}, current {actual_snapshot})",
                input.expected_snapshot_id
            )));
        }
        let main = self.load_current_locked(pool, &scope).await?;
        let (canonical_files, candidate) = self
            .canonicalize_workspace_candidate(pool, &scope, &main, &expected, &raw_files)
            .await?;

        let metadata = revision_metadata(
            "workspace_commit",
            Some(&input.operation_id),
            subject,
            Some(&input.thread_id),
            None,
            None,
        )?;
        let mut write = self.scope_write(pool, &scope).await?;
        let connection = write.connection();
        self.ensure_current_on_connection(connection, &scope)
            .await?;
        let current_row = load_workspace_row(
            connection,
            &scope,
            &input.thread_id,
            Some(&input.workspace_id),
            None,
        )
        .await?;
        if current_row.status != "active" || current_row.head_revision_id != expected.as_str() {
            return Err(AuthoredDocumentsError::Invalid(
                "workspace head moved before commit".into(),
            ));
        }
        if let Some(outcome) =
            operation_outcome_on(connection, &scope, operation.kind, operation.id).await?
        {
            drop(write);
            return self
                .replay_workspace_commit(pool, &scope, &current_row, operation, outcome)
                .await;
        }
        let (_, parent_files) = self
            .store
            .read_revision(connection, &scope.document_id, &expected)
            .await?;
        let revision = self
            .store
            .insert_revision(
                connection,
                &scope.document_id,
                std::slice::from_ref(&expected),
                &canonical_files,
                &metadata,
            )
            .await?;
        let advanced = sqlx::query(
            "UPDATE authored_subagent_workspaces
             SET head_revision_id = ?, generation = generation + 1
             WHERE workspace_id = ? AND owner_thread_id = ?
               AND document_id = ? AND status = 'active' AND head_revision_id = ?",
        )
        .bind(revision.id.as_str())
        .bind(&input.workspace_id)
        .bind(&input.thread_id)
        .bind(scope.document_id.as_str())
        .bind(expected.as_str())
        .execute(&mut *connection)
        .await
        .map_err(storage("advance authored workspace head"))?
        .rows_affected();
        if advanced != 1 {
            return Err(AuthoredDocumentsError::Invalid(
                "workspace head moved before commit".into(),
            ));
        }
        insert_committed_operation(connection, &scope, operation, Some(&expected), &revision.id)
            .await?;
        self.enqueue_revision_closure(
            connection,
            &scope,
            &revision,
            &canonical_files,
            false,
            Some((operation.kind, operation.id)),
        )
        .await?;
        write.commit().await?;
        replace_workspace_files(&path, &canonical_files).await?;
        Ok(AuthoredWorkspaceCommit {
            id: row.workspace_id,
            revision_id: revision.id.to_string(),
            applied_to_current_workspace: true,
            changed: parent_files != canonical_files,
            document: candidate.projected(),
        })
    }

    pub async fn merge_workspace(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: MergeAuthoredWorkspaceInput,
    ) -> Result<AuthoredWorkspaceMerge> {
        super::validate_token(&input.operation_id, "workspace merge operation id")?;
        let expected_theirs = RevisionId::parse(&input.expected_head_revision_id)?;
        let fingerprint = operation_request_fingerprint(
            "workspace_merge",
            &[&input.workspace_id, expected_theirs.as_str()],
        );
        let (_thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        let row = self
            .active_workspace_row(pool, &scope, &input.thread_id, &input.workspace_id)
            .await?;
        if row.head_revision_id != expected_theirs.as_str() {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace head changed (expected {expected_theirs}, current {})",
                row.head_revision_id
            )));
        }
        let operation = OperationSpec {
            kind: "workspace_merge",
            id: &input.operation_id,
            fingerprint: &fingerprint,
            result_json: None,
        };
        let main = self.load_current_locked(pool, &scope).await?;
        if let Some(outcome) = self
            .operation_outcome(pool, &scope, operation.kind, operation.id)
            .await?
        {
            return replay_workspace_merge(&scope, &main, operation, outcome);
        }

        let path = self.workspace_path(&scope, &row.workspace_id)?;
        let raw_files = read_workspace_files(&path, required_paths(&scope)).await?;
        let mut connection = pool
            .acquire()
            .await
            .map_err(storage("open workspace merge read"))?;
        let (_, their_files) = self
            .store
            .read_revision(&mut connection, &scope.document_id, &expected_theirs)
            .await?;
        if raw_files != their_files {
            return Err(AuthoredDocumentsError::Invalid(
                "workspace has uncommitted changes; commit it before merging".into(),
            ));
        }
        let theirs_is_ancestor = self
            .store
            .is_ancestor(
                &mut connection,
                &scope.document_id,
                &expected_theirs,
                &main.head,
            )
            .await?;
        let (candidate, files, parents) = if theirs_is_ancestor {
            (
                main.document.clone(),
                main.files.clone(),
                vec![main.head.clone()],
            )
        } else {
            let base = RevisionId::parse(&row.base_revision_id)?;
            if !self
                .store
                .is_ancestor(&mut connection, &scope.document_id, &base, &expected_theirs)
                .await?
            {
                return Err(AuthoredDocumentsError::Storage(
                    "workspace head no longer descends from its recorded base".into(),
                ));
            }
            let base = self
                .snapshot_from_revision(&mut connection, &scope, &base)
                .await?;
            let ours = AuthoredSnapshot {
                files: main.files.clone(),
                document: main.document.clone(),
            };
            let theirs = self
                .snapshot_from_revision(&mut connection, &scope, &expected_theirs)
                .await?;
            drop(connection);
            match self
                .merge_snapshots(pool, &scope, &base, &ours, &theirs)
                .await?
            {
                Ok((document, files)) => (
                    document,
                    files,
                    vec![main.head.clone(), expected_theirs.clone()],
                ),
                Err(conflicts) => {
                    let mut write = self.scope_write(pool, &scope).await?;
                    let connection = write.connection();
                    let current = self
                        .ensure_current_on_connection(connection, &scope)
                        .await?;
                    if current.head != main.head {
                        return Err(AuthoredDocumentsError::State(
                            crate::services::authored_state::AuthoredStateError::HeadConflict {
                                document_id: scope.document_id.to_string(),
                                expected: main.head.to_string(),
                                actual: current.head.to_string(),
                            },
                        ));
                    }
                    self.record_operation_conflict_on(
                        connection, &scope, operation, &main.head, &conflicts,
                    )
                    .await?;
                    write.commit().await?;
                    return Ok(AuthoredWorkspaceMerge::Conflicted { conflicts });
                }
            }
        };
        let applied = self
            .apply_candidate_locked(
                pool,
                &scope,
                &main.head,
                main.document.revision(),
                files,
                candidate,
                TrackProjectionAuthority::TrustedRevision,
                operation,
                "Merge agent workspace",
                Some(parents),
                Some(&input.thread_id),
                None,
                None,
            )
            .await?;
        Ok(AuthoredWorkspaceMerge::Merged {
            document_id: applied.state.document_id,
            revision_id: applied.state.revision_id,
            applied_to_current_projection: applied.applied_to_current_projection,
            document: applied.state.document,
        })
    }

    pub async fn remove_workspace(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        workspace_id: &str,
    ) -> Result<()> {
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        let mut write = self.scope_write(pool, &scope).await?;
        let connection = write.connection();
        let row =
            load_workspace_row(connection, &scope, thread_id, Some(workspace_id), None).await?;
        retire_workspace_row(connection, &row).await?;
        write.commit().await?;
        remove_workspace_path(&self.workspace_path(&scope, workspace_id)?).await?;
        Ok(())
    }

    pub(super) async fn retire_thread_workspaces_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        thread_id: &str,
    ) -> Result<()> {
        // Cleanup must not depend on the live score/implementation projection:
        // an authored document may already be archived while its durable
        // conversation is deleted later. The terminal thread transition plus
        // principal admission is the authority for retiring disposable rows.
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage("begin authored workspace retirement"))?;
        let connection = &mut *transaction;
        let authorized: Option<i64> = sqlx::query_scalar(
            "SELECT 1
             FROM agent_threads thread
             CROSS JOIN auth_write_admission admission
             JOIN authored_documents document ON document.document_id = ?
             WHERE thread.id = ?
               AND thread.owner_user_id IS ?
               AND thread.lifecycle_state = 'deleting'
               AND document.principal_key = ?
               AND admission.singleton = 1
               AND admission.armed = 1
               AND admission.accepting = 1
               AND admission.maintenance = 0
               AND admission.remote_writes = 0
               AND admission.active_uid IS thread.owner_user_id",
        )
        .bind(scope.document_id.as_str())
        .bind(thread_id)
        .bind(scope.owner_user_id.as_deref())
        .bind(&scope.principal_key)
        .fetch_optional(&mut *connection)
        .await
        .map_err(storage("authorize authored workspace retirement"))?;
        if authorized.is_none() {
            return Err(AuthoredDocumentsError::Scope(
                "deleting thread no longer owns its authored workspaces".into(),
            ));
        }
        let rows = sqlx::query_as::<_, WorkspaceRow>(
            "SELECT workspace_id, request_id, request_fingerprint, document_id,
                    owner_thread_id, base_revision_id, head_revision_id, status
             FROM authored_subagent_workspaces
             WHERE owner_thread_id = ? AND document_id = ? ORDER BY workspace_id",
        )
        .bind(thread_id)
        .bind(scope.document_id.as_str())
        .fetch_all(&mut *connection)
        .await
        .map_err(storage("list thread authored workspaces"))?;
        for row in &rows {
            retire_workspace_row(connection, row).await?;
        }
        transaction
            .commit()
            .await
            .map_err(storage("commit authored workspace retirement"))?;
        for row in rows {
            remove_workspace_path(&self.workspace_path(scope, &row.workspace_id)?).await?;
        }
        Ok(())
    }

    async fn remove_failed_workspace_allocation(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        row: &WorkspaceRow,
    ) -> Result<()> {
        let mut write = self.scope_write(pool, scope).await?;
        let connection = write.connection();
        let removed = sqlx::query(
            "DELETE FROM authored_subagent_workspaces
             WHERE workspace_id = ? AND owner_thread_id = ? AND document_id = ?
               AND request_fingerprint = ? AND status = 'active'
               AND generation = 0 AND head_revision_id = base_revision_id",
        )
        .bind(&row.workspace_id)
        .bind(&row.owner_thread_id)
        .bind(scope.document_id.as_str())
        .bind(&row.request_fingerprint)
        .execute(&mut *connection)
        .await
        .map_err(storage("remove failed authored workspace allocation"))?
        .rows_affected();
        if removed != 1 {
            return Err(AuthoredDocumentsError::Storage(
                "failed authored workspace allocation changed before cleanup".into(),
            ));
        }
        write.commit().await
    }

    async fn active_workspace_row(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        thread_id: &str,
        workspace_id: &str,
    ) -> Result<WorkspaceRow> {
        validate_workspace_id(workspace_id)?;
        let mut connection = pool
            .acquire()
            .await
            .map_err(storage("open authored workspace"))?;
        let row =
            load_workspace_row(&mut connection, scope, thread_id, Some(workspace_id), None).await?;
        if row.status != "active" {
            return Err(AuthoredDocumentsError::Scope(
                "authored workspace is retired".into(),
            ));
        }
        Ok(row)
    }

    async fn check_workspace_candidate(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: &super::MainState,
        workspace_head: &RevisionId,
        files: &FileMap,
    ) -> Result<AuthoredDocument> {
        match &scope.document {
            DocumentScope::Track(track_scope) => {
                require_exact_paths(files, &[SCORE_PATH])?;
                let context = load_score_dsl_context(pool, track_scope)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                let (mut document, _) = compile_draft_track_document(
                    utf8_file(files, SCORE_PATH)?,
                    String::new(),
                    &context.beat_grid,
                    &context.registry,
                )
                .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let current = require_track_document(&main.document)?;
                let current_ids: HashSet<&str> =
                    current.clips.iter().map(|clip| clip.id.as_str()).collect();
                let needed = document
                    .clips
                    .iter()
                    .filter(|clip| {
                        !is_valid_track_draft_id(&clip.id)
                            && !current_ids.contains(clip.id.as_str())
                    })
                    .map(|clip| clip.id.clone())
                    .collect::<BTreeSet<_>>();
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(storage("open workspace lineage"))?;
                let lineage = self
                    .track_lineage_ids(&mut connection, scope, workspace_head, &needed)
                    .await?;
                check_track_workspace_candidate(
                    pool,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    &document.clips,
                    &lineage,
                )
                .await?;
                document.revision = revision_for_clips(&document.clips);
                Ok(AuthoredDocument::Track(document))
            }
            DocumentScope::Pattern(_) => {
                let document = self.decode_files(scope, files)?;
                let AuthoredDocument::Graph(mut graph) = document else {
                    unreachable!("pattern workspace decoded as score")
                };
                graph.graph = canonicalize_graph(&graph.graph)?;
                graph.revision = super::graph_revision(&graph.graph)?;
                Ok(AuthoredDocument::Graph(graph))
            }
        }
    }

    async fn canonicalize_workspace_candidate(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: &super::MainState,
        workspace_head: &RevisionId,
        files: &FileMap,
    ) -> Result<(FileMap, AuthoredDocument)> {
        match &scope.document {
            DocumentScope::Track(track_scope) => {
                require_exact_paths(files, &[SCORE_PATH])?;
                let context = load_score_dsl_context(pool, track_scope)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                let imported =
                    compile_import_track_document(utf8_file(files, SCORE_PATH)?, &context, true)
                        .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let current = require_track_document(&main.document)?;
                let current_ids: HashSet<&str> =
                    current.clips.iter().map(|clip| clip.id.as_str()).collect();
                let needed = imported
                    .document
                    .clips
                    .iter()
                    .filter(|clip| {
                        !current_ids.contains(clip.id.as_str())
                            && !imported.host_allocated_ids.contains(&clip.id)
                    })
                    .map(|clip| clip.id.clone())
                    .collect::<BTreeSet<_>>();
                let mut connection = pool
                    .acquire()
                    .await
                    .map_err(storage("open workspace lineage"))?;
                let lineage = self
                    .track_lineage_ids(&mut connection, scope, workspace_head, &needed)
                    .await?;
                check_track_projection_candidate(
                    pool,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    &imported.document.clips,
                    TrackProjectionIdentity::Allowed {
                        lineage_ids: &lineage,
                        host_allocated_ids: &imported.host_allocated_ids,
                    },
                )
                .await?;
                Ok((
                    FileMap::from([(
                        SCORE_PATH.to_owned(),
                        imported.canonical_source.into_bytes(),
                    )]),
                    AuthoredDocument::Track(imported.document),
                ))
            }
            DocumentScope::Pattern(_) => {
                let document = self.decode_files(scope, files)?;
                let AuthoredDocument::Graph(mut graph) = document else {
                    unreachable!("pattern workspace decoded as score")
                };
                graph.graph = canonicalize_graph(&graph.graph)?;
                graph.revision = super::graph_revision(&graph.graph)?;
                let canonical = graph_files(&graph.graph)?;
                Ok((canonical, AuthoredDocument::Graph(graph)))
            }
        }
    }

    async fn replay_workspace_commit(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        row: &WorkspaceRow,
        operation: OperationSpec<'_>,
        outcome: OperationOutcomeRow,
    ) -> Result<AuthoredWorkspaceCommit> {
        require_outcome_fingerprint(&outcome, operation.fingerprint)?;
        if outcome.status != "committed" {
            return Err(AuthoredDocumentsError::Storage(
                "workspace commit has a conflicted outcome".into(),
            ));
        }
        let result = outcome.result_revision_id.ok_or_else(|| {
            AuthoredDocumentsError::Storage("workspace commit has no result revision".into())
        })?;
        let head = RevisionId::parse(&row.head_revision_id)?;
        let mut connection = pool
            .acquire()
            .await
            .map_err(storage("open workspace commit replay"))?;
        if !self
            .store
            .is_ancestor(&mut connection, &scope.document_id, &result, &head)
            .await?
        {
            return Err(AuthoredDocumentsError::Storage(
                "workspace commit result is no longer in workspace history".into(),
            ));
        }
        let (result_info, result_files) = self
            .store
            .read_revision(&mut connection, &scope.document_id, &result)
            .await?;
        let (_, head_files) = self
            .store
            .read_revision(&mut connection, &scope.document_id, &head)
            .await?;
        let document = self.decode_files(scope, &head_files)?;
        if result == head {
            replace_workspace_files(&self.workspace_path(scope, &row.workspace_id)?, &head_files)
                .await?;
        }
        let changed = match result_info.parents.first() {
            Some(parent) => {
                let (_, parent_files) = self
                    .store
                    .read_revision(&mut connection, &scope.document_id, parent)
                    .await?;
                parent_files != result_files
            }
            None => true,
        };
        Ok(AuthoredWorkspaceCommit {
            id: row.workspace_id.clone(),
            revision_id: result.to_string(),
            applied_to_current_workspace: result == head,
            changed,
            document: document.projected(),
        })
    }

    fn workspace_path(&self, scope: &ResolvedScope, workspace_id: &str) -> Result<PathBuf> {
        validate_workspace_id(workspace_id)?;
        Ok(self
            .storage
            .authored_workspace_dir(scope.document_id.as_str(), workspace_id))
    }
}

#[derive(Clone, Debug, FromRow)]
struct WorkspaceRow {
    workspace_id: String,
    request_fingerprint: String,
    document_id: String,
    owner_thread_id: String,
    base_revision_id: String,
    head_revision_id: String,
    status: String,
}

async fn load_workspace_row(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    thread_id: &str,
    workspace_id: Option<&str>,
    request_id: Option<&str>,
) -> Result<WorkspaceRow> {
    let (column, value) = match (workspace_id, request_id) {
        (Some(workspace_id), None) => {
            validate_workspace_id(workspace_id)?;
            ("workspace_id", workspace_id)
        }
        (None, Some(request_id)) => ("request_id", request_id),
        _ => {
            return Err(AuthoredDocumentsError::Storage(
                "workspace lookup requires exactly one identity".into(),
            ));
        }
    };
    let sql = format!(
        "SELECT workspace_id, request_fingerprint, document_id,
                owner_thread_id, base_revision_id, head_revision_id, status
         FROM authored_subagent_workspaces
         WHERE owner_thread_id = ? AND document_id = ? AND {column} = ?"
    );
    let row = sqlx::query_as::<_, WorkspaceRow>(sqlx::AssertSqlSafe(sql))
        .bind(thread_id)
        .bind(scope.document_id.as_str())
        .bind(value)
        .fetch_optional(&mut *connection)
        .await
        .map_err(storage("load authored workspace"))?
        .ok_or_else(|| {
            AuthoredDocumentsError::Scope(
                "authored workspace does not exist or belongs to another thread".into(),
            )
        })?;
    if row.document_id != scope.document_id.as_str() || row.owner_thread_id != thread_id {
        return Err(AuthoredDocumentsError::Scope(
            "authored workspace belongs to another document".into(),
        ));
    }
    Ok(row)
}

async fn retire_workspace_row(connection: &mut SqliteConnection, row: &WorkspaceRow) -> Result<()> {
    if row.status == "retired" {
        return Ok(());
    }
    let changed = sqlx::query(
        "UPDATE authored_subagent_workspaces
         SET status = 'retired', retired_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE workspace_id = ? AND owner_thread_id = ? AND status = 'active'",
    )
    .bind(&row.workspace_id)
    .bind(&row.owner_thread_id)
    .execute(&mut *connection)
    .await
    .map_err(storage("retire authored workspace"))?
    .rows_affected();
    if changed != 1 {
        return Err(AuthoredDocumentsError::Storage(
            "authored workspace changed during retirement".into(),
        ));
    }
    Ok(())
}

fn replay_workspace_merge(
    scope: &ResolvedScope,
    main: &super::MainState,
    operation: OperationSpec<'_>,
    outcome: OperationOutcomeRow,
) -> Result<AuthoredWorkspaceMerge> {
    require_outcome_fingerprint(&outcome, operation.fingerprint)?;
    match outcome.status.as_str() {
        "committed" => {
            let revision = outcome.result_revision_id.ok_or_else(|| {
                AuthoredDocumentsError::Storage("workspace merge has no result revision".into())
            })?;
            Ok(AuthoredWorkspaceMerge::Merged {
                document_id: scope.document_id.to_string(),
                revision_id: revision.to_string(),
                applied_to_current_projection: revision == main.head,
                document: main.document.projected(),
            })
        }
        "conflicted" => {
            let conflicts: Vec<AuthoredMergeConflict> =
                serde_json::from_str(outcome.conflicts_json.as_deref().ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "workspace merge conflict has no structured data".into(),
                    )
                })?)
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "decode workspace merge conflicts: {error}"
                    ))
                })?;
            Ok(AuthoredWorkspaceMerge::Conflicted { conflicts })
        }
        _ => Err(AuthoredDocumentsError::Storage(
            "workspace merge has an invalid outcome".into(),
        )),
    }
}

fn require_outcome_fingerprint(outcome: &OperationOutcomeRow, expected: &str) -> Result<()> {
    if outcome.request_fingerprint == expected {
        Ok(())
    } else {
        Err(AuthoredDocumentsError::Invalid(
            "operation id was already used with different workspace input".into(),
        ))
    }
}

fn require_track_document(document: &AuthoredDocument) -> Result<&TrackDocument> {
    match document {
        AuthoredDocument::Track(track) => Ok(track),
        AuthoredDocument::Graph(_) => Err(AuthoredDocumentsError::Storage(
            "score workspace resolved to a graph document".into(),
        )),
    }
}

fn required_paths(scope: &ResolvedScope) -> &'static [&'static str] {
    match &scope.document {
        DocumentScope::Track(_) => &[SCORE_PATH],
        DocumentScope::Pattern(_) => &[GRAPH_PATH, LAYOUT_PATH],
    }
}

fn require_workspace_file_name(scope: &ResolvedScope, file_name: &str) -> Result<()> {
    if required_paths(scope).contains(&file_name) {
        Ok(())
    } else {
        Err(AuthoredDocumentsError::Invalid(format!(
            "{file_name:?} is not an authored file for this document"
        )))
    }
}

fn validate_workspace_limits(files: &FileMap) -> Result<()> {
    if files.len() > MAX_WORKSPACE_FILES {
        return Err(AuthoredDocumentsError::Invalid(
            "authored workspace contains too many files".into(),
        ));
    }
    let mut total = 0_u64;
    for (name, bytes) in files {
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if len > MAX_WORKSPACE_FILE_BYTES {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace file {name} is too large"
            )));
        }
        total = total.checked_add(len).ok_or_else(|| {
            AuthoredDocumentsError::Invalid("workspace byte size overflow".into())
        })?;
        if total > MAX_WORKSPACE_BYTES {
            return Err(AuthoredDocumentsError::Invalid(
                "authored workspace is too large".into(),
            ));
        }
    }
    Ok(())
}

fn validate_workspace_id(workspace_id: &str) -> Result<()> {
    Uuid::parse_str(workspace_id)
        .map(|_| ())
        .map_err(|_| AuthoredDocumentsError::Invalid("workspace id must be a UUID".into()))
}

async fn read_workspace_files(path: &Path, expected: &[&str]) -> Result<FileMap> {
    let metadata = fs::symlink_metadata(path)
        .await
        .map_err(io_error("open authored workspace"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AuthoredDocumentsError::Invalid(
            "authored workspace path is not a plain directory".into(),
        ));
    }
    let mut directory = fs::read_dir(path)
        .await
        .map_err(io_error("read authored workspace"))?;
    let mut files = FileMap::new();
    let mut total = 0_u64;
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(io_error("read authored workspace entry"))?
    {
        if files.len() >= MAX_WORKSPACE_FILES {
            return Err(AuthoredDocumentsError::Invalid(
                "authored workspace contains too many files".into(),
            ));
        }
        let file_type = entry
            .file_type()
            .await
            .map_err(io_error("inspect authored workspace entry"))?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(AuthoredDocumentsError::Invalid(
                "authored workspace may contain only plain files".into(),
            ));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            AuthoredDocumentsError::Invalid("workspace file name is not UTF-8".into())
        })?;
        let metadata = entry
            .metadata()
            .await
            .map_err(io_error("inspect authored workspace file"))?;
        if metadata.len() > MAX_WORKSPACE_FILE_BYTES {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace file {name} is too large"
            )));
        }
        total = total.checked_add(metadata.len()).ok_or_else(|| {
            AuthoredDocumentsError::Invalid("workspace byte size overflow".into())
        })?;
        if total > MAX_WORKSPACE_BYTES {
            return Err(AuthoredDocumentsError::Invalid(
                "authored workspace is too large".into(),
            ));
        }
        let bytes = fs::read(entry.path())
            .await
            .map_err(io_error("read authored workspace file"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len() {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "workspace file {name} changed while it was read"
            )));
        }
        files.insert(name, bytes);
    }
    require_exact_paths(&files, expected)?;
    Ok(files)
}

async fn atomic_write_workspace_file(path: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let directory = path.to_owned();
    let target = path.join(name);
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut temporary = tempfile::Builder::new()
            .prefix(".authored-write-")
            .tempfile_in(&directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&target).map_err(|error| error.error)?;
        Ok(())
    })
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("join authored workspace write: {error}"))
    })?
    .map_err(io_error("write authored workspace file"))
}

async fn replace_workspace_files(path: &Path, files: &FileMap) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        AuthoredDocumentsError::Storage("authored workspace has no parent directory".into())
    })?;
    fs::create_dir_all(parent)
        .await
        .map_err(io_error("create authored workspace root"))?;
    let stage = parent.join(format!(".workspace-stage-{}", Uuid::new_v4()));
    fs::create_dir(&stage)
        .await
        .map_err(io_error("create authored workspace staging directory"))?;
    for (name, bytes) in files {
        if Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name) {
            remove_workspace_path(&stage).await?;
            return Err(AuthoredDocumentsError::Storage(
                "authored revision contains a nested workspace path".into(),
            ));
        }
        if let Err(error) = fs::write(stage.join(name), bytes).await {
            let _ = remove_workspace_path(&stage).await;
            return Err(AuthoredDocumentsError::Storage(format!(
                "write authored workspace file: {error}"
            )));
        }
    }
    remove_workspace_path(path).await?;
    if let Err(error) = fs::rename(&stage, path).await {
        let _ = remove_workspace_path(&stage).await;
        return Err(AuthoredDocumentsError::Storage(format!(
            "publish authored workspace files: {error}"
        )));
    }
    Ok(())
}

async fn remove_workspace_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(AuthoredDocumentsError::Storage(format!(
                "inspect authored workspace cleanup target: {error}"
            )));
        }
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
            .await
            .map_err(io_error("remove authored workspace directory"))
    } else {
        fs::remove_file(path)
            .await
            .map_err(io_error("remove authored workspace path"))
    }
}

fn storage(context: &'static str) -> impl Fn(sqlx::Error) -> AuthoredDocumentsError {
    move |error| AuthoredDocumentsError::Storage(format!("{context}: {error}"))
}

fn io_error(context: &'static str) -> impl Fn(std::io::Error) -> AuthoredDocumentsError {
    move |error| AuthoredDocumentsError::Storage(format!("{context}: {error}"))
}
