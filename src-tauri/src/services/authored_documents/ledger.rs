use super::{
    AuthoredDocumentsError, AuthoredMergeConflict, AuthoredRepositoryId, CommitId, Digest,
    DocumentScope, FromRow, OperationProjection, ResolvedScope, Result, Sha256, SqliteConnection,
    SqlitePool, TurnProjection, Uuid,
};

pub(super) async fn archive_projection_ledger(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    expected_commit: &CommitId,
    expected_state: MaterializationState,
) -> Result<()> {
    let row = sqlx::query_as::<_, LedgerRow>(
        "SELECT repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
                score_id, implementation_id, implementation_name, projected_commit,
                materialization_state
         FROM authored_state_projections WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("read projection ledger for archive: {error}"))
    })?
    .ok_or_else(|| {
        AuthoredDocumentsError::Storage("projection ledger disappeared during archive".into())
    })?;
    validate_ledger_scope(scope, &row)?;
    if row.projected_commit != expected_commit.as_str() {
        return Err(AuthoredDocumentsError::Storage(format!(
            "projection ledger moved during archive (expected {expected_commit}, current {})",
            row.projected_commit
        )));
    }
    let state = MaterializationState::parse(&row.materialization_state)?;
    if state != expected_state {
        return Err(AuthoredDocumentsError::Storage(format!(
            "projection materialization state moved during archive (expected {}, current {})",
            expected_state.as_str(),
            state.as_str()
        )));
    }
    let updated = sqlx::query(
        "UPDATE authored_state_projections
         SET materialization_state = 'archived'
         WHERE repository_id = ? AND projected_commit = ? AND materialization_state = ?",
    )
    .bind(scope.repository_id.as_str())
    .bind(expected_commit.as_str())
    .bind(expected_state.as_str())
    .execute(&mut *connection)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("archive projection ledger: {error}"))
    })?
    .rows_affected();
    if updated != 1 {
        return Err(AuthoredDocumentsError::Storage(
            "projection ledger changed during archive compare-and-swap".into(),
        ));
    }
    Ok(())
}

pub(super) async fn load_ledger(
    pool: &SqlitePool,
    scope: &ResolvedScope,
) -> Result<Option<ProjectionLedger>> {
    let row = sqlx::query_as::<_, LedgerRow>(
        "SELECT repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
                score_id, implementation_id, implementation_name, projected_commit,
                materialization_state
         FROM authored_state_projections WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load projection ledger: {error}")))?;
    row.map(|row| {
        validate_ledger_scope(scope, &row)?;
        Ok(ProjectionLedger {
            projected_commit: CommitId::parse(row.projected_commit)?,
            materialization_state: MaterializationState::parse(&row.materialization_state)?,
            implementation_name: row.implementation_name,
        })
    })
    .transpose()
}

pub(super) async fn write_ledger(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    expected: ProjectionLedgerExpectation<'_>,
    projected_commit: &CommitId,
) -> Result<()> {
    let existing = sqlx::query_as::<_, LedgerRow>(
        "SELECT repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
                score_id, implementation_id, implementation_name, projected_commit,
                materialization_state
         FROM authored_state_projections WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("read projection ledger: {error}")))?;
    let (kind, track_id, venue_id, score_id, implementation_id) = ledger_scope(scope);
    let implementation_name = if let Some(implementation_id) = implementation_id {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT name FROM implementations WHERE id = ? AND pattern_id = ?",
        )
        .bind(implementation_id)
        .bind(&scope.subject_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "load implementation identity for projection ledger: {error}"
            ))
        })?
    } else {
        None
    };
    if let Some(row) = existing {
        validate_ledger_scope(scope, &row)?;
        let (expected_commit, expected_materialization_state) = match expected {
            ProjectionLedgerExpectation::PresentAt(expected_commit) => {
                (expected_commit, MaterializationState::Present)
            }
            ProjectionLedgerExpectation::AbsentAt(expected_commit) => {
                (expected_commit, MaterializationState::Absent)
            }
            ProjectionLedgerExpectation::Missing => {
                return Err(AuthoredDocumentsError::Storage(format!(
                    "projection ledger was created concurrently at {}",
                    row.projected_commit
                )));
            }
        };
        if row.projected_commit != expected_commit.as_str() {
            return Err(AuthoredDocumentsError::Storage(format!(
                "projection ledger moved (expected {expected_commit}, current {})",
                row.projected_commit
            )));
        }
        let materialization_state = MaterializationState::parse(&row.materialization_state)?;
        if materialization_state != expected_materialization_state {
            return Err(AuthoredDocumentsError::Storage(format!(
                "projection materialization state moved (expected {}, current {})",
                expected_materialization_state.as_str(),
                materialization_state.as_str()
            )));
        }
        let updated = sqlx::query(
            "UPDATE authored_state_projections
             SET projected_commit = ?, materialization_state = 'present',
                 implementation_name = ?
             WHERE repository_id = ? AND projected_commit = ? AND materialization_state = ?",
        )
        .bind(projected_commit.as_str())
        .bind(implementation_name.as_deref())
        .bind(scope.repository_id.as_str())
        .bind(expected_commit.as_str())
        .bind(expected_materialization_state.as_str())
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("update projection ledger: {error}"))
        })?
        .rows_affected();
        if updated != 1 {
            return Err(AuthoredDocumentsError::Storage(
                "projection ledger changed during compare-and-swap".into(),
            ));
        }
    } else {
        match expected {
            ProjectionLedgerExpectation::PresentAt(expected_commit)
            | ProjectionLedgerExpectation::AbsentAt(expected_commit) => {
                return Err(AuthoredDocumentsError::Storage(format!(
                    "projection ledger is missing (expected {expected_commit})"
                )));
            }
            ProjectionLedgerExpectation::Missing => {}
        }
        sqlx::query(
            "INSERT INTO authored_state_projections
             (repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
              score_id, implementation_id, implementation_name, projected_commit,
              materialization_state)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'present')",
        )
        .bind(scope.repository_id.as_str())
        .bind(kind)
        .bind(&scope.principal_key)
        .bind(&scope.subject_id)
        .bind(track_id)
        .bind(venue_id)
        .bind(score_id)
        .bind(implementation_id)
        .bind(implementation_name.as_deref())
        .bind(projected_commit.as_str())
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("create projection ledger: {error}"))
        })?;
    }
    Ok(())
}

