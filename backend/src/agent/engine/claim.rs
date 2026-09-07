//! Cross-device exclusion. Local file locking proves a source-device restart
//! is safe; the server rejects every other device until that source releases.

use super::AgentError;
use crate::{dispatch::SharedServices, sync::traits::RemoteClient};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) struct Claim {
    remote: Arc<dyn RemoteClient>,
    state_pool: sqlx::SqlitePool,
    payload: Value,
    pool: sqlx::SqlitePool,
    owner: String,
}

impl Claim {
    pub async fn acquire(
        services: &SharedServices,
        thread: &str,
        owner: Option<&str>,
    ) -> Result<Option<Self>, AgentError> {
        if services.fixture_principal.is_some() {
            return Ok(None);
        }
        let Some(owner) = owner else {
            return Ok(None);
        };
        let auth = crate::database::local::auth::get_current_auth(&services.state_db.0)
            .await
            .map_err(|e| AgentError::Storage(e.to_string()))?
            .ok_or_else(|| AgentError::Invalid("Sign into Luma before starting an agent".into()))?;
        if auth.principal.user_id != owner {
            return Err(AgentError::Invalid(
                "Luma account changed before execution".into(),
            ));
        }
        let device: String = sqlx::query_scalar(
            "SELECT device_id FROM authored_device_identity WHERE singleton = 1",
        )
        .fetch_one(&services.db().0)
        .await
        .map_err(|e| AgentError::Storage(e.to_string()))?;
        let run = uuid::Uuid::new_v4().to_string();
        let payload = json!({"p_thread_id":thread,"p_device_id":device,"p_run_id":run});
        let remote = Arc::clone(services.sync.remote());
        let claimed = remote
            .rpc_json("claim_agent_thread_run", &payload, &auth.access_token)
            .await
            .map_err(|e| AgentError::Storage(format!("could not claim agent execution: {e}")))?;
        if claimed != json!(true) {
            return Err(AgentError::Invalid("This conversation is running on another machine. Stop it there before continuing here.".into()));
        }
        let claim = Self {
            remote,
            state_pool: services.state_db.0.clone(),
            payload,
            pool: services.db().0.clone(),
            owner: owner.into(),
        };
        sqlx::query("INSERT INTO agent_thread_runs (owner_user_id, thread_id, device_id, run_id) VALUES (?, ?, ?, ?) ON CONFLICT(owner_user_id,thread_id) DO UPDATE SET device_id=excluded.device_id, run_id=excluded.run_id")
            .bind(owner).bind(thread).bind(device).bind(run).execute(&claim.pool).await.map_err(|e| AgentError::Storage(e.to_string()))?;
        Ok(Some(claim))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let (remote, state_pool, payload, pool, owner) = (
            Arc::clone(&self.remote),
            self.state_pool.clone(),
            self.payload.clone(),
            self.pool.clone(),
            self.owner.clone(),
        );
        tokio::spawn(async move {
            if let Err(error) = release(&pool, &state_pool, remote.as_ref(), &owner, &payload).await
            {
                eprintln!("[agent] execution cleanup deferred to sync: {error}");
            }
        });
    }
}

async fn release(
    pool: &sqlx::SqlitePool,
    state_pool: &sqlx::SqlitePool,
    remote: &dyn RemoteClient,
    owner: &str,
    payload: &Value,
) -> Result<(), AgentError> {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let auth = crate::database::local::auth::get_current_auth(state_pool)
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?
            .filter(|auth| auth.principal.user_id == owner)
            .ok_or_else(|| {
                AgentError::Invalid(
                    "Execution cleanup is waiting for its account to sign in".into(),
                )
            })?;
        remote
            .rpc_json("release_agent_thread_run", payload, &auth.access_token)
            .await
            .map_err(|error| AgentError::Storage(error.to_string()))?;
        clear_receipt(pool, owner, payload).await
    })
    .await
    .map_err(|_| AgentError::Storage("Execution cleanup timed out".into()))?
}

