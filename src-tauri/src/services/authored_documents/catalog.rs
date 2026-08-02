use super::{
    archive_projection_ledger, commit_message, deterministic_creation_id, exact_graph_json,
    graph_files, insert_creation_association, load_creation_association, load_ledger,
    normalized_creation_request_id, operation_request_fingerprint, pattern_db, principal_key,
    revision_for_clips, score_db, serialize_track, system_author, validate_ledger_scope,
    verify_creation_replay, write_admission, write_ledger, Arc, AuthoredDocuments,
    AuthoredDocumentsError, AuthoredRepositoryId, AuthorizedVenue, BTreeMap, BTreeSet, CommitId,
    DocumentScope, FileMap, FromRow, Graph, HashSet, LedgerRow, MaterializationState, PatternNames,
    PatternSummary, ProjectionLedgerExpectation, ResolvedScope, Result, Score, SqliteConnection,
    SqlitePool, TrackDocument, TrackScope, VenueAccess, VenueResource, Write, MAIN_BRANCH,
    PATTERN_SUMMARY_COLUMNS, SCORE_PATH, TRAILER_OPERATION, TRAILER_OPERATION_ID,
    TRAILER_REQUEST_FINGERPRINT,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum CatalogDeletionOrigin {
    Local,
    Remote,
}

impl AuthoredDocuments {
    /// Create a pattern and its default implementation as one Git/projection
    /// operation. Runtime code never exposes a relational-only graph, even
    /// briefly: the lifecycle gate spans Git preparation, the SQLite insert +
    /// ledger transaction, and the final ref CAS.
    pub async fn create_pattern(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        request_id: &str,
        name: String,
        description: Option<String>,
    ) -> Result<PatternSummary> {
        let request_id = normalized_creation_request_id(request_id)?;
        let principal_key = principal_key(principal);
        let request_fingerprint = operation_request_fingerprint(
            "create_pattern",
            &[
                &name,
                if description.is_some() {
                    "some"
                } else {
                    "none"
                },
                description.as_deref().unwrap_or(""),
            ],
        );
        let pattern_id =
            deterministic_creation_id(&principal_key, "pattern", &request_id, "subject");
        let implementation_id =
            deterministic_creation_id(&principal_key, "pattern", &request_id, "implementation");
        let scope = ResolvedScope::pattern(principal, &pattern_id, &implementation_id)?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        if let Some(existing) =
            load_creation_association(pool, &principal_key, "pattern", &request_id).await?
        {
            verify_creation_replay(
                &existing,
                &request_fingerprint,
                &pattern_id,
                Some(&implementation_id),
            )?;
            self.reconcile_locked(pool, &scope).await?;
            return load_pattern_summary(pool, &pattern_id).await;
        }
        let repository = self.store.ensure_repository(&scope.repository_id)?;
        if load_ledger(pool, &scope).await?.is_some() {
            return Err(AuthoredDocumentsError::Storage(
                "new pattern repository unexpectedly has a projection ledger".into(),
            ));
        }
        let (_, existing_files) = self
            .store
            .read_commit(&scope.repository_id, &repository.main_head)?;
        if !existing_files.is_empty() {
            return Err(AuthoredDocumentsError::Storage(
                "new pattern repository unexpectedly contains authored files".into(),
            ));
        }

        let graph = Graph {
            nodes: Vec::new(),
            edges: Vec::new(),
            args: Vec::new(),
        };
        let files = graph_files(&graph)?;
        let message = commit_message(
            "Create pattern graph",
            &[
                (TRAILER_OPERATION, "initial_import"),
                (TRAILER_OPERATION_ID, &request_id),
                (TRAILER_REQUEST_FINGERPRINT, &request_fingerprint),
            ],
        )?;
        let prepared = self.store.prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&repository.main_head),
            &files,
            &system_author()?,
            &message,
        )?;

        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin Git-backed pattern creation: {error}"))
        })?;
        sqlx::query("INSERT INTO patterns (id, uid, name, description) VALUES (?, ?, ?, ?)")
            .bind(&pattern_id)
            .bind(principal)
            .bind(&name)
            .bind(&description)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!("insert Git-backed pattern: {error}"))
            })?;
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
             VALUES (?, ?, ?, NULL, ?)",
        )
        .bind(&implementation_id)
        .bind(principal)
        .bind(&pattern_id)
        .bind(exact_graph_json(&graph)?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "insert Git-backed default implementation: {error}"
            ))
        })?;
        write_ledger(
            &mut transaction,
            &scope,
            ProjectionLedgerExpectation::Missing,
            &prepared.id,
        )
        .await?;
        insert_creation_association(
            &mut transaction,
            &principal_key,
            "pattern",
            &request_id,
            &request_fingerprint,
            &pattern_id,
            Some(&implementation_id),
            &prepared.id,
        )
        .await?;
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("commit Git-backed pattern: {error}"))
        })?;
        self.store.advance_branch(
            &scope.repository_id,
            MAIN_BRANCH,
            &repository.main_head,
            &prepared.id,
        )?;

        load_pattern_summary(pool, &pattern_id).await
    }

    /// Create an empty score with its initial Git tree and relational catalog
    /// row committed through the same recovery ledger. An authored score owns
    /// a repository from birth; the first clip is not a special migration.
    pub async fn create_score(
        &self,
        pool: &SqlitePool,
        request_id: &str,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
    ) -> Result<Score> {
        let initial_access = VenueAccess::<Write>::write(pool, VenueResource::Venue(venue_id))
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let repository_owner = initial_access.principal().map(str::to_owned);
        drop(initial_access);
        let request_id = normalized_creation_request_id(request_id)?;
        let principal_key = principal_key(repository_owner.as_deref());
        let request_fingerprint = operation_request_fingerprint(
            "create_score",
            &[
                track_id,
                venue_id,
                if name.is_some() { "some" } else { "none" },
                name.unwrap_or(""),
            ],
        );
        let score_id = deterministic_creation_id(&principal_key, "score", &request_id, "subject");
        let track_scope = TrackScope {
            score_id: score_id.clone(),
            track_id: track_id.to_owned(),
            venue_id: venue_id.to_owned(),
        };
        let scope = ResolvedScope::track(repository_owner.as_deref(), track_scope)?;
        let _guard = self.repository_guard(&scope.repository_id).await;
        if let Some(existing) =
            load_creation_association(pool, &principal_key, "score", &request_id).await?
        {
            verify_creation_replay(&existing, &request_fingerprint, &score_id, None)?;
            self.reconcile_locked(pool, &scope).await?;
            return load_score(pool, &score_id).await;
        }
        let repository = self.store.ensure_repository(&scope.repository_id)?;
        if load_ledger(pool, &scope).await?.is_some() {
            return Err(AuthoredDocumentsError::Storage(
                "new score repository unexpectedly has a projection ledger".into(),
            ));
        }
        let (_, existing_files) = self
            .store
            .read_commit(&scope.repository_id, &repository.main_head)?;
        if !existing_files.is_empty() {
            return Err(AuthoredDocumentsError::Storage(
                "new score repository unexpectedly contains authored files".into(),
            ));
        }

        let empty = TrackDocument {
            revision: revision_for_clips(&[]),
            clips: Vec::new(),
        };
        let source = serialize_track(&empty, &PatternNames::new(), None)?;
        let files = FileMap::from([(SCORE_PATH.to_owned(), source.into_bytes())]);
        let message = commit_message(
            "Create track score",
            &[
                (TRAILER_OPERATION, "initial_import"),
                (TRAILER_OPERATION_ID, &request_id),
                (TRAILER_REQUEST_FINGERPRINT, &request_fingerprint),
            ],
        )?;
        let prepared = self.store.prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&repository.main_head),
            &files,
            &system_author()?,
            &message,
        )?;

        let mut access = VenueAccess::<Write>::write(pool, VenueResource::Venue(venue_id))
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        if access.principal() != repository_owner.as_deref() {
            return Err(AuthoredDocumentsError::Scope(
                "score principal changed before creation".into(),
            ));
        }
        let track_is_visible: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM auth_visible_tracks WHERE track_id = ?")
                .bind(track_id)
                .fetch_optional(access.connection())
                .await
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "authorize score track during creation: {error}"
                    ))
                })?;
        if track_is_visible.is_none() {
            return Err(AuthoredDocumentsError::Scope("track does not exist".into()));
        }
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&score_id)
        .bind(repository_owner.as_deref())
        .bind(track_id)
        .bind(venue_id)
        .bind(name)
        .execute(access.connection())
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("insert Git-backed score: {error}"))
        })?;
        write_ledger(
            access.connection(),
            &scope,
            ProjectionLedgerExpectation::Missing,
            &prepared.id,
        )
        .await?;
        insert_creation_association(
            access.connection(),
            &principal_key,
            "score",
            &request_id,
            &request_fingerprint,
            &score_id,
            None,
            &prepared.id,
        )
        .await?;
        access
            .commit()
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        self.store.advance_branch(
            &scope.repository_id,
            MAIN_BRANCH,
            &repository.main_head,
            &prepared.id,
        )?;

        load_score(pool, &score_id).await
    }

    /// Intentionally archive a score. Its complete clip projection and catalog
    /// disappear together, while the exact Git `main`, immutable commits, and
    /// creation records remain. Repeating a completed deletion is naturally
    /// idempotent.
    pub async fn archive_score(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        score_id: &str,
    ) -> Result<()> {
        self.archive_score_with_origin(pool, principal, score_id, CatalogDeletionOrigin::Local)
            .await?;
        Ok(())
    }

    /// Apply another device's score tombstone through the exact same archive
    /// protocol as a local delete. `false` means the catalog was already gone.
    pub(crate) async fn archive_score_from_remote(
        &self,
        pool: &SqlitePool,
        principal: &str,
        score_id: &str,
    ) -> Result<bool> {
        self.archive_score_with_origin(
            pool,
            Some(principal),
            score_id,
            CatalogDeletionOrigin::Remote,
        )
        .await
    }

    /// Apply a server-visible tombstone for a score owned by another
    /// principal. The immutable catalog row determines repository ownership;
    /// the currently signed-in member is never impersonated as that owner.
    /// `None` means the row has no exact retained Git ledger and may only be
    /// removed by the caller's separately checked empty-container path.
    pub(crate) async fn archive_git_backed_score_from_server(
        &self,
        pool: &SqlitePool,
        score_id: &str,
    ) -> Result<Option<bool>> {
        let Some(catalog) = optional_track_scope_from_catalog(pool, score_id).await? else {
            return Ok(None);
        };
        let scope = ResolvedScope::track(catalog.owner.as_deref(), catalog.track_scope)?;
        if load_ledger(pool, &scope).await?.is_none() {
            let colliding: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM authored_state_projections
                 WHERE document_kind = 'track_score' AND score_id = ?",
            )
            .bind(score_id)
            .fetch_one(pool)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "inspect server score tombstone ledger: {error}"
                ))
            })?;
            if colliding != 0 {
                return Err(AuthoredDocumentsError::Storage(
                    "server score tombstone collides with another repository identity".into(),
                ));
            }
            return Ok(None);
        }
        self.archive_score_with_origin(
            pool,
            catalog.owner.as_deref(),
            score_id,
            CatalogDeletionOrigin::Remote,
        )
        .await
        .map(Some)
    }

    async fn archive_score_with_origin(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        score_id: &str,
        origin: CatalogDeletionOrigin,
    ) -> Result<bool> {
        let _lifecycle = Arc::clone(&self.lifecycle_lock).write_owned().await;
        let Some(catalog) = optional_track_scope_from_catalog(pool, score_id).await? else {
            return self
                .archive_missing_score(pool, principal, score_id, origin)
                .await;
        };
        if catalog.owner.as_deref() != principal {
            return Err(AuthoredDocumentsError::Scope(format!(
                "score {score_id} does not exist"
            )));
        }

        let scope = ResolvedScope::track(catalog.owner.as_deref(), catalog.track_scope.clone())?;
        let _repository = self
            .repository_guard_inside_lifecycle(&scope.repository_id)
            .await;
        let main = self.reconcile_locked(pool, &scope).await?;

        match origin {
            CatalogDeletionOrigin::Local => {
                let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(score_id))
                    .await
                    .map_err(AuthoredDocumentsError::Scope)?;
                access
                    .require_venue(&catalog.track_scope.venue_id)
                    .map_err(AuthoredDocumentsError::Scope)?;
                if access.principal() != catalog.owner.as_deref() || access.principal() != principal
                {
                    return Err(AuthoredDocumentsError::Scope(format!(
                        "score {score_id} does not exist"
                    )));
                }
                archive_score_projection(access.connection(), &scope, &main.head, score_id, false)
                    .await?;
                access
                    .commit()
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
            }
            CatalogDeletionOrigin::Remote => {
                let mut transaction =
                    pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
                        AuthoredDocumentsError::Storage(format!(
                            "begin remote authored score archive: {error}"
                        ))
                    })?;
                write_admission::enter_remote_writes(&mut transaction)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                archive_score_projection(&mut transaction, &scope, &main.head, score_id, true)
                    .await?;
                write_admission::leave_remote_writes(&mut transaction)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                transaction.commit().await.map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "commit remote authored score archive: {error}"
                    ))
                })?;
            }
        }
        Ok(true)
    }

    /// Intentionally archive an unused pattern and every one of its graph
    /// implementations. All repositories are fenced together, then every
    /// ledger and the catalog deletion commit in one SQLite transaction.
    pub async fn archive_pattern(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        pattern_id: &str,
    ) -> Result<()> {
        self.archive_pattern_with_origin(pool, principal, pattern_id, CatalogDeletionOrigin::Local)
            .await?;
        Ok(())
    }

    /// Apply another device's pattern tombstone through the local authored
    /// archive protocol. `false` means the catalog was already gone.
    pub(crate) async fn archive_pattern_from_remote(
        &self,
        pool: &SqlitePool,
        principal: &str,
        pattern_id: &str,
    ) -> Result<bool> {
        self.archive_pattern_with_origin(
            pool,
            Some(principal),
            pattern_id,
            CatalogDeletionOrigin::Remote,
        )
        .await
    }

    /// Apply a server-visible tombstone to another principal's Git-backed
    /// pattern without treating the current member as its owner. Every live
    /// implementation must already have its exact retained ledger; a partial
    /// or colliding projection set fails closed instead of initial-importing
    /// foreign graph state.
    pub(crate) async fn archive_git_backed_pattern_from_server(
        &self,
        pool: &SqlitePool,
        pattern_id: &str,
    ) -> Result<Option<bool>> {
        let owner: Option<Option<String>> =
            sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
                .bind(pattern_id)
                .fetch_optional(pool)
                .await
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "resolve server pattern tombstone owner: {error}"
                    ))
                })?;
        let Some(owner) = owner else {
            return Ok(None);
        };
        let ledger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM authored_state_projections
             WHERE document_kind = 'pattern_graph' AND subject_id = ?",
        )
        .bind(pattern_id)
        .fetch_one(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "inspect server pattern tombstone ledgers: {error}"
            ))
        })?;
        if ledger_count == 0 {
            return Ok(None);
        }
        let scopes = pattern_projection_scopes(pool, pattern_id).await?;
        for scope in &scopes {
            if load_ledger(pool, scope).await?.is_none() {
                return Err(AuthoredDocumentsError::Storage(
                    "server pattern tombstone found a partial or colliding projection set".into(),
                ));
            }
        }
        self.archive_pattern_with_origin(
            pool,
            owner.as_deref(),
            pattern_id,
            CatalogDeletionOrigin::Remote,
        )
        .await
        .map(Some)
    }

    async fn archive_pattern_with_origin(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        pattern_id: &str,
        origin: CatalogDeletionOrigin,
    ) -> Result<bool> {
        let _lifecycle = Arc::clone(&self.lifecycle_lock).write_owned().await;
        let owner: Option<Option<String>> =
            sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
                .bind(pattern_id)
                .fetch_optional(pool)
                .await
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "resolve pattern catalog for archive: {error}"
                    ))
                })?;
        let Some(owner) = owner else {
            return self
                .archive_missing_pattern(pool, principal, pattern_id, origin)
                .await;
        };
        if owner.as_deref() != principal {
            return Err(AuthoredDocumentsError::Scope(format!(
                "pattern {pattern_id} does not exist"
            )));
        }

        let has_graph_state: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM implementations WHERE pattern_id = ?1)
                    OR EXISTS(
                        SELECT 1 FROM authored_state_projections
                        WHERE document_kind = 'pattern_graph' AND subject_id = ?1
                    )",
        )
        .bind(pattern_id)
        .fetch_one(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "inspect pattern graph state for archive: {error}"
            ))
        })?;
        let mut scopes = if has_graph_state == 0 {
            Vec::new()
        } else {
            pattern_projection_scopes(pool, pattern_id).await?
        };
        scopes.sort_by(|left, right| {
            left.repository_id
                .as_str()
                .cmp(right.repository_id.as_str())
        });
        let mut repository_guards = Vec::with_capacity(scopes.len());
        for scope in &scopes {
            repository_guards.push(
                self.repository_guard_inside_lifecycle(&scope.repository_id)
                    .await,
            );
        }
        let mut main_heads = Vec::with_capacity(scopes.len());
        for scope in &scopes {
            main_heads.push(self.reconcile_locked(pool, scope).await?.head);
        }

        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin authored pattern archive: {error}"))
        })?;
        if origin == CatalogDeletionOrigin::Remote {
            write_admission::enter_remote_writes(&mut transaction)
                .await
                .map_err(AuthoredDocumentsError::Storage)?;
        }
        verify_pattern_archive_scopes(&mut transaction, pattern_id, owner.as_deref(), &scopes)
            .await?;
        for (scope, main_head) in scopes.iter().zip(&main_heads) {
            archive_projection_ledger(
                &mut transaction,
                scope,
                main_head,
                MaterializationState::Present,
            )
            .await?;
        }
        if origin == CatalogDeletionOrigin::Remote {
            let updated = sqlx::query("UPDATE patterns SET origin = 'remote' WHERE id = ?")
                .bind(pattern_id)
                .execute(&mut *transaction)
                .await
                .map_err(|error| {
                    AuthoredDocumentsError::Storage(format!(
                        "mark remotely archived pattern provenance: {error}"
                    ))
                })?
                .rows_affected();
            if updated != 1 {
                return Err(AuthoredDocumentsError::Storage(
                    "pattern disappeared during authored archive".into(),
                ));
            }
        }
        pattern_db::delete_unused_pattern_for_authored_archive(
            &mut transaction,
            pattern_id,
            owner.as_deref(),
        )
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
        if origin == CatalogDeletionOrigin::Remote {
            write_admission::leave_remote_writes(&mut transaction)
                .await
                .map_err(AuthoredDocumentsError::Storage)?;
        }
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("commit authored pattern archive: {error}"))
        })?;
        drop(repository_guards);
        Ok(true)
    }

    /// A remote tombstone can arrive before sign-in has restored catalog rows
    /// that sign-out intentionally removed. In that case the retained Git
    /// ledger is already authoritative; terminalize `absent` without first
    /// recreating a catalog row only to delete it again.
    async fn archive_missing_score(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        score_id: &str,
        origin: CatalogDeletionOrigin,
    ) -> Result<bool> {
        let rows: Vec<LedgerRow> = sqlx::query_as(
            "SELECT repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
                    score_id, implementation_id, implementation_name, projected_commit,
                    materialization_state
             FROM authored_state_projections
             WHERE document_kind = 'track_score' AND score_id = ?",
        )
        .bind(score_id)
        .fetch_all(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "inspect missing score archive ledger: {error}"
            ))
        })?;
        if rows.is_empty() {
            return Ok(false);
        }
        let expected_principal_key = principal_key(principal);
        if rows
            .iter()
            .any(|row| row.principal_key != expected_principal_key)
        {
            return Err(AuthoredDocumentsError::Scope(format!(
                "score {score_id} does not exist"
            )));
        }
        let states = rows
            .iter()
            .map(|row| MaterializationState::parse(&row.materialization_state))
            .collect::<Result<Vec<_>>>()?;
        if states
            .iter()
            .all(|state| *state == MaterializationState::Archived)
        {
            return Ok(false);
        }
        if origin != CatalogDeletionOrigin::Remote
            || states
                .iter()
                .any(|state| *state != MaterializationState::Absent)
        {
            return Err(AuthoredDocumentsError::Scope(format!(
                "score {score_id} is not materialized and has not been archived"
            )));
        }
        if rows.len() != 1 {
            return Err(AuthoredDocumentsError::Storage(
                "score has more than one retained projection ledger".into(),
            ));
        }
        let row = &rows[0];
        let scope = ResolvedScope::track(
            principal,
            TrackScope {
                score_id: score_id.to_owned(),
                track_id: row.track_id.clone().ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "retained score ledger is missing its track identity".into(),
                    )
                })?,
                venue_id: row.venue_id.clone().ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "retained score ledger is missing its venue identity".into(),
                    )
                })?,
            },
        )?;
        validate_ledger_scope(&scope, row)?;
        let projected_commit = CommitId::parse(&row.projected_commit)?;
        let _repository = self
            .repository_guard_inside_lifecycle(&scope.repository_id)
            .await;
        if self.store.main_head(&scope.repository_id)? != projected_commit {
            return Err(AuthoredDocumentsError::Storage(
                "absent score ledger does not match Git main during remote archive".into(),
            ));
        }
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin absent score terminal archive: {error}"))
        })?;
        archive_projection_ledger(
            &mut transaction,
            &scope,
            &projected_commit,
            MaterializationState::Absent,
        )
        .await?;
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "commit absent score terminal archive: {error}"
            ))
        })?;
        Ok(true)
    }

    async fn archive_missing_pattern(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        pattern_id: &str,
        origin: CatalogDeletionOrigin,
    ) -> Result<bool> {
        let rows: Vec<LedgerRow> = sqlx::query_as(
            "SELECT repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
                    score_id, implementation_id, implementation_name, projected_commit,
                    materialization_state
             FROM authored_state_projections
             WHERE document_kind = 'pattern_graph' AND subject_id = ?
             ORDER BY repository_id",
        )
        .bind(pattern_id)
        .fetch_all(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "inspect missing pattern archive ledgers: {error}"
            ))
        })?;
        if rows.is_empty() {
            return Ok(false);
        }
        let expected_principal_key = principal_key(principal);
        if rows
            .iter()
            .any(|row| row.principal_key != expected_principal_key)
        {
            return Err(AuthoredDocumentsError::Scope(format!(
                "pattern {pattern_id} does not exist"
            )));
        }
        let states = rows
            .iter()
            .map(|row| MaterializationState::parse(&row.materialization_state))
            .collect::<Result<Vec<_>>>()?;
        if states
            .iter()
            .all(|state| *state == MaterializationState::Archived)
        {
            return Ok(false);
        }
        if origin != CatalogDeletionOrigin::Remote
            || states
                .iter()
                .any(|state| *state != MaterializationState::Absent)
        {
            return Err(AuthoredDocumentsError::Scope(format!(
                "pattern {pattern_id} is not materialized and has not been archived"
            )));
        }

        let mut scopes = Vec::with_capacity(rows.len());
        let mut projected_commits = BTreeMap::new();
        for row in &rows {
            let implementation_id = row.implementation_id.as_deref().ok_or_else(|| {
                AuthoredDocumentsError::Storage(
                    "retained pattern ledger is missing its implementation identity".into(),
                )
            })?;
            let scope = ResolvedScope::pattern(principal, pattern_id, implementation_id)?;
            validate_ledger_scope(&scope, row)?;
            projected_commits.insert(
                scope.repository_id.clone(),
                CommitId::parse(&row.projected_commit)?,
            );
            scopes.push(scope);
        }
        scopes.sort_by(|left, right| {
            left.repository_id
                .as_str()
                .cmp(right.repository_id.as_str())
        });
        let mut repository_guards = Vec::with_capacity(scopes.len());
        for scope in &scopes {
            repository_guards.push(
                self.repository_guard_inside_lifecycle(&scope.repository_id)
                    .await,
            );
            let projected_commit =
                projected_commits.get(&scope.repository_id).ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "pattern archive lost a retained projected commit".into(),
                    )
                })?;
            if self.store.main_head(&scope.repository_id)? != *projected_commit {
                return Err(AuthoredDocumentsError::Storage(
                    "absent pattern ledger does not match Git main during remote archive".into(),
                ));
            }
        }
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "begin absent pattern terminal archive: {error}"
            ))
        })?;
        let current_ledgers: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT repository_id, principal_key, materialization_state
             FROM authored_state_projections
             WHERE document_kind = 'pattern_graph' AND subject_id = ?
             ORDER BY repository_id",
        )
        .bind(pattern_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "revalidate absent pattern archive ledgers: {error}"
            ))
        })?;
        let current_repositories: BTreeSet<String> = current_ledgers
            .iter()
            .map(|(repository_id, _, _)| repository_id.clone())
            .collect();
        let expected_repositories: BTreeSet<String> = scopes
            .iter()
            .map(|scope| scope.repository_id.as_str().to_owned())
            .collect();
        if current_repositories != expected_repositories {
            return Err(AuthoredDocumentsError::Storage(
                "absent pattern projection set changed during remote archive".into(),
            ));
        }
        for (_, principal, state) in &current_ledgers {
            if principal != &expected_principal_key
                || MaterializationState::parse(state)? != MaterializationState::Absent
            {
                return Err(AuthoredDocumentsError::Storage(
                    "absent pattern projection set changed during remote archive".into(),
                ));
            }
        }
        for scope in &scopes {
            archive_projection_ledger(
                &mut transaction,
                scope,
                projected_commits.get(&scope.repository_id).ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "pattern archive lost a retained projected commit".into(),
                    )
                })?,
                MaterializationState::Absent,
            )
            .await?;
        }
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "commit absent pattern terminal archive: {error}"
            ))
        })?;
        drop(repository_guards);
        Ok(true)
    }
}

