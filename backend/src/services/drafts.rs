//! A subagent works on a draft, never on the live score.
//!
//! A draft holds two copies of the score: `base_json`, as it stood when the
//! child started, and `state_json`, what the child has made of it. Merging
//! diffs the two per clip and per definition and applies only the differences,
//! so work the parent did meanwhile survives — a whole-document overwrite
//! would silently undo it.

use luma_patterns::Score;
use sqlx::SqliteConnection;

use crate::database::local::scores::rows;

/// Open a draft of `score_id` for `thread_id`. Idempotent: a thread that
/// already has a draft of this score keeps the one it has, because re-creating
/// would discard whatever the child has written so far.
pub async fn create(
    connection: &mut SqliteConnection,
    score_id: &str,
    thread_id: &str,
    uid: &str,
) -> Result<String, String> {
    if let Some(id) = of_thread(&mut *connection, thread_id, score_id).await? {
        return Ok(id);
    }
    let live = rows::load_score(&mut *connection, score_id).await?;
    let source = serde_json::to_string(&live).map_err(|error| error.to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO drafts (id, uid, score_id, thread_id, base_json, state_json)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(uid)
    .bind(score_id)
    .bind(thread_id)
    .bind(&source)
    .bind(&source)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("failed to open a draft of the score: {error}"))?;
    Ok(id)
}

/// The draft a thread is working in, if it has one.
pub async fn of_thread(
    connection: &mut SqliteConnection,
    thread_id: &str,
    score_id: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT id FROM drafts WHERE thread_id = ? AND score_id = ?")
        .bind(thread_id)
        .bind(score_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| format!("failed to look up the thread's draft: {error}"))
}

/// Read a draft's working state — what the child has made of the score.
pub async fn state(connection: &mut SqliteConnection, draft_id: &str) -> Result<Score, String> {
    let (_, state) = both(connection, draft_id).await?;
    Ok(state)
}

/// Replace a draft's working state. The live rows are untouched.
pub async fn apply(
    connection: &mut SqliteConnection,
    draft_id: &str,
    candidate: &Score,
) -> Result<(), String> {
    let source = serde_json::to_string(candidate).map_err(|error| error.to_string())?;
    let changed = sqlx::query("UPDATE drafts SET state_json = ? WHERE id = ?")
        .bind(&source)
        .bind(draft_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("failed to write the draft: {error}"))?
        .rows_affected();
    if changed != 1 {
        return Err(format!("draft {draft_id} does not exist"));
    }
    Ok(())
}

/// Apply the draft's own changes to the live score and close it.
///
/// Only what the child touched moves: a clip the child did not change keeps
/// whatever the live rows now say, and a clip the child deleted is deleted even
/// if the parent edited it meanwhile — a deletion is a decision, and reviving
/// the row would be a second one nobody made.
pub async fn merge(connection: &mut SqliteConnection, draft_id: &str) -> Result<Score, String> {
    let (base, state) = both(&mut *connection, draft_id).await?;
    let (score_id, uid) = scope(&mut *connection, draft_id).await?;
    let mut live = rows::load_score(&mut *connection, &score_id).await?;

    for (id, clip) in &state.clips {
        if base.clips.get(id) != Some(clip) {
            live.clips.insert(id.clone(), clip.clone());
        }
    }
    for id in base
        .clips
        .keys()
        .filter(|id| !state.clips.contains_key(*id))
    {
        live.clips.remove(id);
    }
    for (id, definition) in &state.definitions {
        if base.definitions.get(id) != Some(definition) {
            live.definitions.insert(id.clone(), definition.clone());
        }
    }
    for id in base
        .definitions
        .keys()
        .filter(|id| !state.definitions.contains_key(*id))
    {
        live.definitions.remove(id);
    }

    rows::save_score(&mut *connection, &score_id, &uid, &live).await?;
    discard(connection, draft_id).await?;
    Ok(live)
}

/// Close a draft without applying it.
pub async fn discard(connection: &mut SqliteConnection, draft_id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM drafts WHERE id = ?")
        .bind(draft_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("failed to discard the draft: {error}"))?;
    Ok(())
}

async fn both(connection: &mut SqliteConnection, draft_id: &str) -> Result<(Score, Score), String> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT base_json, state_json FROM drafts WHERE id = ?")
            .bind(draft_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| format!("failed to read the draft: {error}"))?;
    let (base, state) = row.ok_or_else(|| format!("draft {draft_id} does not exist"))?;
    Ok((read(&base)?, read(&state)?))
}

async fn scope(
    connection: &mut SqliteConnection,
    draft_id: &str,
) -> Result<(String, String), String> {
    sqlx::query_as("SELECT score_id, uid FROM drafts WHERE id = ?")
        .bind(draft_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| format!("failed to read the draft: {error}"))?
        .ok_or_else(|| format!("draft {draft_id} does not exist"))
}

