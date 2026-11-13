//! A person's verdicts on what an agent made: one per turn, one per clip.
//!
//! A turn is named by the message that started it, which is what the server's
//! history records against every row the turn wrote, so a verdict on a turn
//! joins to its clips there. A verdict on a clip names the clip, and is kept
//! when the clip is deleted: rejecting a clip and deleting it are usually one
//! thought.
//!
//! "One per turn" and "one per clip" are kept here, by [`set`], inside one
//! write transaction. The table has no unique index for it
//! (`migrations/20261113120000_provenance.sql` says why).

use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

/// What a verdict is about.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Target {
    /// An agent's turn, by the message that started it.
    Turn {
        thread_id: String,
        message_id: String,
    },
    /// One clip.
    Clip { clip_id: String },
}

/// Good or bad. Stored as `1` and `-1`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Good,
    Bad,
}

impl Verdict {
    const fn stored(self) -> i64 {
        match self {
            Self::Good => 1,
            Self::Bad => -1,
        }
    }

    const fn read(stored: i64) -> Self {
        if stored > 0 {
            Self::Good
        } else {
            Self::Bad
        }
    }
}

/// One verdict, as a reader needs it: what it is about and what it says.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Feedback {
    pub target: Target,
    pub verdict: Verdict,
    pub note: Option<String>,
}

/// The score a target belongs to: the thread's pinned score for a turn, the
/// clip's own for a clip. `None` for a conversation about a venue or a
/// pattern, and for a clip that is gone.
async fn score_of(
    connection: &mut SqliteConnection,
    target: &Target,
) -> Result<Option<String>, String> {
    let (statement, id) = match target {
        Target::Turn { thread_id, .. } => {
            ("SELECT score_id FROM agent_threads WHERE id = ?", thread_id)
        }
        Target::Clip { clip_id } => ("SELECT score_id FROM clips WHERE id = ?", clip_id),
    };
    sqlx::query_scalar::<_, Option<String>>(statement)
        .bind(id)
        .fetch_optional(connection)
        .await
        .map(Option::flatten)
        .map_err(|error| format!("Failed to find the score feedback is about: {error}"))
}

/// The id of `owner`'s verdict on `target`, if there is one.
async fn held(
    connection: &mut SqliteConnection,
    target: &Target,
    owner: &str,
) -> Result<Option<String>, String> {
    let query = match target {
        Target::Turn { message_id, .. } => sqlx::query_scalar(
            "SELECT id FROM feedback WHERE uid = ? AND message_id = ? AND clip_id IS NULL",
        )
        .bind(owner)
        .bind(message_id),
        Target::Clip { clip_id } => {
            sqlx::query_scalar("SELECT id FROM feedback WHERE uid = ? AND clip_id = ?")
                .bind(owner)
                .bind(clip_id)
        }
    };
    query
        .fetch_optional(connection)
        .await
        .map_err(|error| format!("Failed to read feedback: {error}"))
}

/// Record `owner`'s verdict on `target`, replacing the one it had. `None`
/// takes the verdict back.
///
/// # Errors
///
/// If `owner` is not the account writes are admitted for, or the write fails.
pub async fn set(
    pool: &SqlitePool,
    target: &Target,
    verdict: Option<Verdict>,
    note: Option<&str>,
    owner: &str,
) -> Result<(), String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin the feedback write: {error}"))?;
    let existing = held(&mut transaction, target, owner).await?;
    let written = match (verdict, existing) {
        (None, None) => 1,
        (None, Some(id)) => sqlx::query(
            "DELETE FROM feedback WHERE id = ? AND uid = (
                 SELECT active_uid FROM auth_write_admission
                 WHERE singleton = 1 AND armed = 1 AND accepting = 1
                   AND maintenance = 0 AND remote_writes = 0)",
        )
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to take feedback back: {error}"))?
        .rows_affected(),
        (Some(verdict), Some(id)) => sqlx::query(
            "UPDATE feedback SET rating = ?, note = ? WHERE id = ? AND uid = (
                 SELECT active_uid FROM auth_write_admission
                 WHERE singleton = 1 AND armed = 1 AND accepting = 1
                   AND maintenance = 0 AND remote_writes = 0)",
        )
        .bind(verdict.stored())
        .bind(note)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to change feedback: {error}"))?
        .rows_affected(),
        (Some(verdict), None) => {
            let score = score_of(&mut transaction, target).await?;
            let (thread, message, clip) = match target {
                Target::Turn {
                    thread_id,
                    message_id,
                } => (Some(thread_id), Some(message_id), None),
                Target::Clip { clip_id } => (None, None, Some(clip_id)),
            };
            sqlx::query(
                "INSERT INTO feedback
                     (id, uid, score_id, thread_id, message_id, clip_id, rating, note)
                 SELECT ?, admission.active_uid, ?, ?, ?, ?, ?, ?
                 FROM auth_write_admission admission
                 WHERE admission.singleton = 1 AND admission.armed = 1
                   AND admission.accepting = 1 AND admission.maintenance = 0
                   AND admission.remote_writes = 0 AND admission.active_uid = ?",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(score)
            .bind(thread)
            .bind(message)
            .bind(clip)
            .bind(verdict.stored())
            .bind(note)
            .bind(owner)
            .execute(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to record feedback: {error}"))?
            .rows_affected()
        }
    };
    if written == 0 {
        return Err("Feedback can only be recorded by the signed-in account".into());
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit the feedback write: {error}"))
}

/// `owner`'s verdicts on the turns of `thread_id`.
///
/// # Errors
///
/// If the query fails.
pub async fn of_thread(
    pool: &SqlitePool,
    thread_id: &str,
    owner: &str,
) -> Result<Vec<Feedback>, String> {
    let rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT message_id, rating, note FROM feedback
         WHERE uid = ? AND thread_id = ? AND clip_id IS NULL AND message_id IS NOT NULL",
    )
    .bind(owner)
    .bind(thread_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("Failed to read a thread's feedback: {error}"))?;
    Ok(rows
        .into_iter()
        .map(|(message_id, verdict, note)| Feedback {
            target: Target::Turn {
                thread_id: thread_id.to_owned(),
                message_id,
            },
            verdict: Verdict::read(verdict),
            note,
        })
        .collect())
}

