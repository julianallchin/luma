use super::{
    agent_threads, commit_changed, commit_message, find_commit_with_trailers, graph_files,
    operation_association, operation_request_fingerprint, parse_trailers,
    pending_turn_preparations, system_author, turn_association, validate_ref_component,
    write_turn_conflict, write_turn_preparation, AgentThread, AgentThreadMessage,
    AuthoredDocuments, AuthoredDocumentsError, AuthoredHistoryEntry, AuthoredHistoryPage,
    AuthoredOperationKind, AuthoredRestoreResult, AuthoredSnapshot, AuthoredTurnCommit,
    CommitAuthor, CommitId, CommitInfo, DateTime, DocumentScope, FinalizeAuthoredTurnInput,
    FixedOffset, HashMap, OperationOutcome, OperationProjection, PrepareAuthoredTurnInput,
    PreparedAuthoredTurn, ProjectionLedgerExpectation, ProjectionMetadata, ResolvedScope, Result,
    SqlitePool, TimeZone, TrackProjectionAuthority, TurnOutcome, TurnProjection, MAIN_BRANCH,
    MAX_HISTORY_PAGE, TRAILER_MESSAGE, TRAILER_OPERATION, TRAILER_OPERATION_ID, TRAILER_RESTORE,
    TRAILER_THREAD,
};

