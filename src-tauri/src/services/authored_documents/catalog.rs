use chrono::Utc;
use sqlx::{SqliteConnection, SqlitePool};

use super::edits::PatternForkSource;
use super::operations::enqueue_local_row;
use super::{
    deterministic_creation_id, exact_graph_json, graph_files, normalized_creation_request_id,
    operation_request_fingerprint, principal_key, revision_for_clips, revision_metadata,
    serialize_track, write_admission, AuthoredDocuments, AuthoredDocumentsError,
    AuthoredIdentitySwitch, FileMap, ForkPatternInput, ForkPatternResult, Graph,
    NewAuthoredDocument, PatternNames, PatternSummary, ResolvedScope, Result, Score, TrackDocument,
    TrackScope, VenueAccess, VenueResource, Write, PATTERN_SUMMARY_COLUMNS, SCORE_PATH,
};
use crate::database::local::venue_access::AuthorizedVenue;
use crate::services::authored_state::AuthoredDocumentKind;
use crate::sync::authored_remote::ArchiveAuthoredDocumentInput;
use crate::sync::pending;

impl AuthoredDocuments {
    /// Import every live projection owned by the currently admitted principal
    /// into the relational revision substrate. The absence of an
    /// `authored_documents` row is the durable work queue, so this is
    /// idempotent and a failed codec/validation pass leaves that route wholly
    /// absent for the next startup or pull. No score DSL or graph encoding is
    /// duplicated in SQL.
    pub(crate) async fn bootstrap_live_projections(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
    ) -> Result<usize> {
        let scopes = self.bootstrap_scopes(pool, principal).await?;
        let mut imported = self.bootstrap_legacy_archives(pool, principal).await?;
        for scope in scopes {
            if self.bootstrap_scope(pool, &scope).await? {
                imported += 1;
            }
        }
        Ok(imported)
    }

    /// Identity activation already owns the lifecycle write fence. This
    /// variant keeps that fence across the fallible import and acquires only
    /// per-document locks, so rollback cannot race a newly admitted writer.
    pub(crate) async fn bootstrap_live_projections_during_identity_switch(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        _identity_switch: &AuthoredIdentitySwitch,
    ) -> Result<usize> {
        let scopes = self.bootstrap_scopes(pool, principal).await?;
        let mut imported = self
            .bootstrap_legacy_archives_inside_identity_switch(pool, principal)
            .await?;
        for scope in scopes {
            let _guard = self
                .document_guard_inside_lifecycle(&scope.document_id)
                .await;
            if self.bootstrap_scope_locked(pool, &scope).await? {
                imported += 1;
            }
        }
        Ok(imported)
    }

    async fn bootstrap_scopes(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
    ) -> Result<Vec<ResolvedScope>> {
        let admitted = crate::database::local::auth::admitted_principal(pool)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        if admitted.as_deref() != principal {
            return Err(AuthoredDocumentsError::Scope(
                "authored projection bootstrap principal is not admitted".into(),
            ));
        }

        let scores: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT score.id, score.track_id, score.venue_id
             FROM scores AS score
             JOIN tracks AS track ON track.id = score.track_id
             JOIN venues AS venue ON venue.id = score.venue_id
             WHERE score.uid IS ?
               AND venue.uid IS ?
               AND score.venue_id IS NOT NULL
             ORDER BY score.id",
        )
        .bind(principal)
        .bind(principal)
        .fetch_all(pool)
        .await
        .map_err(storage("scan legacy score projections"))?;

        let graphs: Vec<(String, String)> = sqlx::query_as(
            "SELECT pattern.id, implementation.id
             FROM implementations AS implementation
             JOIN patterns AS pattern ON pattern.id = implementation.pattern_id
             WHERE pattern.uid IS ? AND implementation.uid IS ?
             ORDER BY pattern.id, implementation.id",
        )
        .bind(principal)
        .bind(principal)
        .fetch_all(pool)
        .await
        .map_err(storage("scan legacy graph projections"))?;

