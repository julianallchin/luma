use super::{
    agent_threads, canonicalize_graph, check_track_projection_candidate,
    check_track_worktree_candidate, commit_changed, commit_message, compile_draft_track_document,
    compile_import_track_document, file_snapshot_id, find_commit_with_trailers, graph_files,
    graph_revision, is_valid_track_draft_id, load_score_dsl_context, normalized_subject,
    operation_association, operation_request_fingerprint, parse_trailers,
    record_operation_conflict, require_exact_paths, revision_for_clips, system_author, utf8_file,
    validate_ref_component, worktree_source_manifest, AuthoredDocument, AuthoredDocuments,
    AuthoredDocumentsError, AuthoredSnapshot, AuthoredStateError, AuthoredWorktree,
    AuthoredWorktreeCheck, AuthoredWorktreeCommit, AuthoredWorktreeMerge, BTreeSet,
    CommitAuthoredWorktreeInput, CommitId, CreateAuthoredWorktreeInput, DocumentScope, FileMap,
    FromRow, HashSet, MergeAuthoredWorktreeInput, OperationOutcome, OperationProjection,
    ProjectionLedgerExpectation, ProjectionMetadata, ResolvedScope, Result, SqlitePool,
    TrackDocument, TrackProjectionAuthority, TrackProjectionIdentity, WorktreeId, MAIN_BRANCH,
    SCORE_PATH, TRAILER_OPERATION, TRAILER_OPERATION_ID, TRAILER_REQUEST_FINGERPRINT,
    TRAILER_THREAD, TRAILER_WORKTREE, TRAILER_WORKTREE_HEAD, TRAILER_WORKTREE_SOURCE_MANIFEST,
};