pub(super) fn ledger_scope(
    scope: &ResolvedScope,
) -> (
    &'static str,
    Option<&str>,
    Option<&str>,
    Option<&str>,
    Option<&str>,
) {
    match &scope.document {
        DocumentScope::Track(track) => (
            "track_score",
            Some(track.track_id.as_str()),
            Some(track.venue_id.as_str()),
            Some(track.score_id.as_str()),
            None,
        ),
        DocumentScope::Pattern(graph) => (
            "pattern_graph",
            None,
            None,
            None,
            Some(graph.implementation_id.as_str()),
        ),
    }
}

pub(super) fn validate_ledger_scope(scope: &ResolvedScope, row: &LedgerRow) -> Result<()> {
    let (kind, track_id, venue_id, score_id, implementation_id) = ledger_scope(scope);
    if row.repository_id != scope.repository_id.as_str()
        || row.document_kind != kind
        || row.principal_key != scope.principal_key
        || row.subject_id != scope.subject_id
        || row.track_id.as_deref() != track_id
        || row.venue_id.as_deref() != venue_id
        || row.score_id.as_deref() != score_id
        || row.implementation_id.as_deref() != implementation_id
    {
        return Err(AuthoredDocumentsError::Scope(
            "projection ledger does not match authored scope".into(),
        ));
    }
    Ok(())
}

pub(super) async fn write_turn_association(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    main_commit: &CommitId,
    turn: &TurnProjection,
) -> Result<()> {
    sqlx::query(
        "UPDATE authored_state_turn_commits
         SET main_commit = ?, status = 'committed', conflicts_json = NULL
         WHERE thread_id = ? AND assistant_message_id = ? AND repository_id = ?
           AND branch_commit = ? AND status = 'prepared'",
    )
    .bind(main_commit.as_str())
    .bind(&turn.thread_id)
    .bind(&turn.assistant_message_id)
    .bind(scope.repository_id.as_str())
    .bind(turn.branch_commit.as_str())
    .execute(&mut *connection)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("finalize turn commit: {error}")))?;
    let row = sqlx::query_as::<_, TurnAssociationRow>(
        "SELECT repository_id, branch_commit, main_commit, status, conflicts_json
         FROM authored_state_turn_commits
         WHERE thread_id = ? AND assistant_message_id = ?",
    )
    .bind(&turn.thread_id)
    .bind(&turn.assistant_message_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("verify turn commit: {error}")))?;
    if row.repository_id != scope.repository_id.as_str()
        || row.branch_commit != turn.branch_commit.as_str()
        || row.main_commit.as_deref() != Some(main_commit.as_str())
        || row.status != "committed"
        || row.conflicts_json.is_some()
    {
        return Err(AuthoredDocumentsError::Scope(
            "assistant message is already associated with different authored state".into(),
        ));
    }
    Ok(())
}

