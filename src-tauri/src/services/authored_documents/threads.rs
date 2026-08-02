use super::{
    agent_threads, deterministic_creation_id, ensure_thread_owned, insert_creation_association,
    load_creation_association, normalized_creation_request_id, operation_request_fingerprint,
    principal_key, verify_creation_replay, AgentThread, Arc, AuthoredDocumentGuard,
    AuthoredDocuments, AuthoredDocumentsError, AuthoredRepositoryId, AuthoredStateError, CommitId,
    CreateAgentThreadInput, FromRow, Future, OwnedRwLockReadGuard, ResolvedScope, Result,
    SqlitePool,
};

pub(super) struct ActiveThreadLockRequest {
    thread_id: String,
    repository_id: AuthoredRepositoryId,
}

impl AuthoredDocuments {
    /// Resolve a thread, enter its repository critical section, then verify
    /// that deletion did not begin while the operation was waiting. Deletion
    /// takes the same lock before its durable `active -> deleting` transition,
    /// so an operation either completes before cleanup or is rejected before
    /// touching Git or live authored state.
    pub(super) async fn lock_active_thread(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
    ) -> Result<(AgentThread, ResolvedScope, AuthoredDocumentGuard)> {
        let request = self
            .resolve_active_thread_lock(pool, principal, thread_id)
            .await?;
        self.acquire_active_thread_lock(pool, principal, request)
            .await
    }