#[derive(Clone)]
pub(super) struct CatalogTrackScope {
    pub(super) owner: Option<String>,
    pub(super) track_scope: TrackScope,
}

pub(super) async fn track_scope_from_catalog(
    pool: &SqlitePool,
    score_id: &str,
) -> Result<CatalogTrackScope> {
    optional_track_scope_from_catalog(pool, score_id)
        .await?
        .ok_or_else(|| AuthoredDocumentsError::Scope(format!("score {score_id} does not exist")))
}

pub(super) async fn optional_track_scope_from_catalog(
    pool: &SqlitePool,
    score_id: &str,
) -> Result<Option<CatalogTrackScope>> {
    #[derive(FromRow)]
    struct Row {
        owner: Option<String>,
        track_id: String,
        venue_id: Option<String>,
    }

    let row = sqlx::query_as::<_, Row>(
        "SELECT uid AS owner, track_id, venue_id FROM scores WHERE id = ?",
    )
    .bind(score_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("resolve score projection catalog: {error}"))
    })?;
    let Some(row) = row else {
        return Ok(None);
    };
    let venue_id = row.venue_id.ok_or_else(|| {
        AuthoredDocumentsError::Storage(format!(
            "score {score_id} has no venue identity for authored Git routing"
        ))
    })?;
    Ok(Some(CatalogTrackScope {
        owner: row.owner,
        track_scope: TrackScope {
            score_id: score_id.to_owned(),
            track_id: row.track_id,
            venue_id,
        },
    }))
}