pub(super) async fn write_turn_conflict(
    pool: &SqlitePool,
    scope: &ResolvedScope,
    thread_id: &str,
    assistant_message_id: &str,
    branch_commit: &CommitId,
    conflicts: &[AuthoredMergeConflict],
) -> Result<()> {
    let conflicts_json = serde_json::to_string(conflicts).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("encode turn conflicts: {error}"))
    })?;
    sqlx::query(
        "UPDATE authored_state_turn_commits
         SET status = 'conflicted', conflicts_json = ?, main_commit = NULL
         WHERE thread_id = ? AND assistant_message_id = ? AND repository_id = ?
           AND branch_commit = ? AND status = 'prepared'",
    )
    .bind(&conflicts_json)
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(scope.repository_id.as_str())
    .bind(branch_commit.as_str())
    .execute(pool)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("record turn conflict: {error}")))?;
    let association = turn_association(pool, thread_id, assistant_message_id, &scope.repository_id)
        .await?
        .ok_or_else(|| AuthoredDocumentsError::Storage("turn conflict disappeared".into()))?;
    match association.outcome {
        TurnOutcome::Conflicted(stored) if stored == conflicts => Ok(()),
        _ => Err(AuthoredDocumentsError::Scope(
            "assistant message has a different terminal authored outcome".into(),
        )),
    }
}

pub(super) async fn write_turn_preparation(
    pool: &SqlitePool,
    scope: &ResolvedScope,
    thread_id: &str,
    assistant_message_id: &str,
    branch_commit: &CommitId,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO authored_state_turn_commits
         (thread_id, assistant_message_id, repository_id, branch_commit, main_commit)
         VALUES (?, ?, ?, ?, NULL)
         ON CONFLICT(thread_id, assistant_message_id) DO NOTHING",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(scope.repository_id.as_str())
    .bind(branch_commit.as_str())
    .execute(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("record turn preparation: {error}"))
    })?;
    let association = turn_association(pool, thread_id, assistant_message_id, &scope.repository_id)
        .await?
        .ok_or_else(|| {
            AuthoredDocumentsError::Storage("turn preparation disappeared after insert".into())
        })?;
    if association.branch_commit != *branch_commit {
        return Err(AuthoredDocumentsError::Scope(
            "assistant message is already prepared from different authored state".into(),
        ));
    }
    Ok(())
}

pub(super) async fn pending_turn_preparations(
    pool: &SqlitePool,
    thread_id: &str,
    repository_id: &AuthoredRepositoryId,
) -> Result<Vec<(String, CommitId)>> {
    #[derive(FromRow)]
    struct PendingTurnRow {
        assistant_message_id: String,
        branch_commit: String,
    }
    let rows = sqlx::query_as::<_, PendingTurnRow>(
        "SELECT assistant_message_id, branch_commit
         FROM authored_state_turn_commits
         WHERE thread_id = ? AND repository_id = ? AND status = 'prepared'
         ORDER BY created_at, assistant_message_id",
    )
    .bind(thread_id)
    .bind(repository_id.as_str())
    .fetch_all(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("list pending turn preparations: {error}"))
    })?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.assistant_message_id,
                CommitId::parse(row.branch_commit)?,
            ))
        })
        .collect()
}

