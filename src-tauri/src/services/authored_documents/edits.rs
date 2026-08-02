use super::{
    check_track_projection_candidate, commit_changed, commit_message,
    compile_import_track_document, exact_graph_json, file_snapshot_id, graph_files,
    is_valid_track_draft_id, load_creation_association, load_ledger, load_score_dsl_context,
    load_score_pattern_names, load_unscoped_graph_document_for_connection,
    normalized_creation_request_id, operation_association, operation_association_for_connection,
    operation_request_fingerprint, plan_track_snapshot_replacement, principal_key,
    remap_track_snapshot_result, revision_for_clips, serialize_track, system_author,
    track_clips_semantically_equal, utf8_file, validate_ref_component,
    validate_track_draft_envelope, write_ledger, write_operation_association, AppliedAuthoredState,
    AppliedAuthoredTrackEdit, AuthoredDocument, AuthoredDocuments, AuthoredDocumentsError,
    BTreeMap, BTreeSet, BlendMode, CommitId, CommitInfo, CommittedOperationReplay,
    CreateTrackScoreInput, CreationAssociation, CreationProjection, DeleteTrackScoreInput,
    Deserialize, Digest, DocumentScope, FileMap, ForkPatternInput, ForkPatternResult, Graph,
    GraphDocument, HashMap, HashSet, MainState, OperationAssociation, OperationOutcome,
    OperationProjection, PatternSummary, ProjectionLedgerExpectation, ProjectionMetadata,
    ResolvedScope, Result, Serialize, Sha256, SqlitePool, TrackClip, TrackDocument, TrackEditError,
    TrackEditPlan, TrackEditResult, TrackProjectionAuthority, TrackProjectionIdentity, TrackScope,
    TrackScore, UpdateTrackScoreInput, Uuid, MAIN_BRANCH, PATTERN_SUMMARY_COLUMNS, SCORE_PATH,
    TRAILER_FORK_SOURCE_IMPLEMENTATION, TRAILER_FORK_SOURCE_PATTERN, TRAILER_OPERATION,
    TRAILER_OPERATION_ID, TRAILER_REQUEST_FINGERPRINT,
};