        let mut scopes = Vec::with_capacity(scores.len() + graphs.len());
        for (score_id, track_id, venue_id) in scores {
            scopes.push(ResolvedScope::track(
                principal,
                TrackScope {
                    score_id,
                    track_id,
                    venue_id,
                },
            )?);
        }
        for (pattern_id, implementation_id) in graphs {
            scopes.push(ResolvedScope::pattern(
                principal,
                &pattern_id,
                &implementation_id,
            )?);
        }
        Ok(scopes)
    }

    async fn bootstrap_legacy_archives(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
    ) -> Result<usize> {
        let routes = legacy_archived_routes(pool, principal).await?;
        if routes.is_empty() {
            drop_empty_legacy_archive_queue(pool).await?;
            return Ok(0);
        }
        let _lifecycle = self.lifecycle_lock.clone().read_owned().await;
        self.bootstrap_legacy_archives_locked(pool, principal, routes)
            .await
    }

    async fn bootstrap_legacy_archives_inside_identity_switch(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
    ) -> Result<usize> {
        let routes = legacy_archived_routes(pool, principal).await?;
        if routes.is_empty() {
            drop_empty_legacy_archive_queue(pool).await?;
            return Ok(0);
        }
        self.bootstrap_legacy_archives_locked(pool, principal, routes)
            .await
    }

    async fn bootstrap_legacy_archives_locked(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        routes: Vec<LegacyArchivedRoute>,
    ) -> Result<usize> {
        let mut documents = routes
            .into_iter()
            .map(|route| {
                let document = route.document()?;
                Ok((document, route))
            })
            .collect::<Result<Vec<_>>>()?;
        documents.sort_by(|(left, _), (right, _)| left.id.cmp(&right.id));

        // Hold every route lock for both phases. All pattern implementation
        // identities must exist unarchived before the first sibling becomes
        // terminal, otherwise the anti-resurrection guard would correctly
        // reject the remaining sibling identities.
        let mut guards = Vec::with_capacity(documents.len());
        for (document, _) in &documents {
            guards.push(self.document_guard_inside_lifecycle(&document.id).await);
        }

        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage("begin legacy terminal-route bootstrap"))?;
        let device_id: String = sqlx::query_scalar(
            "SELECT device_id FROM authored_device_identity WHERE singleton = 1",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage("load authored device identity for legacy archive"))?;

        // Phase one: insert every immutable identity and snapshot its
        // unarchived payload into the sync queue. Archive RPC operations are
        // enqueued only in phase two, preserving dependency order.
        for (document, route) in &documents {
            sqlx::query(
                "INSERT INTO authored_documents
                 (document_id, document_kind, principal_key, subject_id,
                  track_id, venue_id, score_id, implementation_id, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            .bind(&route.created_at)
            .execute(&mut *transaction)
            .await
            .map_err(storage("insert legacy terminal document identity"))?;
            let stored = self.store.document(&mut transaction, &document.id).await?;
            if stored.spec != *document {
                return Err(AuthoredDocumentsError::Storage(format!(
                    "legacy terminal route {} conflicts with another document scope",
                    document.id
                )));
            }
            if stored.archived_at.is_none() {
                if let Some(user_id) = principal {
                    enqueue_local_row(
                        &mut transaction,
                        user_id,
                        "authored_documents",
                        document.id.as_str(),
                    )
                    .await?;
                }
            }
        }

        // Phase two: archive at a current head that may have arrived from
        // another device; otherwise emit an identity-only archive fact.
        // Orphan revisions without a head mean pull is incomplete, so leave
        // the concrete queue intact and retry after its closure arrives.
        for (document, route) in &documents {
            let head: Option<String> = sqlx::query_scalar(
                "SELECT revision_id FROM authored_document_heads WHERE document_id = ?",
            )
            .bind(document.id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(storage("inspect legacy archive head"))?;
            let revision_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM authored_revisions WHERE document_id = ?")
                    .bind(document.id.as_str())
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(storage("inspect legacy archive history"))?;
            if head.is_none() && revision_count != 0 {
                return Err(AuthoredDocumentsError::Storage(format!(
                    "legacy terminal route {} has revisions but no head; retry after pull completes",
                    document.id
                )));
            }

            let existing_archived_at: Option<String> = sqlx::query_scalar(
                "SELECT archived_at FROM authored_documents WHERE document_id = ?",
            )
            .bind(document.id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage("inspect legacy archive state"))?;
            let archived_at = if let Some(archived_at) = existing_archived_at {
                archived_at
            } else if let Some(head_value) = head.as_deref() {
                let head_id = super::RevisionId::parse(head_value.to_owned())?;
                self.store
                    .archive_document(&mut transaction, &document.id, &head_id, &route.archived_at)
                    .await?;
                route.archived_at.clone()
            } else {
                let updated = sqlx::query(
                    "UPDATE authored_documents SET archived_at = ?
                     WHERE document_id = ? AND archived_at IS NULL",
                )
                .bind(&route.archived_at)
                .bind(document.id.as_str())
                .execute(&mut *transaction)
                .await
                .map_err(storage("mark legacy identity-only document archived"))?;
                if updated.rows_affected() != 1 {
                    return Err(AuthoredDocumentsError::Storage(format!(
                        "legacy terminal route {} changed during bootstrap",
                        document.id
                    )));
                }
                route.archived_at.clone()
            };

            let catalog_kind = match document.kind {
                AuthoredDocumentKind::TrackScore => "legacy_track_score",
                AuthoredDocumentKind::PatternGraph => "legacy_pattern_graph",
            };
            let (operation_id, archive_id) = archive_request_ids(
                &document.principal_key,
                catalog_kind,
                &route.legacy_repository_id,
                document.id.as_str(),
                &device_id,
            );
            sqlx::query(
                "INSERT INTO authored_document_archives
                 (archive_id, principal_key, document_id, device_id,
                  operation_id, requested_revision_id, archived_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(archive_id) DO NOTHING",
            )
            .bind(&archive_id)
            .bind(&document.principal_key)
            .bind(document.id.as_str())
            .bind(&device_id)
            .bind(&operation_id)
            .bind(head.as_deref())
            .bind(&archived_at)
            .execute(&mut *transaction)
            .await
            .map_err(storage("insert legacy headless archive"))?;

            if let Some(user_id) = principal {
                let input = ArchiveAuthoredDocumentInput {
                    archive_id,
                    document_id: document.id.to_string(),
                    device_id: device_id.clone(),
                    operation_id,
                    requested_revision_id: head,
                    archived_at,
                };
                pending::enqueue_authored_archive_on(&mut transaction, user_id, &input)
                    .await
                    .map_err(|error| {
                        AuthoredDocumentsError::Storage(format!(
                            "enqueue legacy authored archive: {error}"
                        ))
                    })?;
            }
            sqlx::query(
                "DELETE FROM relational_upgrade_archived_routes
                 WHERE legacy_repository_id = ? AND principal_key = ?",
            )
            .bind(&route.legacy_repository_id)
            .bind(&document.principal_key)
            .execute(&mut *transaction)
            .await
            .map_err(storage("drain legacy terminal-route bootstrap"))?;
        }

        transaction
            .commit()
            .await
            .map_err(storage("commit legacy terminal-route bootstrap"))?;
        drop(guards);
        drop_empty_legacy_archive_queue(pool).await?;
        Ok(documents.len())
    }

    async fn bootstrap_scope(&self, pool: &SqlitePool, scope: &ResolvedScope) -> Result<bool> {
        let _guard = self.document_guard(&scope.document_id).await;
        self.bootstrap_scope_locked(pool, scope).await
    }

    async fn bootstrap_scope_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
    ) -> Result<bool> {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM authored_documents
             WHERE document_id = ? AND principal_key = ?",
        )
        .bind(scope.document_id.as_str())
        .bind(&scope.principal_key)
        .fetch_optional(pool)
        .await
        .map_err(storage("inspect authored projection bootstrap marker"))?;
        if exists.is_some() {
            return Ok(false);
        }
        self.load_current_locked(pool, scope).await?;
        Ok(true)
    }

    /// Recovery scan run after every pull. Any online device can finish the
    /// live-projection half of every server archive; no originating device or
    /// later generic catalog tombstone is required for liveness.
    pub(crate) async fn reconcile_remote_archives(
        &self,
        pool: &SqlitePool,
        principal: &str,
    ) -> Result<usize> {
        let expected_principal = principal_key(Some(principal));
        let document_ids: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT document.document_id
             FROM authored_documents document
             JOIN authored_document_archives archive
               ON archive.document_id = document.document_id
             WHERE document.principal_key = ?
               AND document.archived_at IS NOT NULL
               AND archive.server_archive_seq IS NOT NULL
             ORDER BY document.document_id",
        )
        .bind(expected_principal)
        .fetch_all(pool)
        .await
        .map_err(storage("scan terminal remote authored archives"))?;
        for document_id in &document_ids {
            self.finalize_remote_archive(pool, principal, document_id)
                .await?;
        }
        Ok(document_ids.len())
    }

    /// Materialize the terminal effect of a pulled server archive fact. The
    /// immutable archive row, not a best-effort later catalog tombstone, is
    /// the authority for removing the live score/graph projection. This is
    /// idempotent so every device can run it while pulling the same trace.
    pub(crate) async fn finalize_remote_archive(
        &self,
        pool: &SqlitePool,
        principal: &str,
        document_id: &str,
    ) -> Result<()> {
        let document_id =
            crate::services::authored_state::AuthoredDocumentId::parse(document_id.to_owned())?;
        let _guard = self.document_guard(&document_id).await;
        let expected_principal = principal_key(Some(principal));
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage("begin remote authored archive finalization"))?;
        write_admission::enter_remote_writes(&mut transaction)
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        let document = self.store.document(&mut transaction, &document_id).await?;
        if document.spec.principal_key != expected_principal {
            return Err(AuthoredDocumentsError::Scope(
                "remote archive does not belong to the admitted principal".into(),
            ));
        }
        if document.archived_at.is_none() {
            return Err(AuthoredDocumentsError::Storage(format!(
                "remote archive for {document_id} arrived before its terminal document fact"
            )));
        }
        let receipt: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM authored_document_archives
             WHERE document_id = ? AND principal_key = ?
               AND server_archive_seq IS NOT NULL",
        )
        .bind(document_id.as_str())
        .bind(&expected_principal)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage("verify remote authored archive receipt"))?;
        if receipt.is_none() {
            return Err(AuthoredDocumentsError::Storage(format!(
                "remote archive for {document_id} has no authoritative server receipt"
            )));
        }

        match document.spec.kind {
            crate::services::authored_state::AuthoredDocumentKind::TrackScore => {
                let score_id = document.spec.score_id.ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "archived score document has no score id".into(),
                    )
                })?;
                sqlx::query("DELETE FROM track_scores WHERE score_id = ?")
                    .bind(&score_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage("remove remotely archived score clips"))?;
                sqlx::query("DELETE FROM scores WHERE id = ?")
                    .bind(&score_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage("remove remotely archived score"))?;
            }
            crate::services::authored_state::AuthoredDocumentKind::PatternGraph => {
                let implementation_id = document.spec.implementation_id.ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "archived graph document has no implementation id".into(),
                    )
                })?;
                sqlx::query("DELETE FROM implementations WHERE id = ?")
                    .bind(&implementation_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage("remove remotely archived graph projection"))?;
                let pattern_id = document.spec.subject_id;
                let unfinished: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*)
                     FROM authored_documents candidate
                     WHERE candidate.document_kind = 'pattern_graph'
                       AND candidate.principal_key = ?
                       AND candidate.subject_id = ?
                       AND (
                           candidate.archived_at IS NULL
                           OR NOT EXISTS (
                               SELECT 1 FROM authored_document_archives archive
                               WHERE archive.document_id = candidate.document_id
                                 AND archive.server_archive_seq IS NOT NULL
                           )
                       )",
                )
                .bind(&expected_principal)
                .bind(&pattern_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(storage("inspect pattern archive completion"))?;
                if unfinished == 0 {
                    sqlx::query("DELETE FROM implementations WHERE pattern_id = ?")
                        .bind(&pattern_id)
                        .execute(&mut *transaction)
                        .await
                        .map_err(storage("remove terminal pattern implementations"))?;
                    sqlx::query("DELETE FROM patterns WHERE id = ?")
                        .bind(&pattern_id)
                        .execute(&mut *transaction)
                        .await
                        .map_err(storage("remove terminal pattern"))?;
                }
            }
        }
        write_admission::leave_remote_writes(&mut transaction)
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(storage("commit remote authored archive finalization"))?;
        Ok(())
    }

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
        let fingerprint = operation_request_fingerprint(
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
        let _guard = self.document_guard(&scope.document_id).await;
        if let Some(pattern) = optional_pattern_summary(pool, &pattern_id).await? {
            self.verify_creation_replay(pool, &scope, "create_pattern", &request_id, &fingerprint)
                .await?;
            return Ok(pattern);
        }

        let graph = Graph {
            nodes: Vec::new(),
            edges: Vec::new(),
            args: Vec::new(),
        };
        let files = graph_files(&graph)?;
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin pattern creation: {error}"))
        })?;
        require_admitted_principal(&mut transaction, principal).await?;
        sqlx::query("INSERT INTO patterns (id, uid, name, description) VALUES (?, ?, ?, ?)")
            .bind(&pattern_id)
            .bind(principal)
            .bind(&name)
            .bind(&description)
            .execute(&mut *transaction)
            .await
            .map_err(storage("insert pattern"))?;
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
        .map_err(storage("insert default pattern implementation"))?;
        let revision = self
            .create_document_initial_revision(
                &mut transaction,
                &scope,
                &files,
                "create_pattern",
                &request_id,
                &fingerprint,
                "Create pattern graph",
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(storage("commit pattern creation"))?;
        let pattern = load_pattern_summary(pool, &pattern_id).await?;
        debug_assert!(!revision.id.as_str().is_empty());
        Ok(pattern)
    }

    pub async fn create_score(
        &self,
        pool: &SqlitePool,
        request_id: &str,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
    ) -> Result<Score> {
        self.create_score_inner(pool, request_id, track_id, venue_id, name, false)
            .await
    }

    /// Ensure the track has durable membership in the venue without changing
    /// [`Self::create_score`]'s intentional multi-score behavior. The lookup
    /// and optional creation share one `BEGIN IMMEDIATE` venue transaction, so
    /// repeated Add actions with different request ids cannot race into two
    /// memberships.
    pub async fn ensure_venue_score(
        &self,
        pool: &SqlitePool,
        request_id: &str,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
    ) -> Result<Score> {
        self.create_score_inner(pool, request_id, track_id, venue_id, name, true)
            .await
    }

    async fn create_score_inner(
        &self,
        pool: &SqlitePool,
        request_id: &str,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
        reuse_existing_membership: bool,
    ) -> Result<Score> {
        let mut access = VenueAccess::<Write>::write(pool, VenueResource::Venue(venue_id))
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let owner = access.principal().map(str::to_owned);
        let request_id = normalized_creation_request_id(request_id)?;
        let principal_key = principal_key(owner.as_deref());
        let fingerprint = operation_request_fingerprint(
            "create_score",
            &[
                track_id,
                venue_id,
                if name.is_some() { "some" } else { "none" },
                name.unwrap_or(""),
            ],
        );
        let score_id = deterministic_creation_id(&principal_key, "score", &request_id, "subject");
        if score_exists(access.connection(), &score_id).await? {
            drop(access);
            let scope = ResolvedScope::track(
                owner.as_deref(),
                TrackScope {
                    score_id: score_id.clone(),
                    track_id: track_id.to_owned(),
                    venue_id: venue_id.to_owned(),
                },
            )?;
            self.verify_creation_replay(pool, &scope, "create_score", &request_id, &fingerprint)
                .await?;
            return load_score(pool, &score_id).await;
        }
        if reuse_existing_membership {
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT s.id
                 FROM scores s
                 JOIN auth_venue_access allowed ON allowed.venue_id = s.venue_id
                 WHERE s.track_id = ? AND s.venue_id = ?
                 ORDER BY s.created_at, s.id
                 LIMIT 1",
            )
            .bind(track_id)
            .bind(venue_id)
            .fetch_optional(access.connection())
            .await
            .map_err(storage("find existing venue membership"))?;
            if let Some(existing) = existing {
                drop(access);
                return load_score(pool, &existing).await;
            }
        }
        let track_visible: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM auth_visible_tracks WHERE track_id = ?")
                .bind(track_id)
                .fetch_optional(access.connection())
                .await
                .map_err(storage("authorize score track"))?;
        if track_visible.is_none() {
            return Err(AuthoredDocumentsError::Scope("track does not exist".into()));
        }
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&score_id)
        .bind(owner.as_deref())
        .bind(track_id)
        .bind(venue_id)
        .bind(name)
        .execute(access.connection())
        .await
        .map_err(storage("insert score"))?;
        let scope = ResolvedScope::track(
            owner.as_deref(),
            TrackScope {
                score_id: score_id.clone(),
                track_id: track_id.to_owned(),
                venue_id: venue_id.to_owned(),
            },
        )?;
        let _guard = self.document_guard(&scope.document_id).await;
        let empty = TrackDocument {
            revision: revision_for_clips(&[]),
            clips: Vec::new(),
        };
        let source = serialize_track(&empty, &PatternNames::new(), None)?;
        self.create_document_initial_revision(
            access.connection(),
            &scope,
            &FileMap::from([(SCORE_PATH.to_owned(), source.into_bytes())]),
            "create_score",
            &request_id,
            &fingerprint,
            "Create track score",
        )
        .await?;
        access
            .commit()
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        load_score(pool, &score_id).await
    }

    pub(super) async fn create_pattern_fork(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: ForkPatternInput,
        source: PatternForkSource,
    ) -> Result<ForkPatternResult> {
        let principal_key = principal_key(principal);
        let pattern_id =
            deterministic_creation_id(&principal_key, "pattern_fork", &input.request_id, "subject");
        let implementation_id = deterministic_creation_id(
            &principal_key,
            "pattern_fork",
            &input.request_id,
            "implementation",
        );
        let fingerprint = operation_request_fingerprint(
            "pattern_fork",
            &[&input.source_pattern_id, &input.source_implementation_id],
        );
        let scope = ResolvedScope::pattern(principal, &pattern_id, &implementation_id)?;
        let _guard = self.document_guard(&scope.document_id).await;
        if let Some(pattern) = optional_pattern_summary(pool, &pattern_id).await? {
            let outcome = self
                .verify_creation_replay(
                    pool,
                    &scope,
                    "pattern_fork",
                    &input.request_id,
                    &fingerprint,
                )
                .await?;
            return Ok(ForkPatternResult {
                pattern,
                implementation_id,
                document_id: scope.document_id.to_string(),
                revision_id: outcome.to_string(),
                applied_to_current_projection: true,
            });
        }
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin pattern fork: {error}"))
        })?;
        require_admitted_principal(&mut transaction, principal).await?;
        sqlx::query(
            "INSERT INTO patterns (id, uid, name, description, forked_from_id)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&pattern_id)
        .bind(principal)
        .bind(format!("{}_fork", source.pattern.name))
        .bind(&source.pattern.description)
        .bind(&input.source_pattern_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage("insert forked pattern"))?;
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
             VALUES (?, ?, ?, NULL, ?)",
        )
        .bind(&implementation_id)
        .bind(principal)
        .bind(&pattern_id)
        .bind(exact_graph_json(&source.graph.graph)?)
        .execute(&mut *transaction)
        .await
        .map_err(storage("insert forked pattern implementation"))?;
        let files = graph_files(&source.graph.graph)?;
        let revision = self
            .create_document_initial_revision(
                &mut transaction,
                &scope,
                &files,
                "pattern_fork",
                &input.request_id,
                &fingerprint,
                "Fork pattern graph",
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(storage("commit pattern fork"))?;
        Ok(ForkPatternResult {
            pattern: load_pattern_summary(pool, &pattern_id).await?,
            implementation_id,
            document_id: scope.document_id.to_string(),
            revision_id: revision.id.to_string(),
            applied_to_current_projection: true,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_document_initial_revision(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        files: &FileMap,
        operation_kind: &'static str,
        operation_id: &str,
        fingerprint: &str,
        subject: &str,
    ) -> Result<super::RevisionInfo> {
        self.store
            .insert_document(connection, &scope.specification()?)
            .await?;
        let metadata = revision_metadata(
            operation_kind,
            Some(operation_id),
            subject,
            None,
            None,
            None,
        )?;
        let revision = self
            .store
            .insert_revision(connection, &scope.document_id, &[], files, &metadata)
            .await?;
        self.store
            .create_head(connection, &scope.document_id, &revision.id)
            .await?;
        sqlx::query(
            "INSERT INTO authored_operation_outcomes
             (principal_key, document_id, operation_kind, operation_id,
              request_fingerprint, status, result_revision_id)
             VALUES (?, ?, ?, ?, ?, 'committed', ?)",
        )
        .bind(&scope.principal_key)
        .bind(scope.document_id.as_str())
        .bind(operation_kind)
        .bind(operation_id)
        .bind(fingerprint)
        .bind(revision.id.as_str())
        .execute(&mut *connection)
        .await
        .map_err(storage("record authored creation outcome"))?;
        self.enqueue_revision_closure(
            connection,
            scope,
            &revision,
            files,
            true,
            Some((operation_kind, operation_id)),
        )
        .await?;
        self.create_head_proposal(connection, scope, None, &revision.id, operation_id)
            .await?;
        Ok(revision)
    }

    async fn verify_creation_replay(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        operation_kind: &str,
        operation_id: &str,
        fingerprint: &str,
    ) -> Result<super::RevisionId> {
        let outcome = self
            .operation_outcome(pool, scope, operation_kind, operation_id)
            .await?
            .ok_or_else(|| {
                AuthoredDocumentsError::Storage(
                    "created catalog row is missing its authored operation outcome".into(),
                )
            })?;
        if outcome.request_fingerprint != fingerprint || outcome.status != "committed" {
            return Err(AuthoredDocumentsError::Invalid(
                "creation request id was already used with different input".into(),
            ));
        }
        outcome.result_revision_id.ok_or_else(|| {
            AuthoredDocumentsError::Storage(
                "committed creation outcome has no result revision".into(),
            )
        })
    }

    pub async fn archive_score(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        score_id: &str,
    ) -> Result<()> {
        let Some(scope) = optional_track_scope_from_catalog(pool, score_id).await? else {
            if completed_score_archive_replay(pool, principal, score_id).await? {
                return Ok(());
            }
            return Err(AuthoredDocumentsError::Scope(format!(
                "score {score_id} does not exist"
            )));
        };
        if scope.owner.as_deref() != principal {
            return Err(AuthoredDocumentsError::Scope("score does not exist".into()));
        }
        let resolved = ResolvedScope::track(principal, scope.track_scope)?;
        let _guard = self.document_guard(&resolved.document_id).await;
        let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(score_id))
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let current = self
            .ensure_current_on_connection(access.connection(), &resolved)
            .await?;
        self.archive_document_on_connection(
            access.connection(),
            &resolved,
            &current.head,
            "score",
            score_id,
        )
        .await?;
        access
            .enter_maintenance()
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        sqlx::query("DELETE FROM track_scores WHERE score_id = ?")
            .bind(score_id)
            .execute(access.connection())
            .await
            .map_err(storage("delete archived score clips"))?;
        sqlx::query("DELETE FROM scores WHERE id = ?")
            .bind(score_id)
            .execute(access.connection())
            .await
            .map_err(storage("delete archived score"))?;
        access
            .leave_maintenance()
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        access
            .commit()
            .await
            .map_err(AuthoredDocumentsError::Storage)
    }

    pub(crate) async fn archive_score_from_remote(
        &self,
        pool: &SqlitePool,
        principal: &str,
        score_id: &str,
    ) -> Result<bool> {
        let Some(scope) = optional_track_scope_from_catalog(pool, score_id).await? else {
            return Ok(false);
        };
        if scope.owner.as_deref() != Some(principal) {
            return Err(AuthoredDocumentsError::Scope("score does not exist".into()));
        }
        self.delete_remotely_archived_score(pool, scope).await?;
        Ok(true)
    }

    async fn delete_remotely_archived_score(
        &self,
        pool: &SqlitePool,
        scope: TrackCatalogScope,
    ) -> Result<()> {
        let resolved = ResolvedScope::track(scope.owner.as_deref(), scope.track_scope)?;
        let _guard = self.document_guard(&resolved.document_id).await;
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage("begin remote score archive"))?;
        write_admission::enter_remote_writes(&mut transaction)
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        require_terminal_remote_archive(&mut transaction, &resolved).await?;
        sqlx::query("DELETE FROM track_scores WHERE score_id = ?")
            .bind(resolved.track_scope().expect("score").score_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(storage("delete remotely archived score clips"))?;
        sqlx::query("DELETE FROM scores WHERE id = ?")
            .bind(resolved.track_scope().expect("score").score_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(storage("delete remotely archived score"))?;
        write_admission::leave_remote_writes(&mut transaction)
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(storage("commit remote score archive"))
    }

    pub async fn archive_pattern(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        pattern_id: &str,
    ) -> Result<()> {
        self.archive_pattern_inner(pool, principal, pattern_id, true)
            .await?;
        Ok(())
    }

    pub(crate) async fn archive_pattern_from_remote(
        &self,
        pool: &SqlitePool,
        principal: &str,
        pattern_id: &str,
    ) -> Result<bool> {
        self.archive_pattern_inner(pool, Some(principal), pattern_id, false)
            .await
    }

    async fn archive_pattern_inner(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        pattern_id: &str,
        publish: bool,
    ) -> Result<bool> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT implementation.id, pattern.uid
             FROM patterns pattern
             JOIN implementations implementation ON implementation.pattern_id = pattern.id
             WHERE pattern.id = ? ORDER BY implementation.id",
        )
        .bind(pattern_id)
        .fetch_all(pool)
        .await
        .map_err(storage("list pattern implementations for archive"))?;
        if rows.is_empty() {
            if publish {
                return completed_pattern_archive_replay(pool, principal, pattern_id).await;
            }
            return Ok(false);
        }
        if rows.iter().any(|(_, owner)| owner.as_deref() != principal) {
            return Err(AuthoredDocumentsError::Scope(
                "pattern does not exist".into(),
            ));
        }
        let scopes = rows
            .iter()
            .map(|(implementation_id, _)| {
                ResolvedScope::pattern(principal, pattern_id, implementation_id)
            })
            .collect::<Result<Vec<_>>>()?;
        let _lifecycle = self.lifecycle_lock.clone().write_owned().await;
        let mut guards = Vec::with_capacity(scopes.len());
        for scope in &scopes {
            guards.push(
                self.document_guard_inside_lifecycle(&scope.document_id)
                    .await,
            );
        }
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage("begin pattern archive"))?;
        if publish {
            require_admitted_principal(&mut transaction, principal).await?;
        } else {
            write_admission::enter_remote_writes(&mut transaction)
                .await
                .map_err(AuthoredDocumentsError::Storage)?;
        }
        if publish {
            // Establish every sibling identity while the pattern route is
            // still live. Archiving the first implementation makes the route
            // terminal only when no other live authored sibling exists, so a
            // sequential create-then-archive loop would reject the second
            // implementation as an attempted resurrection.
            let mut heads = Vec::with_capacity(scopes.len());
            for scope in &scopes {
                let current = self
                    .ensure_current_on_connection(&mut transaction, scope)
                    .await?;
                heads.push(current.head);
            }
            for (scope, head) in scopes.iter().zip(&heads) {
                self.archive_document_on_connection(
                    &mut transaction,
                    scope,
                    head,
                    "pattern",
                    pattern_id,
                )
                .await?;
            }
        } else {
            for scope in &scopes {
                require_terminal_remote_archive(&mut transaction, scope).await?;
            }
        }
        sqlx::query("DELETE FROM implementations WHERE pattern_id = ?")
            .bind(pattern_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage("delete archived pattern implementations"))?;
        sqlx::query("DELETE FROM patterns WHERE id = ?")
            .bind(pattern_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage("delete archived pattern"))?;
        if !publish {
            write_admission::leave_remote_writes(&mut transaction)
                .await
                .map_err(AuthoredDocumentsError::Storage)?;
        }
        transaction
            .commit()
            .await
            .map_err(storage("commit pattern archive"))?;
        drop(guards);
        Ok(true)
    }

    async fn archive_document_on_connection(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        head: &super::RevisionId,
        catalog_kind: &str,
        catalog_id: &str,
    ) -> Result<()> {
        let archived_at = Utc::now().to_rfc3339();
        let device_id: String = sqlx::query_scalar(
            "SELECT device_id FROM authored_device_identity WHERE singleton = 1",
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(storage("load authored device identity"))?;
        let (operation_id, archive_id) = archive_request_ids(
            &scope.principal_key,
            catalog_kind,
            catalog_id,
            scope.document_id.as_str(),
            &device_id,
        );
        self.store
            .archive_document(connection, &scope.document_id, head, &archived_at)
            .await?;
        sqlx::query(
            "INSERT INTO authored_document_archives
             (archive_id, principal_key, document_id, device_id, operation_id,
              requested_revision_id, archived_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&archive_id)
        .bind(&scope.principal_key)
        .bind(scope.document_id.as_str())
        .bind(&device_id)
        .bind(&operation_id)
        .bind(head.as_str())
        .bind(&archived_at)
        .execute(&mut *connection)
        .await
        .map_err(storage("insert authored archive request"))?;
        if scope.owner_user_id.is_none() {
            return Ok(());
        }
        let input = ArchiveAuthoredDocumentInput {
            archive_id,
            document_id: scope.document_id.to_string(),
            device_id,
            operation_id,
            requested_revision_id: Some(head.to_string()),
            archived_at,
        };
        pending::enqueue_authored_archive_on(
            connection,
            scope.owner_user_id.as_deref().expect("checked"),
            &input,
        )
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("enqueue authored archive: {error}"))
        })
    }
}