pub(super) async fn turn_association(
    pool: &SqlitePool,
    thread_id: &str,
    assistant_message_id: &str,
    repository_id: &AuthoredRepositoryId,
) -> Result<Option<TurnAssociation>> {
    let row = sqlx::query_as::<_, TurnAssociationRow>(
        "SELECT repository_id, branch_commit, main_commit, status, conflicts_json
         FROM authored_state_turn_commits
         WHERE thread_id = ? AND assistant_message_id = ?",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load turn commit: {error}")))?;
    row.map(|row| {
        if row.repository_id != repository_id.as_str() {
            return Err(AuthoredDocumentsError::Scope(
                "assistant message belongs to another authored repository".into(),
            ));
        }
        let outcome = match row.status.as_str() {
            "prepared" if row.main_commit.is_none() && row.conflicts_json.is_none() => {
                TurnOutcome::Prepared
            }
            "committed" if row.conflicts_json.is_none() => {
                TurnOutcome::Committed(CommitId::parse(row.main_commit.ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "committed turn is missing its main commit".into(),
                    )
                })?)?)
            }
            "conflicted" if row.main_commit.is_none() => {
                let conflicts =
                    serde_json::from_str(row.conflicts_json.as_deref().ok_or_else(|| {
                        AuthoredDocumentsError::Storage(
                            "conflicted turn is missing structured conflicts".into(),
                        )
                    })?)
                    .map_err(|error| {
                        AuthoredDocumentsError::Storage(format!(
                            "decode stored turn conflicts: {error}"
                        ))
                    })?;
                TurnOutcome::Conflicted(conflicts)
            }
            _ => {
                return Err(AuthoredDocumentsError::Storage(
                    "turn association has an invalid status payload".into(),
                ));
            }
        };
        Ok(TurnAssociation {
            branch_commit: CommitId::parse(row.branch_commit)?,
            outcome,
        })
    })
    .transpose()
}

pub(super) async fn write_operation_association(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    commit: &CommitId,
    operation: &OperationProjection,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO authored_state_operations
         (repository_id, operation_kind, operation_id, request_fingerprint,
          base_main_commit, commit_id, status, conflicts_json, result_json)
         VALUES (?, ?, ?, ?, ?, ?, 'committed', NULL, ?)
         ON CONFLICT(repository_id, operation_kind, operation_id) DO NOTHING",
    )
    .bind(scope.repository_id.as_str())
    .bind(operation.kind)
    .bind(&operation.operation_id)
    .bind(&operation.request_fingerprint)
    .bind(operation.base_main_commit.as_str())
    .bind(commit.as_str())
    .bind(operation.result_json.as_deref())
    .execute(&mut *connection)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("record authored operation: {error}"))
    })?;
    let found: (Option<String>, String, String, String, Option<String>) = sqlx::query_as(
        "SELECT commit_id, request_fingerprint, base_main_commit, status, result_json
         FROM authored_state_operations
         WHERE repository_id = ? AND operation_kind = ? AND operation_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .bind(operation.kind)
    .bind(&operation.operation_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("verify authored operation: {error}"))
    })?;
    if found.0.as_deref() != Some(commit.as_str())
        || found.1 != operation.request_fingerprint
        || found.2 != operation.base_main_commit.as_str()
        || found.3 != "committed"
        || found.4 != operation.result_json
    {
        return Err(AuthoredDocumentsError::Scope(
            "operation id is already associated with another authored request".into(),
        ));
    }
    Ok(())
}

pub(super) async fn record_operation_conflict(
    pool: &SqlitePool,
    scope: &ResolvedScope,
    expected_main: &CommitId,
    kind: &str,
    operation_id: &str,
    request_fingerprint: &str,
    conflicts: &[AuthoredMergeConflict],
) -> Result<()> {
    let conflicts_json = serde_json::to_string(conflicts).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("encode authored operation conflicts: {error}"))
    })?;
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
        AuthoredDocumentsError::Storage(format!("begin conflicted authored operation: {error}"))
    })?;
    let ledger = sqlx::query_as::<_, LedgerRow>(
        "SELECT repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
                score_id, implementation_id, implementation_name, projected_commit,
                materialization_state
         FROM authored_state_projections WHERE repository_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("read projection ledger: {error}")))?
    .ok_or_else(|| AuthoredDocumentsError::Storage("projection ledger is missing".into()))?;
    validate_ledger_scope(scope, &ledger)?;
    if MaterializationState::parse(&ledger.materialization_state)? != MaterializationState::Present
    {
        return Err(AuthoredDocumentsError::Storage(
            "cannot record an authored operation against a non-present projection".into(),
        ));
    }
    if ledger.projected_commit != expected_main.as_str() {
        return Err(AuthoredDocumentsError::Storage(format!(
            "projection ledger moved (expected {expected_main}, current {})",
            ledger.projected_commit
        )));
    }
    sqlx::query(
        "INSERT INTO authored_state_operations
         (repository_id, operation_kind, operation_id, request_fingerprint,
          base_main_commit, commit_id, status, conflicts_json, result_json)
         VALUES (?, ?, ?, ?, ?, NULL, 'conflicted', ?, NULL)
         ON CONFLICT(repository_id, operation_kind, operation_id) DO NOTHING",
    )
    .bind(scope.repository_id.as_str())
    .bind(kind)
    .bind(operation_id)
    .bind(request_fingerprint)
    .bind(expected_main.as_str())
    .bind(&conflicts_json)
    .execute(&mut *transaction)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("record conflicted authored operation: {error}"))
    })?;
    let found: (String, String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT request_fingerprint, base_main_commit, status, commit_id, conflicts_json
         FROM authored_state_operations
         WHERE repository_id = ? AND operation_kind = ? AND operation_id = ?",
    )
    .bind(scope.repository_id.as_str())
    .bind(kind)
    .bind(operation_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("verify conflicted authored operation: {error}"))
    })?;
    if found.0 != request_fingerprint
        || found.1 != expected_main.as_str()
        || found.2 != "conflicted"
        || found.3.is_some()
        || found.4.as_deref() != Some(conflicts_json.as_str())
    {
        return Err(AuthoredDocumentsError::Scope(
            "operation id is already associated with another authored request".into(),
        ));
    }
    transaction.commit().await.map_err(|error| {
        AuthoredDocumentsError::Storage(format!("commit conflicted authored operation: {error}"))
    })?;
    Ok(())
}

