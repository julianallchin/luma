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
use crate::models::agent_threads::AgentThread;
use crate::services::authored_documents::AuthoredDocuments;

#[derive(Default)]
pub struct GraphRunStore {
    runs: Mutex<HashMap<String, Arc<GraphEvaluation>>>,
}

impl GraphRunStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn publish_unchecked(&self, thread_id: &str, evaluation: Arc<GraphEvaluation>) {
        self.runs
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), evaluation);
    }

    /// Publish an evaluation and its live-scene effect at one exact lifecycle
    /// boundary. Deletion uses the same authored repository gate for its
    /// durable `active -> deleting` transition, so this closure runs wholly
    /// before that transition or not at all after it.
    pub async fn commit_evaluation<ApplyScene>(
        &self,
        pool: &SqlitePool,
        authored: &AuthoredDocuments,
        thread_id: &str,
        owner_user_id: Option<&str>,
        evaluation: Arc<GraphEvaluation>,
        apply_scene: ApplyScene,
    ) -> Result<(), String>
    where
        ApplyScene: FnOnce(),
    {
        authored
            .fence_active_thread_effect(pool, owner_user_id, thread_id, |thread| {
                validate_publish_target(thread)?;
                self.publish_unchecked(thread_id, evaluation);
                apply_scene();
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub(crate) fn publish_for_test(&self, thread_id: &str, evaluation: Arc<GraphEvaluation>) {
        self.publish_unchecked(thread_id, evaluation);
    }

    pub fn latest(&self, thread_id: &str) -> Option<Arc<GraphEvaluation>> {
        self.runs.lock().unwrap().get(thread_id).cloned()
    }

    /// Thread deletion — the run belonged to a conversation that no longer
    /// exists.
    pub fn forget(&self, thread_id: &str) {
        self.runs.lock().unwrap().remove(thread_id);
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
    use crate::eval::{Plan, ResidentContext};
    use crate::models::agent_threads::CreateAgentThreadInput;
    use crate::storage::StorageRoot;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

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

        store.publish_unchecked("t1", evaluation("track-1"));
        store.publish_unchecked("t2", evaluation("track-2"));
        assert_eq!(store.latest("t1").unwrap().track_id, "track-1");
        assert_eq!(store.latest("t2").unwrap().track_id, "track-2");

        // Latest wins, and forgetting one leaves the other alone.
        store.publish_unchecked("t1", evaluation("track-3"));
        assert_eq!(store.latest("t1").unwrap().track_id, "track-3");
        store.forget("t1");
        assert!(store.latest("t1").is_none());
        assert!(store.latest("t2").is_some());
    }

    #[tokio::test]
    async fn publishing_requires_the_owned_pattern_thread() {
        let directory = tempfile::tempdir().unwrap();
        let crate::database::local::database::Db(pool) =
            crate::database::local::database::init_app_db_at(directory.path())
                .await
                .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'Pattern')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('implementation', 'alice', 'pattern',
                     '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_threads
                (id, owner_user_id, agent_kind, subject_kind, subject_id,
                 implementation_id, created_at, updated_at)
             VALUES
                ('pattern-thread', 'alice', 'pattern_graph', 'pattern', 'pattern',
                 'implementation', '', ''),
                ('track-thread', 'alice', 'track_copilot', 'track', 'track',
                 NULL, '', '');",
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

    #[tokio::test]
    async fn deleting_thread_rejects_late_evaluation_publish_and_scene_effect() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("luma-test.db");
        let migrate_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .unwrap();
        migrate_pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO patterns (id, name) VALUES ('pattern', 'Pattern')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json)
             VALUES ('implementation', 'pattern', '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        let authored = Arc::new(AuthoredDocuments::new(StorageRoot::from_path(
            directory.path().join("storage"),
        )));
        let thread = authored
            .create_thread_with_authored_state(
                &pool,
                CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: "pattern_graph".into(),
                    subject_kind: Some("pattern".into()),
                    subject_id: Some("pattern".into()),
                    implementation_id: Some("implementation".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let runs = Arc::new(GraphRunStore::new());
        let evaluation_started = Arc::new(Notify::new());
        let finish_evaluation = Arc::new(Notify::new());
        let scene_updates = Arc::new(AtomicUsize::new(0));
        let late_run = {
            let pool = pool.clone();
            let authored = Arc::clone(&authored);
            let runs = Arc::clone(&runs);
            let thread_id = thread.id.clone();
            let evaluation_started = Arc::clone(&evaluation_started);
            let finish_evaluation = Arc::clone(&finish_evaluation);
            let scene_updates = Arc::clone(&scene_updates);
            tokio::spawn(async move {
                // Match the command's fail-fast check, then hold the expensive
                // evaluation at a deterministic await boundary.
                authorize_publish_target(&pool, &thread_id, None)
                    .await
                    .unwrap();
                evaluation_started.notify_one();
                finish_evaluation.notified().await;

                runs.commit_evaluation(
                    &pool,
                    &authored,
                    &thread_id,
                    None,
                    evaluation("track"),
                    move || {
                        scene_updates.fetch_add(1, Ordering::SeqCst);
                    },
                )
                .await
            })
        };

        evaluation_started.notified().await;
        // The evaluation remains paused. Let deletion finish its durable row,
        // routing, and in-memory cleanup before allowing the result to return.
        authored
            .delete_thread_with_authored_state(&pool, None, &thread.id, || async {
                runs.forget(&thread.id);
                Ok(())
            })
            .await
            .unwrap();
        finish_evaluation.notify_one();

        let error = late_run.await.unwrap().unwrap_err();
        assert!(error.contains("not found"), "{error}");
        assert!(runs.latest(&thread.id).is_none());
        assert_eq!(scene_updates.load(Ordering::SeqCst), 0);
    }
}