async fn completed_score_archive_replay(
    pool: &SqlitePool,
    principal: Option<&str>,
    score_id: &str,
) -> Result<bool> {
    completed_catalog_archive_replay(pool, principal, "score", score_id, true).await
}

async fn completed_pattern_archive_replay(
    pool: &SqlitePool,
    principal: Option<&str>,
    pattern_id: &str,
) -> Result<bool> {
    completed_catalog_archive_replay(pool, principal, "pattern", pattern_id, false).await
}

/// Resolve a completed one-way deletion from permanent authored history after
/// its live catalog route has disappeared. Every document in a pattern route
/// must carry this device's exact immutable receipt; a partial archive is not a
/// successful replay.
async fn completed_catalog_archive_replay(
    pool: &SqlitePool,
    principal: Option<&str>,
    catalog_kind: &str,
    catalog_id: &str,
    require_single_document: bool,
) -> Result<bool> {
    let principal_key = principal_key(principal);
    let mut transaction = pool
        .begin()
        .await
        .map_err(storage("begin authored archive replay"))?;
    require_admitted_principal(&mut transaction, principal).await?;
    let device_id: String =
        sqlx::query_scalar("SELECT device_id FROM authored_device_identity WHERE singleton = 1")
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage("load authored archive replay device"))?;
    let (route_column, document_kind) = match catalog_kind {
        "score" => ("score_id", "track_score"),
        "pattern" => ("subject_id", "pattern_graph"),
        _ => {
            return Err(AuthoredDocumentsError::Storage(
                "authored archive replay has an unknown catalog kind".into(),
            ));
        }
    };
    let query = format!(
        "SELECT document_id FROM authored_documents
         WHERE principal_key = ? AND document_kind = ? AND {route_column} = ?
         ORDER BY document_id"
    );
    let document_ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .bind(&principal_key)
        .bind(document_kind)
        .bind(catalog_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage("resolve authored archive replay route"))?;
    if document_ids.is_empty() {
        transaction
            .commit()
            .await
            .map_err(storage("finish unknown authored archive replay"))?;
        return Ok(false);
    }
    if require_single_document && document_ids.len() != 1 {
        return Err(AuthoredDocumentsError::Storage(format!(
            "score {catalog_id} resolves to multiple authored documents"
        )));
    }

    for document_id in &document_ids {
        let (operation_id, archive_id) = archive_request_ids(
            &principal_key,
            catalog_kind,
            catalog_id,
            document_id,
            &device_id,
        );
        let complete: Option<i64> = sqlx::query_scalar(
            "SELECT 1
             FROM authored_documents document
             JOIN authored_document_archives archive
               ON archive.document_id = document.document_id
              AND archive.principal_key = document.principal_key
             WHERE document.document_id = ? AND document.principal_key = ?
               AND document.archived_at IS NOT NULL
               AND archive.archive_id = ? AND archive.device_id = ?
               AND archive.operation_id = ?",
        )
        .bind(document_id)
        .bind(&principal_key)
        .bind(&archive_id)
        .bind(&device_id)
        .bind(&operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage("verify authored archive replay receipt"))?;
        if complete.is_none() {
            transaction
                .commit()
                .await
                .map_err(storage("finish incomplete authored archive replay"))?;
            return Ok(false);
        }
    }
    transaction
        .commit()
        .await
        .map_err(storage("finish authored archive replay"))?;
    Ok(true)
}