/// Retry durable cleanup receipts without touching a locally running turn.
/// A receipt survives network failures and process restarts.
pub(crate) async fn release_pending(
    pool: &sqlx::SqlitePool,
    state_pool: &sqlx::SqlitePool,
    remote: &dyn RemoteClient,
    root: &std::path::Path,
    owner: &str,
) -> Vec<String> {
    let rows: Vec<(String, String, String)> = match sqlx::query_as(
        "SELECT thread_id, device_id, run_id FROM agent_thread_runs WHERE owner_user_id = ?",
    )
    .bind(owner)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => return vec![format!("execution cleanup: {error}")],
    };
    let mut errors = Vec::new();
    for (thread, device, run) in rows {
        let Ok(_lease) = super::state::RunLease::acquire(root, &thread, Some(owner)) else {
            continue;
        };
        let payload = json!({"p_thread_id": thread, "p_device_id": device, "p_run_id": run});
        if let Err(error) = release(pool, state_pool, remote, owner, &payload).await {
            errors.push(format!("execution cleanup: {error}"));
            break;
        }
    }
    errors
}

async fn clear_receipt(
    pool: &sqlx::SqlitePool,
    owner: &str,
    payload: &Value,
) -> Result<(), AgentError> {
    sqlx::query(
        "DELETE FROM agent_thread_runs WHERE owner_user_id = ? AND thread_id = ? AND run_id = ?",
    )
    .bind(owner)
    .bind(payload["p_thread_id"].as_str())
    .bind(payload["p_run_id"].as_str())
    .execute(pool)
    .await
    .map_err(|e| AgentError::Storage(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::error::SyncError;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Remote {
        fail: AtomicBool,
        calls: std::sync::Mutex<Vec<String>>,
        called: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl RemoteClient for Remote {
        async fn rpc_json(
            &self,
            function: &str,
            _: &Value,
            token: &str,
        ) -> Result<Value, SyncError> {
            if function == "claim_agent_thread_run" {
                return Ok(json!(true));
            }
            assert_eq!(function, "release_agent_thread_run");
            let fail = self.fail.load(Ordering::SeqCst);
            self.calls.lock().unwrap().push(token.into());
            self.called.notify_one();
            if fail {
                Err(SyncError::Api {
                    status: 401,
                    message: "JWT expired".into(),
                })
            } else {
                Ok(json!(true))
            }
        }
        async fn select_json(&self, _: &str, _: &str, _: &str) -> Result<Vec<Value>, SyncError> {
            unreachable!()
        }
        async fn upsert_json(&self, _: &str, _: &Value, _: &str, _: &str) -> Result<(), SyncError> {
            unreachable!()
        }
        async fn patch_json(&self, _: &str, _: &str, _: &Value, _: &str) -> Result<(), SyncError> {
            unreachable!()
        }
        async fn upload_file(
            &self,
            _: &str,
            _: &str,
            _: Vec<u8>,
            _: &str,
            _: &str,
        ) -> Result<String, SyncError> {
            unreachable!()
        }
        async fn download_file(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, SyncError> {
            unreachable!()
        }
    }

    struct Fixture {
        root: tempfile::TempDir,
        pool: sqlx::SqlitePool,
        state: sqlx::SqlitePool,
        remote: Arc<Remote>,
    }

    impl Fixture {
        async fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let state = crate::database::local::state::init_state_db_at(root.path())
                .await
                .unwrap()
                .0;
            crate::database::local::auth::install_test_principal(&state, "owner")
                .await
                .unwrap();
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap();
            sqlx::query("CREATE TABLE agent_thread_runs (owner_user_id TEXT, thread_id TEXT, device_id TEXT, run_id TEXT); INSERT INTO agent_thread_runs VALUES ('owner','thread','device','run')")
                .execute(&pool).await.unwrap();
            Self {
                root,
                pool,
                state,
                remote: Arc::new(Remote::default()),
            }
        }
        fn claim(&self) -> Claim {
            Claim {
                remote: self.remote.clone(),
                state_pool: self.state.clone(),
                pool: self.pool.clone(),
                owner: "owner".into(),
                payload: json!({"p_thread_id":"thread","p_device_id":"device","p_run_id":"run"}),
            }
        }
        async fn count(&self) -> i64 {
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_thread_runs")
                .fetch_one(&self.pool)
                .await
                .unwrap()
        }
        async fn retry(&self) -> Vec<String> {
            release_pending(
                &self.pool,
                &self.state,
                self.remote.as_ref(),
                self.root.path(),
                "owner",
            )
            .await
        }
    }

    #[tokio::test]
    async fn cleanup_failure_is_detached_and_receipt_retries_afterward() {
        let fixture = Fixture::new().await;
        fixture.remote.fail.store(true, Ordering::SeqCst);
        let local_result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let _claim = fixture.claim();
            crate::agent::TurnOutcome::Completed
        })
        .await
        .unwrap();
        assert_eq!(local_result, crate::agent::TurnOutcome::Completed);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            fixture.remote.called.notified(),
        )
        .await
        .unwrap();
        assert_eq!(fixture.count().await, 1);
        fixture.remote.fail.store(false, Ordering::SeqCst);
        assert!(fixture.retry().await.is_empty());
        assert_eq!(fixture.count().await, 0);
    }

    #[tokio::test]
    async fn retry_never_releases_a_running_local_turn() {
        let fixture = Fixture::new().await;
        let lease =
            super::super::state::RunLease::acquire(fixture.root.path(), "thread", Some("owner"))
                .unwrap();
        assert!(fixture.retry().await.is_empty());
        assert!(fixture.remote.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.count().await, 1);
        drop(lease);
        assert!(fixture.retry().await.is_empty());
        assert_eq!(fixture.count().await, 0);
    }

    #[tokio::test]
    async fn cleanup_never_uses_another_accounts_token() {
        let fixture = Fixture::new().await;
        let mut claim = fixture.claim();
        claim.owner = "previous-account".into();
        assert!(release(
            &claim.pool,
            &claim.state_pool,
            claim.remote.as_ref(),
            &claim.owner,
            &claim.payload
        )
        .await
        .is_err());
        assert!(fixture.remote.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.count().await, 1);
        let current = crate::database::local::auth::get_current_auth(&fixture.state)
            .await
            .unwrap()
            .unwrap();
        assert!(fixture.retry().await.is_empty());
        assert_eq!(
            fixture.remote.calls.lock().unwrap().as_slice(),
            &[current.access_token]
        );
    }
    #[tokio::test]
    async fn successful_turn_stays_successful_when_cloud_release_fails() {
        use crate::agent::model::{scripted::ScriptedModel, ModelEvent, StopReason, Usage};
        use crate::agent::{AgentService, ThreadScope, TurnEvent, TurnOutcome};
        use futures_util::StreamExt;
        let fixture = Fixture::new().await;
        fixture.remote.fail.store(true, Ordering::SeqCst);
        let storage = crate::storage::StorageRoot::from_path(fixture.root.path().to_path_buf());
        let db = crate::database::local::database::init_app_db_at(storage.path())
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&db.0, Some("owner"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue','owner','Venue')")
            .execute(&db.0)
            .await
            .unwrap();
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python needed".into())),
            ),
        );
        let mut services = crate::dispatch::AppServices::headless(
            db,
            crate::database::local::state::StateDb(fixture.state.clone()),
            storage,
            fixture.root.path().join("fixtures"),
            workspaces,
        );
        services.sync = crate::sync::orchestrator::SyncEngine::new(
            services.db.0.clone(),
            fixture.state.clone(),
            fixture.remote.clone(),
            services.authored.clone(),
        );
        let services = services.into_shared();
        let agent =
            AgentService::new(services.clone()).with_model(Arc::new(ScriptedModel::new(vec![
                vec![
                    ModelEvent::TextDelta("Finished locally".into()),
                    ModelEvent::StepEnded {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage::default(),
                    },
                ],
            ])));
        let thread = agent
            .new_thread(&ThreadScope::venue("venue"))
            .await
            .unwrap()
            .thread;
        let events: Vec<_> = agent
            .turn(&thread.id, "finish".to_string().into())
            .collect()
            .await;
        assert_eq!(
            events.last(),
            Some(&TurnEvent::TurnEnded {
                outcome: TurnOutcome::Completed
            }),
            "{events:?}"
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture.remote.called.notified(),
        )
        .await
        .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_thread_runs WHERE thread_id = ?")
                .bind(&thread.id)
                .fetch_one(&services.db.0)
                .await
                .unwrap();
        assert_eq!(count, 1, "failed cleanup must remain retryable");
        let saved = agent.open_thread(&thread.id).await.unwrap();
        assert_eq!(saved.messages.len(), 2, "local result must be durable");
    }
}