pub(super) async fn operation_association(
    pool: &SqlitePool,
    repository_id: &AuthoredRepositoryId,
    kind: &str,
    operation_id: &str,
) -> Result<Option<OperationAssociation>> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, String, Option<String>, Option<String>)>(
        "SELECT request_fingerprint, base_main_commit, commit_id, status, conflicts_json, result_json
         FROM authored_state_operations
         WHERE repository_id = ? AND operation_kind = ? AND operation_id = ?",
    )
    .bind(repository_id.as_str())
    .bind(kind)
    .bind(operation_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("load authored operation: {error}"))
    })?;
    row.map(decode_operation_association).transpose()
}

pub(super) async fn operation_association_for_connection(
    connection: &mut SqliteConnection,
    repository_id: &AuthoredRepositoryId,
    kind: &str,
    operation_id: &str,
) -> Result<Option<OperationAssociation>> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, String, Option<String>, Option<String>)>(
        "SELECT request_fingerprint, base_main_commit, commit_id, status, conflicts_json, result_json
         FROM authored_state_operations
         WHERE repository_id = ? AND operation_kind = ? AND operation_id = ?",
    )
    .bind(repository_id.as_str())
    .bind(kind)
    .bind(operation_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("load transactional authored operation: {error}"))
    })?;
    row.map(decode_operation_association).transpose()
}

pub(super) fn decode_operation_association(
    (request_fingerprint, base_main_commit, commit, status, conflicts_json, result_json): (
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    ),
) -> Result<OperationAssociation> {
    let outcome = match (status.as_str(), commit, conflicts_json) {
        ("committed", Some(commit), None) => OperationOutcome::Committed(CommitId::parse(commit)?),
        ("conflicted", None, Some(conflicts)) => {
            OperationOutcome::Conflicted(serde_json::from_str(&conflicts).map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "decode authored operation conflicts: {error}"
                ))
            })?)
        }
        _ => {
            return Err(AuthoredDocumentsError::Storage(
                "authored operation has an invalid status payload".into(),
            ));
        }
    };
    Ok(OperationAssociation {
        request_fingerprint,
        base_main_commit: CommitId::parse(base_main_commit)?,
        outcome,
        result_json,
    })
}

// Transaction-local readers mirror the authoritative services so projection
// CAS is based on state read after BEGIN IMMEDIATE, not a stale outer read.

pub(super) struct CreationAssociation {
    pub(super) request_fingerprint: String,
    pub(super) subject_id: String,
    pub(super) auxiliary_id: Option<String>,
    pub(super) commit_id: CommitId,
}

pub(super) fn normalized_creation_request_id(request_id: &str) -> Result<String> {
    Uuid::parse_str(request_id)
        .map(|id| id.to_string())
        .map_err(|_| {
            AuthoredDocumentsError::Invalid("authored creation request_id must be a UUID".into())
        })
}