#[derive(sqlx::FromRow)]
struct LegacyArchivedRoute {
    legacy_repository_id: String,
    document_kind: String,
    principal_key: String,
    subject_id: String,
    track_id: Option<String>,
    venue_id: Option<String>,
    score_id: Option<String>,
    implementation_id: Option<String>,
    created_at: String,
    archived_at: String,
}

impl LegacyArchivedRoute {
    fn document(&self) -> Result<NewAuthoredDocument> {
        if self.legacy_repository_id.is_empty() {
            return Err(AuthoredDocumentsError::Storage(
                "legacy terminal route has an empty repository identity".into(),
            ));
        }
        for (value, field) in [
            (&self.created_at, "created_at"),
            (&self.archived_at, "archived_at"),
        ] {
            chrono::DateTime::parse_from_rfc3339(value).map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "legacy terminal route has invalid {field}: {error}"
                ))
            })?;
        }
        match self.document_kind.as_str() {
            "track_score" => {
                let track_id = required_legacy_route_part(&self.track_id, "track id")?;
                let venue_id = required_legacy_route_part(&self.venue_id, "venue id")?;
                let score_id = required_legacy_route_part(&self.score_id, "score id")?;
                if self.subject_id != track_id || self.implementation_id.is_some() {
                    return Err(AuthoredDocumentsError::Storage(
                        "legacy track-score terminal route is inconsistent".into(),
                    ));
                }
                NewAuthoredDocument::track_score(&self.principal_key, track_id, venue_id, score_id)
                    .map_err(Into::into)
            }
            "pattern_graph" => {
                let implementation_id =
                    required_legacy_route_part(&self.implementation_id, "implementation id")?;
                if self.track_id.is_some() || self.venue_id.is_some() || self.score_id.is_some() {
                    return Err(AuthoredDocumentsError::Storage(
                        "legacy pattern-graph terminal route is inconsistent".into(),
                    ));
                }
                NewAuthoredDocument::pattern_graph(
                    &self.principal_key,
                    &self.subject_id,
                    implementation_id,
                )
                .map_err(Into::into)
            }
            other => Err(AuthoredDocumentsError::Storage(format!(
                "legacy terminal route has unknown kind {other:?}"
            ))),
        }
    }
}

