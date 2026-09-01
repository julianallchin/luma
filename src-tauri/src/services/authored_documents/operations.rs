use super::{
    agent_threads, apply_graph_edit_in_transaction, apply_track_projection_in_transaction,
    AuthoredDocument, AuthoredDocuments, AuthoredDocumentsError, AuthoredProjectedDocument,
    DocumentScope, FileMap, GraphDocumentError, GraphEditPlan, MainState, ResolvedScope, Result,
    RevisionId, RevisionMetadata, TrackEditError, TrackEditPlan, TrackEditResult,
    TrackProjectionAuthority, VenueAccess, VenueResource, Write,
};
use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::authored_state::AppliedAuthoredState;
use crate::services::authored_state::{Actor, AuthoredStateError};
use crate::sync::registry;
use serde_json::Value;
use sqlx::{Row, Sqlite, SqliteConnection, SqlitePool, Transaction};

#[derive(Clone, Copy)]
pub(super) struct OperationSpec<'a> {
    pub kind: &'static str,
    pub id: &'a str,
    pub fingerprint: &'a str,
    pub result_json: Option<&'a str>,
}

pub(super) struct CandidateApplication {
    pub state: AppliedAuthoredState,
    pub track_edit: Option<TrackEditResult>,
    pub applied_to_current_projection: bool,
}

#[derive(Clone)]
pub(super) struct OperationOutcomeRow {
    pub request_fingerprint: String,
    pub status: String,
    pub result_revision_id: Option<RevisionId>,
    pub conflicts_json: Option<String>,
    pub result_json: Option<String>,
}

pub(super) enum ScopeWrite<'a> {
    Track(VenueAccess<'a, Write>),
    Pattern(Transaction<'a, Sqlite>),
}

impl ScopeWrite<'_> {
    pub(super) fn connection(&mut self) -> &mut SqliteConnection {
        match self {
            Self::Track(access) => access.connection(),
            Self::Pattern(transaction) => transaction,
        }
    }

    pub(super) async fn commit(self) -> Result<()> {
        match self {
            Self::Track(access) => access
                .commit()
                .await
                .map_err(AuthoredDocumentsError::Storage),
            Self::Pattern(transaction) => transaction.commit().await.map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "commit authored pattern transaction: {error}"
                ))
            }),
        }
    }
}