impl AuthoredDocuments {
    /// Human/manual graph saves use the same Git/projection transaction as an
    /// agent turn, but make an ordinary one-parent main commit.
    pub async fn apply_graph_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        pattern_id: &str,
        implementation_id: &str,
        operation_id: &str,
        graph: Graph,
        expected_revision: &str,
        subject: &str,
    ) -> Result<AppliedAuthoredState> {
        validate_ref_component(operation_id, "graph edit operation id")?;
        let scope = ResolvedScope::pattern(principal, pattern_id, implementation_id)?;
        let files = graph_files(&graph)?;
        let candidate_snapshot = file_snapshot_id(&files)?;
        let request_fingerprint = operation_request_fingerprint(
            "graph_edit",
            &[
                pattern_id,
                implementation_id,
                expected_revision,
                &candidate_snapshot,
            ],
        );
        let _guard = self.repository_guard(&scope.repository_id).await;
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(replayed) = self
            .replay_committed_operation(
                pool,
                &scope,
                &main,
                "graph_edit",
                "graph edit",
                operation_id,
                &request_fingerprint,
            )
            .await?
        {
            return self.applied_state_for_replay(&scope, &main, &replayed);
        }
        let candidate = self.decode_files(&scope, &files)?;
        let base_main_commit = main.head.clone();
        self.apply_direct_locked(
            pool,
            &scope,
            main,
            files,
            candidate,
            expected_revision,
            TrackProjectionAuthority::ExistingOnly,
            subject,
            ProjectionMetadata {
                operation: Some(OperationProjection {
                    kind: "graph_edit",
                    operation_id: operation_id.to_owned(),
                    request_fingerprint,
                    base_main_commit,
                    result_json: None,
                }),
                ..ProjectionMetadata::default()
            },
        )
        .await
    }

    /// Fork one exact graph implementation into a new, independently authored
    /// pattern. Target identities are derived from the caller's durable
    /// request ID, so recovery never has to guess which partially completed
    /// pattern belongs to an operation.
    pub async fn fork_pattern(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: ForkPatternInput,
    ) -> Result<ForkPatternResult> {
        validate_ref_component(&input.request_id, "pattern fork request id")?;
        let principal_key = principal_key(principal);
        let target_pattern_id =
            pattern_fork_target_id(&principal_key, &input.request_id, "pattern");
        let target_implementation_id =
            pattern_fork_target_id(&principal_key, &input.request_id, "implementation");
        let target_scope =
            ResolvedScope::pattern(principal, &target_pattern_id, &target_implementation_id)?;
        let request_fingerprint = operation_request_fingerprint(
            "pattern_fork",
            &[
                &principal_key,
                &input.source_pattern_id,
                &input.source_implementation_id,
            ],
        );
        let _guard = self.repository_guard(&target_scope.repository_id).await;

        if let Some(existing) = operation_association(
            pool,
            &target_scope.repository_id,
            "pattern_fork",
            &input.request_id,
        )
        .await?
        {
            return self
                .finish_pattern_fork(pool, &target_scope, &input, &request_fingerprint, existing)
                .await;
        }

        let source = load_pattern_fork_source(
            pool,
            &input.source_pattern_id,
            &input.source_implementation_id,
        )
        .await?;
        let repository = self.store.ensure_repository(&target_scope.repository_id)?;
        if load_ledger(pool, &target_scope).await?.is_some() {
            return Err(AuthoredDocumentsError::Storage(
                "pattern fork target has a projection ledger without its durable operation".into(),
            ));
        }
        let (_, base_files) = self
            .store
            .read_commit(&target_scope.repository_id, &repository.main_head)?;
        if !base_files.is_empty() {
            return Err(AuthoredDocumentsError::Storage(
                "pattern fork target repository already contains authored files".into(),
            ));
        }

        let files = graph_files(&source.graph.graph)?;
        let message = commit_message(
            "Fork pattern graph",
            &[
                (TRAILER_OPERATION, "pattern_fork"),
                (TRAILER_OPERATION_ID, &input.request_id),
                (TRAILER_FORK_SOURCE_PATTERN, &input.source_pattern_id),
                (
                    TRAILER_FORK_SOURCE_IMPLEMENTATION,
                    &input.source_implementation_id,
                ),
                (TRAILER_REQUEST_FINGERPRINT, &request_fingerprint),
            ],
        )?;
        let prepared = self.store.prepare_commit(
            &target_scope.repository_id,
            std::slice::from_ref(&repository.main_head),
            &files,
            &system_author()?,
            &message,
        )?;
        let existing = self
            .project_pattern_fork_sqlite(
                pool,
                principal,
                &target_scope,
                &input,
                &source,
                &repository.main_head,
                &prepared,
                &request_fingerprint,
            )
            .await?;
        if let Some(existing) = existing {
            return self
                .finish_pattern_fork(pool, &target_scope, &input, &request_fingerprint, existing)
                .await;
        }
        self.store.advance_branch(
            &target_scope.repository_id,
            MAIN_BRANCH,
            &repository.main_head,
            &prepared.id,
        )?;
        self.finish_pattern_fork(
            pool,
            &target_scope,
            &input,
            &request_fingerprint,
            OperationAssociation {
                request_fingerprint: request_fingerprint.clone(),
                base_main_commit: repository.main_head,
                outcome: OperationOutcome::Committed(prepared.id),
                result_json: None,
            },
        )
        .await
    }

    pub(super) async fn project_pattern_fork_sqlite(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        target_scope: &ResolvedScope,
        input: &ForkPatternInput,
        source: &PatternForkSource,
        base_main_commit: &CommitId,
        prepared: &CommitInfo,
        request_fingerprint: &str,
    ) -> Result<Option<OperationAssociation>> {
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin pattern fork: {error}"))
        })?;
        if let Some(existing) = operation_association_for_connection(
            &mut transaction,
            &target_scope.repository_id,
            "pattern_fork",
            &input.request_id,
        )
        .await?
        {
            transaction.rollback().await.map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "release replayed pattern fork transaction: {error}"
                ))
            })?;
            return Ok(Some(existing));
        }

        let graph_json = exact_graph_json(&source.graph.graph)?;
        sqlx::query(
            "INSERT INTO patterns (id, uid, name, description, forked_from_id)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&target_scope.subject_id)
        .bind(principal)
        .bind(format!("{}_fork", source.pattern.name))
        .bind(&source.pattern.description)
        .bind(&input.source_pattern_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("insert forked pattern: {error}"))
        })?;
        let target_graph_scope = match &target_scope.document {
            DocumentScope::Pattern(scope) => scope,
            DocumentScope::Track(_) => unreachable!("pattern fork target is always a graph"),
        };
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
             VALUES (?, ?, ?, NULL, ?)",
        )
        .bind(&target_graph_scope.implementation_id)
        .bind(principal)
        .bind(&target_graph_scope.pattern_id)
        .bind(graph_json)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("insert forked implementation: {error}"))
        })?;
        write_ledger(
            &mut transaction,
            target_scope,
            ProjectionLedgerExpectation::Missing,
            &prepared.id,
        )
        .await?;
        write_operation_association(
            &mut transaction,
            target_scope,
            &prepared.id,
            &OperationProjection {
                kind: "pattern_fork",
                operation_id: input.request_id.clone(),
                request_fingerprint: request_fingerprint.to_owned(),
                base_main_commit: base_main_commit.clone(),
                result_json: None,
            },
        )
        .await?;
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("commit pattern fork: {error}"))
        })?;
        Ok(None)
    }

    async fn finish_pattern_fork(
        &self,
        pool: &SqlitePool,
        target_scope: &ResolvedScope,
        input: &ForkPatternInput,
        request_fingerprint: &str,
        existing: OperationAssociation,
    ) -> Result<ForkPatternResult> {
        if existing.request_fingerprint != request_fingerprint {
            return Err(AuthoredDocumentsError::Scope(
                "pattern fork request id is already bound to another source implementation".into(),
            ));
        }
        let OperationOutcome::Committed(commit) = existing.outcome else {
            return Err(AuthoredDocumentsError::Storage(
                "pattern fork operation has a conflicted outcome".into(),
            ));
        };
        let main = self.reconcile_locked(pool, target_scope).await?;
        if !self.store.is_ancestor(
            &target_scope.repository_id,
            &existing.base_main_commit,
            &main.head,
        )? || !self
            .store
            .is_ancestor(&target_scope.repository_id, &commit, &main.head)?
        {
            return Err(AuthoredDocumentsError::Storage(
                "pattern fork operation is no longer in target main history".into(),
            ));
        }
        let pattern = load_forked_pattern(pool, target_scope, &input.source_pattern_id).await?;
        let implementation_id = match &target_scope.document {
            DocumentScope::Pattern(scope) => scope.implementation_id.clone(),
            DocumentScope::Track(_) => unreachable!("pattern fork target is always a graph"),
        };
        Ok(ForkPatternResult {
            pattern,
            implementation_id,
            repository_id: target_scope.repository_id.to_string(),
            commit_id: commit.to_string(),
            applied_to_current_projection: commit == main.head,
        })
    }

    /// Human/imported score source. Missing clip IDs are allocated before the
    /// canonical Git file is written; supplied UUIDs remain stable and are
    /// ownership-checked by the projection service.
    pub async fn apply_score_source_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        operation_id: &str,
        source: &str,
        expected_revision: &str,
        subject: &str,
    ) -> Result<AppliedAuthoredState> {
        validate_ref_component(operation_id, "score edit operation id")?;
        let request_fingerprint = operation_request_fingerprint(
            "score_dsl_import",
            &[
                &track_scope.score_id,
                &track_scope.track_id,
                &track_scope.venue_id,
                expected_revision,
                source,
            ],
        );
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(replayed) = self
            .replay_committed_operation(
                pool,
                &scope,
                &main,
                "score_edit",
                "score edit",
                operation_id,
                &request_fingerprint,
            )
            .await?
        {
            return self.applied_state_for_replay(&scope, &main, &replayed);
        }
        let context = load_score_dsl_context(pool, scope.track_scope().expect("track scope"))
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        let imported = compile_import_track_document(source, &context, true)
            .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
        let authority =
            TrackProjectionAuthority::HostAllocated(imported.host_allocated_ids.clone());
        let files = FileMap::from([(
            SCORE_PATH.to_owned(),
            imported.canonical_source.into_bytes(),
        )]);
        let current = match &main.document {
            AuthoredDocument::Track(document) => document,
            AuthoredDocument::Graph(_) => unreachable!("score import resolved as graph"),
        };
        let edit = track_edit_result(&current.clips, &imported.document.clips, BTreeMap::new());
        let result_json = serde_json::to_string(&ScoreEditOperationResult {
            id_map: BTreeMap::new(),
            added: edit.added,
            updated: edit.updated,
            removed: edit.removed,
        })
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("encode durable score import result: {error}"))
        })?;
        let base_main_commit = main.head.clone();
        self.apply_direct_locked(
            pool,
            &scope,
            main,
            files,
            AuthoredDocument::Track(imported.document),
            expected_revision,
            authority,
            subject,
            ProjectionMetadata {
                operation: Some(OperationProjection {
                    kind: "score_edit",
                    operation_id: operation_id.to_owned(),
                    request_fingerprint,
                    base_main_commit,
                    result_json: Some(result_json),
                }),
                ..ProjectionMetadata::default()
            },
        )
        .await
    }

    /// The durable thread, not Python, supplies mutation authority. Holding the thread repository
    /// gate through projection means a cell that loses a race with deletion
    /// cannot publish stale work after its conversation has been retired.
    pub(crate) async fn replay_track_edit_for_thread(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        expected_track_scope: &TrackScope,
        operation_id: &str,
        request_fingerprint: &str,
    ) -> Result<Option<AppliedAuthoredTrackEdit>> {
        validate_ref_component(operation_id, "Python score edit operation id")?;
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        if scope.track_scope() != Some(expected_track_scope) {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread does not own the requested track score scope".into(),
            ));
        }
        let main = self.reconcile_locked(pool, &scope).await?;
        self.replay_score_edit_operation(pool, &scope, &main, operation_id, request_fingerprint)
            .await
    }

    /// Apply a new Python score edit, or return its durable result when the
    /// exact operation reached Git before its host response was lost.
    pub async fn apply_track_edit_for_thread(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        expected_track_scope: &TrackScope,
        operation_id: &str,
        request_fingerprint: &str,
        plan: TrackEditPlan,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        validate_ref_component(operation_id, "Python score edit operation id")?;
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        if scope.track_scope() != Some(expected_track_scope) {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread does not own the requested track score scope".into(),
            ));
        }
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(replayed) = self
            .replay_score_edit_operation(pool, &scope, &main, operation_id, request_fingerprint)
            .await?
        {
            return Ok(replayed);
        }
        self.apply_track_edit_locked(
            pool,
            &scope,
            main,
            plan,
            subject,
            PendingTrackEditMetadata {
                operation: Some(PendingScoreEditOperation {
                    operation_id: operation_id.to_owned(),
                    request_fingerprint: request_fingerprint.to_owned(),
                    client_ids_by_draft: BTreeMap::new(),
                }),
                ..PendingTrackEditMetadata::default()
            },
        )
        .await
    }

    /// Lossless undo/redo/full-document adapter for persistence-shaped UI
    /// snapshots. New client IDs become drafts before entering the Git
    /// authority, and the result map is translated back to client IDs.
    pub async fn replace_track_scores_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        base: &[TrackScore],
        candidate: &[TrackScore],
        operation_id: &str,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        validate_ref_component(operation_id, "score edit operation id")?;
        let request_fingerprint = score_edit_request_fingerprint(
            "replace",
            &track_scope,
            &serde_json::json!({ "base": base, "candidate": candidate }),
        )?;
        let replacement = plan_track_snapshot_replacement(&track_scope.score_id, base, candidate)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(replayed) = self
            .replay_score_edit_operation(pool, &scope, &main, operation_id, &request_fingerprint)
            .await?
        {
            return Ok(replayed);
        }
        self.apply_track_edit_locked(
            pool,
            &scope,
            main,
            replacement.plan,
            subject,
            PendingTrackEditMetadata {
                operation: Some(PendingScoreEditOperation {
                    operation_id: operation_id.to_owned(),
                    request_fingerprint,
                    client_ids_by_draft: replacement.client_ids_by_draft,
                }),
                ..PendingTrackEditMetadata::default()
            },
        )
        .await
    }

    /// Resolve an exact score-edit retry before inspecting any row that the
    /// original operation may have deleted. The operation's correlation result
    /// is durable SQLite metadata; the current document always comes from the
    /// reconciled Git main so a late retry cannot rewind a newer projection.
    async fn replay_score_edit_operation(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: &MainState,
        operation_id: &str,
        request_fingerprint: &str,
    ) -> Result<Option<AppliedAuthoredTrackEdit>> {
        let Some(existing) = self
            .replay_committed_operation(
                pool,
                scope,
                main,
                "score_edit",
                "score edit",
                operation_id,
                request_fingerprint,
            )
            .await?
        else {
            return Ok(None);
        };
        let result: ScoreEditOperationResult =
            serde_json::from_str(existing.result_json.as_deref().ok_or_else(|| {
                AuthoredDocumentsError::Storage(
                    "score edit operation is missing its durable result".into(),
                )
            })?)
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "decode durable score edit result: {error}"
                ))
            })?;
        let current = match &main.document {
            AuthoredDocument::Track(document) => document,
            AuthoredDocument::Graph(_) => unreachable!("score edit resolved as graph"),
        };
        let (operation_commit, _) = self
            .store
            .read_commit(&scope.repository_id, &existing.commit)?;
        Ok(Some(AppliedAuthoredTrackEdit {
            authored: AppliedAuthoredState {
                repository_id: scope.repository_id.to_string(),
                commit_id: existing.commit.to_string(),
                changed: commit_changed(&self.store, &scope.repository_id, &operation_commit)?,
                document: main.document.projected(),
            },
            edit: TrackEditResult {
                revision: current.revision.clone(),
                clips: current.clips.clone(),
                id_map: result.id_map,
                created_clip_id: None,
                added: result.added,
                updated: result.updated,
                removed: result.removed,
                applied_to_current_projection: existing.commit == main.head,
            },
        }))
    }

    async fn replay_committed_operation(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: &MainState,
        kind: &str,
        label: &str,
        operation_id: &str,
        request_fingerprint: &str,
    ) -> Result<Option<CommittedOperationReplay>> {
        let Some(existing) =
            operation_association(pool, &scope.repository_id, kind, operation_id).await?
        else {
            return Ok(None);
        };
        if existing.request_fingerprint != request_fingerprint {
            return Err(AuthoredDocumentsError::Scope(format!(
                "{label} operation id is already bound to different content"
            )));
        }
        let OperationOutcome::Committed(commit) = existing.outcome else {
            return Err(AuthoredDocumentsError::Storage(format!(
                "{label} operation has a conflicted outcome"
            )));
        };
        if !self
            .store
            .is_ancestor(&scope.repository_id, &existing.base_main_commit, &main.head)?
            || !self
                .store
                .is_ancestor(&scope.repository_id, &commit, &main.head)?
        {
            return Err(AuthoredDocumentsError::Storage(format!(
                "{label} operation is no longer in main history"
            )));
        }
        Ok(Some(CommittedOperationReplay {
            commit,
            result_json: existing.result_json,
        }))
    }

    fn applied_state_for_replay(
        &self,
        scope: &ResolvedScope,
        main: &MainState,
        replayed: &CommittedOperationReplay,
    ) -> Result<AppliedAuthoredState> {
        let (operation_commit, _) = self
            .store
            .read_commit(&scope.repository_id, &replayed.commit)?;
        Ok(AppliedAuthoredState {
            repository_id: scope.repository_id.to_string(),
            commit_id: replayed.commit.to_string(),
            changed: commit_changed(&self.store, &scope.repository_id, &operation_commit)?,
            document: main.document.projected(),
        })
    }

    /// Create one clip as an atomic read-modify-commit under the repository
    /// lock. Concurrent partial edits therefore compose instead of racing on a
    /// client-loaded full document.
    pub async fn create_track_score_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        payload: CreateTrackScoreInput,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        assert_track_payload_scope(&track_scope, &payload.score_id, &payload.track_id)?;
        let request_id = normalized_creation_request_id(&payload.request_id)?;
        let blend_mode = payload.blend_mode.clone().unwrap_or(BlendMode::Replace);
        let args = payload
            .args
            .clone()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        let request_fingerprint =
            track_clip_creation_fingerprint(&track_scope, &payload, &blend_mode, &args)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        if let Some(existing) =
            load_creation_association(pool, &scope.principal_key, "track_clip", &request_id).await?
        {
            let clip_id = verify_track_clip_creation_replay(
                &existing,
                &request_fingerprint,
                scope.track_scope().expect("track scope").score_id.as_str(),
            )?;
            let main = self.reconcile_locked(pool, &scope).await?;
            if !self
                .store
                .is_ancestor(&scope.repository_id, &existing.commit_id, &main.head)?
            {
                return Err(AuthoredDocumentsError::Storage(
                    "track clip creation outcome is not in current authored history".into(),
                ));
            }
            let current = match &main.document {
                AuthoredDocument::Track(document) => document,
                AuthoredDocument::Graph(_) => unreachable!("track scope resolved as graph"),
            };
            return Ok(AppliedAuthoredTrackEdit {
                authored: AppliedAuthoredState {
                    repository_id: scope.repository_id.to_string(),
                    commit_id: existing.commit_id.to_string(),
                    changed: false,
                    document: main.document.projected(),
                },
                edit: TrackEditResult {
                    revision: current.revision.clone(),
                    clips: current.clips.clone(),
                    id_map: BTreeMap::from([("new:partial-create".to_owned(), clip_id.to_owned())]),
                    created_clip_id: Some(clip_id.to_owned()),
                    added: 1,
                    updated: 0,
                    removed: 0,
                    applied_to_current_projection: existing.commit_id == main.head,
                },
            });
        }
        let main = self.reconcile_locked(pool, &scope).await?;
        let mut candidate = match &main.document {
            AuthoredDocument::Track(document) => document.clips.clone(),
            AuthoredDocument::Graph(_) => unreachable!("track scope resolved as graph"),
        };
        let draft_id = "new:partial-create".to_owned();
        candidate.push(TrackClip {
            id: draft_id.clone(),
            pattern_id: payload.pattern_id,
            start_time: payload.start_time,
            end_time: payload.end_time,
            z_index: payload.z_index,
            blend_mode,
            args,
        });
        let base_revision = main.document.revision().to_owned();
        let result = self
            .apply_track_edit_locked(
                pool,
                &scope,
                main,
                TrackEditPlan {
                    base_revision,
                    candidate,
                },
                subject,
                PendingTrackEditMetadata {
                    creation: Some(PendingTrackClipCreation {
                        request_id,
                        request_fingerprint,
                        draft_id: draft_id.clone(),
                        score_id: scope.track_scope().expect("track scope").score_id.clone(),
                    }),
                    ..PendingTrackEditMetadata::default()
                },
            )
            .await?;
        let id = result.edit.id_map.get(&draft_id).cloned().ok_or_else(|| {
            AuthoredDocumentsError::Storage(
                "partial create completed without allocating its clip identity".into(),
            )
        })?;
        let mut result = result;
        result.edit.created_clip_id = Some(id);
        Ok(result)
    }

    /// Apply only the fields present in `payload` to the clip as it exists at
    /// lock acquisition time. Missing IDs fail closed inside the exact scope.
    pub async fn update_track_score_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        payload: UpdateTrackScoreInput,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        assert_track_payload_scope(&track_scope, &payload.score_id, &payload.track_id)?;
        validate_ref_component(&payload.operation_id, "score edit operation id")?;
        let request_fingerprint = score_edit_request_fingerprint("update", &track_scope, &payload)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(replayed) = self
            .replay_score_edit_operation(
                pool,
                &scope,
                &main,
                &payload.operation_id,
                &request_fingerprint,
            )
            .await?
        {
            return Ok(replayed);
        }
        let mut candidate = match &main.document {
            AuthoredDocument::Track(document) => document.clips.clone(),
            AuthoredDocument::Graph(_) => unreachable!("track scope resolved as graph"),
        };
        let clip = candidate
            .iter_mut()
            .find(|clip| clip.id == payload.id)
            .ok_or_else(|| {
                AuthoredDocumentsError::Scope(format!(
                    "clip {} does not belong to this score",
                    payload.id
                ))
            })?;
        if let Some(value) = payload.start_time {
            clip.start_time = value;
        }
        if let Some(value) = payload.end_time {
            clip.end_time = value;
        }
        if let Some(value) = payload.z_index {
            clip.z_index = value;
        }
        if let Some(value) = payload.blend_mode {
            clip.blend_mode = value;
        }
        if let Some(value) = payload.args {
            clip.args = value;
        }
        let base_revision = main.document.revision().to_owned();
        self.apply_track_edit_locked(
            pool,
            &scope,
            main,
            TrackEditPlan {
                base_revision,
                candidate,
            },
            subject,
            PendingTrackEditMetadata {
                operation: Some(PendingScoreEditOperation {
                    operation_id: payload.operation_id,
                    request_fingerprint,
                    client_ids_by_draft: BTreeMap::new(),
                }),
                ..PendingTrackEditMetadata::default()
            },
        )
        .await
    }

    /// Delete one exact-scope clip as an atomic authored commit.
    pub async fn delete_track_score_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        payload: DeleteTrackScoreInput,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        assert_track_payload_scope(&track_scope, &payload.score_id, &payload.track_id)?;
        validate_ref_component(&payload.operation_id, "score edit operation id")?;
        let request_fingerprint = score_edit_request_fingerprint("delete", &track_scope, &payload)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(replayed) = self
            .replay_score_edit_operation(
                pool,
                &scope,
                &main,
                &payload.operation_id,
                &request_fingerprint,
            )
            .await?
        {
            return Ok(replayed);
        }
        let mut candidate = match &main.document {
            AuthoredDocument::Track(document) => document.clips.clone(),
            AuthoredDocument::Graph(_) => unreachable!("track scope resolved as graph"),
        };
        let before = candidate.len();
        candidate.retain(|clip| clip.id != payload.id);
        if candidate.len() == before {
            return Err(AuthoredDocumentsError::Scope(format!(
                "clip {} does not belong to this score",
                payload.id
            )));
        }
        let base_revision = main.document.revision().to_owned();
        self.apply_track_edit_locked(
            pool,
            &scope,
            main,
            TrackEditPlan {
                base_revision,
                candidate,
            },
            subject,
            PendingTrackEditMetadata {
                operation: Some(PendingScoreEditOperation {
                    operation_id: payload.operation_id,
                    request_fingerprint,
                    client_ids_by_draft: BTreeMap::new(),
                }),
                ..PendingTrackEditMetadata::default()
            },
        )
        .await
    }

    async fn apply_track_edit_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: MainState,
        plan: TrackEditPlan,
        subject: &str,
        pending: PendingTrackEditMetadata,
    ) -> Result<AppliedAuthoredTrackEdit> {
        let current = match &main.document {
            AuthoredDocument::Track(document) => document.clone(),
            AuthoredDocument::Graph(_) => unreachable!("track scope resolved as graph"),
        };
        if plan.base_revision != current.revision {
            return Err(AuthoredDocumentsError::Track(TrackEditError::Conflict {
                expected_revision: plan.base_revision,
                current_revision: current.revision,
            }));
        }

        validate_track_draft_envelope(&plan.candidate)?;

        let current_ids: HashSet<&str> =
            current.clips.iter().map(|clip| clip.id.as_str()).collect();
        let mut host_allocated_ids = BTreeSet::new();
        let mut id_map = BTreeMap::new();
        let mut candidate = plan.candidate;
        for clip in &mut candidate {
            if current_ids.contains(clip.id.as_str()) {
                continue;
            }
            if is_valid_track_draft_id(&clip.id) {
                let draft_id = std::mem::take(&mut clip.id);
                let stable_id = Uuid::new_v4().to_string();
                host_allocated_ids.insert(stable_id.clone());
                id_map.insert(draft_id, stable_id.clone());
                clip.id = stable_id;
            }
        }
        let requested_lineage_ids: BTreeSet<String> = candidate
            .iter()
            .filter(|clip| {
                !current_ids.contains(clip.id.as_str()) && !host_allocated_ids.contains(&clip.id)
            })
            .map(|clip| clip.id.clone())
            .collect();
        let lineage_ids = self.track_lineage_ids(scope, MAIN_BRANCH, &requested_lineage_ids)?;
        sort_track_clips(&mut candidate);
        let track_scope = scope.track_scope().expect("track scope");
        check_track_projection_candidate(
            pool,
            track_scope,
            scope.owner_user_id.as_deref(),
            &candidate,
            TrackProjectionIdentity::Allowed {
                lineage_ids: &lineage_ids,
                host_allocated_ids: &host_allocated_ids,
            },
        )
        .await?;
        let document = TrackDocument {
            revision: revision_for_clips(&candidate),
            clips: candidate,
        };
        let pattern_names = load_score_pattern_names(pool)
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        let prior_source = Some(utf8_file(&main.files, SCORE_PATH)?);
        let source = serialize_track(&document, &pattern_names, prior_source)?;
        let files = FileMap::from([(SCORE_PATH.to_owned(), source.into_bytes())]);
        let mut edit = track_edit_result(&current.clips, &document.clips, id_map);
        let creation = match pending.creation {
            Some(creation) => {
                let clip_id = edit
                    .id_map
                    .get(&creation.draft_id)
                    .cloned()
                    .ok_or_else(|| {
                        AuthoredDocumentsError::Storage(
                            "track clip creation did not allocate its requested draft identity"
                                .into(),
                        )
                    })?;
                if creation.score_id != track_scope.score_id {
                    return Err(AuthoredDocumentsError::Storage(
                        "track clip creation metadata belongs to another score".into(),
                    ));
                }
                Some(CreationProjection {
                    kind: "track_clip",
                    request_id: creation.request_id,
                    request_fingerprint: creation.request_fingerprint,
                    subject_id: clip_id,
                    auxiliary_id: Some(creation.score_id),
                })
            }
            None => None,
        };
        let operation = match pending.operation {
            Some(operation) => {
                edit = remap_track_snapshot_result(edit, operation.client_ids_by_draft);
                let result_json = serde_json::to_string(&ScoreEditOperationResult {
                    id_map: edit.id_map.clone(),
                    added: edit.added,
                    updated: edit.updated,
                    removed: edit.removed,
                })
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "encode durable score edit result: {error}"
                    ))
                })?;
                Some(OperationProjection {
                    kind: "score_edit",
                    operation_id: operation.operation_id,
                    request_fingerprint: operation.request_fingerprint,
                    base_main_commit: main.head.clone(),
                    result_json: Some(result_json),
                })
            }
            None => None,
        };
        let metadata = ProjectionMetadata {
            creation,
            operation,
            ..ProjectionMetadata::default()
        };
        let authored = self
            .apply_direct_locked(
                pool,
                scope,
                main,
                files,
                AuthoredDocument::Track(document),
                &plan.base_revision,
                TrackProjectionAuthority::Allowed {
                    lineage_ids,
                    host_allocated_ids,
                },
                subject,
                metadata,
            )
            .await?;
        Ok(AppliedAuthoredTrackEdit { authored, edit })
    }
}