    /// Commit one synchronous external effect while deletion's repository gate
    /// still proves the owning thread is active. The effect either finishes
    /// before `active -> deleting`, or never runs after that transition; callers
    /// cannot accidentally release the guard between the final lifecycle check
    /// and the state they publish.
    pub(crate) async fn fence_active_thread_effect<T, Effect>(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        effect: Effect,
    ) -> Result<T>
    where
        Effect: FnOnce(&AgentThread) -> std::result::Result<T, String>,
    {
        let (thread, _scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        effect(&thread).map_err(AuthoredDocumentsError::Scope)
    }

    /// Capture the optimistic active-thread identity before waiting for the
    /// repository. Acquisition below revalidates it under the lock, so a
    /// deletion that wins the wait remains terminal.
    pub(super) async fn resolve_active_thread_lock(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
    ) -> Result<ActiveThreadLockRequest> {
        let initial = agent_threads::get_thread_row(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let initial_scope = ResolvedScope::from_thread(&initial, principal)?;
        Ok(ActiveThreadLockRequest {
            thread_id: thread_id.to_string(),
            repository_id: initial_scope.repository_id,
        })
    }

    pub(super) async fn acquire_active_thread_lock(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        request: ActiveThreadLockRequest,
    ) -> Result<(AgentThread, ResolvedScope, AuthoredDocumentGuard)> {
        let guard = self.repository_guard(&request.repository_id).await;
        let thread = self
            .assert_active_thread_locked(
                pool,
                principal,
                &request.thread_id,
                &request.repository_id,
            )
            .await?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        Ok((thread, scope, guard))
    }

    pub(super) async fn assert_active_thread_locked(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        expected_repository: &AuthoredRepositoryId,
    ) -> Result<AgentThread> {
        let thread = agent_threads::get_thread_row(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        if &scope.repository_id != expected_repository {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread authored scope changed while acquiring its repository lock".into(),
            ));
        }
        Ok(thread)
    }

    /// Create the durable transcript row and its authored branch through one
    /// shared lifecycle path. A failed initialization compensates both SQL
    /// routing and an unchanged Git branch; a crash at any boundary is
    /// recoverable because the branch name is deterministic.
    pub async fn create_thread_with_authored_state(
        &self,
        pool: &SqlitePool,
        input: CreateAgentThreadInput,
        principal: Option<&str>,
    ) -> Result<AgentThread> {
        input
            .authored_route()
            .map_err(AuthoredDocumentsError::Invalid)?;
        // Thread identity is authored routing. Enter the same lifecycle gate
        // as every repository operation before the first SQL insert, so an
        // account switch cannot split row ownership from branch ownership.
        let lifecycle = Arc::clone(&self.lifecycle_lock).read_owned().await;
        let request_id = normalized_creation_request_id(&input.request_id)?;
        let principal_key = principal_key(principal);
        let request_fingerprint = agent_thread_creation_fingerprint(&input);
        let thread_id =
            deterministic_creation_id(&principal_key, "agent_thread", &request_id, "subject");
        if let Some(existing) =
            load_creation_association(pool, &principal_key, "agent_thread", &request_id).await?
        {
            verify_creation_replay(&existing, &request_fingerprint, &thread_id, None)?;
            let thread = agent_threads::get_thread_row(pool, &thread_id, principal)
                .await
                .map_err(|_| {
                    AuthoredDocumentsError::Scope(format!(
                        "agent thread creation request was already applied, but thread {thread_id} was later deleted; refusing to recreate it"
                    ))
            })?;
            verify_agent_thread_creation_scope(&thread, &input, principal)?;
            self.ensure_thread_branch_inside_lifecycle(pool, &thread, principal, &lifecycle)
                .await?;
            let scope = ResolvedScope::from_thread(&thread, principal)?;
            let branch = format!("agents/threads/{}", thread.id);
            let branch_head = self.store.branch_head(&scope.repository_id, &branch)?;
            if !self
                .store
                .is_ancestor(&scope.repository_id, &existing.commit_id, &branch_head)?
            {
                return Err(AuthoredDocumentsError::Storage(
                    "agent thread creation outcome is not in its authored branch history".into(),
                ));
            }
            return Ok(thread);
        }

        let (thread, created_now) = match agent_threads::find_thread_row_including_deleting(
            pool, &thread_id, principal,
        )
        .await
        .map_err(AuthoredDocumentsError::Storage)?
        {
            Some(thread) => (thread, false),
            None => match agent_threads::create_thread_with_id(
                pool,
                &thread_id,
                input.clone(),
                principal,
            )
            .await
            {
                Ok(thread) => (thread, true),
                Err(insert_error) => {
                    let concurrent = agent_threads::find_thread_row_including_deleting(
                            pool, &thread_id, principal,
                        )
                        .await
                        .map_err(|load_error| {
                            AuthoredDocumentsError::Storage(format!(
                                "create deterministic agent thread: {insert_error}; reload failed: {load_error}"
                            ))
                        })?
                        .ok_or_else(|| {
                            AuthoredDocumentsError::Storage(format!(
                                "create deterministic agent thread: {insert_error}"
                            ))
                        })?;
                    (concurrent, false)
                }
            },
        };
        verify_agent_thread_recovery_row(&thread, &input, principal)?;
        if let Err(initialization) = self
            .ensure_thread_branch_inside_lifecycle(pool, &thread, principal, &lifecycle)
            .await
        {
            if !created_now {
                return Err(initialization);
            }
            let cleanup = self
                .compensate_failed_thread_create_inside_lifecycle(
                    pool, &thread, principal, &lifecycle,
                )
                .await;
            return Err(match cleanup {
                Ok(()) => initialization,
                Err(cleanup) => AuthoredDocumentsError::Storage(format!(
                    "initialize authored thread: {initialization}; compensate failed create: {cleanup}"
                )),
            });
        }
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        let branch = format!("agents/threads/{}", thread.id);
        let branch_head = self.store.branch_head(&scope.repository_id, &branch)?;
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "begin agent thread creation association: {error}"
            ))
        })?;
        let inserted = insert_creation_association(
            &mut transaction,
            &principal_key,
            "agent_thread",
            &request_id,
            &request_fingerprint,
            &thread_id,
            None,
            &branch_head,
        )
        .await;
        if let Err(error) = inserted {
            transaction.rollback().await.map_err(|rollback| {
                AuthoredDocumentsError::Storage(format!(
                    "record agent thread creation: {error}; rollback failed: {rollback}"
                ))
            })?;
            let existing =
                load_creation_association(pool, &principal_key, "agent_thread", &request_id)
                    .await?
                    .ok_or(error)?;
            verify_creation_replay(&existing, &request_fingerprint, &thread_id, None)?;
            return Ok(thread);
        }
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "commit agent thread creation association: {error}"
            ))
        })?;
        Ok(thread)
    }

    /// Close the durable lifecycle gate under the repository lock, release that
    /// lock while the caller cancels and drains execution-owned resources, then
    /// reacquire it for worktree retirement and identity removal. Never holding
    /// the repository lock while waiting on a kernel is what lets a running
    /// `track.apply` finish or reject without lock inversion.
    ///
    /// `deleting` is terminal and retryable: after any failure, the same call
    /// resumes cleanup while normal thread operations stay closed. The thread
    /// branch itself remains durable Git history.
    pub async fn delete_thread_with_authored_state<Cleanup, CleanupFuture>(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        cleanup: Cleanup,
    ) -> Result<Option<AgentThread>>
    where
        Cleanup: FnOnce() -> CleanupFuture,
        CleanupFuture: Future<Output = std::result::Result<(), String>>,
    {
        let thread =
            match agent_threads::find_thread_row_including_deleting(pool, thread_id, principal)
                .await
                .map_err(AuthoredDocumentsError::Scope)?
            {
                Some(thread) => thread,
                None => {
                    if agent_threads::find_thread_deletion_receipt(pool, thread_id, principal)
                        .await
                        .map_err(AuthoredDocumentsError::Storage)?
                        .is_some()
                    {
                        return Ok(None);
                    }
                    return Err(AuthoredDocumentsError::Scope(format!(
                        "Agent thread not found: {thread_id}"
                    )));
                }
            };
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        {
            let _guard = self.repository_guard(&scope.repository_id).await;
            let locked =
                match agent_threads::find_thread_row_including_deleting(pool, thread_id, principal)
                    .await
                    .map_err(AuthoredDocumentsError::Scope)?
                {
                    Some(thread) => thread,
                    None => {
                        let receipt =
                            agent_threads::find_thread_deletion_receipt(pool, thread_id, principal)
                                .await
                                .map_err(AuthoredDocumentsError::Storage)?;
                        if receipt.as_deref() == Some(scope.repository_id.as_str()) {
                            return Ok(None);
                        }
                        return Err(AuthoredDocumentsError::Scope(format!(
                            "Agent thread {thread_id} disappeared while beginning deletion"
                        )));
                    }
                };
            let locked_scope = ResolvedScope::from_thread(&locked, principal)?;
            if locked_scope.repository_id != scope.repository_id {
                return Err(AuthoredDocumentsError::Scope(
                    "agent thread authored scope changed while beginning deletion".into(),
                ));
            }
            agent_threads::mark_thread_deleting(pool, thread_id, principal)
                .await
                .map_err(AuthoredDocumentsError::Scope)?;
        }

        // No repository lock is held here. The callback closes cell admission,
        // cancels every starting/running execution, and does not return until
        // those leases (and therefore all kernel/host-call locks) are gone.
        cleanup().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "clean up agent thread resources before deletion: {error}"
            ))
        })?;

        let _guard = self.repository_guard(&scope.repository_id).await;
        let locked =
            match agent_threads::find_thread_row_including_deleting(pool, thread_id, principal)
                .await
                .map_err(AuthoredDocumentsError::Scope)?
            {
                Some(thread) => thread,
                None => {
                    let receipt =
                        agent_threads::find_thread_deletion_receipt(pool, thread_id, principal)
                            .await
                            .map_err(AuthoredDocumentsError::Storage)?;
                    if receipt.as_deref() == Some(scope.repository_id.as_str()) {
                        return Ok(None);
                    }
                    return Err(AuthoredDocumentsError::Scope(format!(
                        "Agent thread {thread_id} disappeared while finishing deletion"
                    )));
                }
            };
        let locked_scope = ResolvedScope::from_thread(&locked, principal)?;
        if locked_scope.repository_id != scope.repository_id {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread authored scope changed while finishing deletion".into(),
            ));
        }
        // Reassert the terminal state so recovery remains fail-closed even if a
        // damaged database was externally changed between the two phases.
        agent_threads::mark_thread_deleting(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        self.retire_thread_worktrees_locked(pool, &scope, thread_id)
            .await?;
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin authored thread deletion: {error}"))
        })?;
        agent_threads::insert_thread_deletion_receipt(
            &mut transaction,
            thread_id,
            principal,
            scope.repository_id.as_str(),
        )
        .await
        .map_err(AuthoredDocumentsError::Storage)?;
        sqlx::query(
            "DELETE FROM authored_state_thread_branches
             WHERE thread_id = ? AND repository_id = ?",
        )
        .bind(thread_id)
        .bind(scope.repository_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("remove authored thread routing: {error}"))
        })?;
        let deleted = match principal {
            Some(principal) => {
                sqlx::query(
                    "DELETE FROM agent_threads
                     WHERE id = ? AND owner_user_id = ? AND lifecycle_state = 'deleting'",
                )
                .bind(thread_id)
                .bind(principal)
                .execute(&mut *transaction)
                .await
            }
            None => {
                sqlx::query(
                    "DELETE FROM agent_threads
                     WHERE id = ? AND owner_user_id IS NULL AND lifecycle_state = 'deleting'",
                )
                .bind(thread_id)
                .execute(&mut *transaction)
                .await
            }
        }
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("delete authored thread: {error}"))
        })?;
        if deleted.rows_affected() != 1 {
            return Err(AuthoredDocumentsError::Scope(format!(
                "thread {thread_id} disappeared during deletion"
            )));
        }
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("commit authored thread deletion: {error}"))
        })?;
        Ok(Some(thread))
    }

    /// Establish the exact `agents/threads/<thread-id>` branch as part
    /// of durable thread creation. Repeating it is safe after a crash.
    async fn ensure_thread_branch_inside_lifecycle(
        &self,
        pool: &SqlitePool,
        thread: &AgentThread,
        principal: Option<&str>,
        _lifecycle: &OwnedRwLockReadGuard<()>,
    ) -> Result<()> {
        ensure_thread_owned(thread, principal)?;
        let scope = ResolvedScope::from_thread(thread, principal)?;
        let _repository = self
            .repository_guard_inside_lifecycle(&scope.repository_id)
            .await;
        agent_threads::get_thread_row(pool, &thread.id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let main = self.reconcile_locked(pool, &scope).await?;
        self.ensure_thread_branch_locked(pool, &scope, &main.head)
            .await?;
        Ok(())
    }

    async fn compensate_failed_thread_create_inside_lifecycle(
        &self,
        pool: &SqlitePool,
        thread: &AgentThread,
        principal: Option<&str>,
        _lifecycle: &OwnedRwLockReadGuard<()>,
    ) -> Result<()> {
        ensure_thread_owned(thread, principal)?;
        let scope = ResolvedScope::from_thread(thread, principal)?;
        let _repository = self
            .repository_guard_inside_lifecycle(&scope.repository_id)
            .await;
        let branch = format!("agents/threads/{}", thread.id);
        match self.store.branch_head(&scope.repository_id, &branch) {
            Ok(head) => {
                let main = self.store.main_head(&scope.repository_id)?;
                if head != main {
                    return Err(AuthoredDocumentsError::Storage(
                        "failed thread branch advanced before create compensation".into(),
                    ));
                }
                self.store
                    .delete_branch_at(&scope.repository_id, &branch, &head)?;
            }
            Err(AuthoredStateError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin failed thread cleanup: {error}"))
        })?;
        sqlx::query(
            "DELETE FROM authored_state_thread_branches
             WHERE thread_id = ? AND repository_id = ?",
        )
        .bind(&thread.id)
        .bind(scope.repository_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("remove failed thread routing: {error}"))
        })?;
        let deleted = match principal {
            Some(principal) => {
                sqlx::query("DELETE FROM agent_threads WHERE id = ? AND owner_user_id = ?")
                    .bind(&thread.id)
                    .bind(principal)
                    .execute(&mut *transaction)
                    .await
            }
            None => {
                sqlx::query("DELETE FROM agent_threads WHERE id = ? AND owner_user_id IS NULL")
                    .bind(&thread.id)
                    .execute(&mut *transaction)
                    .await
            }
        }
        .map_err(|error| {
            AuthoredDocumentsError::Storage(format!("remove failed thread: {error}"))
        })?;
        if deleted.rows_affected() != 1 {
            return Err(AuthoredDocumentsError::Storage(
                "failed thread disappeared during create compensation".into(),
            ));
        }
        transaction.commit().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("commit failed thread cleanup: {error}"))
        })?;
        Ok(())
    }

    pub(super) async fn ensure_thread_branch_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main_head: &CommitId,
    ) -> Result<ThreadBranchRow> {
        let expected = format!(
            "agents/threads/{}",
            scope.thread_id.as_deref().ok_or_else(|| {
                AuthoredDocumentsError::Invalid(
                    "thread branch requested for a non-thread scope".into(),
                )
            })?
        );
        if let Some(row) = sqlx::query_as::<_, ThreadBranchRow>(
            "SELECT repository_id, branch_name
             FROM authored_state_thread_branches WHERE thread_id = ?",
        )
        .bind(scope.thread_id.as_deref())
        .fetch_optional(pool)
        .await
        .map_err(|error| AuthoredDocumentsError::Storage(format!("load thread branch: {error}")))?
        {
            if row.repository_id != scope.repository_id.as_str() || row.branch_name != expected {
                return Err(AuthoredDocumentsError::Scope(
                    "thread branch metadata does not match its durable scope".into(),
                ));
            }
            self.store
                .branch_head(&scope.repository_id, &row.branch_name)?;
            return Ok(row);
        }

        match self.store.branch_head(&scope.repository_id, &expected) {
            Ok(_) => {
                // Crash recovery: branch publication may have succeeded before
                // its routing row was inserted. Its deterministic name is
                // scoped to this trusted thread/repository, so adopt it.
            }
            Err(AuthoredStateError::NotFound(_)) => {
                self.store
                    .create_branch(&scope.repository_id, &expected, main_head)?;
            }
            Err(error) => return Err(error.into()),
        }
        let inserted = sqlx::query(
            "INSERT INTO authored_state_thread_branches (thread_id, repository_id, branch_name)
             VALUES (?, ?, ?)",
        )
        .bind(scope.thread_id.as_deref())
        .bind(scope.repository_id.as_str())
        .bind(&expected)
        .execute(pool)
        .await;
        if let Err(error) = inserted {
            // A concurrent creator may have won. Accept only the exact same
            // server-derived mapping.
            let row = sqlx::query_as::<_, ThreadBranchRow>(
                "SELECT repository_id, branch_name
                 FROM authored_state_thread_branches WHERE thread_id = ?",
            )
            .bind(scope.thread_id.as_deref())
            .fetch_optional(pool)
            .await
            .map_err(|load| {
                AuthoredDocumentsError::Storage(format!(
                    "record thread branch: {error}; reload failed: {load}"
                ))
            })?;
            if let Some(row) = row {
                if row.repository_id == scope.repository_id.as_str() && row.branch_name == expected
                {
                    return Ok(row);
                }
            }
            return Err(AuthoredDocumentsError::Storage(format!(
                "record thread branch: {error}"
            )));
        }
        Ok(ThreadBranchRow {
            repository_id: scope.repository_id.to_string(),
            branch_name: expected,
        })
    }
}

