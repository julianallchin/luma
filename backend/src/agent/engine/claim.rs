//! The local record of which device is running a thread.
//!
//! Cross-device exclusion was a server RPC: a second machine was refused while
//! the first held the claim. With no write server there is nobody to ask, so
//! this keeps only the local half — the `agent_thread_runs` row that says this
//! device is running the thread, cleared when the turn ends. Phase two restores
//! the refusal on top of it.

use super::AgentError;
use crate::dispatch::SharedServices;

/// This process's identity for the run it holds. A restart is a new device as
/// far as the row is concerned, which is exactly right: the old run is gone.
fn device_id() -> &'static str {
    static DEVICE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DEVICE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

pub(crate) struct Claim {
    pool: sqlx::SqlitePool,
    owner: String,
    thread: String,
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
        let run = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO agent_thread_runs (uid, thread_id, device_id, run_id)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(uid, thread_id) DO UPDATE
                SET device_id = excluded.device_id, run_id = excluded.run_id",
        )
        .bind(owner)
        .bind(thread)
        .bind(device_id())
        .bind(&run)
        .execute(&services.db().0)
        .await
        .map_err(|error| AgentError::Storage(error.to_string()))?;
        Ok(Some(Self {
            pool: services.db().0.clone(),
            owner: owner.into(),
            thread: thread.into(),
        }))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let (pool, owner, thread) = (self.pool.clone(), self.owner.clone(), self.thread.clone());
        tokio::spawn(async move {
            if let Err(error) =
                sqlx::query("DELETE FROM agent_thread_runs WHERE uid = ? AND thread_id = ?")
                    .bind(&owner)
                    .bind(&thread)
                    .execute(&pool)
                    .await
            {
                eprintln!("[agent] could not clear the execution claim: {error}");
            }
        });
    }
}