pub(super) async fn pattern_projection_scopes(
    pool: &SqlitePool,
    pattern_id: &str,
) -> Result<Vec<ResolvedScope>> {
    let owner: Option<Option<String>> = sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
        .bind(pattern_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("resolve pattern projection catalog: {error}"))
        })?;
    let owner = owner.ok_or_else(|| {
        AuthoredDocumentsError::Scope(format!("pattern {pattern_id} does not exist"))
    })?;
    let implementations: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, uid FROM implementations WHERE pattern_id = ? ORDER BY id")
            .bind(pattern_id)
            .fetch_all(pool)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "list live pattern implementations for reconciliation: {error}"
                ))
            })?;
    let mut implementation_ids = BTreeSet::new();
    for (implementation_id, implementation_owner) in implementations {
        if implementation_owner != owner {
            return Err(AuthoredDocumentsError::Storage(format!(
                "implementation {implementation_id} owner does not match pattern {pattern_id}"
            )));
        }
        implementation_ids.insert(implementation_id);
    }

    let retained: Vec<(Option<String>, String)> = sqlx::query_as(
        "SELECT implementation_id, principal_key
         FROM authored_state_projections
         WHERE document_kind = 'pattern_graph' AND subject_id = ?",
    )
    .bind(pattern_id)
    .fetch_all(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!(
            "list retained pattern projections for reconciliation: {error}"
        ))
    })?;
    let expected_principal = principal_key(owner.as_deref());
    for (implementation_id, retained_principal) in retained {
        if retained_principal == expected_principal {
            let implementation_id = implementation_id.ok_or_else(|| {
                AuthoredDocumentsError::Storage(
                    "pattern projection ledger is missing its implementation identity".into(),
                )
            })?;
            implementation_ids.insert(implementation_id);
        }
    }
    if implementation_ids.is_empty() {
        return Err(AuthoredDocumentsError::Scope(format!(
            "pattern {pattern_id} has no live or Git-backed implementation"
        )));
    }

    implementation_ids
        .into_iter()
        .map(|implementation_id| {
            ResolvedScope::pattern(owner.as_deref(), pattern_id, &implementation_id)
        })
        .collect()
}

