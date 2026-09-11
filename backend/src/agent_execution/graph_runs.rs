//! The latest graph evaluation per agent thread (design §11.2).
//!
//! `run_graph` produces far more than the editor draws; the agent's `luma.graph.run`
//! branch wants exactly that surplus. Rather than push the evaluation through the
//! frontend and back (dense float buffers the UI has no business shipping), the
//! command parks it here under the thread id the caller named, and the next cell's
//! binding assembly picks it up.
//!
//! One slot per execution: a parent uses its thread id while each detached child
//! uses its workspace id. Each execution only ever looks at its most recent run, and the
//! provider re-checks compatibility with the *current* scope before publishing it,
//! so a stale entry is inert rather than misleading.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;

use crate::eval::graph_run::GraphEvaluation;
use crate::models::agent_threads::AgentThread;

#[derive(Default)]
pub struct GraphRunStore {
    runs: Mutex<HashMap<String, Arc<GraphEvaluation>>>,
}

impl GraphRunStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn publish_unchecked(&self, execution_id: &str, evaluation: Arc<GraphEvaluation>) {
        self.runs
            .lock()
            .unwrap()
            .insert(execution_id.to_string(), evaluation);
    }

    /// Publish an evaluation and its live-scene effect together, once the
    /// thread has been checked as a valid target.
    pub async fn commit_evaluation<ApplyScene>(
        &self,
        pool: &SqlitePool,
        thread_id: &str,
        owner_user_id: Option<&str>,
        execution_id: &str,
        evaluation: Arc<GraphEvaluation>,
        apply_scene: ApplyScene,
    ) -> Result<(), String>
    where
        ApplyScene: FnOnce(),
    {
        authorize_publish_target(pool, thread_id, owner_user_id).await?;
        self.publish_unchecked(execution_id, evaluation);
        apply_scene();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn publish_for_test(&self, thread_id: &str, evaluation: Arc<GraphEvaluation>) {
        self.publish_unchecked(thread_id, evaluation);
    }

    pub fn latest(&self, execution_id: &str) -> Option<Arc<GraphEvaluation>> {
        self.runs.lock().unwrap().get(execution_id).cloned()
    }

    /// Thread deletion — the run belonged to a conversation that no longer
    /// exists.
    pub fn forget(&self, execution_id: &str) {
        self.runs.lock().unwrap().remove(execution_id);
    }

    pub fn clear(&self) {
        self.runs.lock().unwrap().clear();
    }
}

/// Resolve the durable capability target for a graph run. Publishing is a
/// write into a thread-owned Python input, so a raw thread id is never enough:
/// the current principal and agent kind must both match.
pub async fn authorize_publish_target(
    pool: &SqlitePool,
    thread_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let thread =
        crate::database::local::agent_threads::get_thread_row(pool, thread_id, owner_user_id)
            .await
            .map_err(|e| format!("agent thread '{thread_id}' is not available: {e}"))?;

    validate_publish_target(&thread)
}

fn validate_publish_target(thread: &AgentThread) -> Result<(), String> {
    if thread.agent_kind == "pattern_graph"
        && thread.subject_kind.as_deref() == Some("pattern")
        && thread.implementation_id.is_some()
    {
        Ok(())
    } else {
        Err("graph runs may be published only to a pattern agent thread".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::agent_threads::AgentThread;

    fn thread(kind: &str, subject: Option<&str>, implementation: Option<&str>) -> AgentThread {
        AgentThread {
            id: "thread".into(),
            owner_user_id: None,
            agent_kind: kind.into(),
            subject_kind: subject.map(|_| "pattern".into()),
            subject_id: subject.map(ToOwned::to_owned),
            implementation_id: implementation.map(ToOwned::to_owned),
            venue_id: None,
            score_id: None,
            forked_from_thread_id: None,
            forked_at_message_id: None,
            parent_thread_id: None,
            parent_call_id: None,
            title: None,
            actor: None,
            engine: "api".into(),
            model: None,
            provider: None,
            effort: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// Publishing is a write into a thread-owned Python input, so a raw thread
    /// id is never enough: the agent kind and its exact implementation are the
    /// capability.
    #[test]
    fn only_a_pattern_thread_with_an_implementation_may_be_published_to() {
        assert!(validate_publish_target(&thread("pattern_graph", Some("p"), Some("i"))).is_ok());
        assert!(validate_publish_target(&thread("pattern_graph", Some("p"), None)).is_err());
        assert!(validate_publish_target(&thread("track_copilot", Some("p"), Some("i"))).is_err());
        assert!(validate_publish_target(&thread("pattern_graph", None, Some("i"))).is_err());
    }
}