pub(super) struct PatternForkSource {
    pattern: PatternSummary,
    pub(super) graph: GraphDocument,
}

struct PendingTrackClipCreation {
    request_id: String,
    request_fingerprint: String,
    draft_id: String,
    score_id: String,
}

#[derive(Default)]
struct PendingTrackEditMetadata {
    creation: Option<PendingTrackClipCreation>,
    operation: Option<PendingScoreEditOperation>,
}

struct PendingScoreEditOperation {
    operation_id: String,
    request_fingerprint: String,
    client_ids_by_draft: BTreeMap<String, String>,
}

/// Durable response correlation for a score edit. The authored document stays
/// exclusively in Git; this stores only the values that cannot be reconstructed
/// unambiguously after randomly allocated clip IDs have crossed the IPC
/// response-loss boundary.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScoreEditOperationResult {
    id_map: BTreeMap<String, String>,
    added: usize,
    updated: usize,
    removed: usize,
}

fn assert_track_payload_scope(scope: &TrackScope, score_id: &str, track_id: &str) -> Result<()> {
    if scope.score_id != score_id || scope.track_id != track_id {
        return Err(AuthoredDocumentsError::Scope(
            "track mutation payload does not match its trusted score scope".into(),
        ));
    }
    Ok(())
}

fn track_clip_creation_fingerprint(
    scope: &TrackScope,
    payload: &CreateTrackScoreInput,
    blend_mode: &BlendMode,
    args: &serde_json::Value,
) -> Result<String> {
    let start_time = format!("{:016x}", payload.start_time.to_bits());
    let end_time = format!("{:016x}", payload.end_time.to_bits());
    let z_index = payload.z_index.to_string();
    let blend_mode = serde_json::to_string(blend_mode).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("encode track clip creation blend mode: {error}"))
    })?;
    let args = crate::canonical_json::to_string(args);
    Ok(operation_request_fingerprint(
        "create_track_clip",
        &[
            &scope.score_id,
            &scope.track_id,
            &scope.venue_id,
            &payload.pattern_id,
            &start_time,
            &end_time,
            &z_index,
            &blend_mode,
            &args,
        ],
    ))
}