fn required_legacy_route_part<'a>(value: &'a Option<String>, field: &str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AuthoredDocumentsError::Storage(format!("legacy terminal route has no {field}"))
        })
}

async fn legacy_archived_routes(
    pool: &SqlitePool,
    principal: Option<&str>,
) -> Result<Vec<LegacyArchivedRoute>> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'relational_upgrade_archived_routes'",
    )
    .fetch_one(pool)
    .await
    .map_err(storage("inspect legacy terminal-route queue"))?;
    if exists == 0 {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "SELECT legacy_repository_id, document_kind, principal_key, subject_id,
                track_id, venue_id, score_id, implementation_id, created_at, archived_at
         FROM relational_upgrade_archived_routes
         WHERE principal_key = ?
         ORDER BY document_kind, subject_id, implementation_id, legacy_repository_id",
    )
    .bind(principal_key(principal))
    .fetch_all(pool)
    .await
    .map_err(storage("load legacy terminal-route queue"))
}

async fn drop_empty_legacy_archive_queue(pool: &SqlitePool) -> Result<()> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'relational_upgrade_archived_routes'",
    )
    .fetch_one(pool)
    .await
    .map_err(storage("inspect legacy terminal-route queue cleanup"))?;
    if exists == 0 {
        return Ok(());
    }
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM relational_upgrade_archived_routes")
            .fetch_one(pool)
            .await
            .map_err(storage("count legacy terminal-route queue"))?;
    if remaining == 0 {
        sqlx::query("DROP TABLE relational_upgrade_archived_routes")
            .execute(pool)
            .await
            .map_err(storage("remove drained legacy terminal-route queue"))?;
    }
    Ok(())
}

