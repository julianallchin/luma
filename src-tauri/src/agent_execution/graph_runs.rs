//! The latest graph evaluation per agent thread (design §11.2).
//!
//! `run_graph` produces far more than the editor draws; the agent's `luma.graph.run`
//! branch wants exactly that surplus. Rather than push the evaluation through the
//! frontend and back (dense float buffers the UI has no business shipping), the
//! command parks it here under the thread id the caller named, and the next cell's
//! binding assembly picks it up.
//!
//! One slot per thread: a thread only ever looks at its most recent run, and the
//! provider re-checks compatibility with the *current* scope before publishing it,
//! so a stale entry is inert rather than misleading.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;

use crate::eval::graph_run::GraphEvaluation;

#[derive(Default)]
pub struct GraphRunStore {
    runs: Mutex<HashMap<String, Arc<GraphEvaluation>>>,
}

impl GraphRunStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the thread's latest run.
    pub fn publish(&self, thread_id: &str, evaluation: Arc<GraphEvaluation>) {
        self.runs
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), evaluation);
    }

    pub fn latest(&self, thread_id: &str) -> Option<Arc<GraphEvaluation>> {
        self.runs.lock().unwrap().get(thread_id).cloned()
    }

    /// Thread reset or deletion — the run belonged to a conversation that no
    /// longer exists.
    pub fn forget(&self, thread_id: &str) {
        self.runs.lock().unwrap().remove(thread_id);
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

    if thread.agent_kind != "pattern_graph" || thread.subject_kind.as_deref() != Some("pattern") {
        return Err("graph runs may be published only to a pattern agent thread".into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Plan, ResidentContext};

    fn evaluation(track_id: &str) -> Arc<GraphEvaluation> {
        Arc::new(GraphEvaluation {
            plan: Arc::new(Plan {
                ops: Vec::new(),
                slots: Vec::new(),
                slot_channels: Vec::new(),
                n: 0,
                primitive_ids: Vec::new(),
                outputs: Default::default(),
                ctx: ResidentContext {
                    span: (0.0, 1.0),
                    ..Default::default()
                },
                prologue_baked: Vec::new(),
                views: Vec::new(),
            }),
            views: HashMap::new(),
            mel_views: None,
            times_s: Vec::new(),
            primitive_ids: Vec::new(),
            positions: Vec::new(),
            span: (0.0, 1.0),
            graph_hash: "g".into(),
            arg_hash: "a".into(),
            selection_hash: "s".into(),
            track_id: track_id.into(),
            venue_id: "ven".into(),
            universe_state: None,
        })
    }

    #[test]
    fn threads_have_independent_slots() {
        let store = GraphRunStore::new();
        assert!(store.latest("t1").is_none());

        store.publish("t1", evaluation("track-1"));
        store.publish("t2", evaluation("track-2"));
        assert_eq!(store.latest("t1").unwrap().track_id, "track-1");
        assert_eq!(store.latest("t2").unwrap().track_id, "track-2");

        // Latest wins, and forgetting one leaves the other alone.
        store.publish("t1", evaluation("track-3"));
        assert_eq!(store.latest("t1").unwrap().track_id, "track-3");
        store.forget("t1");
        assert!(store.latest("t1").is_none());
        assert!(store.latest("t2").is_some());
    }

    #[tokio::test]
    async fn publishing_requires_the_owned_pattern_thread() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE agent_threads (
                id TEXT PRIMARY KEY,
                owner_user_id TEXT,
                agent_kind TEXT NOT NULL,
                subject_kind TEXT,
                subject_id TEXT,
                venue_id TEXT,
                score_id TEXT,
                title TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             INSERT INTO agent_threads
                (id, owner_user_id, agent_kind, subject_kind, created_at, updated_at)
             VALUES
                ('pattern-thread', 'alice', 'pattern_graph', 'pattern', '', ''),
                ('track-thread', 'alice', 'track_copilot', 'track', '', '');",
        )
        .execute(&pool)
        .await
        .unwrap();

        authorize_publish_target(&pool, "pattern-thread", Some("alice"))
            .await
            .unwrap();
        assert!(
            authorize_publish_target(&pool, "pattern-thread", Some("bob"))
                .await
                .is_err()
        );
        assert!(
            authorize_publish_target(&pool, "track-thread", Some("alice"))
                .await
                .is_err()
        );
    }
}
