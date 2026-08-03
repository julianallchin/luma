use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use super::operations::{
    enqueue_local_row, insert_committed_operation, operation_outcome_on, OperationSpec,
};
use super::{
    agent_threads, graph_files, operation_request_fingerprint, revision_metadata,
    AuthoredConversationCheckpoint, AuthoredDocuments, AuthoredDocumentsError,
    AuthoredHistoryEntry, AuthoredHistoryPage, AuthoredMergeConflict, AuthoredOperationKind,
    AuthoredRestoreMode, AuthoredRestoreResult, AuthoredRevisionPosition, AuthoredSnapshot,
    AuthoredTurnCommit, DocumentScope, FinalizeAuthoredTurnInput, PrepareAuthoredTurnInput,
    PreparedAuthoredTurn, ResolvedScope, Result, RevisionId, RevisionInfo,
    TrackProjectionAuthority, MAX_HISTORY_PAGE,
};

impl AuthoredDocuments {
    pub async fn prepare_turn(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: PrepareAuthoredTurnInput,
    ) -> Result<PreparedAuthoredTurn> {
        let (_thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        let mut write = self.scope_write(pool, &scope).await?;
        let connection = write.connection();
        let main = self
            .ensure_current_on_connection(connection, &scope)
            .await?;
        if let Some(existing) = load_turn_preparation(
            connection,
            &scope,
            &input.thread_id,
            &input.assistant_message_id,
        )
        .await?
        {
            let snapshot = self
                .snapshot_from_revision(connection, &scope, &existing)
                .await?;
            if let Some(graph) = input.graph {
                let expected = graph_files(&graph)?;
                if expected != snapshot.files {
                    return Err(AuthoredDocumentsError::Invalid(
                        "assistant message id was already prepared with another graph".into(),
                    ));
                }
            }
            write.commit().await?;
            return Ok(PreparedAuthoredTurn {
                document_id: scope.document_id.to_string(),
                prepared_revision_id: existing.to_string(),
                document: snapshot.document.projected(),
            });
        }

        let snapshot = match (&scope.document, input.graph) {
            (DocumentScope::Track(_), Some(_)) => {
                return Err(AuthoredDocumentsError::Invalid(
                    "track assistant turns cannot prepare a graph".into(),
                ));
            }
            (DocumentScope::Pattern(_), Some(graph)) => {
                let files = graph_files(&graph)?;
                let document = self.decode_files(&scope, &files)?;
                AuthoredSnapshot { files, document }
            }
            (_, None) => AuthoredSnapshot {
                files: main.files.clone(),
                document: main.document.clone(),
            },
        };
        let metadata = revision_metadata(
            "agent_turn_prepare",
            Some(&input.assistant_message_id),
            "Prepare assistant turn state",
            Some(&input.thread_id),
            None,
            None,
        )?;
        let revision = self
            .store
            .insert_revision(
                connection,
                &scope.document_id,
                std::slice::from_ref(&main.head),
                &snapshot.files,
                &metadata,
            )
            .await?;
        sqlx::query(
            "INSERT INTO authored_turn_preparations
             (thread_id, assistant_message_id, owner_user_id, principal_key,
              document_id, prepared_revision_id)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.thread_id)
        .bind(&input.assistant_message_id)
        .bind(scope.owner_user_id.as_deref())
        .bind(&scope.principal_key)
        .bind(scope.document_id.as_str())
        .bind(revision.id.as_str())
        .execute(&mut *connection)
        .await
        .map_err(storage("record authored turn preparation"))?;
        self.enqueue_revision_closure(connection, &scope, &revision, &snapshot.files, false, None)
            .await?;
        if let Some(user_id) = scope.owner_user_id.as_deref() {
            enqueue_local_row(
                connection,
                user_id,
                "authored_turn_preparations",
                &format!("{}:{}", input.thread_id, input.assistant_message_id),
            )
            .await?;
        }
        write.commit().await?;
        Ok(PreparedAuthoredTurn {
            document_id: scope.document_id.to_string(),
            prepared_revision_id: revision.id.to_string(),
            document: snapshot.document.projected(),
        })
    }

    pub async fn finalize_turn(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: FinalizeAuthoredTurnInput,
    ) -> Result<AuthoredTurnCommit> {
        let (_thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        let prepared_id = RevisionId::parse(&input.prepared_revision_id)?;
        let mut write = self.scope_write(pool, &scope).await?;
        let connection = write.connection();
        let stored_prepared = load_turn_preparation(
            connection,
            &scope,
            &input.thread_id,
            &input.assistant_message_id,
        )
        .await?
        .ok_or_else(|| AuthoredDocumentsError::Invalid("assistant turn was not prepared".into()))?;
        if stored_prepared != prepared_id {
            return Err(AuthoredDocumentsError::Invalid(
                "assistant turn prepared revision does not match".into(),
            ));
        }
        if let Some(existing) = load_turn_outcome(
            connection,
            &scope,
            &input.thread_id,
            &input.assistant_message_id,
        )
        .await?
        {
            let result = self
                .turn_outcome_result(connection, &scope, existing)
                .await?;
            write.commit().await?;
            return Ok(result);
        }
        require_assistant_message(
            connection,
            &input.thread_id,
            &input.assistant_message_id,
            principal,
        )
        .await?;
        let current = self
            .ensure_current_on_connection(connection, &scope)
            .await?;
        let (prepared_info, prepared_files) = self
            .store
            .read_revision(connection, &scope.document_id, &prepared_id)
            .await?;
        let [base_id] = prepared_info.parents.as_slice() else {
            return Err(AuthoredDocumentsError::Storage(
                "prepared turn revision must have exactly one base parent".into(),
            ));
        };
        let prepared = AuthoredSnapshot {
            document: self.decode_files(&scope, &prepared_files)?,
            files: prepared_files,
        };
        let merge = if current.head == *base_id {
            Ok((prepared.document.clone(), prepared.files.clone()))
        } else {
            let base = self
                .snapshot_from_revision(connection, &scope, base_id)
                .await?;
            let ours = AuthoredSnapshot {
                files: current.files.clone(),
                document: current.document.clone(),
            };
            self.merge_snapshots(pool, &scope, &base, &ours, &prepared)
                .await?
        };
        let (candidate, files) = match merge {
            Ok(merged) => merged,
            Err(conflicts) => {
                insert_turn_conflict(
                    connection,
                    &scope,
                    &input.thread_id,
                    &input.assistant_message_id,
                    &prepared_id,
                    &conflicts,
                )
                .await?;
                enqueue_turn_outcome(
                    connection,
                    &scope,
                    &input.thread_id,
                    &input.assistant_message_id,
                )
                .await?;
                write.commit().await?;
                return Ok(AuthoredTurnCommit::Conflicted {
                    document_id: scope.document_id.to_string(),
                    prepared_revision_id: prepared_id.to_string(),
                    conflicts,
                });
            }
        };
        let metadata = revision_metadata(
            "agent_turn",
            Some(&input.assistant_message_id),
            "Apply assistant turn",
            Some(&input.thread_id),
            Some(&input.assistant_message_id),
            None,
        )?;
        let final_revision = self
            .store
            .insert_revision(
                connection,
                &scope.document_id,
                &[current.head.clone(), prepared_id.clone()],
                &files,
                &metadata,
            )
            .await?;
        let changed = files != current.files;
        let (_, projected, _) = self
            .project_candidate_on_connection(
                connection,
                &scope,
                candidate,
                current.document.revision(),
                TrackProjectionAuthority::TrustedRevision,
            )
            .await?;
        self.store
            .compare_and_swap_head(
                connection,
                &scope.document_id,
                &current.head,
                &final_revision.id,
            )
            .await?;
        insert_turn_commit(
            connection,
            &scope,
            &input.thread_id,
            &input.assistant_message_id,
            &prepared_id,
            &final_revision.id,
        )
        .await?;
        self.enqueue_revision_closure(connection, &scope, &final_revision, &files, false, None)
            .await?;
        enqueue_turn_outcome(
            connection,
            &scope,
            &input.thread_id,
            &input.assistant_message_id,
        )
        .await?;
        self.create_head_proposal(
            connection,
            &scope,
            Some(&current.head),
            &final_revision.id,
            &input.assistant_message_id,
        )
        .await?;
        write.commit().await?;
        Ok(AuthoredTurnCommit::Committed {
            document_id: scope.document_id.to_string(),
            revision_id: final_revision.id.to_string(),
            applied_to_current_projection: true,
            changed,
            document: projected,
        })
    }

    pub async fn recover_turns(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
    ) -> Result<Vec<AuthoredTurnCommit>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT preparation.assistant_message_id, preparation.prepared_revision_id
             FROM authored_turn_preparations preparation
             JOIN agent_thread_messages message
               ON message.id = preparation.assistant_message_id
              AND message.created_in_thread_id = preparation.thread_id
              AND message.role = 'assistant'
             LEFT JOIN authored_turn_outcomes outcome
               ON outcome.thread_id = preparation.thread_id
              AND outcome.assistant_message_id = preparation.assistant_message_id
             WHERE preparation.thread_id = ?
               AND preparation.owner_user_id IS ?
               AND outcome.thread_id IS NULL
             ORDER BY preparation.created_at, preparation.assistant_message_id",
        )
        .bind(thread_id)
        .bind(principal)
        .fetch_all(pool)
        .await
        .map_err(storage("list recoverable authored turns"))?;
        let mut recovered = Vec::with_capacity(rows.len());
        for (assistant_message_id, prepared_revision_id) in rows {
            recovered.push(
                self.finalize_turn(
                    pool,
                    principal,
                    FinalizeAuthoredTurnInput {
                        thread_id: thread_id.to_owned(),
                        assistant_message_id,
                        prepared_revision_id,
                    },
                )
                .await?,
            );
        }
        Ok(recovered)
    }