#[derive(FromRow)]
pub(super) struct ThreadBranchRow {
    pub(super) repository_id: String,
    pub(super) branch_name: String,
}

fn agent_thread_creation_fingerprint(input: &CreateAgentThreadInput) -> String {
    let value = serde_json::json!({
        "agentKind": input.agent_kind,
        "subjectKind": input.subject_kind,
        "subjectId": input.subject_id,
        "implementationId": input.implementation_id,
        "venueId": input.venue_id,
        "scoreId": input.score_id,
        "title": input.title,
    });
    let canonical = crate::canonical_json::to_string(&value);
    operation_request_fingerprint("create_agent_thread", &[&canonical])
}

fn verify_agent_thread_creation_scope(
    thread: &AgentThread,
    input: &CreateAgentThreadInput,
    principal: Option<&str>,
) -> Result<()> {
    if thread.owner_user_id.as_deref() != principal
        || thread.agent_kind != input.agent_kind
        || thread.subject_kind != input.subject_kind
        || thread.subject_id != input.subject_id
        || thread.implementation_id != input.implementation_id
        || thread.venue_id != input.venue_id
        || thread.score_id != input.score_id
    {
        return Err(AuthoredDocumentsError::Invalid(
            "agent thread creation request_id was already used with another scope".into(),
        ));
    }
    Ok(())
}

fn verify_agent_thread_recovery_row(
    thread: &AgentThread,
    input: &CreateAgentThreadInput,
    principal: Option<&str>,
) -> Result<()> {
    verify_agent_thread_creation_scope(thread, input, principal)?;
    if thread.title != input.title {
        return Err(AuthoredDocumentsError::Invalid(
            "agent thread creation request_id was already used with another title".into(),
        ));
    }
    Ok(())
}