impl AuthoredDocuments {
    pub(super) async fn scope_write<'a>(
        &self,
        pool: &'a SqlitePool,
        scope: &ResolvedScope,
    ) -> Result<ScopeWrite<'a>> {
        match &scope.document {
            DocumentScope::Track(track_scope) => {
                let access =
                    VenueAccess::<Write>::write(pool, VenueResource::Score(&track_scope.score_id))
                        .await
                        .map_err(AuthoredDocumentsError::Scope)?;
                access
                    .require_venue(&track_scope.venue_id)
                    .map_err(AuthoredDocumentsError::Scope)?;
                if access.principal() != scope.owner_user_id.as_deref() {
                    return Err(AuthoredDocumentsError::Scope(
                        "authored score principal changed".into(),
                    ));
                }
                Ok(ScopeWrite::Track(access))
            }
            DocumentScope::Pattern(graph_scope) => {
                let mut transaction =
                    pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
                        AuthoredDocumentsError::Storage(format!(
                            "begin authored pattern transaction: {error}"
                        ))
                    })?;
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
                        "authorize authored pattern transaction: {error}"
                    ))
                })?;
                let Some((principal,)) = admitted else {
                    return Err(AuthoredDocumentsError::Scope(
                        "pattern does not belong to the admitted principal".into(),
                    ));
                };
                if principal.as_deref() != scope.owner_user_id.as_deref() {
                    return Err(AuthoredDocumentsError::Scope(
                        "authored pattern principal changed".into(),
                    ));
                }
                Ok(ScopeWrite::Pattern(transaction))
            }
        }
    }

    pub(super) async fn load_current_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
    ) -> Result<MainState> {
        let mut write = self.scope_write(pool, scope).await?;
        let current = self
            .ensure_current_on_connection(write.connection(), scope)
            .await?;
        write.commit().await?;
        Ok(current)
    }

    pub(super) async fn ensure_current_on_connection(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
    ) -> Result<MainState> {
        if let Some(thread_id) = scope.thread_id.as_deref() {
            agent_threads::assert_thread_active(
                connection,
                thread_id,
                scope.owner_user_id.as_deref(),
            )
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        }
        let specification = scope.specification()?;
        let existed: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM authored_documents WHERE document_id = ?")
                .bind(scope.document_id.as_str())
                .fetch_optional(&mut *connection)
                .await
                .map_err(storage("inspect authored document"))?;
        self.store
            .insert_document(connection, &specification)
            .await?;

        let head_value: Option<String> = sqlx::query_scalar(
            "SELECT revision_id FROM authored_document_heads WHERE document_id = ?",
        )
        .bind(scope.document_id.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(storage("load authored document head"))?;

        if let Some(head_value) = head_value {
            let head = RevisionId::parse(head_value)?;
            let snapshot = self
                .snapshot_from_revision(connection, scope, &head)
                .await?;
            let live = self
                .snapshot_from_connection(connection, scope, Some(&snapshot.files))
                .await?;
            if live.document.revision() != snapshot.document.revision() {
                return Err(AuthoredDocumentsError::Storage(format!(
                    "live projection for {} diverged from revision head {}; authored writes must use AuthoredDocuments",
                    scope.document_id, head
                )));
            }
            return Ok(MainState {
                head,
                files: snapshot.files,
                document: snapshot.document,
            });
        }

        if existed.is_some() {
            return Err(AuthoredDocumentsError::Storage(format!(
                "authored document {} exists without its permanent head",
                scope.document_id
            )));
        }
        let snapshot = self
            .snapshot_from_connection(connection, scope, None)
            .await?;
        // All devices importing the same legacy projection must derive the
        // same root revision. Wall-clock migration time would fork identical
        // history before server ordering even begins.
        let metadata = RevisionMetadata {
            operation_kind: "initial_import".into(),
            operation_id: None,
            message: "Import existing authored state".into(),
            // Constant like the timestamp beside it, and for the same reason:
            // every device importing the same legacy projection must derive
            // one root. Who first authored the imported state is not recorded
            // anywhere, and the human whose library it is is the closest true
            // answer — the same one the migration's backfill gives.
            actor: Actor::user(),
            author_name: "Luma".into(),
            author_email: "authored-state@luma.local".into(),
            authored_at: "1970-01-01T00:00:00Z".into(),
            thread_id: None,
            assistant_message_id: None,
            restored_revision_id: None,
        };
        let revision = self
            .store
            .insert_revision(
                connection,
                &scope.document_id,
                &[],
                &snapshot.files,
                &metadata,
            )
            .await?;
        self.store
            .create_head(connection, &scope.document_id, &revision.id)
            .await?;
        self.create_head_proposal(
            connection,
            scope,
            None,
            &revision.id,
            &revision.id.to_string(),
        )
        .await?;
        Ok(MainState {
            head: revision.id,
            files: snapshot.files,
            document: snapshot.document,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn apply_candidate_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        expected_head: &RevisionId,
        expected_projection_revision: &str,
        files: FileMap,
        candidate: AuthoredDocument,
        track_authority: TrackProjectionAuthority,
        operation: OperationSpec<'_>,
        subject: &str,
        parents: Option<Vec<RevisionId>>,
        assistant_message_id: Option<&str>,
        restored_revision_id: Option<RevisionId>,
    ) -> Result<CandidateApplication> {
        if let Some(existing) = self
            .operation_outcome(pool, scope, operation.kind, operation.id)
            .await?
        {
            require_operation_fingerprint(&existing, operation.fingerprint)?;
            return self
                .candidate_replay(pool, scope, existing, operation.result_json)
                .await;
        }

        let mut write = self.scope_write(pool, scope).await?;
        let connection = write.connection();
        let main = self.ensure_current_on_connection(connection, scope).await?;
        if main.head != *expected_head {
            return Err(AuthoredDocumentsError::State(
                AuthoredStateError::HeadConflict {
                    document_id: scope.document_id.to_string(),
                    expected: expected_head.to_string(),
                    actual: main.head.to_string(),
                },
            ));
        }
        if let Some(existing) =
            operation_outcome_on(connection, scope, operation.kind, operation.id).await?
        {
            require_operation_fingerprint(&existing, operation.fingerprint)?;
            drop(write);
            return self
                .candidate_replay(pool, scope, existing, operation.result_json)
                .await;
        }

        let parents = parents.unwrap_or_else(|| vec![main.head.clone()]);
        if parents.first() != Some(&main.head) {
            return Err(AuthoredDocumentsError::Invalid(
                "the first parent of a live revision must be the current head".into(),
            ));
        }
        let metadata = self.revision_metadata(
            scope,
            operation.kind,
            Some(operation.id),
            subject,
            assistant_message_id,
            restored_revision_id,
        )?;
        let revision = self
            .store
            .insert_revision(connection, &scope.document_id, &parents, &files, &metadata)
            .await?;

        let (changed, projected, track_edit) = self
            .project_candidate_on_connection(
                connection,
                scope,
                candidate,
                expected_projection_revision,
                track_authority,
            )
            .await?;
        self.store
            .compare_and_swap_head(connection, &scope.document_id, &main.head, &revision.id)
            .await?;
        insert_committed_operation(connection, scope, operation, Some(&main.head), &revision.id)
            .await?;
        self.create_head_proposal(
            connection,
            scope,
            Some(&main.head),
            &revision.id,
            operation.id,
        )
        .await?;
        write.commit().await?;
        Ok(CandidateApplication {
            state: AppliedAuthoredState {
                document_id: scope.document_id.to_string(),
                revision_id: revision.id.to_string(),
                changed,
                document: projected,
            },
            track_edit,
            applied_to_current_projection: true,
        })
    }

    pub(super) async fn project_candidate_on_connection(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        candidate: AuthoredDocument,
        expected_projection_revision: &str,
        track_authority: TrackProjectionAuthority,
    ) -> Result<(bool, AuthoredProjectedDocument, Option<TrackEditResult>)> {
        if let Some(thread_id) = scope.thread_id.as_deref() {
            agent_threads::assert_thread_active(
                connection,
                thread_id,
                scope.owner_user_id.as_deref(),
            )
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        }
        match (&scope.document, candidate) {
            (DocumentScope::Track(track_scope), AuthoredDocument::Track(candidate)) => {
                let current = super::projection::load_track_document_for_connection(
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
                let edit = apply_track_projection_in_transaction(
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
                let changed = edit.added + edit.updated + edit.removed > 0;
                let document = AuthoredProjectedDocument::TrackScore {
                    revision: edit.revision.clone(),
                };
                Ok((changed, document, Some(edit)))
            }
            (DocumentScope::Pattern(graph_scope), AuthoredDocument::Graph(candidate)) => {
                let current =
                    super::projection::load_graph_document_for_connection(connection, graph_scope)
                        .await?;
                if current.revision != expected_projection_revision {
                    return Err(AuthoredDocumentsError::Graph(
                        GraphDocumentError::Conflict {
                            expected_revision: expected_projection_revision.to_owned(),
                            current_revision: current.revision,
                        },
                    ));
                }
                let edit = apply_graph_edit_in_transaction(
                    connection,
                    graph_scope,
                    GraphEditPlan {
                        base_revision: expected_projection_revision.to_owned(),
                        candidate: candidate.graph,
                    },
                )
                .await?;
                Ok((
                    edit.changed,
                    AuthoredProjectedDocument::PatternGraph {
                        implementation_id: graph_scope.implementation_id.clone(),
                        revision: edit.revision,
                        graph: edit.graph,
                    },
                    None,
                ))
            }
            _ => Err(AuthoredDocumentsError::Storage(
                "attempted to project the wrong authored document kind".into(),
            )),
        }
    }

    pub(super) async fn operation_outcome(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        operation_kind: &str,
        operation_id: &str,
    ) -> Result<Option<OperationOutcomeRow>> {
        let mut connection = pool
            .acquire()
            .await
            .map_err(storage("open operation outcome"))?;
        operation_outcome_on(&mut connection, scope, operation_kind, operation_id).await
    }

    pub(super) async fn record_operation_conflict_on(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        operation: OperationSpec<'_>,
        base_revision_id: &RevisionId,
        conflicts: &[crate::models::authored_state::AuthoredMergeConflict],
    ) -> Result<()> {
        let conflicts_json = serde_json::to_string(conflicts).map_err(|error| {
            AuthoredDocumentsError::Storage(format!("encode authored operation conflicts: {error}"))
        })?;
        sqlx::query(
            "INSERT INTO authored_operation_outcomes
             (principal_key, document_id, operation_kind, operation_id,
              request_fingerprint, base_revision_id, status, conflicts_json)
             VALUES (?, ?, ?, ?, ?, ?, 'conflicted', ?)",
        )
        .bind(&scope.principal_key)
        .bind(scope.document_id.as_str())
        .bind(operation.kind)
        .bind(operation.id)
        .bind(operation.fingerprint)
        .bind(base_revision_id.as_str())
        .bind(conflicts_json)
        .execute(&mut *connection)
        .await
        .map_err(storage("record conflicted authored operation"))?;
        Ok(())
    }

    async fn candidate_replay(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        outcome: OperationOutcomeRow,
        expected_result_json: Option<&str>,
    ) -> Result<CandidateApplication> {
        if outcome.status != "committed" {
            return Err(AuthoredDocumentsError::Invalid(
                "operation previously completed with structured conflicts".into(),
            ));
        }
        if outcome.result_json.as_deref() != expected_result_json {
            return Err(AuthoredDocumentsError::Storage(
                "committed operation result does not match its idempotent replay".into(),
            ));
        }
        let revision_id = outcome.result_revision_id.ok_or_else(|| {
            AuthoredDocumentsError::Storage("committed operation has no result revision".into())
        })?;
        let mut connection = pool
            .acquire()
            .await
            .map_err(storage("open operation replay"))?;
        let head = self.store.head(&mut connection, &scope.document_id).await?;
        let current = self
            .snapshot_from_revision(&mut connection, scope, &head.revision_id)
            .await?;
        let info = self
            .store
            .revision_info(&mut connection, &scope.document_id, &revision_id)
            .await?;
        let changed = match info.parents.first() {
            Some(parent) => {
                let (_, parent_files) = self
                    .store
                    .read_revision(&mut connection, &scope.document_id, parent)
                    .await?;
                let (_, files) = self
                    .store
                    .read_revision(&mut connection, &scope.document_id, &revision_id)
                    .await?;
                files != parent_files
            }
            None => true,
        };
        let track_edit = outcome
            .result_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "decode committed track operation result: {error}"
                ))
            })?;
        Ok(CandidateApplication {
            state: AppliedAuthoredState {
                document_id: scope.document_id.to_string(),
                revision_id: revision_id.to_string(),
                changed,
                document: current.document.projected(),
            },
            track_edit,
            applied_to_current_projection: revision_id == head.revision_id,
        })
    }

    pub(super) async fn create_head_proposal(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        base_revision_id: Option<&RevisionId>,
        proposed_revision_id: &RevisionId,
        operation_id: &str,
    ) -> Result<()> {
        let Some(user_id) = scope.owner_user_id.as_deref() else {
            return Ok(());
        };
        let device_id: String = sqlx::query_scalar(
            "SELECT device_id FROM authored_device_identity WHERE singleton = 1",
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(storage("load authored device identity"))?;
        let proposal_id = deterministic_proposal_id(
            &device_id,
            &scope.document_id.to_string(),
            proposed_revision_id.as_str(),
        );
        sqlx::query(
            "INSERT INTO authored_head_proposals
             (proposal_id, principal_key, document_id, device_id, operation_id,
              base_revision_id, proposed_revision_id)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(proposal_id) DO NOTHING",
        )
        .bind(&proposal_id)
        .bind(&scope.principal_key)
        .bind(scope.document_id.as_str())
        .bind(&device_id)
        .bind(operation_id)
        .bind(base_revision_id.map(RevisionId::as_str))
        .bind(proposed_revision_id.as_str())
        .execute(&mut *connection)
        .await
        .map_err(storage("insert authored head proposal"))?;
        let created_at: String = sqlx::query_scalar(
            "SELECT created_at FROM authored_head_proposals WHERE proposal_id = ?",
        )
        .bind(&proposal_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(storage("load authored head proposal timestamp"))?;
        // No queue entry: the row just written *is* the RPC's input, and push
        // finds it by its NULL `server_proposal_seq`.
        let _ = (user_id, created_at);
        Ok(())
    }
}

pub(super) async fn operation_outcome_on(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    operation_kind: &str,
    operation_id: &str,
) -> Result<Option<OperationOutcomeRow>> {
    let row: Option<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT request_fingerprint, status, result_revision_id, conflicts_json, result_json
             FROM authored_operation_outcomes
             WHERE document_id = ? AND operation_kind = ? AND operation_id = ?",
    )
    .bind(scope.document_id.as_str())
    .bind(operation_kind)
    .bind(operation_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(storage("load authored operation outcome"))?;
    row.map(
        |(request_fingerprint, status, result_revision_id, conflicts_json, result_json)| {
            Ok(OperationOutcomeRow {
                request_fingerprint,
                status,
                result_revision_id: result_revision_id.map(RevisionId::parse).transpose()?,
                conflicts_json,
                result_json,
            })
        },
    )
    .transpose()
}

pub(super) async fn insert_committed_operation(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    operation: OperationSpec<'_>,
    base_revision_id: Option<&RevisionId>,
    result_revision_id: &RevisionId,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO authored_operation_outcomes
         (principal_key, document_id, operation_kind, operation_id,
          request_fingerprint, base_revision_id, status, result_revision_id, result_json)
         VALUES (?, ?, ?, ?, ?, ?, 'committed', ?, ?)",
    )
    .bind(&scope.principal_key)
    .bind(scope.document_id.as_str())
    .bind(operation.kind)
    .bind(operation.id)
    .bind(operation.fingerprint)
    .bind(base_revision_id.map(RevisionId::as_str))
    .bind(result_revision_id.as_str())
    .bind(operation.result_json)
    .execute(&mut *connection)
    .await
    .map_err(storage("record committed authored operation"))?;
    Ok(())
}

fn require_operation_fingerprint(existing: &OperationOutcomeRow, expected: &str) -> Result<()> {
    if existing.request_fingerprint == expected {
        Ok(())
    } else {
        Err(AuthoredDocumentsError::Invalid(
            "operation id was already used with different input".into(),
        ))
    }
}

pub(super) async fn local_row_payload(
    connection: &mut SqliteConnection,
    table_name: &str,
    record_id: &str,
) -> Result<(&'static registry::TableMeta, Value)> {
    let table = registry::get_table(table_name).ok_or_else(|| {
        AuthoredDocumentsError::Storage(format!(
            "authored sync table {table_name} is not registered"
        ))
    })?;
    let columns = table.columns.join(", ");
    let sql = format!(
        "SELECT {columns} FROM {} WHERE {}",
        table.name,
        table.pk_where()
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    let pk_values = table.decode_record_id(record_id).ok_or_else(|| {
        AuthoredDocumentsError::Storage(format!(
            "authored sync record id {table_name}.{record_id} does not name every primary-key column"
        ))
    })?;
    for value in pk_values {
        query = query.bind(value);
    }
    let row = query
        .fetch_optional(&mut *connection)
        .await
        .map_err(storage("read immutable authored sync row"))?
        .ok_or_else(|| {
            AuthoredDocumentsError::Storage(format!(
                "immutable authored row {table_name}.{record_id} disappeared"
            ))
        })?;
    let mut payload = serde_json::Map::new();
    for column in table.remote_columns() {
        let value = if registry::is_binary_column(table.name, column) {
            Value::String(postgres_bytea(&row.try_get::<Vec<u8>, _>(column).map_err(
                |error| {
                    AuthoredDocumentsError::Storage(format!(
                        "read {table_name}.{column} bytes: {error}"
                    ))
                },
            )?))
        } else if let Ok(value) = row.try_get::<Option<String>, _>(column) {
            value.map(Value::String).unwrap_or(Value::Null)
        } else if let Ok(value) = row.try_get::<i64, _>(column) {
            Value::Number(value.into())
        } else if let Ok(value) = row.try_get::<f64, _>(column) {
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        payload.insert(column.to_owned(), value);
    }
    Ok((table, Value::Object(payload)))
}

fn postgres_bytea(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(2 + bytes.len() * 2);
    encoded.push_str("\\x");
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn deterministic_proposal_id(device_id: &str, document_id: &str, revision_id: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hash = Sha256::new();
    hash.update(b"luma.authored-head-proposal.v1\0");
    for value in [device_id, document_id, revision_id] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("ap-{:x}", hash.finalize())
}

fn storage(context: &'static str) -> impl Fn(sqlx::Error) -> AuthoredDocumentsError {
    move |error| AuthoredDocumentsError::Storage(format!("{context}: {error}"))
}
