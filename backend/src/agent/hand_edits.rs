//! What the user changed in a score by hand since the agent last saw it.
//!
//! A thread remembers the score as it stood when its last reply ended, one
//! [`Mark`] per clip. The next message compares the score against those marks
//! and carries the difference in its editor context: the user's own edits are
//! decisions, and the agent should read them before it touches anything.
//! A thread with no marks yet has nothing to compare against and reports
//! nothing. An edit made while a reply is still running is folded into the
//! marks that reply leaves, so it is not reported.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use luma_patterns::{Clip, Score};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::database::local::scores::rows;

/// One clip as a thread remembers it. `look` is a digest of everything that
/// decides what the clip paints: its graph, blend and seed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Mark {
    name: String,
    start: f64,
    duration: f64,
    z: i64,
    lights: String,
    look: u64,
}

impl Mark {
    fn of(clip: &Clip) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(&clip.graph)
            .unwrap_or_default()
            .hash(&mut hasher);
        clip.blend_mode.name().hash(&mut hasher);
        clip.seed.hash(&mut hasher);
        Self {
            name: clip.name.clone(),
            start: clip.start,
            duration: clip.duration,
            z: clip.z_index,
            lights: serde_json::to_string(&clip.selection).unwrap_or_default(),
            look: hasher.finish(),
        }
    }

    /// What differs between two marks of one clip, in the words the context
    /// block uses.
    fn changes(&self, now: &Self) -> Vec<&'static str> {
        const MOVED: f64 = 1e-6;
        let moved = (self.start - now.start).abs() > MOVED;
        let resized = (self.duration - now.duration).abs() > MOVED;
        [
            (self.name != now.name, "renamed"),
            (moved, "moved"),
            (resized, "resized"),
            (self.z != now.z, "layer"),
            (self.lights != now.lights, "lights"),
            (self.look != now.look, "look"),
        ]
        .into_iter()
        .filter_map(|(changed, word)| changed.then_some(word))
        .collect()
    }
}

type Marks = BTreeMap<String, Mark>;

fn marks_of(score: &Score) -> Marks {
    score
        .clips
        .iter()
        .map(|(id, clip)| (id.clone(), Mark::of(clip)))
        .collect()
}

/// What happened to one clip.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Added,
    Removed,
    /// What about it changed: `renamed`, `moved`, `resized`, `layer`,
    /// `lights`, `look`.
    Changed(Vec<&'static str>),
}

/// One clip the user changed, where it stands now (or stood, when removed),
/// in beats.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub id: String,
    pub name: String,
    pub start: f64,
    pub end: f64,
    pub change: Change,
}

async fn score(pool: &SqlitePool, score_id: &str) -> Result<Score, String> {
    let mut connection = pool.acquire().await.map_err(|error| error.to_string())?;
    rows::load_score(&mut connection, score_id).await
}

/// Remember the score as it stands now as what `thread_id`'s agent last saw.
pub async fn remember(pool: &SqlitePool, thread_id: &str, score_id: &str) -> Result<(), String> {
    let marks = serde_json::to_string(&marks_of(&score(pool, score_id).await?))
        .map_err(|error| error.to_string())?;
    sqlx::query(
        "INSERT INTO agent_thread_score_marks (thread_id, score_id, marks_json) VALUES (?, ?, ?)
         ON CONFLICT (thread_id) DO UPDATE SET
             score_id = excluded.score_id, marks_json = excluded.marks_json,
             recorded_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
    )
    .bind(thread_id)
    .bind(score_id)
    .bind(marks)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("failed to remember the score: {error}"))
}

/// The clips that differ from what `thread_id`'s agent last saw, sorted by
/// start. `None` when the thread remembers nothing of this score.
pub async fn since(
    pool: &SqlitePool,
    thread_id: &str,
    score_id: &str,
) -> Result<Option<Vec<Edit>>, String> {
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT marks_json FROM agent_thread_score_marks WHERE thread_id = ? AND score_id = ?",
    )
    .bind(thread_id)
    .bind(score_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("failed to read the remembered score: {error}"))?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let before: Marks = serde_json::from_str(&stored).map_err(|error| error.to_string())?;
    Ok(Some(diff(
        &before,
        &marks_of(&score(pool, score_id).await?),
    )))
}