pub(super) fn archive_request_ids(
    principal_key: &str,
    catalog_kind: &str,
    catalog_id: &str,
    document_id: &str,
    device_id: &str,
) -> (String, String) {
    let request = operation_request_fingerprint(
        "archive_document",
        &[
            principal_key,
            catalog_kind,
            catalog_id,
            document_id,
            device_id,
        ],
    );
    (
        deterministic_creation_id(principal_key, "authored_archive", &request, "operation"),
        deterministic_creation_id(principal_key, "authored_archive", &request, "receipt"),
    )
}

/// A catalog tombstone is only the cleanup half of a remote authored archive.
/// Never manufacture an archive locally in response to a generic row delete:
/// the server-authored terminal document timestamp and archive receipt must
/// both have arrived first. Returning an error deliberately leaves the pull
/// cursor behind the tombstone so a later cycle retries after those parent
/// facts have been materialized.
async fn require_terminal_remote_archive(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
) -> Result<()> {
    let terminal: Option<i64> = sqlx::query_scalar(
        "SELECT 1
         FROM authored_documents document
         JOIN authored_document_archives archive
           ON archive.document_id = document.document_id
         WHERE document.document_id = ?
           AND document.principal_key = ?
           AND document.archived_at IS NOT NULL
           AND archive.server_archive_seq IS NOT NULL",
    )
    .bind(scope.document_id.as_str())
    .bind(&scope.principal_key)
    .fetch_optional(&mut *connection)
    .await
    .map_err(storage("verify terminal remote authored archive"))?;
    if terminal.is_some() {
        Ok(())
    } else {
        Err(AuthoredDocumentsError::Storage(format!(
            "remote catalog deletion for {} is waiting for its terminal authored archive",
            scope.document_id
        )))
    }
}