fn score_edit_request_fingerprint(
    edit_kind: &str,
    scope: &TrackScope,
    payload: &impl Serialize,
) -> Result<String> {
    let mut value = serde_json::to_value(payload).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("encode score edit fingerprint: {error}"))
    })?;
    if let serde_json::Value::Object(fields) = &mut value {
        fields.remove("operationId");
    }
    let canonical = crate::canonical_json::to_string(&serde_json::json!({
        "kind": edit_kind,
        "scoreId": scope.score_id,
        "trackId": scope.track_id,
        "venueId": scope.venue_id,
        "payload": value,
    }));
    Ok(operation_request_fingerprint("score_edit", &[&canonical]))
}

pub(super) fn pattern_fork_target_id(principal_key: &str, request_id: &str, kind: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"luma.pattern-fork-target.v1\0");
    for field in [principal_key, request_id, kind] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 4122 variant with version 8 (application-defined deterministic
    // payload). This preserves the UUID-shaped IDs expected throughout Luma
    // without adding SHA-1 or another dependency.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

pub(super) async fn load_pattern_fork_source(
    pool: &SqlitePool,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<PatternForkSource> {
    let mut transaction = pool.begin().await.map_err(|error| {
        AuthoredDocumentsError::Storage(format!("begin pattern fork source snapshot: {error}"))
    })?;
    let pattern = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "SELECT {PATTERN_SUMMARY_COLUMNS}
         FROM patterns
         JOIN auth_visible_patterns visible ON visible.pattern_id = patterns.id
         WHERE patterns.id = ?"
    )))
    .bind(pattern_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load pattern fork source: {error}")))?
    .ok_or_else(|| AuthoredDocumentsError::Scope("fork source pattern does not exist".into()))?;
    let graph = load_unscoped_graph_document_for_connection(
        &mut transaction,
        pattern_id,
        implementation_id,
    )
    .await?;
    transaction.commit().await.map_err(|error| {
        AuthoredDocumentsError::Storage(format!("finish pattern fork source snapshot: {error}"))
    })?;
    Ok(PatternForkSource { pattern, graph })
}