fn diff(before: &Marks, now: &Marks) -> Vec<Edit> {
    let edit = |id: &str, mark: &Mark, change| Edit {
        id: id.to_owned(),
        name: mark.name.clone(),
        start: mark.start,
        end: mark.start + mark.duration,
        change,
    };
    let mut edits: Vec<Edit> = now
        .iter()
        .filter_map(|(id, mark)| match before.get(id) {
            None => Some(edit(id, mark, Change::Added)),
            Some(old) if old != mark => Some(edit(id, mark, Change::Changed(old.changes(mark)))),
            Some(_) => None,
        })
        .chain(
            before
                .iter()
                .filter(|(id, _)| !now.contains_key(*id))
                .map(|(id, mark)| edit(id, mark, Change::Removed)),
        )
        .collect();
    edits.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.id.cmp(&b.id)));
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(name: &str, start: f64, look: u64) -> Mark {
        Mark {
            name: name.into(),
            start,
            duration: 4.0,
            z: 0,
            lights: "all".into(),
            look,
        }
    }

    /// The round trip a thread makes: nothing to say before it has seen the
    /// score, nothing while the score stands as it left it, and then exactly
    /// what was changed by hand.
    #[tokio::test]
    async fn a_thread_reports_what_changed_since_it_last_looked() {
        use crate::database::local::scores::tests::test_pool;
        use luma_patterns::{BlendMode, Selection};

        let (_directory, pool) = test_pool().await;
        for statement in [
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('t', 'alice', 'h', '/t')",
            "INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Venue')",
            "INSERT INTO scores (id, uid, track_id, venue_id) VALUES ('s', 'alice', 't', 'v')",
            "INSERT INTO agent_threads (id, agent_kind, uid) VALUES ('thread', 'track_copilot', 'alice')",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        let clip = |start: f64| Clip {
            name: "Wash".into(),
            start,
            duration: 4.0,
            seed: 7,
            selection: Selection::all(),
            z_index: 0,
            blend_mode: BlendMode::Replace,
            graph: serde_json::from_value(serde_json::json!(
                {"version": 3, "nodes": {"strobe1": {"kind": "strobe"}}}
            ))
            .unwrap(),
        };
        let mut live = Score::default();
        live.clips.insert("kept".into(), clip(0.0));
        live.clips.insert("moved".into(), clip(8.0));
        let save = |score: Score| {
            let pool = pool.clone();
            async move {
                let mut connection = pool.acquire().await.unwrap();
                rows::save_score(&mut connection, "s", &score)
                    .await
                    .unwrap();
            }
        };
        save(live.clone()).await;

        assert_eq!(since(&pool, "thread", "s").await.unwrap(), None);
        remember(&pool, "thread", "s").await.unwrap();
        assert_eq!(since(&pool, "thread", "s").await.unwrap(), Some(vec![]));

        live.clips.get_mut("moved").unwrap().start = 12.0;
        live.clips.insert("new".into(), clip(4.0));
        save(live).await;
        let edits = since(&pool, "thread", "s").await.unwrap().unwrap();
        let told: Vec<(&str, &Change)> = edits
            .iter()
            .map(|edit| (edit.id.as_str(), &edit.change))
            .collect();
        assert_eq!(
            told,
            [
                ("new", &Change::Added),
                ("moved", &Change::Changed(vec!["moved"])),
            ]
        );

        remember(&pool, "thread", "s").await.unwrap();
        assert_eq!(since(&pool, "thread", "s").await.unwrap(), Some(vec![]));
    }

    #[test]
    fn only_what_differs_is_reported_and_says_how() {
        let before: Marks = [
            ("kept".to_owned(), mark("Wash", 0.0, 1)),
            ("moved".to_owned(), mark("Chase", 8.0, 2)),
            ("gone".to_owned(), mark("Roll", 16.0, 3)),
        ]
        .into();
        let mut now = before.clone();
        now.remove("gone");
        now.insert("moved".into(), mark("Chase", 12.0, 9));
        now.insert("new".into(), mark("Hit", 4.0, 4));

        let edits = diff(&before, &now);
        let told: Vec<(&str, &Change)> = edits
            .iter()
            .map(|edit| (edit.id.as_str(), &edit.change))
            .collect();
        assert_eq!(
            told,
            [
                ("new", &Change::Added),
                ("moved", &Change::Changed(vec!["moved", "look"])),
                ("gone", &Change::Removed),
            ]
        );
        assert_eq!(edits[1].end, 16.0);
        assert!(diff(&now, &now).is_empty());
    }
}