pub(super) fn deterministic_creation_id(
    principal_key: &str,
    creation_kind: &str,
    request_id: &str,
    role: &str,
) -> String {
    let mut hash = Sha256::new();
    for field in [
        "luma.authored-creation-id.v1",
        principal_key,
        creation_kind,
        request_id,
        role,
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 9562 UUIDv8: application-defined deterministic payload, RFC variant.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

pub(super) async fn load_creation_association(
    pool: &SqlitePool,
    principal_key: &str,
    creation_kind: &str,
    request_id: &str,
) -> Result<Option<CreationAssociation>> {
    let row: Option<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT request_fingerprint, subject_id, auxiliary_id, commit_id
         FROM authored_state_creations
         WHERE principal_key = ? AND creation_kind = ? AND request_id = ?",
    )
    .bind(principal_key)
    .bind(creation_kind)
    .bind(request_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("load authored creation association: {error}"))
    })?;
    row.map(
        |(request_fingerprint, subject_id, auxiliary_id, commit_id)| {
            Ok(CreationAssociation {
                request_fingerprint,
                subject_id,
                auxiliary_id,
                commit_id: CommitId::parse(&commit_id)?,
            })
        },
    )
    .transpose()
}

pub(super) fn verify_creation_replay(
    existing: &CreationAssociation,
    request_fingerprint: &str,
    subject_id: &str,
    auxiliary_id: Option<&str>,
) -> Result<()> {
    if existing.request_fingerprint != request_fingerprint
        || existing.subject_id != subject_id
        || existing.auxiliary_id.as_deref() != auxiliary_id
    {
        return Err(AuthoredDocumentsError::Invalid(
            "authored creation request_id was already used with another request".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_creation_association(
    connection: &mut SqliteConnection,
    principal_key: &str,
    creation_kind: &str,
    request_id: &str,
    request_fingerprint: &str,
    subject_id: &str,
    auxiliary_id: Option<&str>,
    commit_id: &CommitId,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO authored_state_creations
         (principal_key, creation_kind, request_id, request_fingerprint,
          subject_id, auxiliary_id, commit_id)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(principal_key)
    .bind(creation_kind)
    .bind(request_id)
    .bind(request_fingerprint)
    .bind(subject_id)
    .bind(auxiliary_id)
    .bind(commit_id.as_str())
    .execute(connection)
    .await
    .map_err(|error| {
        AuthoredDocumentsError::Storage(format!("record authored creation association: {error}"))
    })?;
    Ok(())
}

#[derive(FromRow)]
pub(super) struct LedgerRow {
    pub(super) repository_id: String,
    pub(super) document_kind: String,
    pub(super) principal_key: String,
    pub(super) subject_id: String,
    pub(super) track_id: Option<String>,
    pub(super) venue_id: Option<String>,
    pub(super) score_id: Option<String>,
    pub(super) implementation_id: Option<String>,
    pub(super) implementation_name: Option<String>,
    pub(super) projected_commit: String,
    pub(super) materialization_state: String,
}

pub(super) struct ProjectionLedger {
    pub(super) projected_commit: CommitId,
    pub(super) materialization_state: MaterializationState,
    pub(super) implementation_name: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MaterializationState {
    Present,
    Absent,
    Archived,
}

impl MaterializationState {
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "present" => Ok(Self::Present),
            "absent" => Ok(Self::Absent),
            "archived" => Ok(Self::Archived),
            _ => Err(AuthoredDocumentsError::Storage(format!(
                "invalid authored projection materialization state {value}"
            ))),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Archived => "archived",
        }
    }
}

pub(super) struct OperationAssociation {
    pub(super) request_fingerprint: String,
    pub(super) base_main_commit: CommitId,
    pub(super) outcome: OperationOutcome,
    pub(super) result_json: Option<String>,
}

pub(super) struct CommittedOperationReplay {
    pub(super) commit: CommitId,
    pub(super) result_json: Option<String>,
}

pub(super) enum OperationOutcome {
    Committed(CommitId),
    Conflicted(Vec<AuthoredMergeConflict>),
}

#[derive(Clone, Copy)]
pub(super) enum ProjectionLedgerExpectation<'a> {
    Missing,
    PresentAt(&'a CommitId),
    AbsentAt(&'a CommitId),
}

#[derive(FromRow)]
pub(super) struct TurnAssociationRow {
    pub(super) repository_id: String,
    pub(super) branch_commit: String,
    pub(super) main_commit: Option<String>,
    pub(super) status: String,
    pub(super) conflicts_json: Option<String>,
}

pub(super) struct TurnAssociation {
    pub(super) branch_commit: CommitId,
    pub(super) outcome: TurnOutcome,
}

pub(super) enum TurnOutcome {
    Prepared,
    Committed(CommitId),
    Conflicted(Vec<AuthoredMergeConflict>),
}
