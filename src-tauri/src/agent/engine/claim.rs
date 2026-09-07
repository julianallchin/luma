//! Cross-device exclusion. Local file locking proves a source-device restart
//! is safe; the server rejects every other device until that source releases.

use super::AgentError;
use crate::{dispatch::SharedServices, sync::traits::RemoteClient};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) struct Claim {
    remote: Arc<dyn RemoteClient>,
    token: String,
    payload: Value,
    pool: sqlx::SqlitePool,
    owner: String,
    released: bool,
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
            token: auth.access_token,
            payload,
            pool: services.db().0.clone(),
            owner: owner.into(),
            released: false,
        };
        sqlx::query("INSERT INTO agent_thread_runs (owner_user_id, thread_id, device_id, run_id) VALUES (?, ?, ?, ?) ON CONFLICT(owner_user_id,thread_id) DO UPDATE SET device_id=excluded.device_id, run_id=excluded.run_id")
            .bind(owner).bind(thread).bind(device).bind(run).execute(&claim.pool).await.map_err(|e| AgentError::Storage(e.to_string()))?;
        Ok(Some(claim))
    }
    pub async fn release(mut self) -> Result<(), AgentError> {
        self.remote
            .rpc_json("release_agent_thread_run", &self.payload, &self.token)
            .await
            .map_err(|e| AgentError::Storage(format!("could not release execution claim: {e}")))?;
        self.released = true;
        clear_receipt(&self.pool, &self.owner, &self.payload).await
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let (remote, token, payload, pool, owner) = (
            Arc::clone(&self.remote),
            self.token.clone(),
            self.payload.clone(),
            self.pool.clone(),
            self.owner.clone(),
        );
        tokio::spawn(async move {
            if let Err(error) = remote
                .rpc_json("release_agent_thread_run", &payload, &token)
                .await
            {
                eprintln!("[agent] execution claim remains on this device: {error}");
                return;
            }
            if let Err(error) = clear_receipt(&pool, &owner, &payload).await {
                eprintln!("[agent] could not clear execution receipt: {error}");
            }
        });
    }
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