    pub async fn list_history(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<AuthoredHistoryPage> {
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        let limit = limit.unwrap_or(50);
        if limit == 0 || limit > MAX_HISTORY_PAGE {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "history limit must be between 1 and {MAX_HISTORY_PAGE}"
            )));
        }
        let mut connection = pool
            .acquire()
            .await
            .map_err(storage("open authored history"))?;
        let head = self.store.head(&mut connection, &scope.document_id).await?;
        let mut lineage = Vec::new();
        let mut current = head.revision_id.clone();
        loop {
            let info = self
                .store
                .revision_info(&mut connection, &scope.document_id, &current)
                .await?;
            let next = info.parents.first().cloned();
            lineage.push(info);
            let Some(parent) = next else { break };
            current = parent;
        }
        let lineage_ids: HashSet<RevisionId> = lineage.iter().map(|info| info.id.clone()).collect();
        let proposals: Vec<(String, Option<i64>)> = sqlx::query_as(
            "SELECT proposal.proposed_revision_id, proposal.server_proposal_seq
             FROM authored_head_proposals proposal
             JOIN authored_head_integrations integration
               ON integration.proposal_id = proposal.proposal_id
             WHERE proposal.document_id = ?
             ORDER BY proposal.server_proposal_seq, proposal.proposal_id",
        )
        .bind(scope.document_id.as_str())
        .fetch_all(&mut *connection)
        .await
        .map_err(storage("load integrated authored proposals"))?;
        let mut proposal_sequences = HashMap::new();
        let mut superseded = Vec::new();
        for (revision, sequence) in proposals {
            let revision = RevisionId::parse(revision)?;
            proposal_sequences
                .entry(revision.clone())
                .and_modify(|existing: &mut Option<i64>| {
                    if existing.is_none() || sequence < *existing {
                        *existing = sequence;
                    }
                })
                .or_insert(sequence);
            if !lineage_ids.contains(&revision)
                && !superseded
                    .iter()
                    .any(|info: &RevisionInfo| info.id == revision)
            {
                superseded.push(
                    self.store
                        .revision_info(&mut connection, &scope.document_id, &revision)
                        .await?,
                );
            }
        }
        let head_id = head.revision_id;
        let mut entries = lineage
            .into_iter()
            .map(|info| {
                let revision_id = info.id.clone();
                let position = if head_id == revision_id {
                    AuthoredRevisionPosition::Current
                } else {
                    AuthoredRevisionPosition::Ancestor
                };
                let sequence = proposal_sequences.get(&revision_id).copied().flatten();
                history_entry(info, position, sequence)
            })
            .chain(superseded.into_iter().map(|info| {
                let sequence = proposal_sequences.get(&info.id).copied().flatten();
                history_entry(info, AuthoredRevisionPosition::Superseded, sequence)
            }))
            .collect::<Result<Vec<_>>>()?;
        entries.sort_by(|left, right| match (left.position, right.position) {
            (AuthoredRevisionPosition::Current, AuthoredRevisionPosition::Current) => {
                std::cmp::Ordering::Equal
            }
            (AuthoredRevisionPosition::Current, _) => std::cmp::Ordering::Less,
            (_, AuthoredRevisionPosition::Current) => std::cmp::Ordering::Greater,
            _ => right
                .authored_at
                .cmp(&left.authored_at)
                .then(right.revision_id.cmp(&left.revision_id)),
        });
        let start = match cursor {
            None => 0,
            Some(cursor) => entries
                .iter()
                .position(|entry| entry.revision_id == cursor)
                .ok_or_else(|| {
                    AuthoredDocumentsError::Invalid(
                        "history cursor is not in the current history view".into(),
                    )
                })?,
        };
        let next_cursor = entries
            .get(start.saturating_add(limit))
            .map(|entry| entry.revision_id.clone());
        Ok(AuthoredHistoryPage {
            entries: entries.into_iter().skip(start).take(limit).collect(),
            next_cursor,
        })
    }

    pub async fn restore(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        target_revision_id: &str,
        operation_id: &str,
        mode: AuthoredRestoreMode,
    ) -> Result<AuthoredRestoreResult> {
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        let target_id = RevisionId::parse(target_revision_id)?;
        let mode_name = match mode {
            AuthoredRestoreMode::StateOnly => "state_only",
            AuthoredRestoreMode::StateAndConversation => "state_and_conversation",
        };
        let fingerprint =
            operation_request_fingerprint("restore", &[thread_id, target_revision_id, mode_name]);
        let mut write = self.scope_write(pool, &scope).await?;
        let connection = write.connection();
        let current = self
            .ensure_current_on_connection(connection, &scope)
            .await?;
        if let Some(existing) =
            operation_outcome_on(connection, &scope, "restore", operation_id).await?
        {
            if existing.request_fingerprint != fingerprint {
                return Err(AuthoredDocumentsError::Invalid(
                    "restore operation id was already used with different input".into(),
                ));
            }
            let result_revision = existing.result_revision_id.ok_or_else(|| {
                AuthoredDocumentsError::Storage("restore outcome has no revision".into())
            })?;
            let result_json = existing.result_json.as_deref().ok_or_else(|| {
                AuthoredDocumentsError::Storage("restore outcome has no result".into())
            })?;
            let replay: RestoreOperationResult =
                serde_json::from_str(result_json).map_err(|error| {
                    AuthoredDocumentsError::Storage(format!("decode restore outcome: {error}"))
                })?;
            write.commit().await?;
            return Ok(AuthoredRestoreResult {
                document_id: scope.document_id.to_string(),
                revision_id: result_revision.to_string(),
                applied_to_current_projection: result_revision == current.head,
                document: current.document.projected(),
                forked_thread_id: replay.forked_thread_id,
            });
        }
        let (target_info, target_files) = self
            .store
            .read_revision(connection, &scope.document_id, &target_id)
            .await?;
        let target_document = self.decode_files(&scope, &target_files)?;
        let forked_thread_id = match mode {
            AuthoredRestoreMode::StateOnly => None,
            AuthoredRestoreMode::StateAndConversation => {
                let checkpoint_thread = target_info.metadata.thread_id.as_deref();
                let checkpoint_message = target_info.metadata.assistant_message_id.as_deref();
                if checkpoint_thread != Some(thread_id) || checkpoint_message.is_none() {
                    return Err(AuthoredDocumentsError::Invalid(
                        "this state has no conversation checkpoint in the selected thread".into(),
                    ));
                }
                let id = deterministic_restore_thread_id(
                    &scope.principal_key,
                    &scope.document_id.to_string(),
                    thread_id,
                    operation_id,
                );
                agent_threads::fork_thread_for_connection(
                    connection,
                    &id,
                    thread_id,
                    checkpoint_message,
                    Some("Restored conversation"),
                    principal,
                )
                .await
                .map_err(AuthoredDocumentsError::Storage)?;
                Some(id)
            }
        };
        let metadata = revision_metadata(
            "restore",
            Some(operation_id),
            "Restore authored state",
            Some(thread_id),
            None,
            Some(target_id.clone()),
        )?;
        let revision = self
            .store
            .insert_revision(
                connection,
                &scope.document_id,
                std::slice::from_ref(&current.head),
                &target_files,
                &metadata,
            )
            .await?;
        let (_, projected, _) = self
            .project_candidate_on_connection(
                connection,
                &scope,
                target_document,
                current.document.revision(),
                TrackProjectionAuthority::TrustedRevision,
            )
            .await?;
        self.store
            .compare_and_swap_head(connection, &scope.document_id, &current.head, &revision.id)
            .await?;
        let result_json = serde_json::to_string(&RestoreOperationResult {
            forked_thread_id: forked_thread_id.clone(),
        })
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("encode restore outcome: {error}"))
        })?;
        insert_committed_operation(
            connection,
            &scope,
            OperationSpec {
                kind: "restore",
                id: operation_id,
                fingerprint: &fingerprint,
                result_json: Some(&result_json),
            },
            Some(&current.head),
            &revision.id,
        )
        .await?;
        self.enqueue_revision_closure(
            connection,
            &scope,
            &revision,
            &target_files,
            false,
            Some(("restore", operation_id)),
        )
        .await?;
        self.create_head_proposal(
            connection,
            &scope,
            Some(&current.head),
            &revision.id,
            operation_id,
        )
        .await?;
        write.commit().await?;
        Ok(AuthoredRestoreResult {
            document_id: scope.document_id.to_string(),
            revision_id: revision.id.to_string(),
            applied_to_current_projection: true,
            document: projected,
            forked_thread_id,
        })
    }

    async fn turn_outcome_result(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        outcome: StoredTurnOutcome,
    ) -> Result<AuthoredTurnCommit> {
        match outcome {
            StoredTurnOutcome::Committed {
                prepared_revision_id,
                result_revision_id,
            } => {
                let (result_info, result_files) = self
                    .store
                    .read_revision(connection, &scope.document_id, &result_revision_id)
                    .await?;
                if result_info.parents.get(1) != Some(&prepared_revision_id) {
                    return Err(AuthoredDocumentsError::Storage(
                        "committed turn outcome does not match its prepared revision".into(),
                    ));
                }
                let first_parent = result_info.parents.first().ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "committed turn revision has no live-state parent".into(),
                    )
                })?;
                let (_, parent_files) = self
                    .store
                    .read_revision(connection, &scope.document_id, first_parent)
                    .await?;
                let head = self.store.head(connection, &scope.document_id).await?;
                let current = self
                    .snapshot_from_revision(connection, scope, &head.revision_id)
                    .await?;
                Ok(AuthoredTurnCommit::Committed {
                    document_id: scope.document_id.to_string(),
                    revision_id: result_revision_id.to_string(),
                    applied_to_current_projection: result_revision_id == head.revision_id,
                    changed: result_files != parent_files,
                    document: current.document.projected(),
                })
            }
            StoredTurnOutcome::Conflicted {
                prepared_revision_id,
                conflicts,
            } => Ok(AuthoredTurnCommit::Conflicted {
                document_id: scope.document_id.to_string(),
                prepared_revision_id: prepared_revision_id.to_string(),
                conflicts,
            }),
        }
    }
}