pub(super) async fn available_projection_scopes(pool: &SqlitePool) -> Result<Vec<ResolvedScope>> {
    #[derive(FromRow)]
    struct ScoreRow {
        score_id: String,
        owner: Option<String>,
        track_id: String,
        venue_id: Option<String>,
        writable_venue: i64,
    }

    #[derive(FromRow)]
    struct ImplementationRow {
        pattern_id: String,
        pattern_owner: Option<String>,
        implementation_id: String,
        implementation_owner: Option<String>,
    }

    #[derive(FromRow)]
    struct RetainedGraphRow {
        pattern_id: String,
        pattern_owner: Option<String>,
        implementation_id: Option<String>,
        retained_principal: String,
    }

    let mut scopes = BTreeMap::<AuthoredRepositoryId, ResolvedScope>::new();
    let admitted_principal = sqlx::query_scalar::<_, Option<String>>(
        "SELECT active_uid FROM auth_write_admission
             WHERE singleton = 1 AND armed = 1 AND accepting = 1
               AND maintenance = 0 AND remote_writes = 0",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!(
            "load current principal for authored reconciliation: {error}"
        ))
    })?;
    let retained_repository_ids =
        sqlx::query_scalar::<_, String>("SELECT repository_id FROM authored_state_projections")
            .fetch_all(pool)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "list retained authored repositories for reconciliation: {error}"
                ))
            })?
            .into_iter()
            .collect::<HashSet<_>>();
    let retained_score_ids = sqlx::query_scalar::<_, String>(
        "SELECT score_id FROM authored_state_projections
         WHERE document_kind = 'track_score' AND score_id IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!(
            "list retained authored scores for reconciliation: {error}"
        ))
    })?
    .into_iter()
    .collect::<HashSet<_>>();

    let scores = sqlx::query_as::<_, ScoreRow>(
        "SELECT id AS score_id, uid AS owner, track_id, venue_id,
                EXISTS(
                    SELECT 1 FROM auth_venue_access access
                    WHERE access.venue_id = scores.venue_id
                      AND access.owner_access = 1
                ) AS writable_venue
         FROM scores ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!(
            "list score projections for reconciliation: {error}"
        ))
    })?;
    for score in scores {
        let principal_writable = admitted_principal.as_ref().is_some_and(|principal| {
            principal.as_deref() == score.owner.as_deref() && score.writable_venue == 1
        });
        if !principal_writable && !retained_score_ids.contains(&score.score_id) {
            continue;
        }
        let venue_id = score.venue_id.ok_or_else(|| {
            AuthoredDocumentsError::Storage(format!(
                "score {} has no venue identity for authored Git routing",
                score.score_id
            ))
        })?;
        let scope = ResolvedScope::track(
            score.owner.as_deref(),
            TrackScope {
                score_id: score.score_id,
                track_id: score.track_id,
                venue_id,
            },
        )?;
        if !principal_writable && !retained_repository_ids.contains(scope.repository_id.as_str()) {
            continue;
        }
        scopes.insert(scope.repository_id.clone(), scope);
    }

    let implementations = sqlx::query_as::<_, ImplementationRow>(
        "SELECT p.id AS pattern_id, p.uid AS pattern_owner,
                i.id AS implementation_id, i.uid AS implementation_owner
         FROM implementations i
         JOIN patterns p ON p.id = i.pattern_id
         ORDER BY p.id, i.id",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!(
            "list graph projections for reconciliation: {error}"
        ))
    })?;
    for implementation in implementations {
        let scope = ResolvedScope::pattern(
            implementation.pattern_owner.as_deref(),
            &implementation.pattern_id,
            &implementation.implementation_id,
        )?;
        let principal_writable = admitted_principal.as_ref().is_some_and(|principal| {
            principal.as_deref() == implementation.pattern_owner.as_deref()
        });
        if !principal_writable && !retained_repository_ids.contains(scope.repository_id.as_str()) {
            continue;
        }
        if implementation.implementation_owner != implementation.pattern_owner {
            return Err(AuthoredDocumentsError::Storage(format!(
                "implementation {} owner does not match pattern {}",
                implementation.implementation_id, implementation.pattern_id
            )));
        }
        scopes.insert(scope.repository_id.clone(), scope);
    }

    // After sign-out the implementation row is deliberately absent. Its
    // retained routing ledger is therefore the only honest way to discover
    // which Git repositories can be rematerialized once the pattern catalog
    // row returns.
    let retained_graphs = sqlx::query_as::<_, RetainedGraphRow>(
        "SELECT p.id AS pattern_id, p.uid AS pattern_owner,
                projection.implementation_id,
                projection.principal_key AS retained_principal
         FROM authored_state_projections projection
         JOIN patterns p ON p.id = projection.subject_id
         WHERE projection.document_kind = 'pattern_graph'
         ORDER BY p.id, projection.implementation_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!(
            "list retained graph projections for reconciliation: {error}"
        ))
    })?;
    for retained in retained_graphs {
        if retained.retained_principal != principal_key(retained.pattern_owner.as_deref()) {
            continue;
        }
        let implementation_id = retained.implementation_id.ok_or_else(|| {
            AuthoredDocumentsError::Storage(
                "pattern projection ledger is missing its implementation identity".into(),
            )
        })?;
        let scope = ResolvedScope::pattern(
            retained.pattern_owner.as_deref(),
            &retained.pattern_id,
            &implementation_id,
        )?;
        scopes.insert(scope.repository_id.clone(), scope);
    }

    Ok(scopes.into_values().collect())
}

