use async_trait::async_trait;
use chrono::DateTime;
use serde_json::Value;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

use super::operations::local_row_payload;
use super::{
    apply_graph_edit_in_transaction, apply_track_projection_in_transaction, canonicalize_graph,
    exact_graph_json, graph_files, operation_request_fingerprint, principal_key, Actor,
    AuthoredDocument, AuthoredDocuments, AuthoredDocumentsError, AuthoredSnapshot, DocumentScope,
    GraphDocument, GraphEditPlan, ResolvedScope, Result, RevisionId, RevisionMetadata,
    TrackEditPlan, TrackProjectionAuthority,
};
use crate::database::local::write_admission;
use crate::services::authored_state::{AuthoredDocumentId, AuthoredStateError};
use crate::services::authored_sync_merge::{
    merge_graph_total, merge_track_total, SyncMergeResolution,
};
use crate::sync::authored_remote::{
    self, HeadIntegrationReceipt, HeadIntegrationResolution, HeadProposalIntegrator,
    IntegrateHeadProposalInput,
};
use crate::sync::error::SyncError;
use crate::sync::registry;
use crate::sync::traits::RemoteClient;

#[derive(FromRow)]
struct ProposalRow {
    proposal_id: String,
    principal_key: String,
    document_id: String,
    base_revision_id: Option<String>,
    proposed_revision_id: String,
    server_proposal_seq: Option<i64>,
    created_at: String,
}

struct ResolvedSyncSnapshot {
    snapshot: AuthoredSnapshot,
    resolution: SyncMergeResolution,
}

impl AuthoredDocuments {
    /// Apply the exact server-authoritative head and projection. Optimistic
    /// proposal tips remain immutable, visible in history, and restorable, but
    /// never replace the server head as a second current-state authority.
    pub(crate) async fn apply_server_head(
        &self,
        pool: &SqlitePool,
        admitted_user_id: &str,
        document_id: &str,
        server_revision_id: &str,
        server_generation: i64,
        server_updated_at: &str,
    ) -> Result<()> {
        if server_generation < 0 {
            return Err(AuthoredDocumentsError::Storage(
                "server authored head has a negative generation".into(),
            ));
        }
        DateTime::parse_from_rfc3339(server_updated_at).map_err(|_| {
            AuthoredDocumentsError::Storage(
                "server authored head has an invalid updated_at timestamp".into(),
            )
        })?;
        self.apply_server_head_observation(
            pool,
            admitted_user_id,
            document_id,
            server_revision_id,
            Some((server_generation, server_updated_at)),
        )
        .await
    }

    /// Re-apply a server head after observing a terminal integration row. The
    /// RPC receipt does not carry the projection clock, so this makes the
    /// result visible immediately with one local CAS; the ordered head-table
    /// pull subsequently installs the exact server generation and timestamp.
    pub(crate) async fn apply_integrated_server_head(
        &self,
        pool: &SqlitePool,
        admitted_user_id: &str,
        document_id: &str,
        server_revision_id: &str,
    ) -> Result<()> {
        self.apply_server_head_observation(
            pool,
            admitted_user_id,
            document_id,
            server_revision_id,
            None,
        )
        .await
    }