enum StoredTurnOutcome {
    Committed {
        prepared_revision_id: RevisionId,
        result_revision_id: RevisionId,
    },
    Conflicted {
        prepared_revision_id: RevisionId,
        conflicts: Vec<AuthoredMergeConflict>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreOperationResult {
    forked_thread_id: Option<String>,
}

async fn load_turn_preparation(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    thread_id: &str,
    assistant_message_id: &str,
) -> Result<Option<RevisionId>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT prepared_revision_id FROM authored_turn_preparations
         WHERE thread_id = ? AND assistant_message_id = ?
           AND principal_key = ? AND document_id = ?",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(&scope.principal_key)
    .bind(scope.document_id.as_str())
    .fetch_optional(&mut *connection)
    .await
    .map_err(storage("load authored turn preparation"))?;
    value.map(RevisionId::parse).transpose().map_err(Into::into)
}

async fn load_turn_outcome(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    thread_id: &str,
    assistant_message_id: &str,
) -> Result<Option<StoredTurnOutcome>> {
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT prepared_revision_id, status, result_revision_id, conflicts_json
         FROM authored_turn_outcomes
         WHERE thread_id = ? AND assistant_message_id = ?
           AND principal_key = ? AND document_id = ?",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(&scope.principal_key)
    .bind(scope.document_id.as_str())
    .fetch_optional(&mut *connection)
    .await
    .map_err(storage("load authored turn outcome"))?;
    row.map(|(prepared, status, result, conflicts)| {
        let prepared_revision_id = RevisionId::parse(prepared)?;
        match status.as_str() {
            "committed" => Ok(StoredTurnOutcome::Committed {
                prepared_revision_id,
                result_revision_id: RevisionId::parse(result.ok_or_else(|| {
                    AuthoredDocumentsError::Storage(
                        "committed turn outcome has no result revision".into(),
                    )
                })?)?,
            }),
            "conflicted" => Ok(StoredTurnOutcome::Conflicted {
                prepared_revision_id,
                conflicts: serde_json::from_str(conflicts.as_deref().unwrap_or("[]")).map_err(
                    |error| {
                        AuthoredDocumentsError::Storage(format!("decode turn conflicts: {error}"))
                    },
                )?,
            }),
            _ => Err(AuthoredDocumentsError::Storage(
                "authored turn outcome has invalid status".into(),
            )),
        }
    })
    .transpose()
}

async fn require_assistant_message(
    connection: &mut SqliteConnection,
    thread_id: &str,
    assistant_message_id: &str,
    principal: Option<&str>,
) -> Result<()> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM agent_thread_messages
         WHERE id = ? AND created_in_thread_id = ?
           AND owner_user_id IS ? AND role = 'assistant'",
    )
    .bind(assistant_message_id)
    .bind(thread_id)
    .bind(principal)
    .fetch_optional(&mut *connection)
    .await
    .map_err(storage("verify assistant turn message"))?;
    found.map(|_| ()).ok_or_else(|| {
        AuthoredDocumentsError::Invalid(
            "assistant message must be persisted before finalizing its turn".into(),
        )
    })
}