#[derive(Clone)]
pub(super) struct TrackCatalogScope {
    pub owner: Option<String>,
    pub track_scope: TrackScope,
}

pub(super) async fn optional_track_scope_from_catalog(
    pool: &SqlitePool,
    score_id: &str,
) -> Result<Option<TrackCatalogScope>> {
    let row: Option<(Option<String>, String, Option<String>)> =
        sqlx::query_as("SELECT uid, track_id, venue_id FROM scores WHERE id = ?")
            .bind(score_id)
            .fetch_optional(pool)
            .await
            .map_err(storage("resolve score catalog scope"))?;
    row.map(|(owner, track_id, venue_id)| {
        Ok(TrackCatalogScope {
            owner,
            track_scope: TrackScope {
                score_id: score_id.to_owned(),
                track_id,
                venue_id: venue_id
                    .ok_or_else(|| AuthoredDocumentsError::Storage("score has no venue".into()))?,
            },
        })
    })
    .transpose()
}

async fn require_admitted_principal(
    connection: &mut SqliteConnection,
    principal: Option<&str>,
) -> Result<()> {
    let admitted: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND remote_writes = 0 AND active_uid IS ?",
    )
    .bind(principal)
    .fetch_optional(&mut *connection)
    .await
    .map_err(storage("authorize authored catalog mutation"))?;
    if admitted.is_some() {
        Ok(())
    } else {
        Err(AuthoredDocumentsError::Scope(
            "catalog mutation principal is not admitted".into(),
        ))
    }
}

