use std::future::Future;

use sqlx::SqlitePool;

use crate::models::agent_threads::CreateAgentThreadInput;

use super::{
    agent_threads, deterministic_creation_id, ensure_thread_owned, normalized_creation_request_id,
    principal_key, AgentThread, AuthoredDocumentGuard, AuthoredDocumentId, AuthoredDocuments,
    AuthoredDocumentsError, ResolvedScope, Result,
};

pub(super) struct ActiveThreadLockRequest {
    thread_id: String,
    document_id: AuthoredDocumentId,
}

impl AuthoredDocuments {
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

    pub(super) async fn resolve_active_thread_lock(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
    ) -> Result<ActiveThreadLockRequest> {
        let thread = agent_threads::get_thread_row(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        Ok(ActiveThreadLockRequest {
            thread_id: thread_id.to_owned(),
            document_id: scope.document_id,
        })
    }

    pub(super) async fn acquire_active_thread_lock(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        request: ActiveThreadLockRequest,
    ) -> Result<(AgentThread, ResolvedScope, AuthoredDocumentGuard)> {
        let guard = self.document_guard(&request.document_id).await;
        let thread = agent_threads::get_thread_row(pool, &request.thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        if scope.document_id != request.document_id {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread authored scope changed while acquiring its document lock".into(),
            ));
        }
        Ok((thread, scope, guard))
    }

    pub async fn create_thread_with_authored_state(
        &self,
        pool: &SqlitePool,
        input: CreateAgentThreadInput,
        principal: Option<&str>,
    ) -> Result<AgentThread> {
        input
            .authored_route()
            .map_err(AuthoredDocumentsError::Invalid)?;
        let request_id = normalized_creation_request_id(&input.request_id)?;
        let principal_key = principal_key(principal);
        let thread_id =
            deterministic_creation_id(&principal_key, "agent_thread", &request_id, "subject");
        if let Some(existing) =
            agent_threads::find_thread_row_including_deleting(pool, &thread_id, principal)
                .await
                .map_err(AuthoredDocumentsError::Storage)?
        {
            verify_agent_thread_creation_scope(&existing, &input, principal)?;
            if agent_threads::get_thread_row(pool, &thread_id, principal)
                .await
                .is_err()
            {
                return Err(AuthoredDocumentsError::Scope(
                    "agent thread creation was already applied and deletion is terminal".into(),
                ));
            }
            return Ok(existing);
        }
        if agent_threads::find_thread_deletion_receipt(pool, &thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Storage)?
            .is_some()
        {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread creation was already applied and later deleted".into(),
            ));
        }

        let provisional = provisional_scope(&input, principal)?;
        let _guard = self.document_guard(&provisional.document_id).await;
        // Import a legacy live document before binding the thread. A failed
        // thread insert may leave legitimate authored history, never a partial
        // conversation or a second authority.
        self.load_current_locked(pool, &provisional).await?;
        match agent_threads::create_thread_with_id(pool, &thread_id, input.clone(), principal).await
        {
            Ok(thread) => Ok(thread),
            Err(error) => {
                let existing =
                    agent_threads::find_thread_row_including_deleting(pool, &thread_id, principal)
                        .await
                        .map_err(|load| {
                            AuthoredDocumentsError::Storage(format!(
                                "create deterministic agent thread: {error}; reload failed: {load}"
                            ))
                        })?
                        .ok_or_else(|| {
                            AuthoredDocumentsError::Storage(format!(
                                "create deterministic agent thread: {error}"
                            ))
                        })?;
                verify_agent_thread_creation_scope(&existing, &input, principal)?;
                Ok(existing)
            }
        }
    }

    pub async fn delete_thread_with_authored_state<Cleanup, CleanupFuture>(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        cleanup: Cleanup,
    ) -> Result<Option<AgentThread>>
    where
        Cleanup: FnOnce(Vec<String>) -> CleanupFuture,
        CleanupFuture: Future<Output = std::result::Result<(), String>>,
    {
        let Some(thread) =
            agent_threads::find_thread_row_including_deleting(pool, thread_id, principal)
                .await
                .map_err(AuthoredDocumentsError::Scope)?
        else {
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
        };
        ensure_thread_owned(&thread, principal)?;
        let scope = ResolvedScope::from_thread(&thread, principal)?;
        {
            let _guard = self.document_guard(&scope.document_id).await;
            agent_threads::mark_thread_deleting(pool, thread_id, principal)
                .await
                .map_err(AuthoredDocumentsError::Scope)?;
        }

        let workspace_ids = self
            .thread_workspace_ids_for_cleanup(pool, &scope, thread_id)
            .await?;

        cleanup(workspace_ids).await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!(
                "clean up agent thread resources before deletion: {error}"
            ))
        })?;

        let _guard = self.document_guard(&scope.document_id).await;
        agent_threads::mark_thread_deleting(pool, thread_id, principal)
            .await
            .map_err(AuthoredDocumentsError::Scope)?;
        self.retire_thread_workspaces_locked(pool, &scope, thread_id)
            .await?;
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("begin authored thread deletion: {error}"))
        })?;
        let inserted_receipt = agent_threads::insert_thread_deletion_receipt(
            &mut transaction,
            thread_id,
            principal,
            scope.document_id.as_str(),
        )
        .await
        .map_err(AuthoredDocumentsError::Storage)?;
        if !inserted_receipt {
            // An existing exact receipt was hydrated from the server. Its
            // terminal fact supersedes any locally dirty mutable thread
            // snapshot, which must not be replayed after final cleanup.
            sqlx::query(
                "DELETE FROM pending_ops
                 WHERE principal_key = ? AND table_name = 'agent_threads'
                   AND record_id = ? AND op_type = 'upsert_explicit'",
            )
            .bind(&scope.principal_key)
            .bind(thread_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!(
                    "discard terminal agent thread projection: {error}"
                ))
            })?;
        }
        let deleted = sqlx::query(
            "DELETE FROM agent_threads
             WHERE id = ? AND owner_user_id IS ? AND lifecycle_state = 'deleting'",
        )
        .bind(thread_id)
        .bind(principal)
        .execute(&mut *transaction)
        .await
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
}

fn provisional_scope(
    input: &CreateAgentThreadInput,
    principal: Option<&str>,
) -> Result<ResolvedScope> {
    match input
        .authored_route()
        .map_err(AuthoredDocumentsError::Invalid)?
    {
        crate::models::agent_threads::AuthoredThreadRoute::Track {
            track_id,
            venue_id,
            score_id,
        } => ResolvedScope::track(
            principal,
            super::TrackScope {
                score_id: score_id.to_owned(),
                track_id: track_id.to_owned(),
                venue_id: venue_id.to_owned(),
            },
        ),
        crate::models::agent_threads::AuthoredThreadRoute::Pattern {
            pattern_id,
            implementation_id,
        } => ResolvedScope::pattern(principal, pattern_id, implementation_id),
    }
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
            "agent thread request id was already used with another authored scope".into(),
        ));
    }
    Ok(())
}