async fn insert_turn_commit(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    thread_id: &str,
    assistant_message_id: &str,
    prepared_revision_id: &RevisionId,
    result_revision_id: &RevisionId,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO authored_turn_outcomes
         (thread_id, assistant_message_id, owner_user_id, principal_key,
          document_id, prepared_revision_id, status, result_revision_id)
         VALUES (?, ?, ?, ?, ?, ?, 'committed', ?)",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(scope.owner_user_id.as_deref())
    .bind(&scope.principal_key)
    .bind(scope.document_id.as_str())
    .bind(prepared_revision_id.as_str())
    .bind(result_revision_id.as_str())
    .execute(&mut *connection)
    .await
    .map_err(storage("record committed authored turn"))?;
    Ok(())
}

async fn insert_turn_conflict(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    thread_id: &str,
    assistant_message_id: &str,
    prepared_revision_id: &RevisionId,
    conflicts: &[AuthoredMergeConflict],
) -> Result<()> {
    let conflicts = serde_json::to_string(conflicts).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("encode turn conflicts: {error}"))
    })?;
    sqlx::query(
        "INSERT INTO authored_turn_outcomes
         (thread_id, assistant_message_id, owner_user_id, principal_key,
          document_id, prepared_revision_id, status, conflicts_json)
         VALUES (?, ?, ?, ?, ?, ?, 'conflicted', ?)",
    )
    .bind(thread_id)
    .bind(assistant_message_id)
    .bind(scope.owner_user_id.as_deref())
    .bind(&scope.principal_key)
    .bind(scope.document_id.as_str())
    .bind(prepared_revision_id.as_str())
    .bind(conflicts)
    .execute(&mut *connection)
    .await
    .map_err(storage("record conflicted authored turn"))?;
    Ok(())
}

