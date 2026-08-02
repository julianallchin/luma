use super::{
    agent_threads, apply_graph_edit_in_transaction, apply_track_projection_in_transaction,
    available_projection_scopes, canonicalize_graph, clips_to_canonical_document, commit_message,
    decode_canonical_track_document, exact_graph_json, graph_files, graph_from_files,
    graph_revision, insert_creation_association, load_graph_document, load_ledger,
    load_score_pattern_names, load_track_document_for_principal, merge_document_trivia,
    merge_graphs, merge_track_documents, normalized_subject, parse_score_document,
    pattern_names_from_document, pattern_projection_scopes, require_exact_paths,
    revision_for_clips, serialize_canonical, serialize_track, system_author,
    track_scope_from_catalog, utf8_file, write_ledger, write_operation_association,
    write_turn_association, AppliedAuthoredState, Arc, AuthoredDocument, AuthoredDocuments,
    AuthoredDocumentsError, AuthoredMergeConflict, AuthoredMergeConflictKind,
    AuthoredMergePathSegment, AuthoredMergeValue, AuthoredProjectedDocument, AuthoredSnapshot,
    AuthorizedVenue, BTreeSet, CommitId, CommitInfo, DocumentScope, FileMap, FromRow, Graph,
    GraphDocument, GraphDocumentError, GraphEditPlan, GraphScope, GraphValidationIssue, MainState,
    MaterializationState, ProjectionLedger, ProjectionLedgerExpectation, ProjectionMetadata,
    ResolvedScope, Result, SqliteConnection, SqlitePool, TrackDocument, TrackEditError,
    TrackEditPlan, TrackProjectionAuthority, TrackProjectionIdentity, TrackScope, VenueAccess,
    VenueResource, Write, GRAPH_PATH, LAYOUT_PATH, MAIN_BRANCH, SCORE_PATH, TRAILER_OPERATION,
};

impl AuthoredDocuments {
    /// Reconcile every authored document whose relational catalog identity is
    /// currently available. Startup calls this before exposing the app, and
    /// sync calls it after catalog pulls. That makes migration of pre-Git rows
    /// and sign-in rematerialization the same operation rather than two data
    /// paths with subtly different authority rules.
    pub async fn reconcile_available_projections(&self, pool: &SqlitePool) -> Result<()> {
        let _lifecycle = Arc::clone(&self.lifecycle_lock).write_owned().await;
        self.reconcile_available_inside_lifecycle(pool).await
    }

    pub(super) async fn reconcile_available_inside_lifecycle(
        &self,
        pool: &SqlitePool,
    ) -> Result<()> {
        let scopes = available_projection_scopes(pool).await?;
        for scope in scopes {
            let _repository = self
                .repository_guard_inside_lifecycle(&scope.repository_id)
                .await;
            self.reconcile_locked(pool, &scope).await?;
        }
        Ok(())
    }

    /// Ensure one score's live projection is available before a normal read.
    /// The catalog row supplies the trusted repository owner; readers do not
    /// get to choose another principal through an IPC payload.
    pub async fn reconcile_track_score_for_read(
        &self,
        pool: &SqlitePool,
        score_id: &str,
    ) -> Result<TrackScope> {
        let _lifecycle = Arc::clone(&self.lifecycle_lock).read_owned().await;
        let scope = track_scope_from_catalog(pool, score_id).await?;
        let resolved = ResolvedScope::track(scope.owner.as_deref(), scope.track_scope.clone())?;
        let _repository = self
            .repository_guard_inside_lifecycle(&resolved.repository_id)
            .await;
        self.reconcile_locked(pool, &resolved).await?;
        Ok(scope.track_scope)
    }