async fn score_exists(connection: &mut SqliteConnection, score_id: &str) -> Result<bool> {
    sqlx::query_scalar::<_, i64>("SELECT 1 FROM scores WHERE id = ?")
        .bind(score_id)
        .fetch_optional(&mut *connection)
        .await
        .map(|row| row.is_some())
        .map_err(storage("inspect score creation replay"))
}

async fn load_score(pool: &SqlitePool, score_id: &str) -> Result<Score> {
    sqlx::query_as(
        "SELECT id, uid, track_id, venue_id, name, created_at, updated_at
         FROM scores WHERE id = ?",
    )
    .bind(score_id)
    .fetch_optional(pool)
    .await
    .map_err(storage("load created score"))?
    .ok_or_else(|| AuthoredDocumentsError::Storage("created score is missing".into()))
}

async fn optional_pattern_summary(
    pool: &SqlitePool,
    pattern_id: &str,
) -> Result<Option<PatternSummary>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {PATTERN_SUMMARY_COLUMNS} FROM patterns WHERE id = ?"
    )))
    .bind(pattern_id)
    .fetch_optional(pool)
    .await
    .map_err(storage("load pattern creation replay"))
}

async fn load_pattern_summary(pool: &SqlitePool, pattern_id: &str) -> Result<PatternSummary> {
    optional_pattern_summary(pool, pattern_id)
        .await?
        .ok_or_else(|| AuthoredDocumentsError::Storage("created pattern is missing".into()))
}

fn storage(context: &'static str) -> impl Fn(sqlx::Error) -> AuthoredDocumentsError {
    move |error| AuthoredDocumentsError::Storage(format!("{context}: {error}"))
}