async fn enqueue_turn_outcome(
    connection: &mut SqliteConnection,
    scope: &ResolvedScope,
    thread_id: &str,
    assistant_message_id: &str,
) -> Result<()> {
    if let Some(user_id) = scope.owner_user_id.as_deref() {
        enqueue_local_row(
            connection,
            user_id,
            "authored_turn_outcomes",
            &format!("{thread_id}:{assistant_message_id}"),
        )
        .await?;
    }
    Ok(())
}

fn history_entry(
    info: RevisionInfo,
    position: AuthoredRevisionPosition,
    proposal_sequence: Option<i64>,
) -> Result<AuthoredHistoryEntry> {
    let kind = match info.metadata.operation_kind.as_str() {
        "initial_import" | "create_score" | "create_pattern" => {
            AuthoredOperationKind::InitialImport
        }
        "graph_edit" | "score_edit" => AuthoredOperationKind::Edit,
        "agent_turn" => AuthoredOperationKind::AgentTurn,
        "restore" => AuthoredOperationKind::Restore,
        "pattern_fork" => AuthoredOperationKind::PatternFork,
        "workspace_merge" => AuthoredOperationKind::WorkspaceMerge,
        "sync_integration" => AuthoredOperationKind::SyncIntegration,
        _ => AuthoredOperationKind::Revision,
    };
    let conversation_checkpoint = match (
        info.metadata.thread_id.as_ref(),
        info.metadata.assistant_message_id.as_ref(),
    ) {
        (Some(thread_id), Some(assistant_message_id)) => Some(AuthoredConversationCheckpoint {
            thread_id: thread_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
        }),
        _ => None,
    };
    Ok(AuthoredHistoryEntry {
        revision_id: info.id.to_string(),
        parent_ids: info
            .parents
            .into_iter()
            .map(|parent| parent.to_string())
            .collect(),
        message: info.metadata.message,
        authored_at: info.metadata.authored_at,
        thread_id: info.metadata.thread_id,
        assistant_message_id: info.metadata.assistant_message_id,
        kind,
        position,
        proposal_sequence,
        conversation_checkpoint,
    })
}

fn deterministic_restore_thread_id(
    principal_key: &str,
    document_id: &str,
    thread_id: &str,
    operation_id: &str,
) -> String {
    let request = operation_request_fingerprint(
        "restore_conversation_fork",
        &[principal_key, document_id, thread_id, operation_id],
    );
    super::deterministic_creation_id(principal_key, "restore_fork", &request, "thread")
}

fn storage(context: &'static str) -> impl Fn(sqlx::Error) -> AuthoredDocumentsError {
    move |error| AuthoredDocumentsError::Storage(format!("{context}: {error}"))
}