impl AuthoredDocuments {
    pub async fn create_worktree(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: CreateAuthoredWorktreeInput,
    ) -> Result<AuthoredWorktree> {
        validate_ref_component(&input.request_id, "worktree request id")?;
        let expected_base = CommitId::parse(&input.expected_base_commit_id)?;
        let (thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        let main = self.reconcile_locked(pool, &scope).await?;
        if !self
            .store
            .first_parent_contains(&scope.repository_id, MAIN_BRANCH, &expected_base)?
        {
            return Err(AuthoredDocumentsError::Scope(
                "worktree base is not in this document's main history".into(),
            ));
        }
        let request_fingerprint = operation_request_fingerprint(
            "worktree_create",
            &[
                scope.repository_id.as_str(),
                &thread.id,
                expected_base.as_str(),
            ],
        );
        self.ensure_thread_branch_locked(pool, &scope, &main.head)
            .await?;
        let proposed_id = WorktreeId::new();
        let proposed_branch = format!("agents/worktrees/{}/{}", thread.id, proposed_id.as_str());
        sqlx::query(
            "INSERT INTO authored_state_worktrees
             (worktree_id, request_id, request_fingerprint, repository_id,
              owner_thread_id, branch_name, base_commit, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'preparing')
             ON CONFLICT(owner_thread_id, request_id) DO NOTHING",
        )
        .bind(proposed_id.as_str())
        .bind(&input.request_id)
        .bind(&request_fingerprint)
        .bind(scope.repository_id.as_str())
        .bind(&thread.id)
        .bind(&proposed_branch)
        .bind(expected_base.as_str())
        .execute(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("reserve authored worktree: {error}"))
        })?;
        let row = sqlx::query_as::<_, WorktreeRow>(
            "SELECT worktree_id, request_fingerprint, repository_id, branch_name,
                    base_commit, status
             FROM authored_state_worktrees
             WHERE owner_thread_id = ? AND request_id = ?",
        )
        .bind(&thread.id)
        .bind(&input.request_id)
        .fetch_one(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("load worktree reservation: {error}"))
        })?;
        if row.repository_id != scope.repository_id.as_str()
            || row.request_fingerprint != request_fingerprint
            || row.base_commit != expected_base.as_str()
        {
            return Err(AuthoredDocumentsError::Scope(
                "worktree request id is already bound to a different base or authored scope".into(),
            ));
        }
        if row.status == "retired" {
            return Err(AuthoredDocumentsError::Scope(
                "worktree request is retired".into(),
            ));
        }
        let id = WorktreeId::parse(&row.worktree_id)?;
        let base = CommitId::parse(&row.base_commit)?;
        match self
            .store
            .branch_head(&scope.repository_id, &row.branch_name)
        {
            Ok(_) => {}
            Err(AuthoredStateError::NotFound(_)) => {
                self.store
                    .create_branch(&scope.repository_id, &row.branch_name, &base)?;
            }
            Err(error) => return Err(error.into()),
        }
        let info = self
            .store
            .create_worktree(&scope.repository_id, &row.branch_name, &id)?;
        let activated = sqlx::query(
            "UPDATE authored_state_worktrees SET status = 'active'
             WHERE worktree_id = ? AND owner_thread_id = ? AND status IN ('preparing', 'active')",
        )
        .bind(id.as_str())
        .bind(&thread.id)
        .execute(pool)
        .await;
        let activation_error = match activated {
            Ok(result) if result.rows_affected() == 1 => None,
            Ok(_) => Some("thread deletion retired the worktree reservation".to_owned()),
            Err(error) => Some(format!("activate authored worktree: {error}")),
        };
        if let Some(activation_error) = activation_error {
            let cleanup =
                match self
                    .store
                    .remove_worktree(&scope.repository_id, &id, &row.branch_name, false)
                {
                    Ok(()) | Err(AuthoredStateError::NotFound(_)) => None,
                    Err(error) => Some(error.to_string()),
                };
            return Err(AuthoredDocumentsError::Storage(match cleanup {
                Some(cleanup) => {
                    format!("{activation_error}; compensate unpublished worktree failed: {cleanup}")
                }
                None => activation_error,
            }));
        }
        Ok(AuthoredWorktree {
            id: id.as_str().to_owned(),
            path: info.path.to_string_lossy().into_owned(),
            branch: row.branch_name,
            base_commit_id: base.to_string(),
            head_commit_id: info.head.to_string(),
        })
    }

    pub async fn check_worktree(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        worktree_id: &str,
    ) -> Result<AuthoredWorktreeCheck> {
        let (scope, row) = self
            .resolve_active_worktree(pool, principal, thread_id, worktree_id)
            .await?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        self.assert_active_thread_locked(pool, principal, thread_id, &scope.repository_id)
            .await?;
        let main = self.reconcile_locked(pool, &scope).await?;
        let id = WorktreeId::parse(&row.worktree_id)?;
        let files = self
            .store
            .read_worktree_files(&scope.repository_id, &id, &row.branch_name)?;
        let snapshot_id = file_snapshot_id(&files)?;
        let head = self
            .store
            .branch_head(&scope.repository_id, &row.branch_name)?;
        let (_, head_files) = self.store.read_commit(&scope.repository_id, &head)?;
        let document = match &scope.document {
            DocumentScope::Track(track_scope) => {
                require_exact_paths(&files, &[SCORE_PATH])?;
                let context = load_score_dsl_context(pool, track_scope)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                let (document, _) = compile_draft_track_document(
                    utf8_file(&files, SCORE_PATH)?,
                    String::new(),
                    &context.beat_grid,
                    &context.registry,
                )
                .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let current = match &main.document {
                    AuthoredDocument::Track(document) => document,
                    AuthoredDocument::Graph(_) => unreachable!("track scope reconciled as graph"),
                };
                let current_ids: HashSet<&str> =
                    current.clips.iter().map(|clip| clip.id.as_str()).collect();
                let requested_lineage_ids = document
                    .clips
                    .iter()
                    .filter(|clip| {
                        !is_valid_track_draft_id(&clip.id)
                            && !current_ids.contains(clip.id.as_str())
                    })
                    .map(|clip| clip.id.clone())
                    .collect::<BTreeSet<_>>();
                let lineage_ids =
                    self.track_lineage_ids(&scope, &row.branch_name, &requested_lineage_ids)?;
                check_track_worktree_candidate(
                    pool,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    &document.clips,
                    &lineage_ids,
                )
                .await?;
                AuthoredDocument::Track(TrackDocument {
                    revision: revision_for_clips(&document.clips),
                    clips: document.clips,
                })
            }
            DocumentScope::Pattern(_) => self.decode_files(&scope, &files)?,
        };
        Ok(AuthoredWorktreeCheck {
            id: row.worktree_id,
            head_commit_id: head.to_string(),
            snapshot_id,
            changed: files != head_files,
            document: document.projected(),
        })
    }

    pub async fn commit_worktree(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: CommitAuthoredWorktreeInput,
    ) -> Result<AuthoredWorktreeCommit> {
        validate_ref_component(&input.operation_id, "worktree commit operation id")?;
        let expected = CommitId::parse(&input.expected_head_commit_id)?;
        let subject = normalized_subject(&input.message)?;
        let request_fingerprint = operation_request_fingerprint(
            "worktree_commit",
            &[
                &input.worktree_id,
                expected.as_str(),
                &input.expected_snapshot_id,
                subject,
            ],
        );
        let (scope, row) = self
            .resolve_active_worktree(pool, principal, &input.thread_id, &input.worktree_id)
            .await?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        self.assert_active_thread_locked(pool, principal, &input.thread_id, &scope.repository_id)
            .await?;
        self.reconcile_locked(pool, &scope).await?;
        let id = WorktreeId::parse(&row.worktree_id)?;
        if let Some(existing) = find_commit_with_trailers(
            &self.store,
            &scope.repository_id,
            &row.branch_name,
            &[
                (TRAILER_WORKTREE, &input.worktree_id),
                (TRAILER_OPERATION_ID, &input.operation_id),
            ],
        )? {
            let metadata = parse_trailers(&existing.message);
            if metadata
                .get(TRAILER_REQUEST_FINGERPRINT)
                .map(String::as_str)
                != Some(request_fingerprint.as_str())
            {
                return Err(AuthoredDocumentsError::Scope(
                    "worktree commit operation id is already bound to another request".into(),
                ));
            }
            let actual = self
                .store
                .branch_head(&scope.repository_id, &row.branch_name)?;
            let is_current = existing.id == actual;
            if !is_current
                && !self
                    .store
                    .is_ancestor(&scope.repository_id, &existing.id, &actual)?
            {
                return Err(AuthoredDocumentsError::Storage(
                    "worktree operation commit is no longer in its branch history".into(),
                ));
            }
            let (_, files) = self.store.read_commit(&scope.repository_id, &actual)?;
            if is_current {
                let source_manifest =
                    metadata
                        .get(TRAILER_WORKTREE_SOURCE_MANIFEST)
                        .ok_or_else(|| {
                            AuthoredDocumentsError::Storage(
                                "worktree commit is missing its source manifest".into(),
                            )
                        })?;
                let recovered = self.store.recover_canonical_worktree_materialization(
                    &scope.repository_id,
                    &id,
                    &row.branch_name,
                    &actual,
                    source_manifest,
                )?;
                if !recovered {
                    return Err(AuthoredDocumentsError::Invalid(
                        "the operation committed, but the worktree contains newer uncommitted edits; canonical materialization recovery preserved them"
                            .into(),
                    ));
                }
            }
            let document = self.decode_files(&scope, &files)?;
            let changed = commit_changed(&self.store, &scope.repository_id, &existing)?;
            return Ok(AuthoredWorktreeCommit {
                id: row.worktree_id,
                commit_id: existing.id.to_string(),
                applied_to_current_worktree: is_current,
                changed,
                document: document.projected(),
            });
        }
        let actual = self
            .store
            .branch_head(&scope.repository_id, &row.branch_name)?;
        if actual != expected {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "worktree head changed (expected {expected}, current {actual})"
            )));
        }
        let (_, old_files) = self.store.read_commit(&scope.repository_id, &expected)?;
        let working_files =
            self.store
                .read_worktree_files(&scope.repository_id, &id, &row.branch_name)?;
        let actual_snapshot_id = file_snapshot_id(&working_files)?;
        if actual_snapshot_id != input.expected_snapshot_id {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "worktree files changed (expected snapshot {}, current {actual_snapshot_id})",
                input.expected_snapshot_id
            )));
        }
        let (files, document) = match &scope.document {
            DocumentScope::Track(track_scope) => {
                require_exact_paths(&working_files, &[SCORE_PATH])?;
                let context = load_score_dsl_context(pool, track_scope)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                let imported = compile_import_track_document(
                    utf8_file(&working_files, SCORE_PATH)?,
                    &context,
                    true,
                )
                .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let current = match self.decode_files(&scope, &old_files)? {
                    AuthoredDocument::Track(document) => document,
                    AuthoredDocument::Graph(_) => unreachable!("track repository decoded as graph"),
                };
                let current_ids: HashSet<&str> =
                    current.clips.iter().map(|clip| clip.id.as_str()).collect();
                let requested_lineage_ids: BTreeSet<String> = imported
                    .document
                    .clips
                    .iter()
                    .filter(|clip| {
                        !current_ids.contains(clip.id.as_str())
                            && !imported.host_allocated_ids.contains(&clip.id)
                    })
                    .map(|clip| clip.id.clone())
                    .collect();
                let lineage_ids =
                    self.track_lineage_ids(&scope, &row.branch_name, &requested_lineage_ids)?;
                check_track_projection_candidate(
                    pool,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    &imported.document.clips,
                    TrackProjectionIdentity::Allowed {
                        lineage_ids: &lineage_ids,
                        host_allocated_ids: &imported.host_allocated_ids,
                    },
                )
                .await?;
                let canonical = FileMap::from([(
                    SCORE_PATH.to_owned(),
                    imported.canonical_source.into_bytes(),
                )]);
                (canonical, AuthoredDocument::Track(imported.document))
            }
            DocumentScope::Pattern(_) => {
                let mut document = self.decode_files(&scope, &working_files)?;
                let canonical = match &mut document {
                    AuthoredDocument::Graph(graph) => {
                        graph.graph = canonicalize_graph(&graph.graph)?;
                        graph.revision = graph_revision(&graph.graph)?;
                        graph_files(&graph.graph)?
                    }
                    _ => unreachable!("graph repository decoded as score"),
                };
                (canonical, document)
            }
        };
        let source_manifest = worktree_source_manifest(&working_files)?;
        let message = commit_message(
            subject,
            &[
                (TRAILER_OPERATION, "worktree_commit"),
                (TRAILER_THREAD, &input.thread_id),
                (TRAILER_WORKTREE, &input.worktree_id),
                (TRAILER_OPERATION_ID, &input.operation_id),
                (TRAILER_WORKTREE_SOURCE_MANIFEST, &source_manifest),
                (TRAILER_REQUEST_FINGERPRINT, &request_fingerprint),
            ],
        )?;
        let commit = self.store.commit_worktree_files(
            &scope.repository_id,
            &id,
            &row.branch_name,
            &expected,
            &working_files,
            &files,
            &system_author()?,
            &message,
        )?;
        Ok(AuthoredWorktreeCommit {
            id: row.worktree_id,
            commit_id: commit.id.to_string(),
            applied_to_current_worktree: true,
            changed: files != old_files,
            document: document.projected(),
        })
    }

    pub async fn merge_worktree(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: MergeAuthoredWorktreeInput,
    ) -> Result<AuthoredWorktreeMerge> {
        validate_ref_component(&input.operation_id, "worktree merge operation id")?;
        let expected_theirs = CommitId::parse(&input.expected_head_commit_id)?;
        let request_fingerprint = operation_request_fingerprint(
            "worktree_merge",
            &[&input.worktree_id, expected_theirs.as_str()],
        );
        let (scope, row) = self
            .resolve_active_worktree(pool, principal, &input.thread_id, &input.worktree_id)
            .await?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        self.assert_active_thread_locked(pool, principal, &input.thread_id, &scope.repository_id)
            .await?;
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(existing) = operation_association(
            pool,
            &scope.repository_id,
            "worktree_merge",
            &input.operation_id,
        )
        .await?
        {
            if existing.request_fingerprint != request_fingerprint {
                return Err(AuthoredDocumentsError::Scope(
                    "worktree merge operation id is already bound to another source".into(),
                ));
            }
            if !self.store.is_ancestor(
                &scope.repository_id,
                &existing.base_main_commit,
                &main.head,
            )? {
                return Err(AuthoredDocumentsError::Storage(
                    "worktree merge base is no longer in main history".into(),
                ));
            }
            return match existing.outcome {
                OperationOutcome::Committed(commit) => {
                    if !self
                        .store
                        .is_ancestor(&scope.repository_id, &commit, &main.head)?
                    {
                        return Err(AuthoredDocumentsError::Storage(
                            "worktree merge operation commit is no longer in main history".into(),
                        ));
                    }
                    Ok(AuthoredWorktreeMerge::Merged {
                        repository_id: scope.repository_id.to_string(),
                        commit_id: commit.to_string(),
                        applied_to_current_projection: commit == main.head,
                        document: main.document.projected(),
                    })
                }
                OperationOutcome::Conflicted(conflicts) => {
                    Ok(AuthoredWorktreeMerge::Conflicted { conflicts })
                }
            };
        }
        let id = WorktreeId::parse(&row.worktree_id)?;
        let theirs = self
            .store
            .branch_head(&scope.repository_id, &row.branch_name)?;
        if theirs != expected_theirs {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "worktree head changed (expected {expected_theirs}, current {theirs})"
            )));
        }
        let working_files =
            self.store
                .read_worktree_files(&scope.repository_id, &id, &row.branch_name)?;
        let (_, theirs_files) = self.store.read_commit(&scope.repository_id, &theirs)?;
        if working_files != theirs_files {
            return Err(AuthoredDocumentsError::Invalid(
                "worktree has uncommitted changes; commit it before merging".into(),
            ));
        }
        let expected_projection_revision = main.document.revision().to_owned();
        let (merged_document, files) =
            if self
                .store
                .is_ancestor(&scope.repository_id, &theirs, &main.head)?
            {
                (main.document.clone(), main.files.clone())
            } else {
                let base = self
                    .store
                    .merge_base(&scope.repository_id, &main.head, &theirs)?;
                let base_snapshot = self.snapshot_from_commit(&scope, &base)?;
                let ours_snapshot = AuthoredSnapshot {
                    files: main.files.clone(),
                    document: main.document.clone(),
                };
                let their_snapshot = self.snapshot_from_commit(&scope, &theirs)?;
                match self
                    .merge_snapshots(
                        pool,
                        &scope,
                        &base_snapshot,
                        &ours_snapshot,
                        &their_snapshot,
                    )
                    .await?
                {
                    Ok(merged) => merged,
                    Err(conflicts) => {
                        record_operation_conflict(
                            pool,
                            &scope,
                            &main.head,
                            "worktree_merge",
                            &input.operation_id,
                            &request_fingerprint,
                            &conflicts,
                        )
                        .await?;
                        return Ok(AuthoredWorktreeMerge::Conflicted { conflicts });
                    }
                }
            };
        let message = commit_message(
            "Merge authored worktree",
            &[
                (TRAILER_OPERATION, "worktree_merge"),
                (TRAILER_THREAD, &input.thread_id),
                (TRAILER_WORKTREE, &input.worktree_id),
                (TRAILER_WORKTREE_HEAD, theirs.as_str()),
                (TRAILER_OPERATION_ID, &input.operation_id),
            ],
        )?;
        let parents = if main.head == theirs {
            vec![main.head.clone()]
        } else {
            vec![main.head.clone(), theirs]
        };
        let prepared = self.store.prepare_commit(
            &scope.repository_id,
            &parents,
            &files,
            &system_author()?,
            &message,
        )?;
        let projected = self
            .project_prepared(
                pool,
                &scope,
                &main.head,
                ProjectionLedgerExpectation::PresentAt(&main.head),
                &prepared,
                merged_document,
                &expected_projection_revision,
                TrackProjectionAuthority::TrustedRepositoryTree,
                ProjectionMetadata {
                    operation: Some(OperationProjection {
                        kind: "worktree_merge",
                        operation_id: input.operation_id,
                        request_fingerprint,
                        base_main_commit: main.head.clone(),
                        result_json: None,
                    }),
                    ..ProjectionMetadata::default()
                },
            )
            .await?;
        Ok(AuthoredWorktreeMerge::Merged {
            repository_id: scope.repository_id.to_string(),
            commit_id: prepared.id.to_string(),
            applied_to_current_projection: true,
            document: projected.document,
        })
    }

    pub async fn remove_worktree(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        worktree_id: &str,
    ) -> Result<()> {
        let thread = agent_threads::get_thread_row(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        let row = sqlx::query_as::<_, WorktreeRow>(
            "SELECT worktree_id, request_fingerprint, repository_id, branch_name,
                    base_commit, status
             FROM authored_state_worktrees
             WHERE worktree_id = ? AND owner_thread_id = ?",
        )
        .bind(worktree_id)
        .bind(thread_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| AuthoredDocumentsError::Storage(format!("load worktree: {error}")))?
        .ok_or_else(|| {
            AuthoredDocumentsError::Scope(
                "worktree does not exist or belongs to another thread".into(),
            )
        })?;
        if row.repository_id != scope.repository_id.as_str() {
            return Err(AuthoredDocumentsError::Scope(
                "worktree belongs to another authored repository".into(),
            ));
        }
        let _guard = self.repository_guard(&scope.repository_id).await;
        self.assert_active_thread_locked(pool, principal, thread_id, &scope.repository_id)
            .await?;
        self.remove_worktree_locked(pool, &scope, thread_id, &row, false)
            .await
    }

    async fn remove_worktree_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        thread_id: &str,
        row: &WorktreeRow,
        archive_dirty: bool,
    ) -> Result<()> {
        if row.status == "retired" {
            return Ok(());
        }
        let id = WorktreeId::parse(&row.worktree_id)?;
        let removal = if archive_dirty {
            let message = commit_message(
                "Archive worktree before thread deletion",
                &[
                    (TRAILER_OPERATION, "thread_delete_archive"),
                    (TRAILER_THREAD, thread_id),
                    (TRAILER_WORKTREE, &row.worktree_id),
                ],
            )?;
            self.store
                .archive_and_remove_worktree(
                    &scope.repository_id,
                    &id,
                    &row.branch_name,
                    &system_author()?,
                    &message,
                )
                .map(|_| ())
        } else {
            self.store
                .remove_worktree(&scope.repository_id, &id, &row.branch_name, false)
        };
        match removal {
            Ok(()) | Err(AuthoredStateError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
        let changed = sqlx::query(
            "UPDATE authored_state_worktrees
             SET status = 'retired', retired_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE worktree_id = ? AND owner_thread_id = ? AND status IN ('preparing', 'active')",
        )
        .bind(&row.worktree_id)
        .bind(thread_id)
        .execute(pool)
        .await
        .map_err(|error| AuthoredDocumentsError::Storage(format!("retire worktree: {error}")))?
        .rows_affected();
        if changed == 0 {
            let status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM authored_state_worktrees
                 WHERE worktree_id = ? AND owner_thread_id = ?",
            )
            .bind(&row.worktree_id)
            .bind(thread_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!("verify retired worktree: {error}"))
            })?;
            if status.as_deref() == Some("retired") {
                return Ok(());
            }
            return Err(AuthoredDocumentsError::Storage(
                "worktree metadata changed during retirement".into(),
            ));
        }
        Ok(())
    }

    /// Remove all linked worktree projections owned by a thread before its
    /// transcript is deleted. Branches remain as Git history; metadata is
    /// retired so no other thread can claim them.
    pub(super) async fn retire_thread_worktrees_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        thread_id: &str,
    ) -> Result<()> {
        let rows = sqlx::query_as::<_, WorktreeRow>(
            "SELECT worktree_id, request_fingerprint, repository_id, branch_name,
                    base_commit, status
             FROM authored_state_worktrees
             WHERE owner_thread_id = ? AND status IN ('preparing', 'active') ORDER BY worktree_id",
        )
        .bind(thread_id)
        .fetch_all(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("list thread worktrees: {error}"))
        })?;
        for row in rows {
            if row.repository_id != scope.repository_id.as_str() {
                return Err(AuthoredDocumentsError::Scope(
                    "thread owns worktree metadata for another authored repository".into(),
                ));
            }
            self.remove_worktree_locked(pool, scope, thread_id, &row, true)
                .await?;
        }
        Ok(())
    }

    async fn resolve_active_worktree(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        worktree_id: &str,
    ) -> Result<(ResolvedScope, WorktreeRow)> {
        let thread = agent_threads::get_thread_row(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        let row = sqlx::query_as::<_, WorktreeRow>(
            "SELECT worktree_id, request_fingerprint, repository_id, branch_name,
                    base_commit, status
             FROM authored_state_worktrees
             WHERE worktree_id = ? AND owner_thread_id = ? AND status = 'active'",
        )
        .bind(worktree_id)
        .bind(thread_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| AuthoredDocumentsError::Storage(format!("load worktree: {error}")))?
        .ok_or_else(|| {
            AuthoredDocumentsError::Scope(
                "active worktree does not exist or belongs to another thread".into(),
            )
        })?;
        if row.repository_id != scope.repository_id.as_str() {
            return Err(AuthoredDocumentsError::Scope(
                "worktree belongs to another authored repository".into(),
            ));
        }
        Ok((scope, row))
    }
}

#[derive(FromRow)]
struct WorktreeRow {
    worktree_id: String,
    request_fingerprint: String,
    repository_id: String,
    branch_name: String,
    base_commit: String,
    status: String,
}