/// `owner`'s verdicts on the clips of `score_id`.
///
/// # Errors
///
/// If the query fails.
pub async fn of_score_clips(
    pool: &SqlitePool,
    score_id: &str,
    owner: &str,
) -> Result<Vec<Feedback>, String> {
    let rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT clip_id, rating, note FROM feedback
         WHERE uid = ? AND score_id = ? AND clip_id IS NOT NULL",
    )
    .bind(owner)
    .bind(score_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("Failed to read a score's feedback: {error}"))?;
    Ok(rows
        .into_iter()
        .map(|(clip_id, verdict, note)| Feedback {
            target: Target::Clip { clip_id },
            verdict: Verdict::read(verdict),
            note,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::local::{auth, database};

    async fn library() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = database::init_app_db_at(directory.path()).await.unwrap().0;
        auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('t', 'alice', 'h', '/t');
             INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Room');
             INSERT INTO scores (id, uid, track_id, venue_id) VALUES ('s', 'alice', 't', 'v');
             INSERT INTO agent_threads (id, uid, agent_kind, score_id)
                 VALUES ('thread', 'alice', 'track_copilot', 's')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (directory, pool)
    }

    fn turn() -> Target {
        Target::Turn {
            thread_id: "thread".into(),
            message_id: "message".into(),
        }
    }

    /// A turn has one verdict a person: a second one changes it, under the
    /// same id, and taking it back leaves none. It names the thread's score.
    #[tokio::test]
    async fn a_turn_holds_one_verdict_which_changes_and_can_be_taken_back() {
        let (_directory, pool) = library().await;
        let rows = || async {
            sqlx::query_as::<_, (String, Option<String>, i64)>(
                "SELECT id, score_id, rating FROM feedback",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        };

        set(&pool, &turn(), Some(Verdict::Good), None, "alice")
            .await
            .unwrap();
        let first = rows().await;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1.as_deref(), Some("s"));

        set(
            &pool,
            &turn(),
            Some(Verdict::Bad),
            Some("too busy"),
            "alice",
        )
        .await
        .unwrap();
        let second = rows().await;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, first[0].0, "the same row, changed");
        assert_eq!(
            of_thread(&pool, "thread", "alice").await.unwrap(),
            vec![Feedback {
                target: turn(),
                verdict: Verdict::Bad,
                note: Some("too busy".into()),
            }]
        );

        set(&pool, &turn(), None, None, "alice").await.unwrap();
        assert!(rows().await.is_empty());
    }

    /// A verdict on a clip is not the turn's, and another account writes
    /// nothing here.
    #[tokio::test]
    async fn a_clips_verdict_is_its_own_and_only_the_signed_in_account_writes() {
        let (_directory, pool) = library().await;
        let clip = Target::Clip {
            clip_id: "clip".into(),
        };
        set(&pool, &turn(), Some(Verdict::Good), None, "alice")
            .await
            .unwrap();
        set(&pool, &clip, Some(Verdict::Bad), None, "alice")
            .await
            .unwrap();
        assert_eq!(of_thread(&pool, "thread", "alice").await.unwrap().len(), 1);

        assert!(set(&pool, &clip, Some(Verdict::Good), None, "bob")
            .await
            .is_err());
    }
}