async fn load_forked_pattern(
    pool: &SqlitePool,
    scope: &ResolvedScope,
    expected_source_pattern_id: &str,
) -> Result<PatternSummary> {
    let pattern = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "SELECT {PATTERN_SUMMARY_COLUMNS} FROM patterns WHERE id = ?"
    )))
    .bind(&scope.subject_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("load forked pattern result: {error}"))
    })?
    .ok_or_else(|| AuthoredDocumentsError::Storage("forked pattern is missing".into()))?;
    if pattern.uid.as_deref() != scope.owner_user_id.as_deref()
        || pattern.forked_from_id.as_deref() != Some(expected_source_pattern_id)
    {
        return Err(AuthoredDocumentsError::Scope(
            "forked pattern metadata does not match its durable operation".into(),
        ));
    }
    let implementation_id = match &scope.document {
        DocumentScope::Pattern(scope) => &scope.implementation_id,
        DocumentScope::Track(_) => unreachable!("pattern fork target is always a graph"),
    };
    let implementation_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM implementations WHERE pattern_id = ?")
            .bind(&scope.subject_id)
            .fetch_one(pool)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "verify forked implementation set: {error}"
                ))
            })?;
    let exact_implementation: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM implementations WHERE id = ? AND pattern_id = ? AND
         ((uid = ?) OR (uid IS NULL AND ? IS NULL))",
    )
    .bind(implementation_id)
    .bind(&scope.subject_id)
    .bind(scope.owner_user_id.as_deref())
    .bind(scope.owner_user_id.as_deref())
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("verify forked implementation: {error}"))
    })?;
    if implementation_count != 1 || exact_implementation.is_none() {
        return Err(AuthoredDocumentsError::Storage(
            "forked pattern implementation identity has diverged".into(),
        ));
    }
    Ok(pattern)
}