impl AuthoredDocuments {
    /// Capture the exact authored tree on the thread branch before the
    /// assistant transcript is persisted. This is the durable half of the
    /// turn protocol that prevents canvas-only graph state from being lost in
    /// the transcript/commit crash window.
    pub async fn prepare_turn(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: PrepareAuthoredTurnInput,
    ) -> Result<PreparedAuthoredTurn> {
        let (thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        if turn_association(
            pool,
            &thread.id,
            &input.assistant_message_id,
            &scope.repository_id,
        )
        .await?
        .is_none()
        {
            let message_exists: i64 = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM agent_thread_messages WHERE id = ?)",
            )
            .bind(&input.assistant_message_id)
            .fetch_one(pool)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "inspect assistant message identity before turn preparation: {error}"
                ))
            })?;
            if message_exists != 0 {
                return Err(AuthoredDocumentsError::Scope(
                    "authored turn cannot reserve an existing assistant message id".into(),
                ));
            }
        }
        let main = self.reconcile_locked(pool, &scope).await?;
        let observed_database = self
            .snapshot_from_database(pool, &scope, Some(&main.files))
            .await?;
        let expected_projection_revision = observed_database.document.revision().to_owned();
        let mut files = match (&scope.document, input.graph) {
            (DocumentScope::Track(_), None) => observed_database.files,
            (DocumentScope::Track(_), Some(_)) => {
                return Err(AuthoredDocumentsError::Invalid(
                    "track turns must not supply a graph".into(),
                ));
            }
            (DocumentScope::Pattern(_), Some(graph)) => graph_files(&graph)?,
            (DocumentScope::Pattern(_), None) => {
                return Err(AuthoredDocumentsError::Invalid(
                    "pattern turns must supply their exact graph candidate".into(),
                ));
            }
        };
        let mut candidate = self.decode_files(&scope, &files)?;
        let branch = self
            .ensure_thread_branch_locked(pool, &scope, &main.head)
            .await?;
        let branch_subject = "Capture completed agent turn";
        let branch_message = commit_message(
            branch_subject,
            &[
                (TRAILER_OPERATION, "agent_turn"),
                (TRAILER_THREAD, &thread.id),
                (TRAILER_MESSAGE, &input.assistant_message_id),
            ],
        )?;

        let existing_branch_commit = find_commit_with_trailers(
            &self.store,
            &scope.repository_id,
            &branch.branch_name,
            &[
                (TRAILER_THREAD, &thread.id),
                (TRAILER_MESSAGE, &input.assistant_message_id),
            ],
        )?;
        if let Some(commit) = &existing_branch_commit {
            // A response-loss retry is pinned to the tree already captured by
            // this message. Never recapture newer state under an old ID.
            files = self.store.read_commit(&scope.repository_id, &commit.id)?.1;
            candidate = self.decode_files(&scope, &files)?;
        }

        if expected_projection_revision != main.document.revision()
            && expected_projection_revision != candidate.revision()
        {
            return Err(AuthoredDocumentsError::Invalid(
                "live authored state changed independently after this turn was captured".into(),
            ));
        }

        let branch_commit = match existing_branch_commit {
            Some(commit) => commit,
            None => {
                let branch_head = self
                    .store
                    .branch_head(&scope.repository_id, &branch.branch_name)?;
                self.store.commit_files(
                    &scope.repository_id,
                    &branch.branch_name,
                    &branch_head,
                    &files,
                    &system_author()?,
                    &branch_message,
                )?
            }
        };
        write_turn_preparation(
            pool,
            &scope,
            &thread.id,
            &input.assistant_message_id,
            &branch_commit.id,
        )
        .await?;

        Ok(PreparedAuthoredTurn {
            repository_id: scope.repository_id.to_string(),
            branch_commit_id: branch_commit.id.to_string(),
            document: candidate.projected(),
        })
    }

    /// Finalize a prepared branch tree only after its assistant message is
    /// durable. The branch commit ID, not recaptured live state, is the source
    /// of truth. Repeating the call returns the original main commit.
    pub async fn finalize_turn(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: FinalizeAuthoredTurnInput,
    ) -> Result<AuthoredTurnCommit> {
        let (thread, scope, _guard) = self
            .lock_active_thread(pool, principal, &input.thread_id)
            .await?;
        let assistant =
            load_assistant_message(pool, &thread, principal, &input.assistant_message_id).await?;
        let branch_commit_id = CommitId::parse(&input.branch_commit_id)?;
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(existing) = turn_association(
            pool,
            &thread.id,
            &input.assistant_message_id,
            &scope.repository_id,
        )
        .await?
        {
            if existing.branch_commit != branch_commit_id {
                return Err(AuthoredDocumentsError::Scope(
                    "assistant message is already finalized from another prepared tree".into(),
                ));
            }
            match existing.outcome {
                TurnOutcome::Prepared => {}
                TurnOutcome::Committed(main_commit) => {
                    let (operation_commit, _) =
                        self.store.read_commit(&scope.repository_id, &main_commit)?;
                    return Ok(AuthoredTurnCommit::Committed {
                        repository_id: scope.repository_id.to_string(),
                        commit_id: main_commit.to_string(),
                        applied_to_current_projection: main_commit == main.head,
                        changed: commit_changed(
                            &self.store,
                            &scope.repository_id,
                            &operation_commit,
                        )?,
                        document: main.document.projected(),
                    });
                }
                TurnOutcome::Conflicted(conflicts) => {
                    return Ok(AuthoredTurnCommit::Conflicted {
                        repository_id: scope.repository_id.to_string(),
                        branch_commit_id: branch_commit_id.to_string(),
                        conflicts,
                    });
                }
            }
        }
        let branch = self
            .ensure_thread_branch_locked(pool, &scope, &main.head)
            .await?;
        let captured = find_commit_with_trailers(
            &self.store,
            &scope.repository_id,
            &branch.branch_name,
            &[
                (TRAILER_THREAD, &thread.id),
                (TRAILER_MESSAGE, &input.assistant_message_id),
            ],
        )?
        .filter(|commit| commit.id == branch_commit_id)
        .ok_or_else(|| {
            AuthoredDocumentsError::Scope(
                "prepared turn commit does not belong to this thread and message".into(),
            )
        })?;
        let base = self
            .store
            .merge_base(&scope.repository_id, &main.head, &captured.id)?;
        let base_snapshot = self.snapshot_from_commit(&scope, &base)?;
        let ours_snapshot = AuthoredSnapshot {
            files: main.files.clone(),
            document: main.document.clone(),
        };
        let theirs_snapshot = self.snapshot_from_commit(&scope, &captured.id)?;
        let (candidate, files) = match self
            .merge_snapshots(
                pool,
                &scope,
                &base_snapshot,
                &ours_snapshot,
                &theirs_snapshot,
            )
            .await?
        {
            Ok(merged) => merged,
            Err(conflicts) => {
                write_turn_conflict(
                    pool,
                    &scope,
                    &thread.id,
                    &input.assistant_message_id,
                    &captured.id,
                    &conflicts,
                )
                .await?;
                return Ok(AuthoredTurnCommit::Conflicted {
                    repository_id: scope.repository_id.to_string(),
                    branch_commit_id: captured.id.to_string(),
                    conflicts,
                });
            }
        };
        let expected_projection_revision = main.document.revision().to_owned();
        let merge_message = commit_message(
            "Apply completed agent turn",
            &[
                (TRAILER_OPERATION, "agent_turn"),
                (TRAILER_THREAD, &thread.id),
                (TRAILER_MESSAGE, &input.assistant_message_id),
            ],
        )?;
        let prepared = self.store.prepare_commit(
            &scope.repository_id,
            &[main.head.clone(), captured.id.clone()],
            &files,
            &author_for_message(&assistant)?,
            &merge_message,
        )?;
        let projected = self
            .project_prepared(
                pool,
                &scope,
                &main.head,
                ProjectionLedgerExpectation::PresentAt(&main.head),
                &prepared,
                candidate,
                &expected_projection_revision,
                TrackProjectionAuthority::TrustedRepositoryTree,
                ProjectionMetadata {
                    turn: Some(TurnProjection {
                        thread_id: thread.id.clone(),
                        assistant_message_id: input.assistant_message_id.clone(),
                        branch_commit: captured.id.clone(),
                    }),
                    ..ProjectionMetadata::default()
                },
            )
            .await?;
        Ok(AuthoredTurnCommit::Committed {
            repository_id: scope.repository_id.to_string(),
            commit_id: prepared.id.to_string(),
            applied_to_current_projection: true,
            changed: projected.changed,
            document: projected.document,
        })
    }

    /// Complete prepared turns whose assistant transcript survived a crash.
    /// Durable messages are traversed in sequence order; SQL stores only the
    /// prepared commit identity, never an authored blob or duplicate tree.
    pub async fn recover_turns(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
    ) -> Result<Vec<AuthoredTurnCommit>> {
        // Each finalization acquires and revalidates the lifecycle gate. Do not
        // hold the non-reentrant repository lock across those nested calls.
        let thread = agent_threads::get_thread_row(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        let messages = agent_threads::list_messages(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let prepared_by_message = pending_turn_preparations(pool, &thread.id, &scope.repository_id)
            .await?
            .into_iter()
            .collect::<HashMap<_, _>>();
        let mut recovered = Vec::new();
        for message in messages
            .into_iter()
            .filter(|message| message.role == "assistant")
        {
            let Some(branch_commit) = prepared_by_message.get(&message.id) else {
                continue;
            };
            recovered.push(
                self.finalize_turn(
                    pool,
                    principal,
                    FinalizeAuthoredTurnInput {
                        thread_id: thread_id.to_owned(),
                        assistant_message_id: message.id,
                        branch_commit_id: branch_commit.to_string(),
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
        let limit = limit.unwrap_or(100);
        if limit == 0 || limit > MAX_HISTORY_PAGE {
            return Err(AuthoredDocumentsError::Invalid(format!(
                "history limit must be between 1 and {MAX_HISTORY_PAGE}"
            )));
        }
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        self.reconcile_locked(pool, &scope).await?;
        let mut scan_cursor = cursor.map(CommitId::parse).transpose()?;
        let mut entries = Vec::with_capacity(limit);
        let mut next_cursor = None;
        loop {
            // Git contains an infrastructure root commit and may gain other
            // non-user-facing commits over time. Page the visible history,
            // not the raw commit chain, so a hidden commit can never produce
            // an empty terminal page or consume a user's requested limit.
            let commits = self.store.first_parent_log_from(
                &scope.repository_id,
                MAIN_BRANCH,
                scan_cursor.as_ref(),
                MAX_HISTORY_PAGE + 1,
            )?;
            let continuation = commits
                .last()
                .and_then(|commit| commit.parents.first())
                .cloned();
            for commit in commits {
                let Some(entry) = history_entry(commit)? else {
                    continue;
                };
                if entries.len() == limit {
                    next_cursor = Some(entry.commit_id);
                    break;
                }
                entries.push(entry);
            }
            if next_cursor.is_some() {
                break;
            }
            let Some(continuation) = continuation else {
                break;
            };
            scan_cursor = Some(continuation);
        }
        Ok(AuthoredHistoryPage {
            entries,
            next_cursor,
        })
    }

    pub async fn restore(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        target_commit: &str,
        operation_id: &str,
    ) -> Result<AuthoredRestoreResult> {
        validate_ref_component(operation_id, "restore operation id")?;
        let (thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        let target = CommitId::parse(target_commit)?;
        let request_fingerprint = operation_request_fingerprint("restore", &[target.as_str()]);
        let main = self.reconcile_locked(pool, &scope).await?;
        if let Some(existing) =
            operation_association(pool, &scope.repository_id, "restore", operation_id).await?
        {
            if existing.request_fingerprint != request_fingerprint {
                return Err(AuthoredDocumentsError::Scope(
                    "restore operation id is already bound to a different target".into(),
                ));
            }
            let OperationOutcome::Committed(commit) = existing.outcome else {
                return Err(AuthoredDocumentsError::Storage(
                    "restore operation has a conflicted outcome".into(),
                ));
            };
            if !self.store.is_ancestor(
                &scope.repository_id,
                &existing.base_main_commit,
                &main.head,
            )? || !self
                .store
                .is_ancestor(&scope.repository_id, &commit, &main.head)?
            {
                return Err(AuthoredDocumentsError::Storage(
                    "restore operation is no longer in main history".into(),
                ));
            }
            return Ok(AuthoredRestoreResult {
                repository_id: scope.repository_id.to_string(),
                commit_id: commit.to_string(),
                applied_to_current_projection: commit == main.head,
                document: main.document.projected(),
            });
        }
        let selectable =
            self.store
                .first_parent_contains(&scope.repository_id, MAIN_BRANCH, &target)?
                && parse_commit_metadata(
                    &self
                        .store
                        .read_commit(&scope.repository_id, &target)?
                        .0
                        .message,
                )
                .is_some();
        if !selectable {
            return Err(AuthoredDocumentsError::Scope(
                "restore target is not in this document's main history".into(),
            ));
        }
        let snapshot = self.snapshot_from_commit(&scope, &target)?;
        let restored_files = self
            .files_for_document(pool, &scope, &snapshot.document, Some(&snapshot.files))
            .await?;
        let author = system_author()?;
        let message = commit_message(
            "Restore authored state",
            &[
                (TRAILER_OPERATION, "restore"),
                (TRAILER_THREAD, &thread.id),
                (TRAILER_RESTORE, target.as_str()),
                (TRAILER_OPERATION_ID, operation_id),
            ],
        )?;
        let prepared = self.store.prepare_commit(
            &scope.repository_id,
            std::slice::from_ref(&main.head),
            &restored_files,
            &author,
            &message,
        )?;
        let expected_projection_revision = main.document.revision().to_owned();
        let projected = self
            .project_prepared(
                pool,
                &scope,
                &main.head,
                ProjectionLedgerExpectation::PresentAt(&main.head),
                &prepared,
                snapshot.document,
                &expected_projection_revision,
                TrackProjectionAuthority::TrustedRepositoryTree,
                ProjectionMetadata {
                    operation: Some(OperationProjection {
                        kind: "restore",
                        operation_id: operation_id.to_owned(),
                        request_fingerprint,
                        base_main_commit: main.head.clone(),
                        result_json: None,
                    }),
                    ..ProjectionMetadata::default()
                },
            )
            .await?;
        Ok(AuthoredRestoreResult {
            repository_id: scope.repository_id.to_string(),
            commit_id: prepared.id.to_string(),
            applied_to_current_projection: true,
            document: projected.document,
        })
    }
}

async fn load_assistant_message(
    pool: &SqlitePool,
    thread: &AgentThread,
    principal: Option<&str>,
    message_id: &str,
) -> Result<AgentThreadMessage> {
    let messages = agent_threads::list_messages(pool, &thread.id, principal)
        .await
        .map_err(AuthoredDocumentsError::Scope)?;
    messages
        .into_iter()
        .find(|message| message.id == message_id && message.role == "assistant")
        .ok_or_else(|| {
            AuthoredDocumentsError::Scope(
                "assistant message does not exist in the durable thread".into(),
            )
        })
}

fn author_for_message(message: &AgentThreadMessage) -> Result<CommitAuthor> {
    let parsed = DateTime::parse_from_rfc3339(&message.created_at).map_err(|error| {
        AuthoredDocumentsError::Storage(format!(
            "assistant message has invalid creation time: {error}"
        ))
    })?;
    CommitAuthor::new(
        "Luma Agent",
        "agent@luma.local",
        parsed.timestamp(),
        parsed.offset().local_minus_utc() / 60,
    )
    .map_err(Into::into)
}

struct CommitMetadata {
    subject: String,
    operation: AuthoredOperationKind,
    thread_id: Option<String>,
    assistant_message_id: Option<String>,
}

fn parse_commit_metadata(message: &str) -> Option<CommitMetadata> {
    let subject = message.lines().next()?.trim().to_owned();
    let trailers = parse_trailers(message);
    let operation = match trailers.get(TRAILER_OPERATION)?.as_str() {
        "initial_import" => AuthoredOperationKind::InitialImport,
        "edit" => AuthoredOperationKind::Edit,
        "agent_turn" => AuthoredOperationKind::AgentTurn,
        "restore" => AuthoredOperationKind::Restore,
        "pattern_fork" => AuthoredOperationKind::PatternFork,
        "worktree_commit" => return None,
        "worktree_merge" => AuthoredOperationKind::WorktreeMerge,
        _ => return None,
    };
    Some(CommitMetadata {
        subject,
        operation,
        thread_id: trailers.get(TRAILER_THREAD).cloned(),
        assistant_message_id: trailers.get(TRAILER_MESSAGE).cloned(),
    })
}

fn history_entry(commit: CommitInfo) -> Result<Option<AuthoredHistoryEntry>> {
    let Some(metadata) = parse_commit_metadata(&commit.message) else {
        return Ok(None);
    };
    let offset = FixedOffset::east_opt(commit.author.offset_minutes * 60).ok_or_else(|| {
        AuthoredDocumentsError::Storage("Git commit has invalid timezone offset".into())
    })?;
    let authored = offset
        .timestamp_opt(commit.author.time_seconds, 0)
        .single()
        .ok_or_else(|| {
            AuthoredDocumentsError::Storage("Git commit has invalid timestamp".into())
        })?;
    Ok(Some(AuthoredHistoryEntry {
        commit_id: commit.id.to_string(),
        parent_ids: commit
            .parents
            .into_iter()
            .map(|id| id.to_string())
            .collect(),
        message: metadata.subject,
        authored_at: authored.to_rfc3339(),
        thread_id: metadata.thread_id,
        assistant_message_id: metadata.assistant_message_id,
        kind: metadata.operation,
    }))
}