async fn load_pattern_summary(pool: &SqlitePool, pattern_id: &str) -> Result<PatternSummary> {
    sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "SELECT {PATTERN_SUMMARY_COLUMNS} FROM patterns WHERE id = ?"
    )))
    .bind(pattern_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load Git-backed pattern: {error}")))
}

async fn load_score(pool: &SqlitePool, score_id: &str) -> Result<Score> {
    sqlx::query_as::<_, Score>(
        "SELECT id, uid, track_id, venue_id, name, created_at, updated_at
         FROM scores WHERE id = ?",
    )
    .bind(score_id)
    .fetch_one(pool)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load Git-backed score: {error}")))
}

async fn verify_score_archive_scope(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
) -> Result<()> {
    let DocumentScope::Track(track_scope) = &scope.document else {
        return Err(AuthoredDocumentsError::Storage(
            "cannot archive a graph repository as a score".into(),
        ));
    };
    let current: Option<(Option<String>, String, Option<String>)> =
        sqlx::query_as("SELECT uid, track_id, venue_id FROM scores WHERE id = ?")
            .bind(&track_scope.score_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!("revalidate score archive scope: {error}"))
            })?;
    let Some((owner, track_id, venue_id)) = current else {
        return Err(AuthoredDocumentsError::Storage(
            "score disappeared during authored archive".into(),
        ));
    };
    if owner != scope.owner_user_id
        || track_id != track_scope.track_id
        || venue_id.as_deref() != Some(track_scope.venue_id.as_str())
    {
        return Err(AuthoredDocumentsError::Storage(
            "score scope changed during authored archive".into(),
        ));
    }
    Ok(())
}