    pub async fn reconcile_track_scores_for_read(
        &self,
        pool: &SqlitePool,
        track_id: &str,
        venue_id: &str,
    ) -> Result<()> {
        let _lifecycle = Arc::clone(&self.lifecycle_lock).read_owned().await;
        let score_ids: Vec<String> = if venue_id.is_empty() {
            sqlx::query_scalar("SELECT id FROM scores WHERE track_id = ? ORDER BY id")
                .bind(track_id)
                .fetch_all(pool)
                .await
        } else {
            sqlx::query_scalar(
                "SELECT id FROM scores WHERE track_id = ? AND venue_id = ? ORDER BY id",
            )
            .bind(track_id)
            .bind(venue_id)
            .fetch_all(pool)
            .await
        }
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "list score projections for normal read: {error}"
            ))
        })?;
        for score_id in score_ids {
            let scope = track_scope_from_catalog(pool, &score_id).await?;
            let resolved = ResolvedScope::track(scope.owner.as_deref(), scope.track_scope.clone())?;
            let _repository = self
                .repository_guard_inside_lifecycle(&resolved.repository_id)
                .await;
            self.reconcile_locked(pool, &resolved).await?;
        }
        Ok(())
    }

    /// Reconcile every implementation of one visible pattern before default,
    /// explicit, or venue-aware resolution reads the relational projection.
    /// Retained ledger rows participate in discovery, which is what lets an
    /// implementation be recreated after sign-out without an agent thread.
    pub async fn reconcile_pattern_graphs_for_read(
        &self,
        pool: &SqlitePool,
        pattern_id: &str,
    ) -> Result<()> {
        let _lifecycle = Arc::clone(&self.lifecycle_lock).read_owned().await;
        let scopes = pattern_projection_scopes(pool, pattern_id).await?;
        for scope in scopes {
            let _repository = self
                .repository_guard_inside_lifecycle(&scope.repository_id)
                .await;
            self.reconcile_locked(pool, &scope).await?;
        }
        Ok(())
    }

    pub(super) async fn apply_direct_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: MainState,
        files: FileMap,
        candidate: AuthoredDocument,
        expected_projection_revision: &str,
        track_authority: TrackProjectionAuthority,
        subject: &str,
        metadata: ProjectionMetadata,
    ) -> Result<AppliedAuthoredState> {
        if files == main.files
            && candidate.revision() == expected_projection_revision
            && metadata.operation.is_none()
        {
            self.assert_database_revision(pool, scope, expected_projection_revision)
                .await?;
            return Ok(AppliedAuthoredState {
                repository_id: scope.repository_id.to_string(),
                commit_id: main.head.to_string(),
                changed: false,
                document: main.document.projected(),
            });
        }
        let message = commit_message(normalized_subject(subject)?, &[(TRAILER_OPERATION, "edit")])?;
        let prepared = self.store.prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&main.head),
            &files,
            &system_author()?,
            &message,
        )?;
        let projected = self
            .project_prepared(
                pool,
                scope,
                &main.head,
                ProjectionLedgerExpectation::PresentAt(&main.head),
                &prepared,
                candidate,
                expected_projection_revision,
                track_authority,
                metadata,
            )
            .await?;
        Ok(AppliedAuthoredState {
            repository_id: scope.repository_id.to_string(),
            commit_id: prepared.id.to_string(),
            changed: projected.changed,
            document: projected.document,
        })
    }

    async fn assert_database_revision(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        expected: &str,
    ) -> Result<()> {
        let current = self.snapshot_from_database(pool, scope, None).await?;
        if current.document.revision() == expected {
            return Ok(());
        }
        match current.document {
            AuthoredDocument::Track(current) => {
                Err(AuthoredDocumentsError::Track(TrackEditError::Conflict {
                    expected_revision: expected.to_owned(),
                    current_revision: current.revision,
                }))
            }
            AuthoredDocument::Graph(current) => Err(AuthoredDocumentsError::Graph(
                GraphDocumentError::Conflict {
                    expected_revision: expected.to_owned(),
                    current_revision: current.revision,
                },
            )),
        }
    }

    pub(super) async fn reconcile_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
    ) -> Result<MainState> {
        self.store.ensure_repository(&scope.repository_id)?;
        // A second pass publishes a prepared descendant after a crash. Keep a
        // hard bound so corrupted metadata fails closed rather than spinning.
        for _ in 0..4 {
            let main_head = self.store.main_head(&scope.repository_id)?;
            let ledger = load_ledger(pool, scope).await?;
            match ledger {
                None => {
                    let existing = self.store.read_commit(&scope.repository_id, &main_head)?.1;
                    if !existing.is_empty() {
                        return Err(AuthoredDocumentsError::Storage(
                            "authored repository has content but no projection ledger".into(),
                        ));
                    }
                    let database = self.snapshot_from_database(pool, scope, None).await?;
                    let message = commit_message(
                        "Import existing authored state",
                        &[(TRAILER_OPERATION, "initial_import")],
                    )?;
                    let prepared = self.store.prepare_commit(
                        &scope.repository_id,
                        std::slice::from_ref(&main_head),
                        &database.files,
                        &system_author()?,
                        &message,
                    )?;
                    let expected_projection_revision = database.document.revision().to_owned();
                    self.project_prepared(
                        pool,
                        scope,
                        &main_head,
                        ProjectionLedgerExpectation::Missing,
                        &prepared,
                        database.document,
                        &expected_projection_revision,
                        TrackProjectionAuthority::ExistingOnly,
                        ProjectionMetadata::default(),
                    )
                    .await?;
                    continue;
                }
                Some(ledger) if ledger.materialization_state == MaterializationState::Archived => {
                    return Err(AuthoredDocumentsError::Scope(format!(
                        "authored {} was archived and cannot be rematerialized",
                        scope.repository_id
                    )));
                }
                Some(ledger) if ledger.projected_commit == main_head => {
                    if ledger.materialization_state == MaterializationState::Absent {
                        // Sign-out removes the live projection but deliberately
                        // retains Git and its routing ledger. Re-materialize
                        // that exact tree only into an absent/equal projection;
                        // a colliding relational blob is never overwritten.
                        let main = self.snapshot_from_commit(scope, &main_head)?;
                        self.materialize_absent_projection(pool, scope, &ledger, &main)
                            .await?;
                        return Ok(MainState {
                            head: main_head,
                            files: main.files,
                            document: main.document,
                        });
                    }
                    let main = self.snapshot_from_commit(scope, &main_head)?;
                    let database = self
                        .snapshot_from_database(pool, scope, Some(&main.files))
                        .await?;
                    if database.document.semantic_eq(&main.document) {
                        return Ok(MainState {
                            head: main_head,
                            files: main.files,
                            document: main.document,
                        });
                    }
                    return Err(AuthoredDocumentsError::Storage(
                        "relational authored projection diverged from Git main; only AuthoredDocuments may mutate a materialized document"
                            .into(),
                    ));
                }
                Some(ledger) => {
                    let base = self.store.merge_base(
                        &scope.repository_id,
                        &main_head,
                        &ledger.projected_commit,
                    )?;
                    if base == main_head {
                        // SQLite already committed this prepared descendant;
                        // publish it now. Any unrelated DB divergence on the
                        // next pass fails closed.
                        self.store.advance_branch(
                            &scope.repository_id,
                            MAIN_BRANCH,
                            &main_head,
                            &ledger.projected_commit,
                        )?;
                        continue;
                    }
                    if base == ledger.projected_commit {
                        return Err(AuthoredDocumentsError::Storage(format!(
                            "authored Git main {} advanced beyond projected commit {}; refusing to project an unauthenticated ref mutation",
                            main_head, ledger.projected_commit
                        )));
                    }
                    return Err(AuthoredDocumentsError::Storage(
                        "authored main and projection ledger diverged".into(),
                    ));
                }
            }
        }
        Err(AuthoredDocumentsError::Storage(
            "authored-state recovery did not converge".into(),
        ))
    }

    /// Recreate a sign-out-absent live projection from the exact Git main
    /// without granting relational rows any merge or overwrite semantics.
    /// Catalog parents must already belong to the returning principal. An
    /// authored row that reappeared through any other path must be either
    /// absent or canonical-equivalent to Git; divergence is corruption.
    async fn materialize_absent_projection(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        ledger: &ProjectionLedger,
        main: &AuthoredSnapshot,
    ) -> Result<()> {
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin absent projection restore: {error}"))
        })?;
        if let Some(thread_id) = scope.thread_id.as_deref() {
            agent_threads::assert_thread_active(
                &mut transaction,
                thread_id,
                scope.owner_user_id.as_deref(),
            )
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        }

        match (&scope.document, &main.document) {
            (DocumentScope::Track(track_scope), AuthoredDocument::Track(candidate)) => {
                let current = load_track_document_for_connection(
                    &mut transaction,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                )
                .await?;
                if current.revision != candidate.revision {
                    if !current.clips.is_empty() {
                        return Err(AuthoredDocumentsError::Storage(
                            "cannot materialize absent Git score over a non-empty relational score projection"
                                .into(),
                        ));
                    }
                    apply_track_projection_in_transaction(
                        &mut transaction,
                        track_scope,
                        scope.owner_user_id.as_deref(),
                        TrackEditPlan {
                            base_revision: current.revision,
                            candidate: candidate.clips.clone(),
                        },
                        TrackProjectionIdentity::TrustedRepositoryTree,
                    )
                    .await?;
                }
            }
            (DocumentScope::Pattern(graph_scope), AuthoredDocument::Graph(candidate)) => {
                let candidate_graph = canonicalize_graph(&candidate.graph)?;
                let candidate_revision = graph_revision(&candidate_graph)?;
                let owns_pattern = match scope.owner_user_id.as_deref() {
                    Some(owner) => {
                        sqlx::query_scalar::<_, i64>(
                            "SELECT 1 FROM patterns WHERE id = ? AND uid = ?",
                        )
                        .bind(&graph_scope.pattern_id)
                        .bind(owner)
                        .fetch_optional(&mut *transaction)
                        .await
                    }
                    None => {
                        sqlx::query_scalar::<_, i64>(
                            "SELECT 1 FROM patterns WHERE id = ? AND uid IS NULL",
                        )
                        .bind(&graph_scope.pattern_id)
                        .fetch_optional(&mut *transaction)
                        .await
                    }
                }
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "authorize absent pattern projection: {error}"
                    ))
                })?
                .is_some();
                if !owns_pattern {
                    return Err(AuthoredDocumentsError::Scope(
                        "pattern does not belong to the current principal".into(),
                    ));
                }

                let existing = sqlx::query_as::<_, (Option<String>, String)>(
                    "SELECT uid, graph_json FROM implementations WHERE id = ? AND pattern_id = ?",
                )
                .bind(&graph_scope.implementation_id)
                .bind(&graph_scope.pattern_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "inspect absent implementation projection: {error}"
                    ))
                })?;
                if let Some((implementation_owner, graph_json)) = existing {
                    if implementation_owner.as_deref() != scope.owner_user_id.as_deref() {
                        return Err(AuthoredDocumentsError::Scope(
                            "colliding implementation does not belong to the current principal"
                                .into(),
                        ));
                    }
                    let graph: Graph = serde_json::from_str(&graph_json).map_err(|error| {
                        AuthoredDocumentsError::Storage(format!(
                            "colliding implementation graph is corrupt: {error}"
                        ))
                    })?;
                    if graph_revision(&graph)? != candidate_revision {
                        return Err(AuthoredDocumentsError::Storage(
                            "cannot materialize absent Git graph over a divergent relational implementation projection"
                                .into(),
                        ));
                    }
                } else {
                    sqlx::query(
                        "INSERT INTO implementations
                         (id, uid, pattern_id, name, graph_json)
                         VALUES (?, ?, ?, ?, ?)",
                    )
                    .bind(&graph_scope.implementation_id)
                    .bind(scope.owner_user_id.as_deref())
                    .bind(&graph_scope.pattern_id)
                    .bind(ledger.implementation_name.as_deref())
                    .bind(exact_graph_json(&candidate_graph)?)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| {
                        AuthoredDocumentsError::Storage(format!(
                            "restore implementation projection from Git: {error}"
                        ))
                    })?;
                }
            }
            _ => {
                return Err(AuthoredDocumentsError::Storage(
                    "cannot materialize mixed authored document kinds".into(),
                ));
            }
        }

        write_ledger(
            &mut transaction,
            scope,
            ProjectionLedgerExpectation::AbsentAt(&ledger.projected_commit),
            &ledger.projected_commit,
        )
        .await?;
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("commit absent projection restore: {error}"))
        })?;
        Ok(())
    }

    pub(super) async fn project_prepared(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        expected_main: &CommitId,
        expected_ledger: ProjectionLedgerExpectation<'_>,
        prepared: &CommitInfo,
        candidate: AuthoredDocument,
        expected_projection_revision: &str,
        track_authority: TrackProjectionAuthority,
        metadata: ProjectionMetadata,
    ) -> Result<Projected> {
        let projected = self
            .project_sqlite(
                pool,
                scope,
                expected_ledger,
                &prepared.id,
                candidate,
                expected_projection_revision,
                track_authority,
                metadata,
            )
            .await?;
        self.store.advance_branch(
            &scope.repository_id,
            MAIN_BRANCH,
            expected_main,
            &prepared.id,
        )?;
        Ok(projected)
    }

    async fn project_sqlite(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        expected_ledger: ProjectionLedgerExpectation<'_>,
        projected_commit: &CommitId,
        candidate: AuthoredDocument,
        expected_projection_revision: &str,
        track_authority: TrackProjectionAuthority,
        metadata: ProjectionMetadata,
    ) -> Result<Projected> {
        match &scope.document {
            DocumentScope::Track(track_scope) => {
                // Git preparation deliberately happens before this point. The
                // prepared commit is unreachable, so it is harmless if
                // admission changed while the model was working. Re-authorize
                // the exact score here, in the transaction which projects the
                // document and ledger. Only a successfully committed guard may
                // return to `project_prepared`, where `main` is advanced.
                let mut access =
                    VenueAccess::<Write>::write(pool, VenueResource::Score(&track_scope.score_id))
                        .await
                        .map_err(AuthoredDocumentsError::Scope)?;
                access
                    .require_venue(&track_scope.venue_id)
                    .map_err(AuthoredDocumentsError::Scope)?;
                if access.principal() != scope.owner_user_id.as_deref() {
                    return Err(AuthoredDocumentsError::Scope(
                        "authored score principal changed before projection".into(),
                    ));
                }
                let projected = self
                    .project_sqlite_on_connection(
                        access.connection(),
                        scope,
                        expected_ledger,
                        projected_commit,
                        candidate,
                        expected_projection_revision,
                        track_authority,
                        metadata,
                    )
                    .await?;
                access
                    .commit()
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                Ok(projected)
            }
            DocumentScope::Pattern(graph_scope) => {
                let mut transaction =
                    pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
                        AuthoredDocumentsError::Storage(format!("begin projection: {error}"))
                    })?;
                if graph_scope.owner_user_id.as_deref() != scope.owner_user_id.as_deref() {
                    return Err(AuthoredDocumentsError::Scope(
                        "authored graph scope has inconsistent principal ownership".into(),
                    ));
                }
                let admitted: Option<(Option<String>,)> = sqlx::query_as(
                    "SELECT admission.active_uid
                     FROM auth_write_admission admission
                     JOIN patterns pattern ON pattern.id = ?
                     JOIN implementations implementation
                       ON implementation.id = ?
                      AND implementation.pattern_id = pattern.id
                     WHERE admission.singleton = 1
                       AND admission.armed = 1
                       AND admission.accepting = 1
                       AND admission.maintenance = 0
                       AND admission.remote_writes = 0
                       AND pattern.uid IS admission.active_uid
                       AND implementation.uid IS admission.active_uid",
                )
                .bind(&graph_scope.pattern_id)
                .bind(&graph_scope.implementation_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "authorize final pattern projection: {error}"
                    ))
                })?;
                let Some((active_principal,)) = admitted else {
                    return Err(AuthoredDocumentsError::Scope(
                        "authored pattern principal changed before projection".into(),
                    ));
                };
                if active_principal.as_deref() != scope.owner_user_id.as_deref() {
                    return Err(AuthoredDocumentsError::Scope(
                        "authored pattern principal changed before projection".into(),
                    ));
                }
                let projected = self
                    .project_sqlite_on_connection(
                        &mut transaction,
                        scope,
                        expected_ledger,
                        projected_commit,
                        candidate,
                        expected_projection_revision,
                        track_authority,
                        metadata,
                    )
                    .await?;
                transaction.commit().await.map_err(|error| {
                    AuthoredDocumentsError::Storage(format!("commit projection: {error}"))
                })?;
                Ok(projected)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn project_sqlite_on_connection(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        expected_ledger: ProjectionLedgerExpectation<'_>,
        projected_commit: &CommitId,
        candidate: AuthoredDocument,
        expected_projection_revision: &str,
        track_authority: TrackProjectionAuthority,
        metadata: ProjectionMetadata,
    ) -> Result<Projected> {
        if let Some(thread_id) = scope.thread_id.as_deref() {
            agent_threads::assert_thread_active(
                connection,
                thread_id,
                scope.owner_user_id.as_deref(),
            )
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        }
        let projected = match (&scope.document, candidate) {
            (DocumentScope::Track(track_scope), AuthoredDocument::Track(candidate)) => {
                let current = load_track_document_for_connection(
                    connection,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                )
                .await?;
                if current.revision != expected_projection_revision {
                    return Err(AuthoredDocumentsError::Track(TrackEditError::Conflict {
                        expected_revision: expected_projection_revision.to_owned(),
                        current_revision: current.revision,
                    }));
                }
                let result = apply_track_projection_in_transaction(
                    connection,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    TrackEditPlan {
                        base_revision: expected_projection_revision.to_owned(),
                        candidate: candidate.clips,
                    },
                    track_authority.identity(),
                )
                .await?;
                Projected {
                    changed: result.added + result.updated + result.removed > 0,
                    document: AuthoredProjectedDocument::TrackScore {
                        revision: result.revision,
                    },
                }
            }
            (DocumentScope::Pattern(graph_scope), AuthoredDocument::Graph(candidate)) => {
                let current = load_graph_document_for_connection(connection, graph_scope).await?;
                if current.revision != expected_projection_revision {
                    return Err(AuthoredDocumentsError::Graph(
                        GraphDocumentError::Conflict {
                            expected_revision: expected_projection_revision.to_owned(),
                            current_revision: current.revision,
                        },
                    ));
                }
                let result = apply_graph_edit_in_transaction(
                    connection,
                    graph_scope,
                    GraphEditPlan {
                        base_revision: expected_projection_revision.to_owned(),
                        candidate: candidate.graph,
                    },
                )
                .await?;
                Projected {
                    changed: result.changed,
                    document: AuthoredProjectedDocument::PatternGraph {
                        implementation_id: graph_scope.implementation_id.clone(),
                        revision: result.revision,
                        graph: result.graph,
                    },
                }
            }
            _ => {
                return Err(AuthoredDocumentsError::Storage(
                    "attempted to project the wrong authored document kind".into(),
                ));
            }
        };
        write_ledger(connection, scope, expected_ledger, projected_commit).await?;
        if let Some(turn) = metadata.turn {
            write_turn_association(connection, scope, projected_commit, &turn).await?;
        }
        if let Some(operation) = metadata.operation {
            write_operation_association(connection, scope, projected_commit, &operation).await?;
        }
        if let Some(creation) = metadata.creation {
            insert_creation_association(
                connection,
                &scope.principal_key,
                creation.kind,
                &creation.request_id,
                &creation.request_fingerprint,
                &creation.subject_id,
                creation.auxiliary_id.as_deref(),
                projected_commit,
            )
            .await?;
        }
        Ok(projected)
    }

    pub(super) async fn snapshot_from_database(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        prior_files: Option<&FileMap>,
    ) -> Result<AuthoredSnapshot> {
        let document = match &scope.document {
            DocumentScope::Track(track_scope) => AuthoredDocument::Track(
                load_track_document_for_principal(
                    pool,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                )
                .await?,
            ),
            DocumentScope::Pattern(graph_scope) => {
                AuthoredDocument::Graph(load_graph_document(pool, graph_scope).await?)
            }
        };
        let files = self
            .files_for_document(pool, scope, &document, prior_files)
            .await?;
        Ok(AuthoredSnapshot { files, document })
    }

    pub(super) fn snapshot_from_commit(
        &self,
        scope: &ResolvedScope,
        commit: &CommitId,
    ) -> Result<AuthoredSnapshot> {
        let (_, files) = self.store.read_commit(&scope.repository_id, commit)?;
        let document = self.decode_files(scope, &files)?;
        Ok(AuthoredSnapshot { files, document })
    }

    pub(super) fn decode_files(
        &self,
        scope: &ResolvedScope,
        files: &FileMap,
    ) -> Result<AuthoredDocument> {
        match &scope.document {
            DocumentScope::Track(_) => {
                require_exact_paths(files, &[SCORE_PATH])?;
                let source = utf8_file(files, SCORE_PATH)?;
                let (document, _) = decode_canonical_track_document(source)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                Ok(AuthoredDocument::Track(document))
            }
            DocumentScope::Pattern(graph_scope) => {
                require_exact_paths(files, &[GRAPH_PATH, LAYOUT_PATH])?;
                let graph = graph_from_files(
                    utf8_file(files, GRAPH_PATH)?,
                    utf8_file(files, LAYOUT_PATH)?,
                )?;
                Ok(AuthoredDocument::Graph(GraphDocument {
                    implementation_id: graph_scope.implementation_id.clone(),
                    revision: graph_revision(&graph)?,
                    graph,
                }))
            }
        }
    }

    pub(super) async fn files_for_document(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        document: &AuthoredDocument,
        prior_files: Option<&FileMap>,
    ) -> Result<FileMap> {
        match (&scope.document, document) {
            (DocumentScope::Track(_), AuthoredDocument::Track(track)) => {
                if let Some(prior) = prior_files {
                    if let Some(source) = prior
                        .get(SCORE_PATH)
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    {
                        if let Ok((parsed, _)) = decode_canonical_track_document(source) {
                            if parsed.revision == track.revision {
                                return Ok(prior.clone());
                            }
                        }
                    }
                }
                let pattern_names = load_score_pattern_names(pool)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                let prior_source = prior_files
                    .and_then(|files| files.get(SCORE_PATH))
                    .and_then(|bytes| std::str::from_utf8(bytes).ok());
                let source = serialize_track(track, &pattern_names, prior_source)?;
                Ok(FileMap::from([(
                    SCORE_PATH.to_owned(),
                    source.into_bytes(),
                )]))
            }
            (DocumentScope::Pattern(_), AuthoredDocument::Graph(graph)) => {
                graph_files(&graph.graph)
            }
            _ => Err(AuthoredDocumentsError::Storage(
                "cannot serialize mixed authored document kinds".into(),
            )),
        }
    }

    /// Merge complete authored snapshots by domain identity, then merge the
    /// score's lossless presentation layer by the same stable clip/layer
    /// identities. No partially merged value escapes a conflict.
    pub(super) async fn merge_snapshots(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        base: &AuthoredSnapshot,
        ours: &AuthoredSnapshot,
        theirs: &AuthoredSnapshot,
    ) -> Result<std::result::Result<(AuthoredDocument, FileMap), Vec<AuthoredMergeConflict>>> {
        match (&base.document, &ours.document, &theirs.document) {
            (
                AuthoredDocument::Track(base_track),
                AuthoredDocument::Track(ours_track),
                AuthoredDocument::Track(theirs_track),
            ) => {
                let semantic = match merge_track_documents(base_track, ours_track, theirs_track)
                    .into_result()
                {
                    Ok(merged) => merged,
                    Err(conflicts) => {
                        return Ok(Err(conflicts.into_iter().map(Into::into).collect()));
                    }
                };
                let base_source = utf8_file(&base.files, SCORE_PATH)?;
                let ours_source = utf8_file(&ours.files, SCORE_PATH)?;
                let theirs_source = utf8_file(&theirs.files, SCORE_PATH)?;
                let base_ast = parse_score_document(base_source)?;
                let ours_ast = parse_score_document(ours_source)?;
                let theirs_ast = parse_score_document(theirs_source)?;
                let mut pattern_names = pattern_names_from_document(&base_ast);
                pattern_names.extend(pattern_names_from_document(&theirs_ast));
                pattern_names.extend(pattern_names_from_document(&ours_ast));
                let semantic_ast = clips_to_canonical_document(&semantic.clips, &pattern_names)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let merged_ast =
                    match merge_document_trivia(&base_ast, &ours_ast, &theirs_ast, semantic_ast)
                        .into_result()
                    {
                        Ok(document) => document,
                        Err(conflicts) => {
                            return Ok(Err(conflicts.into_iter().map(Into::into).collect()));
                        }
                    };
                let source = serialize_canonical(&merged_ast)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let (document, _) = decode_canonical_track_document(&source)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                Ok(Ok((
                    AuthoredDocument::Track(document),
                    FileMap::from([(SCORE_PATH.to_owned(), source.into_bytes())]),
                )))
            }
            (
                AuthoredDocument::Graph(base_graph),
                AuthoredDocument::Graph(ours_graph),
                AuthoredDocument::Graph(theirs_graph),
            ) => match merge_graphs(&base_graph.graph, &ours_graph.graph, &theirs_graph.graph)
                .into_result()
            {
                Ok(graph) => {
                    let graph = match canonicalize_graph(&graph) {
                        Ok(graph) => graph,
                        Err(GraphDocumentError::Invalid { issues }) => {
                            return Ok(Err(graph_validation_conflicts(issues)));
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let implementation_id = match &scope.document {
                        DocumentScope::Pattern(graph_scope) => {
                            graph_scope.implementation_id.clone()
                        }
                        DocumentScope::Track(_) => {
                            return Err(AuthoredDocumentsError::Storage(
                                "graph merge requested for a track repository".into(),
                            ));
                        }
                    };
                    let document = AuthoredDocument::Graph(GraphDocument {
                        implementation_id,
                        revision: graph_revision(&graph)?,
                        graph,
                    });
                    let files = self
                        .files_for_document(pool, scope, &document, None)
                        .await?;
                    Ok(Ok((document, files)))
                }
                Err(conflicts) => Ok(Err(conflicts.into_iter().map(Into::into).collect())),
            },
            _ => Err(AuthoredDocumentsError::Storage(
                "authored repository contains mixed document kinds".into(),
            )),
        }
    }

    /// Collect every stable clip identity already accepted anywhere in the
    /// worktree branch's reachable history. Deleting a clip does not revoke
    /// its identity, but a UUID invented by the caller remains unauthorized.
    pub(super) fn track_lineage_ids(
        &self,
        scope: &ResolvedScope,
        branch: &str,
        needed: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        if needed.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut commits = Vec::new();
        self.store
            .find_reachable_commit(&scope.repository_id, branch, |commit| {
                commits.push(commit.id.clone());
                None::<()>
            })?;
        let mut ids = BTreeSet::new();
        for commit in commits {
            let (_, files) = self.store.read_commit(&scope.repository_id, &commit)?;
            if files.is_empty() {
                continue;
            }
            match self.decode_files(scope, &files)? {
                AuthoredDocument::Track(document) => {
                    for clip in document.clips {
                        if needed.contains(&clip.id) {
                            ids.insert(clip.id);
                        }
                    }
                    if ids.len() == needed.len() {
                        break;
                    }
                }
                AuthoredDocument::Graph(_) => {
                    return Err(AuthoredDocumentsError::Storage(
                        "graph commit found in score lineage".into(),
                    ));
                }
            }
        }
        Ok(ids)
    }
}

fn graph_validation_conflicts(issues: Vec<GraphValidationIssue>) -> Vec<AuthoredMergeConflict> {
    issues
        .into_iter()
        .map(|issue| AuthoredMergeConflict {
            path: vec![AuthoredMergePathSegment::Field(issue.path)],
            kind: AuthoredMergeConflictKind::InvalidInput,
            base: AuthoredMergeValue::Missing,
            ours: AuthoredMergeValue::Missing,
            theirs: AuthoredMergeValue::Missing,
            detail: Some(issue.message),
        })
        .collect()
}

async fn load_track_document_for_connection(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner: Option<&str>,
) -> Result<TrackDocument> {
    #[derive(FromRow)]
    struct Owner {
        track_id: String,
        venue_id: Option<String>,
        uid: Option<String>,
    }
    let found =
        sqlx::query_as::<_, Owner>("SELECT track_id, venue_id, uid FROM scores WHERE id = ?")
            .bind(&scope.score_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| AuthoredDocumentsError::Storage(format!("load score scope: {error}")))?
            .ok_or_else(|| AuthoredDocumentsError::Scope("score does not exist".into()))?;
    if found.track_id != scope.track_id
        || found.venue_id.as_deref() != Some(scope.venue_id.as_str())
        || found.uid.as_deref() != owner
    {
        return Err(AuthoredDocumentsError::Scope(
            "score does not belong to the exact current authored scope".into(),
        ));
    }
    let rows = sqlx::query_as::<_, crate::models::scores::TrackScore>(
        "SELECT id, uid, score_id, pattern_id, start_time, end_time, z_index,
                blend_mode, args_json, created_at, updated_at
         FROM track_scores WHERE score_id = ? ORDER BY start_time, z_index, id",
    )
    .bind(&scope.score_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load score clips: {error}")))?;
    let clips = rows.iter().map(Into::into).collect::<Vec<_>>();
    Ok(TrackDocument {
        revision: revision_for_clips(&clips),
        clips,
    })
}

async fn load_graph_document_for_connection(
    connection: &mut SqliteConnection,
    scope: &GraphScope,
) -> Result<GraphDocument> {
    let owns = match scope.owner_user_id.as_deref() {
        Some(owner) => {
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM patterns WHERE id = ? AND uid = ?")
                .bind(&scope.pattern_id)
                .bind(owner)
                .fetch_optional(&mut *connection)
                .await
        }
        None => {
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM patterns WHERE id = ? AND uid IS NULL")
                .bind(&scope.pattern_id)
                .fetch_optional(&mut *connection)
                .await
        }
    }
    .map_err(|error| AuthoredDocumentsError::Storage(format!("authorize pattern: {error}")))?
    .is_some();
    if !owns {
        return Err(AuthoredDocumentsError::Scope(
            "pattern does not belong to the current principal".into(),
        ));
    }
    let implementation: (Option<String>, String) = sqlx::query_as(
        "SELECT uid, graph_json FROM implementations WHERE id = ? AND pattern_id = ?",
    )
    .bind(&scope.implementation_id)
    .bind(&scope.pattern_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load pattern graph: {error}")))?
    .ok_or_else(|| {
        AuthoredDocumentsError::Scope(format!(
            "implementation {} does not belong to pattern {}",
            scope.implementation_id, scope.pattern_id
        ))
    })?;
    if implementation.0.as_deref() != scope.owner_user_id.as_deref() {
        return Err(AuthoredDocumentsError::Scope(
            "implementation does not belong to the current principal".into(),
        ));
    }
    let graph = serde_json::from_str(&implementation.1).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("stored pattern graph is corrupt: {error}"))
    })?;
    let revision = graph_revision(&graph)?;
    Ok(GraphDocument {
        implementation_id: scope.implementation_id.clone(),
        revision,
        graph,
    })
}

pub(super) struct Projected {
    pub(super) changed: bool,
    pub(super) document: AuthoredProjectedDocument,
}