fn verify_track_clip_creation_replay<'a>(
    existing: &'a CreationAssociation,
    request_fingerprint: &str,
    score_id: &str,
) -> Result<&'a str> {
    if existing.request_fingerprint != request_fingerprint
        || existing.auxiliary_id.as_deref() != Some(score_id)
        || existing.subject_id.is_empty()
    {
        return Err(AuthoredDocumentsError::Invalid(
            "authored creation request_id was already used with another request".into(),
        ));
    }
    Ok(&existing.subject_id)
}

fn sort_track_clips(clips: &mut [TrackClip]) {
    clips.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then(left.z_index.cmp(&right.z_index))
            .then(left.id.cmp(&right.id))
    });
}

fn track_edit_result(
    current: &[TrackClip],
    candidate: &[TrackClip],
    id_map: BTreeMap<String, String>,
) -> TrackEditResult {
    let current_by_id: HashMap<&str, &TrackClip> = current
        .iter()
        .map(|clip| (clip.id.as_str(), clip))
        .collect();
    let candidate_ids: HashSet<&str> = candidate.iter().map(|clip| clip.id.as_str()).collect();
    let added = candidate
        .iter()
        .filter(|clip| !current_by_id.contains_key(clip.id.as_str()))
        .count();
    let updated = candidate
        .iter()
        .filter(|clip| {
            current_by_id
                .get(clip.id.as_str())
                .is_some_and(|current| !track_clips_semantically_equal(current, clip))
        })
        .count();
    let removed = current
        .iter()
        .filter(|clip| !candidate_ids.contains(clip.id.as_str()))
        .count();
    TrackEditResult {
        revision: revision_for_clips(candidate),
        clips: candidate.to_vec(),
        id_map,
        created_clip_id: None,
        added,
        updated,
        removed,
        applied_to_current_projection: true,
    }
}