    async fn apply_server_head_observation(
        &self,
        pool: &SqlitePool,
        admitted_user_id: &str,
        document_id: &str,
        server_revision_id: &str,
        server_clock: Option<(i64, &str)>,
    ) -> Result<()> {
        let document_id = AuthoredDocumentId::parse(document_id.to_owned())?;
        let server_revision_id = RevisionId::parse(server_revision_id.to_owned())?;
        let _guard = self.document_guard(&document_id).await;

        let (scope, server_snapshot, local_head) = {
            let mut connection = pool.acquire().await.map_err(storage("open server head"))?;
            let scope = self
                .scope_for_document(&mut connection, admitted_user_id, &document_id)
                .await?;
            let record = self.store.document(&mut connection, &document_id).await?;
            if record.archived_at.is_some() {
                return Ok(());
            }
            let server_snapshot = self
                .snapshot_from_revision(&mut connection, &scope, &server_revision_id)
                .await?;
            let local_head: Option<String> = sqlx::query_scalar(
                "SELECT revision_id FROM authored_document_heads WHERE document_id = ?",
            )
            .bind(document_id.as_str())
            .fetch_optional(&mut *connection)
            .await
            .map_err(storage("load optimistic authored head"))?;
            (
                scope,
                server_snapshot,
                local_head.map(RevisionId::parse).transpose()?,
            )
        };

        let mut write = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage("begin server head application"))?;
        assert_sync_principal(&mut write, admitted_user_id).await?;
        let record = self.store.document(&mut write, &document_id).await?;
        if record.archived_at.is_some() {
            write
                .commit()
                .await
                .map_err(storage("finish archived server head observation"))?;
            return Ok(());
        }
        let actual_head: Option<String> = sqlx::query_scalar(
            "SELECT revision_id FROM authored_document_heads WHERE document_id = ?",
        )
        .bind(document_id.as_str())
        .fetch_optional(&mut *write)
        .await
        .map_err(storage("reload optimistic authored head"))?;
        let actual_head = actual_head.map(RevisionId::parse).transpose()?;
        if actual_head != local_head {
            return Err(AuthoredDocumentsError::State(
                AuthoredStateError::HeadConflict {
                    document_id: document_id.to_string(),
                    expected: local_head
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "<missing>".into()),
                    actual: actual_head
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "<missing>".into()),
                },
            ));
        }

        let Some(local_head) = actual_head else {
            self.materialize_initial_server_snapshot(&mut write, &scope, server_snapshot)
                .await?;
            match server_clock {
                Some((generation, updated_at)) => {
                    write_admission::enter_remote_writes(&mut write)
                        .await
                        .map_err(AuthoredDocumentsError::Storage)?;
                    self.store
                        .project_server_head(
                            &mut write,
                            &document_id,
                            None,
                            &server_revision_id,
                            generation,
                            updated_at,
                        )
                        .await?;
                    write_admission::leave_remote_writes(&mut write)
                        .await
                        .map_err(AuthoredDocumentsError::Storage)?;
                }
                None => {
                    self.store
                        .create_head(&mut write, &document_id, &server_revision_id)
                        .await?;
                }
            }
            write
                .commit()
                .await
                .map_err(storage("commit initial server head"))?;
            return Ok(());
        };
        if local_head == server_revision_id {
            if let Some((generation, updated_at)) = server_clock {
                write_admission::enter_remote_writes(&mut write)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                self.store
                    .project_server_head(
                        &mut write,
                        &document_id,
                        Some(&local_head),
                        &server_revision_id,
                        generation,
                        updated_at,
                    )
                    .await?;
                write_admission::leave_remote_writes(&mut write)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
            }
            write
                .commit()
                .await
                .map_err(storage("finish current server head observation"))?;
            return Ok(());
        }

        let local_snapshot = self
            .snapshot_from_revision(&mut write, &scope, &local_head)
            .await?;
        let live = self
            .snapshot_from_connection(&mut write, &scope, Some(&local_snapshot.files))
            .await?;
        if live.document.revision() != local_snapshot.document.revision() {
            return Err(AuthoredDocumentsError::Storage(format!(
                "live projection for {document_id} diverged from optimistic head {local_head}"
            )));
        }

        let expected_projection = local_snapshot.document.revision().to_owned();
        self.project_candidate_on_connection(
            &mut write,
            &scope,
            server_snapshot.document,
            &expected_projection,
            TrackProjectionAuthority::TrustedRevision,
        )
        .await?;
        match server_clock {
            Some((generation, updated_at)) => {
                write_admission::enter_remote_writes(&mut write)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                self.store
                    .project_server_head(
                        &mut write,
                        &document_id,
                        Some(&local_head),
                        &server_revision_id,
                        generation,
                        updated_at,
                    )
                    .await?;
                write_admission::leave_remote_writes(&mut write)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
            }
            None => {
                self.store
                    .compare_and_swap_integrated_head(
                        &mut write,
                        &document_id,
                        &local_head,
                        &server_revision_id,
                    )
                    .await?;
            }
        }
        write
            .commit()
            .await
            .map_err(storage("commit authoritative server head projection"))?;
        Ok(())
    }

    async fn scope_for_document(
        &self,
        connection: &mut SqliteConnection,
        admitted_user_id: &str,
        document_id: &AuthoredDocumentId,
    ) -> Result<ResolvedScope> {
        let record = self.store.document(connection, document_id).await?;
        let expected_principal = principal_key(Some(admitted_user_id));
        if record.spec.principal_key != expected_principal {
            return Err(AuthoredDocumentsError::Scope(
                "server authored document does not belong to the admitted principal".into(),
            ));
        }
        let scope = match record.spec.kind {
            crate::services::authored_state::AuthoredDocumentKind::TrackScore => {
                ResolvedScope::track(
                    Some(admitted_user_id),
                    crate::services::track_edits::TrackScope {
                        score_id: record.spec.score_id.ok_or_else(|| {
                            AuthoredDocumentsError::Storage(
                                "authored score routing has no score id".into(),
                            )
                        })?,
                        track_id: record.spec.track_id.ok_or_else(|| {
                            AuthoredDocumentsError::Storage(
                                "authored score routing has no track id".into(),
                            )
                        })?,
                        venue_id: record.spec.venue_id.ok_or_else(|| {
                            AuthoredDocumentsError::Storage(
                                "authored score routing has no venue id".into(),
                            )
                        })?,
                    },
                )?
            }
            crate::services::authored_state::AuthoredDocumentKind::PatternGraph => {
                ResolvedScope::pattern(
                    Some(admitted_user_id),
                    &record.spec.subject_id,
                    record.spec.implementation_id.as_deref().ok_or_else(|| {
                        AuthoredDocumentsError::Storage(
                            "authored graph routing has no implementation id".into(),
                        )
                    })?,
                )?
            }
        };
        if scope.document_id != *document_id {
            return Err(AuthoredDocumentsError::Storage(
                "authored document routing does not reproduce its immutable id".into(),
            ));
        }
        Ok(scope)
    }

    async fn materialize_initial_server_snapshot(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        snapshot: AuthoredSnapshot,
    ) -> Result<()> {
        match (&scope.document, snapshot.document) {
            (DocumentScope::Track(track_scope), AuthoredDocument::Track(track)) => {
                let current = super::projection::load_track_document_for_connection(
                    connection,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                )
                .await?;
                apply_track_projection_in_transaction(
                    connection,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    TrackEditPlan {
                        base_revision: current.revision,
                        candidate: track.clips,
                    },
                    TrackProjectionAuthority::TrustedRevision.identity(),
                )
                .await?;
            }
            (DocumentScope::Pattern(graph_scope), AuthoredDocument::Graph(graph)) => {
                let existing: Option<i64> =
                    sqlx::query_scalar("SELECT 1 FROM implementations WHERE id = ?")
                        .bind(&graph_scope.implementation_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(storage("inspect initial graph projection"))?;
                if existing.is_some() {
                    let current = super::projection::load_graph_document_for_connection(
                        connection,
                        graph_scope,
                    )
                    .await?;
                    apply_graph_edit_in_transaction(
                        connection,
                        graph_scope,
                        GraphEditPlan {
                            base_revision: current.revision,
                            candidate: graph.graph,
                        },
                    )
                    .await?;
                } else {
                    let canonical = canonicalize_graph(&graph.graph)?;
                    sqlx::query(
                        "INSERT INTO implementations
                         (id, uid, pattern_id, name, graph_json)
                         VALUES (?, ?, ?, ?, ?)",
                    )
                    .bind(&graph_scope.implementation_id)
                    .bind(scope.owner_user_id.as_deref())
                    .bind(&graph_scope.pattern_id)
                    .bind(None::<String>)
                    .bind(exact_graph_json(&canonical)?)
                    .execute(&mut *connection)
                    .await
                    .map_err(storage("materialize initial graph projection"))?;
                }
            }
            _ => {
                return Err(AuthoredDocumentsError::Storage(
                    "server revision kind does not match its document".into(),
                ));
            }
        }
        Ok(())
    }

    async fn total_sync_snapshot(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        base: Option<&AuthoredSnapshot>,
        current: &AuthoredSnapshot,
        proposal: &AuthoredSnapshot,
    ) -> Result<Option<ResolvedSyncSnapshot>> {
        let Some(base) = base else {
            return Ok(Some(ResolvedSyncSnapshot {
                snapshot: proposal.clone(),
                resolution: SyncMergeResolution::WholeProposalFallback,
            }));
        };
        match (&base.document, &current.document, &proposal.document) {
            (
                AuthoredDocument::Track(base_track),
                AuthoredDocument::Track(current_track),
                AuthoredDocument::Track(proposal_track),
            ) => {
                let merged = merge_track_total(base_track, current_track, proposal_track);
                let track_scope = scope.track_scope().ok_or_else(|| {
                    AuthoredDocumentsError::Storage("track merge has graph scope".into())
                })?;
                let candidate_valid =
                    crate::services::track_edits::check_track_projection_candidate(
                        pool,
                        track_scope,
                        scope.owner_user_id.as_deref(),
                        &merged.value.clips,
                        TrackProjectionAuthority::TrustedRevision.identity(),
                    )
                    .await
                    .is_ok();
                let (value, resolution) = if candidate_valid {
                    (merged.value, merged.resolution)
                } else if crate::services::track_edits::check_track_projection_candidate(
                    pool,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    &proposal_track.clips,
                    TrackProjectionAuthority::TrustedRevision.identity(),
                )
                .await
                .is_ok()
                {
                    (
                        proposal_track.clone(),
                        SyncMergeResolution::WholeProposalFallback,
                    )
                } else {
                    return Ok(None);
                };
                let files = if resolution == SyncMergeResolution::WholeProposalFallback
                    && value.revision == proposal_track.revision
                {
                    proposal.files.clone()
                } else {
                    match self.merge_track_files_later_wins(base, current, proposal, &value) {
                        Ok(files) => files,
                        Err(_) => {
                            return Ok(Some(ResolvedSyncSnapshot {
                                snapshot: proposal.clone(),
                                resolution: SyncMergeResolution::WholeProposalFallback,
                            }));
                        }
                    }
                };
                let document = AuthoredDocument::Track(value);
                Ok(Some(ResolvedSyncSnapshot {
                    snapshot: AuthoredSnapshot { files, document },
                    resolution,
                }))
            }
            (
                AuthoredDocument::Graph(base_graph),
                AuthoredDocument::Graph(current_graph),
                AuthoredDocument::Graph(proposal_graph),
            ) => {
                let merged = merge_graph_total(
                    &base_graph.graph,
                    &current_graph.graph,
                    &proposal_graph.graph,
                );
                if merged.resolution == SyncMergeResolution::KeptCurrentFallback {
                    return Ok(None);
                }
                let graph = canonicalize_graph(&merged.value)
                    .or_else(|_| canonicalize_graph(&proposal_graph.graph));
                let Ok(graph) = graph else {
                    return Ok(None);
                };
                let document = AuthoredDocument::Graph(GraphDocument {
                    implementation_id: proposal_graph.implementation_id.clone(),
                    revision: crate::services::graph_documents::graph_revision(&graph)?,
                    graph,
                });
                let files = match &document {
                    AuthoredDocument::Graph(graph) => graph_files(&graph.graph)?,
                    AuthoredDocument::Track(_) => unreachable!(),
                };
                let resolution = merged.resolution;
                let snapshot = if resolution == SyncMergeResolution::WholeProposalFallback
                    && document.revision() == proposal.document.revision()
                {
                    proposal.clone()
                } else {
                    AuthoredSnapshot { files, document }
                };
                Ok(Some(ResolvedSyncSnapshot {
                    snapshot,
                    resolution,
                }))
            }
            _ => Ok(None),
        }
    }

    async fn prepare_proposal_integration(
        &self,
        pool: &SqlitePool,
        admitted_user_id: &str,
        proposal: &ProposalRow,
        server_head: Option<RevisionId>,
    ) -> std::result::Result<IntegrateHeadProposalInput, SyncError> {
        let document_id =
            AuthoredDocumentId::parse(proposal.document_id.clone()).map_err(sync_local)?;
        let proposed_id =
            RevisionId::parse(proposal.proposed_revision_id.clone()).map_err(sync_local)?;
        let mut connection = pool.acquire().await?;
        let scope = self
            .scope_for_document(&mut connection, admitted_user_id, &document_id)
            .await
            .map_err(sync_local)?;
        let proposal_snapshot = match self
            .snapshot_from_revision(&mut connection, &scope, &proposed_id)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) if invalid_revision_error(&error) => {
                return Ok(IntegrateHeadProposalInput::quarantined_noop(
                    &proposal.proposal_id,
                    server_head.map(|head| head.to_string()),
                ));
            }
            Err(error) => return Err(sync_local(error)),
        };
        if !self
            .sync_snapshot_is_valid(pool, &scope, &proposal_snapshot)
            .await
        {
            return Ok(IntegrateHeadProposalInput::quarantined_noop(
                &proposal.proposal_id,
                server_head.map(|head| head.to_string()),
            ));
        }
        let Some(current_id) = server_head else {
            return Ok(IntegrateHeadProposalInput::new(
                &proposal.proposal_id,
                None,
                if proposal.base_revision_id.is_none() {
                    HeadIntegrationResolution::FastForward
                } else {
                    HeadIntegrationResolution::WholeProposal
                },
                proposed_id.to_string(),
            ));
        };
        if current_id == proposed_id
            || self
                .store
                .is_ancestor(&mut connection, &document_id, &proposed_id, &current_id)
                .await
                .map_err(sync_local)?
        {
            return Ok(IntegrateHeadProposalInput::new(
                &proposal.proposal_id,
                Some(current_id.to_string()),
                HeadIntegrationResolution::AlreadyAncestor,
                current_id.to_string(),
            ));
        }
        if self
            .store
            .is_ancestor(&mut connection, &document_id, &current_id, &proposed_id)
            .await
            .map_err(sync_local)?
        {
            return Ok(IntegrateHeadProposalInput::new(
                &proposal.proposal_id,
                Some(current_id.to_string()),
                HeadIntegrationResolution::FastForward,
                proposed_id.to_string(),
            ));
        }

        let current_snapshot = match self
            .snapshot_from_revision(&mut connection, &scope, &current_id)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) if invalid_revision_error(&error) => {
                return Ok(IntegrateHeadProposalInput::new(
                    &proposal.proposal_id,
                    Some(current_id.to_string()),
                    HeadIntegrationResolution::WholeProposal,
                    proposed_id.to_string(),
                ));
            }
            Err(error) => return Err(sync_local(error)),
        };
        let base = match self
            .store
            .merge_base(&mut connection, &document_id, &current_id, &proposed_id)
            .await
        {
            Ok(base_id) => match self
                .snapshot_from_revision(&mut connection, &scope, &base_id)
                .await
            {
                Ok(snapshot) => Some(snapshot),
                Err(error) if invalid_revision_error(&error) => None,
                Err(error) => return Err(sync_local(error)),
            },
            Err(
                AuthoredStateError::NotFound(_) | AuthoredStateError::AmbiguousMergeBase { .. },
            ) => None,
            Err(error) => return Err(sync_local(error)),
        };
        drop(connection);
        let Some(merged) = self
            .total_sync_snapshot(
                pool,
                &scope,
                base.as_ref(),
                &current_snapshot,
                &proposal_snapshot,
            )
            .await
            .map_err(sync_local)?
        else {
            return Ok(IntegrateHeadProposalInput::quarantined_noop(
                &proposal.proposal_id,
                Some(current_id.to_string()),
            ));
        };

        // A missing/ambiguous base and an invalid structural result both take
        // the specified whole-proposal terminal fallback.
        if base.is_none()
            || merged.resolution == SyncMergeResolution::WholeProposalFallback
            || merged.snapshot.files == proposal_snapshot.files
        {
            return Ok(IntegrateHeadProposalInput::new(
                &proposal.proposal_id,
                Some(current_id.to_string()),
                HeadIntegrationResolution::WholeProposal,
                proposed_id.to_string(),
            ));
        }

        let Some(metadata) = sync_revision_metadata(
            &proposal.proposal_id,
            &current_id,
            "Integrate server-ordered authored proposal",
            &proposal.created_at,
        ) else {
            // Legacy server rows may predate proposal audit validation. The
            // semantic proposal snapshot was already validated above, so a
            // malformed audit timestamp must choose a terminal total fallback
            // rather than wedge the earliest proposal forever.
            return Ok(IntegrateHeadProposalInput::new(
                &proposal.proposal_id,
                Some(current_id.to_string()),
                HeadIntegrationResolution::WholeProposal,
                proposed_id.to_string(),
            ));
        };
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
        assert_sync_principal(&mut transaction, admitted_user_id)
            .await
            .map_err(sync_local)?;
        let revision = self
            .store
            .insert_revision(
                &mut transaction,
                &document_id,
                &[current_id.clone(), proposed_id.clone()],
                &merged.snapshot.files,
                &metadata,
            )
            .await
            .map_err(sync_local)?;
        transaction.commit().await?;
        Ok(IntegrateHeadProposalInput::new(
            &proposal.proposal_id,
            Some(current_id.to_string()),
            HeadIntegrationResolution::Structural,
            revision.id.to_string(),
        ))
    }

    async fn sync_snapshot_is_valid(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        snapshot: &AuthoredSnapshot,
    ) -> bool {
        match &snapshot.document {
            AuthoredDocument::Track(track) => {
                let Some(track_scope) = scope.track_scope() else {
                    return false;
                };
                crate::services::track_edits::check_track_projection_candidate(
                    pool,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                    &track.clips,
                    TrackProjectionAuthority::TrustedRevision.identity(),
                )
                .await
                .is_ok()
            }
            AuthoredDocument::Graph(graph) => canonicalize_graph(&graph.graph).is_ok(),
        }
    }

    async fn upload_revision_closure(
        &self,
        pool: &SqlitePool,
        remote: &dyn RemoteClient,
        token: &str,
        document_id: &AuthoredDocumentId,
        revision_id: &RevisionId,
    ) -> std::result::Result<(), SyncError> {
        let mut connection = pool.acquire().await?;
        let (revision, files) = self
            .store
            .read_revision(&mut connection, document_id, revision_id)
            .await
            .map_err(sync_local)?;
        let mut rows = vec![("authored_documents", document_id.to_string())];
        rows.push(("authored_revisions", revision_id.to_string()));
        rows.extend(files.keys().map(|path| {
            (
                "authored_revision_files",
                registry::record_id([revision_id.as_str(), path.as_str()]),
            )
        }));
        rows.extend((0..revision.parents.len()).map(|order| {
            (
                "authored_revision_parents",
                registry::record_id([revision_id.as_str(), order.to_string().as_str()]),
            )
        }));
        for (table_name, record_id) in rows {
            let (table, payload) = local_row_payload(&mut connection, table_name, &record_id)
                .await
                .map_err(sync_local)?;
            remote
                .insert_immutable_json(table.name, &payload, table.conflict_key, token)
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl HeadProposalIntegrator for AuthoredDocuments {
    async fn integrate_pending_proposal(
        &self,
        pool: &SqlitePool,
        remote: &dyn RemoteClient,
        token: &str,
        admitted_user_id: &str,
        proposal_id: &str,
    ) -> std::result::Result<HeadIntegrationReceipt, SyncError> {
        let expected_principal = principal_key(Some(admitted_user_id));
        let proposal = sqlx::query_as::<_, ProposalRow>(
            "SELECT proposal_id, principal_key, document_id, base_revision_id,
                    proposed_revision_id, server_proposal_seq, created_at
             FROM authored_head_proposals WHERE proposal_id = ?",
        )
        .bind(proposal_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SyncError::Local(format!("authored proposal {proposal_id} is missing")))?;
        if proposal.principal_key != expected_principal || proposal.server_proposal_seq.is_none() {
            return Err(SyncError::Local(format!(
                "authored proposal {proposal_id} is not an admitted server proposal"
            )));
        }
        if let Some(base) = proposal.base_revision_id.as_deref() {
            RevisionId::parse(base.to_owned()).map_err(sync_local)?;
        }

        let rows = remote
            .select_json(
                "authored_document_heads",
                &format!(
                    "document_id=eq.{}&select=document_id,principal_key,revision_id",
                    proposal.document_id
                ),
                token,
            )
            .await?;
        if rows.len() > 1 {
            return Err(SyncError::Parse(
                "server returned duplicate authored document heads".into(),
            ));
        }
        let server_head = rows
            .first()
            .map(|row| {
                if row.get("document_id").and_then(Value::as_str)
                    != Some(proposal.document_id.as_str())
                    || row.get("principal_key").and_then(Value::as_str)
                        != Some(expected_principal.as_str())
                {
                    return Err(SyncError::Parse(
                        "server authored head has the wrong scope".into(),
                    ));
                }
                RevisionId::parse(
                    row.get("revision_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            SyncError::Parse("server authored head has no revision".into())
                        })?
                        .to_owned(),
                )
                .map_err(sync_local)
            })
            .transpose()?;
        let request = self
            .prepare_proposal_integration(pool, admitted_user_id, &proposal, server_head)
            .await?;
        if request.resolution == HeadIntegrationResolution::Structural {
            let revision_id =
                RevisionId::parse(request.result_revision_id.clone().ok_or_else(|| {
                    SyncError::Local("structural integration has no revision".into())
                })?)
                .map_err(sync_local)?;
            let document_id =
                AuthoredDocumentId::parse(proposal.document_id.clone()).map_err(sync_local)?;
            self.upload_revision_closure(pool, remote, token, &document_id, &revision_id)
                .await?;
        }
        let receipt = authored_remote::integrate_head_proposal(remote, &request, token).await?;
        if receipt.proposal_id != proposal.proposal_id
            || receipt.document_id != proposal.document_id
        {
            return Err(SyncError::Parse(
                "head integration RPC returned a receipt for another proposal".into(),
            ));
        }
        if receipt.is_terminal() {
            if let Some(head) = receipt.current_head_revision_id.as_deref() {
                self.apply_integrated_server_head(
                    pool,
                    admitted_user_id,
                    &proposal.document_id,
                    head,
                )
                .await
                .map_err(sync_local)?;
            }
        }
        Ok(receipt)
    }
}

async fn assert_sync_principal(
    connection: &mut SqliteConnection,
    admitted_user_id: &str,
) -> Result<()> {
    let admitted: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND remote_writes = 0 AND active_uid = ?",
    )
    .bind(admitted_user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(storage("authorize authored sync integration"))?;
    if admitted.is_some() {
        Ok(())
    } else {
        Err(AuthoredDocumentsError::Scope(
            "authored sync principal is not admitted".into(),
        ))
    }
}

fn sync_revision_metadata(
    proposal_id: &str,
    expected_server_head: &RevisionId,
    message: &str,
    authored_at: &str,
) -> Option<RevisionMetadata> {
    DateTime::parse_from_rfc3339(authored_at).ok()?;
    // A structural candidate belongs to one proposal attempt against one
    // exact authoritative head. A `not_earliest` retry after an earlier
    // proposal advances the head must be free to persist a second candidate,
    // while a response-loss retry against the same head remains idempotent.
    let operation_id = operation_request_fingerprint(
        "sync_integration_attempt",
        &[proposal_id, expected_server_head.as_str()],
    );
    Some(RevisionMetadata {
        // No human and no model authored this: the sync layer minted it to
        // converge two devices on the server's ordering.
        actor: Actor::sync(),
        operation_kind: "sync_integration".into(),
        operation_id: Some(operation_id),
        message: message.into(),
        author_name: "Luma Sync".into(),
        author_email: "authored-sync@luma.local".into(),
        authored_at: authored_at.to_owned(),
        thread_id: None,
        assistant_message_id: None,
        restored_revision_id: None,
    })
}

fn invalid_revision_error(error: &AuthoredDocumentsError) -> bool {
    matches!(
        error,
        AuthoredDocumentsError::Invalid(_)
            | AuthoredDocumentsError::Track(_)
            | AuthoredDocumentsError::Graph(_)
            | AuthoredDocumentsError::State(AuthoredStateError::InvalidInput(_))
            | AuthoredDocumentsError::State(AuthoredStateError::Corrupt(_))
    )
}

fn sync_local(error: impl std::fmt::Display) -> SyncError {
    SyncError::Local(error.to_string())
}

fn storage(context: &'static str) -> impl Fn(sqlx::Error) -> AuthoredDocumentsError {
    move |error| AuthoredDocumentsError::Storage(format!("{context}: {error}"))
}