async fn archive_score_projection(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    main_head: &CommitId,
    score_id: &str,
    remote: bool,
) -> Result<()> {
    verify_score_archive_scope(connection, scope).await?;
    archive_projection_ledger(connection, scope, main_head, MaterializationState::Present).await?;
    if remote {
        let updated = sqlx::query("UPDATE scores SET origin = 'remote' WHERE id = ?")
            .bind(score_id)
            .execute(&mut *connection)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "mark remotely archived score provenance: {error}"
                ))
            })?
            .rows_affected();
        if updated != 1 {
            return Err(AuthoredDocumentsError::Storage(
                "score disappeared during authored archive".into(),
            ));
        }
    }
    score_db::delete_score_projection_for_authored_archive(
        connection,
        score_id,
        scope.owner_user_id.as_deref(),
    )
    .await
    .map_err(AuthoredDocumentsError::Scope)
}

async fn verify_pattern_archive_scopes(
    connection: &mut SqliteConnection,
    pattern_id: &str,
    expected_owner: Option<&str>,
    scopes: &[ResolvedScope],
) -> Result<()> {
    let owner: Option<Option<String>> = sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
        .bind(pattern_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("revalidate pattern archive owner: {error}"))
        })?;
    let Some(owner) = owner else {
        return Err(AuthoredDocumentsError::Storage(
            "pattern disappeared during authored archive".into(),
        ));
    };
    if owner.as_deref() != expected_owner {
        return Err(AuthoredDocumentsError::Storage(
            "pattern owner changed during authored archive".into(),
        ));
    }

    let expected_principal = principal_key(expected_owner);
    let mut expected_implementations = BTreeSet::new();
    let mut expected_repositories = BTreeSet::new();
    for scope in scopes {
        let DocumentScope::Pattern(graph_scope) = &scope.document else {
            return Err(AuthoredDocumentsError::Storage(
                "score repository was mixed into a pattern archive".into(),
            ));
        };
        if graph_scope.pattern_id != pattern_id
            || scope.subject_id != pattern_id
            || scope.principal_key != expected_principal
        {
            return Err(AuthoredDocumentsError::Storage(
                "pattern archive contains a mismatched repository scope".into(),
            ));
        }
        expected_implementations.insert(graph_scope.implementation_id.clone());
        expected_repositories.insert(scope.repository_id.as_str().to_owned());
    }

    let implementations: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, uid FROM implementations WHERE pattern_id = ? ORDER BY id")
            .bind(pattern_id)
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "revalidate pattern archive implementations: {error}"
                ))
            })?;
    let actual_implementations: BTreeSet<String> = implementations
        .iter()
        .map(|(implementation_id, _)| implementation_id.clone())
        .collect();
    if actual_implementations != expected_implementations
        || implementations
            .iter()
            .any(|(_, owner)| owner.as_deref() != expected_owner)
    {
        return Err(AuthoredDocumentsError::Storage(
            "pattern implementation set changed during authored archive".into(),
        ));
    }

    let ledgers: Vec<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT repository_id, implementation_id, principal_key, materialization_state
         FROM authored_state_projections
         WHERE document_kind = 'pattern_graph' AND subject_id = ?
         ORDER BY repository_id",
    )
    .bind(pattern_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("revalidate pattern archive ledgers: {error}"))
    })?;
    let actual_repositories: BTreeSet<String> = ledgers
        .iter()
        .map(|(repository_id, _, _, _)| repository_id.clone())
        .collect();
    let ledger_implementations: BTreeSet<String> = ledgers
        .iter()
        .filter_map(|(_, implementation_id, _, _)| implementation_id.clone())
        .collect();
    if actual_repositories != expected_repositories
        || ledger_implementations != expected_implementations
    {
        return Err(AuthoredDocumentsError::Storage(
            "pattern projection set changed during authored archive".into(),
        ));
    }
    for (_, implementation_id, principal, state) in &ledgers {
        if implementation_id.is_none()
            || principal != &expected_principal
            || MaterializationState::parse(state)? != MaterializationState::Present
        {
            return Err(AuthoredDocumentsError::Storage(
                "pattern projection set changed during authored archive".into(),
            ));
        }
    }
    Ok(())
}