fn read(source: &str) -> Result<Score, String> {
    serde_json::from_str(source).map_err(|error| format!("unreadable draft: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::local::scores::tests::test_pool;
    use luma_patterns::{BlendMode, Clip, Selection};

    fn clip(start: f64) -> Clip {
        Clip {
            graph: "strobe".into(),
            start,
            duration: 4.0,
            seed: 7,
            selection_seed: None,
            selection: Selection::all(),
            z_index: 0,
            blend_mode: BlendMode::Replace,
            inputs: Default::default(),
        }
    }

    async fn seeded() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let (directory, pool) = test_pool().await;
        for statement in [
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('t', 'alice', 'h', '/t')",
            "INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Venue')",
            "INSERT INTO scores (id, uid, track_id, venue_id) VALUES ('s', 'alice', 't', 'v')",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        (directory, pool)
    }

    /// The whole reason a draft holds two copies: a merge applies what the
    /// child changed, and leaves what the parent changed meanwhile alone.
    #[tokio::test]
    async fn a_merge_applies_the_childs_changes_and_keeps_the_parents() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let mut live = Score::default();
        live.clips.insert("shared".into(), clip(0.0));
        live.clips.insert("parent".into(), clip(8.0));
        rows::save_score(&mut connection, "s", "alice", &live)
            .await
            .unwrap();

        let draft = create(&mut connection, "s", "child-thread", "alice")
            .await
            .unwrap();

        // The child moves one clip and adds one.
        let mut child = live.clone();
        child.clips.get_mut("shared").unwrap().start = 1.0;
        child.clips.insert("child".into(), clip(16.0));
        apply(&mut connection, &draft, &child).await.unwrap();

        // The parent, meanwhile, edits a clip the child never touched.
        let mut parent = live.clone();
        parent.clips.get_mut("parent").unwrap().z_index = 9;
        rows::save_score(&mut connection, "s", "alice", &parent)
            .await
            .unwrap();

        let merged = merge(&mut connection, &draft).await.unwrap();
        assert_eq!(merged.clips["shared"].start, 1.0, "the child's edit lands");
        assert_eq!(
            merged.clips["parent"].z_index, 9,
            "the parent's edit survives the merge"
        );
        assert!(merged.clips.contains_key("child"));
        assert_eq!(
            rows::load_score(&mut connection, "s").await.unwrap(),
            merged
        );
        assert_eq!(
            of_thread(&mut connection, "child-thread", "s")
                .await
                .unwrap(),
            None,
            "merging closes the draft"
        );
    }

    /// A deletion is a decision. A clip the child removed goes even though the
    /// parent edited it; reviving it would be a second decision nobody made.
    #[tokio::test]
    async fn a_deleted_clip_stays_deleted() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let mut live = Score::default();
        live.clips.insert("doomed".into(), clip(0.0));
        rows::save_score(&mut connection, "s", "alice", &live)
            .await
            .unwrap();
        let draft = create(&mut connection, "s", "child", "alice")
            .await
            .unwrap();

        let mut child = live.clone();
        child.clips.remove("doomed");
        apply(&mut connection, &draft, &child).await.unwrap();

        let mut parent = live.clone();
        parent.clips.get_mut("doomed").unwrap().z_index = 3;
        rows::save_score(&mut connection, "s", "alice", &parent)
            .await
            .unwrap();

        let merged = merge(&mut connection, &draft).await.unwrap();
        assert!(merged.clips.is_empty());
    }

    /// Opening a draft twice keeps the one the child has been writing.
    #[tokio::test]
    async fn creating_a_draft_twice_keeps_the_first() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let first = create(&mut connection, "s", "child", "alice")
            .await
            .unwrap();
        let mut work = Score::default();
        work.clips.insert("a".into(), clip(0.0));
        apply(&mut connection, &first, &work).await.unwrap();
        let again = create(&mut connection, "s", "child", "alice")
            .await
            .unwrap();
        assert_eq!(again, first);
        assert_eq!(state(&mut connection, &first).await.unwrap(), work);
    }

    #[tokio::test]
    async fn discarding_leaves_the_live_score_alone() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let mut live = Score::default();
        live.clips.insert("kept".into(), clip(0.0));
        rows::save_score(&mut connection, "s", "alice", &live)
            .await
            .unwrap();
        let draft = create(&mut connection, "s", "child", "alice")
            .await
            .unwrap();
        apply(&mut connection, &draft, &Score::default())
            .await
            .unwrap();
        discard(&mut connection, &draft).await.unwrap();
        assert_eq!(rows::load_score(&mut connection, "s").await.unwrap(), live);
    }
}
