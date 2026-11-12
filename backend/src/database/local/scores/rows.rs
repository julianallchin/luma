//! A score is rows: one `clips` row per clip, and the `scores` row itself.
//!
//! A clip's id is a random UUID its writer generated, and a document's clip
//! map is keyed by it. It names no score: a clip belongs to one through
//! `score_id`. Saving a document that holds an id the rows no longer have
//! inserts that row again, which is how an undone delete comes back.
//!
//! [`save_score`] makes the rows match a whole document, writing only what
//! differs. A writer's stale copy would revert every clip someone else wrote
//! since it read, so a writer sends a [`luma_patterns::ScoreEdit`] and the
//! document saved is that edit over the rows as they stand.

use luma_patterns::{BlendMode, Clip, Score, Selection};
use sqlx::{Row, SqliteConnection};

/// What one [`save_score`] actually wrote. Zero of everything is a no-op save,
/// which is the common case when an editor re-sends an unchanged document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub inserted: usize,
    pub updated: usize,
    pub deleted: usize,
}

impl Changed {
    #[must_use]
    pub fn any(&self) -> bool {
        self.inserted + self.updated + self.deleted > 0
    }
}

/// One stored clip, in the column spellings the table uses. `seed` is decimal
/// text because SQLite's integers are signed and a clip's seed is a full u64.
///
/// `graph_json` holds the clip's graph. The form columns `graph` and
/// `inputs_json`, and `selection_seed` from when a selection could draw at
/// random, are still in the table until a later migration drops them; nothing
/// reads them, and a new row writes them empty.
struct ClipRow {
    name: String,
    label: Option<String>,
    graph_json: String,
    start: f64,
    duration: f64,
    seed: String,
    selection_json: String,
    z_index: i64,
    blend_mode: String,
}

impl ClipRow {
    fn of(clip: &Clip) -> Result<Self, String> {
        Ok(Self {
            name: clip.name.clone(),
            label: clip.label.clone(),
            graph_json: json(&clip.graph)?,
            start: clip.start,
            duration: clip.duration,
            seed: clip.seed.to_string(),
            selection_json: json(&clip.selection)?,
            z_index: clip.z_index,
            blend_mode: clip.blend_mode.name().to_owned(),
        })
    }

    fn into_clip(self) -> Result<Clip, String> {
        Ok(Clip {
            name: self.name,
            label: self.label,
            graph: from_json(&self.graph_json)?,
            start: self.start,
            duration: self.duration,
            seed: parse_seed(&self.seed)?,
            selection: from_json::<Selection>(&self.selection_json)?,
            z_index: self.z_index,
            blend_mode: from_json::<BlendMode>(&format!("\"{}\"", self.blend_mode))?,
        })
    }

    /// The columns whose values differ, paired with the new value. An empty
    /// result is a clip that needs no statement at all.
    fn changes<'a>(&'a self, stored: &Self) -> Vec<(&'static str, Field<'a>)> {
        let mut changes = Vec::new();
        let mut text = |name, new: &'a str, old: &str| {
            if new != old {
                changes.push((name, Field::Text(new)));
            }
        };
        text("name", &self.name, &stored.name);
        text("graph_json", &self.graph_json, &stored.graph_json);
        text(
            "selection_json",
            &self.selection_json,
            &stored.selection_json,
        );
        text("blend_mode", &self.blend_mode, &stored.blend_mode);
        text("seed", &self.seed, &stored.seed);
        if self.label != stored.label {
            changes.push(("label", Field::Label(self.label.as_deref())));
        }
        if self.start != stored.start {
            changes.push(("start", Field::Real(self.start)));
        }
        if self.duration != stored.duration {
            changes.push(("duration", Field::Real(self.duration)));
        }
        if self.z_index != stored.z_index {
            changes.push(("z_index", Field::Integer(self.z_index)));
        }
        changes
    }
}

enum Field<'a> {
    Text(&'a str),
    Label(Option<&'a str>),
    Real(f64),
    Integer(i64),
}

/// Read one score's clips back into a document.
pub async fn load_score(
    connection: &mut SqliteConnection,
    score_id: &str,
) -> Result<Score, String> {
    let mut score = Score::default();
    let rows = sqlx::query(
        "SELECT id, name, label, graph_json, start, duration, seed, selection_json, z_index,
                blend_mode
         FROM clips WHERE score_id = ? ORDER BY id",
    )
    .bind(score_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| format!("failed to read the score's clips: {error}"))?;
    for row in rows {
        let id: String = row.try_get("id").map_err(|error| error.to_string())?;
        score.clips.insert(id, read_clip(&row)?.into_clip()?);
    }
    Ok(score)
}

/// Write `candidate` onto the score's rows, touching only what differs.
///
/// Runs in the caller's transaction. Whoever edits the score, the rows it
/// writes carry the score's own `uid`, read off the score row in
/// [`insert_clip`], the one place a clip gets its owner.
pub async fn save_score(
    connection: &mut SqliteConnection,
    score_id: &str,
    candidate: &Score,
) -> Result<Changed, String> {
    // The table does not hold labels unique, as a download may pass through
    // a state where one is on two rows. A save is where a duplicate is caught.
    candidate
        .unique_labels()
        .map_err(|error| error.to_string())?;
    let mut changed = Changed::default();
    let mut stored = load_score(&mut *connection, score_id).await?;

    // Statements upload in the order they run, and the server holds a label
    // unique at each one. So deletes go first, and a label is taken off the
    // clip that had it before another clip gets it.
    for id in stored
        .clips
        .keys()
        .filter(|id| !candidate.clips.contains_key(*id))
    {
        sqlx::query("DELETE FROM clips WHERE id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(|error| format!("failed to delete clip {id}: {error}"))?;
        changed.deleted += 1;
    }
    release_labels(&mut *connection, &mut stored, candidate).await?;

    for (id, clip) in &candidate.clips {
        let row = ClipRow::of(clip)?;
        match stored.clips.get(id) {
            None => {
                insert_clip(&mut *connection, score_id, id, &row).await?;
                changed.inserted += 1;
            }
            Some(current) => {
                let updates = row.changes(&ClipRow::of(current)?);
                if !updates.is_empty() {
                    update_clip(&mut *connection, id, &updates).await?;
                    changed.updated += 1;
                }
            }
        }
    }

    // `authored_at` is what "last worked on" reads. Only touched when
    // something moved; the touch trigger moves `updated_at` with it.
    if changed.any() {
        sqlx::query(
            "UPDATE scores SET authored_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?",
        )
        .bind(score_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("failed to touch the score: {error}"))?;
    }
    Ok(changed)
}

/// Clear each stored label the candidate gives to a different clip, in the
/// rows and in `stored`, so that no upload gives a clip a label another
/// still holds on the server.
async fn release_labels(
    connection: &mut SqliteConnection,
    stored: &mut Score,
    candidate: &Score,
) -> Result<(), String> {
    let taken: Vec<String> = stored
        .clips
        .iter()
        .filter(|(id, clip)| {
            clip.label.as_ref().is_some_and(|label| {
                candidate.clips.contains_key(*id)
                    && candidate
                        .clips
                        .iter()
                        .any(|(other, wanted)| other != *id && wanted.label.as_ref() == Some(label))
            })
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in taken {
        sqlx::query("UPDATE clips SET label = NULL WHERE id = ?")
            .bind(&id)
            .execute(&mut *connection)
            .await
            .map_err(|error| format!("failed to update clip {id}: {error}"))?;
        if let Some(clip) = stored.clips.get_mut(&id) {
            clip.label = None;
        }
    }
    Ok(())
}

fn read_clip(row: &sqlx::sqlite::SqliteRow) -> Result<ClipRow, String> {
    fn get<'r, T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>>(
        row: &'r sqlx::sqlite::SqliteRow,
        name: &str,
    ) -> Result<T, String> {
        row.try_get(name).map_err(|error| error.to_string())
    }
    Ok(ClipRow {
        name: get(row, "name")?,
        label: get(row, "label")?,
        graph_json: get(row, "graph_json")?,
        start: get(row, "start")?,
        duration: get(row, "duration")?,
        seed: get(row, "seed")?,
        selection_json: get(row, "selection_json")?,
        z_index: get(row, "z_index")?,
        blend_mode: get(row, "blend_mode")?,
    })
}

async fn insert_clip(
    connection: &mut SqliteConnection,
    score_id: &str,
    id: &str,
    row: &ClipRow,
) -> Result<(), String> {
    // `graph` is the old form id column: NOT NULL with no default, and
    // unread. It goes when the form columns are dropped.
    sqlx::query(
        "INSERT INTO clips (id, uid, score_id, name, graph_json, graph, start, duration, seed,
                            selection_json, z_index, blend_mode, label)
         VALUES (?1, (SELECT uid FROM scores WHERE id = ?2), ?2, ?3, ?4, '', ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )
    .bind(id)
    .bind(score_id)
    .bind(&row.name)
    .bind(&row.graph_json)
    .bind(row.start)
    .bind(row.duration)
    .bind(&row.seed)
    .bind(&row.selection_json)
    .bind(row.z_index)
    .bind(&row.blend_mode)
    .bind(&row.label)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("failed to insert clip {id}: {error}"))?;
    Ok(())
}

async fn update_clip(
    connection: &mut SqliteConnection,
    id: &str,
    updates: &[(&'static str, Field<'_>)],
) -> Result<(), String> {
    let assignments = updates
        .iter()
        .map(|(column, _)| format!("{column} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    // Column names come from the fixed list above, never from input.
    let sql = format!("UPDATE clips SET {assignments} WHERE id = ?");
    let mut statement = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    for (_, value) in updates {
        statement = match value {
            Field::Text(text) => statement.bind(*text),
            Field::Label(label) => statement.bind(*label),
            Field::Real(number) => statement.bind(*number),
            Field::Integer(number) => statement.bind(*number),
        };
    }
    statement
        .bind(id)
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("failed to update clip {id}: {error}"))?;
    Ok(())
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

fn from_json<T: serde::de::DeserializeOwned>(source: &str) -> Result<T, String> {
    serde_json::from_str(source).map_err(|error| format!("unreadable score row: {error}"))
}

fn parse_seed(text: &str) -> Result<u64, String> {
    text.parse()
        .map_err(|_| format!("clip seed `{text}` is not a u64"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::local::scores::tests::test_pool;
    use luma_patterns::{BlendMode, Clip, Selection};

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

    fn clip(seed: u64, start: f64) -> Clip {
        clip_with(seed, start, "Strobe", 0.9)
    }

    fn labelled(seed: u64, start: f64, label: &str) -> Clip {
        Clip {
            label: Some(label.into()),
            ..clip(seed, start)
        }
    }

    /// A strobe clip whose graph holds one value, so a test can change the
    /// graph without touching any other column.
    fn clip_with(seed: u64, start: f64, name: &str, rate: f64) -> Clip {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "start": start,
            "duration": 4.0,
            "seed": seed,
            "selection": Selection::all(),
            "z_index": 0,
            "blend_mode": BlendMode::Replace,
            "graph": {"version": 3, "nodes": {
                "strobe1": {"kind": "strobe", "inputs": {"rate": rate}}}},
        }))
        .expect("a valid clip")
    }

    /// A u64 seed does not fit a SQLite integer, and `json_extract` rounds it
    /// through a double — which is exactly why the column is decimal text and
    /// why this number, the one that exposed it, is the one asserted.
    #[tokio::test]
    async fn a_full_width_seed_round_trips() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let mut score = Score::default();
        score
            .clips
            .insert("c".into(), clip(14_772_579_305_790_499_209, 0.0));
        save_score(&mut connection, "s", &score).await.unwrap();
        let read = load_score(&mut connection, "s").await.unwrap();
        assert_eq!(read.clips["c"].seed, 14_772_579_305_790_499_209);
        assert_eq!(read, score);
    }

    /// A new clip belongs to its score's owner, whoever writes it. A score
    /// with no owner holds no clip: it is refused, never given an empty id.
    #[tokio::test]
    async fn a_clip_carries_its_score_s_owner() {
        let (_directory, pool) = seeded().await;
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id) VALUES ('g', NULL, 't', 'v')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        let mut score = Score::default();
        score.clips.insert("c".into(), clip(1, 0.0));
        save_score(&mut connection, "s", &score).await.unwrap();
        assert!(save_score(&mut connection, "g", &score).await.is_err());
        let owners: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT score_id, uid FROM clips ORDER BY score_id")
                .fetch_all(&mut *connection)
                .await
                .unwrap();
        assert_eq!(owners, [("s".to_string(), Some("alice".to_string()))]);
    }

    /// The point of the diff: an unchanged candidate writes nothing, and a
    /// changed one writes exactly the clips that moved.
    #[tokio::test]
    async fn a_save_writes_only_what_differs() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let mut score = Score::default();
        score.clips.insert("a".into(), clip(1, 0.0));
        score.clips.insert("b".into(), clip(2, 8.0));
        assert_eq!(
            save_score(&mut connection, "s", &score).await.unwrap(),
            Changed {
                inserted: 2,
                updated: 0,
                deleted: 0
            }
        );
        assert_eq!(
            save_score(&mut connection, "s", &score).await.unwrap(),
            Changed::default(),
            "re-sending an unchanged document must write nothing"
        );

        score.clips.get_mut("a").unwrap().start = 2.0;
        score.clips.remove("b");
        score.clips.insert("c".into(), clip(3, 16.0));
        assert_eq!(
            save_score(&mut connection, "s", &score).await.unwrap(),
            Changed {
                inserted: 1,
                updated: 1,
                deleted: 1
            }
        );
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), score);
    }

    /// Only the columns that changed are named in the statement, so two people
    /// editing different fields of one clip do not overwrite each other.
    #[tokio::test]
    async fn an_update_names_only_the_changed_columns() {
        let mut stored = ClipRow::of(&clip(1, 0.0)).unwrap();
        let mut moved = clip(1, 0.0);
        moved.z_index = 4;
        let row = ClipRow::of(&moved).unwrap();
        let changes = row.changes(&stored);
        assert_eq!(
            changes.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec!["z_index"]
        );
        stored.z_index = 4;
        assert!(row.changes(&stored).is_empty());
    }

    /// The name and the graph live in their own columns and come back as
    /// they went in, wires and settings included.
    #[tokio::test]
    async fn a_name_and_a_graph_round_trip() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let chase: Clip = serde_json::from_value(serde_json::json!({
            "name": "Chase",
            "start": 32.0, "duration": 8.0, "seed": 6_348_896_133_488_684_926_u64,
            "selection": Selection::all(),
            "z_index": 0, "blend_mode": "replace",
            "graph": {"version": 3, "nodes": {
                "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
                "curve1": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "space1"},
                                      "shape": {"points": [[0, 1], [1, 0]]},
                                      "low": 60, "high": 360}},
                "time1": {"kind": "time",
                          "inputs": {"every": 2, "phase": {"node": "curve1"}}},
                "curve2": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time1"},
                                      "shape": {"points": [[0, 1], [0.1667, 1], [0.1667, 0], [1, 0]]}}},
                "color1": {"kind": "color",
                           "inputs": {"color": [1, 1, 1], "brightness": {"node": "curve2"}}}}},
        }))
        .expect("the spec's Chase clip");
        let mut score = Score::default();
        score.clips.insert("chase".into(), chase);
        save_score(&mut connection, "s", &score).await.unwrap();
        let (name, graph): (String, String) =
            sqlx::query_as("SELECT name, graph_json FROM clips WHERE id = 'chase'")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert_eq!(name, "Chase");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&graph).unwrap(),
            serde_json::to_value(&score.clips["chase"].graph).unwrap()
        );
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), score);
    }

    /// Undo in the editor re-saves the document it had before a delete. The
    /// clip comes back as the row it was, under the id it had, which is what
    /// revives it on the server.
    #[tokio::test]
    async fn a_deleted_clip_saved_again_is_the_same_row() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut before = Score::default();
        before.clips.insert(id.clone(), labelled(1, 0.0, "drop"));
        before
            .clips
            .insert(uuid::Uuid::new_v4().to_string(), clip(2, 8.0));
        save_score(&mut connection, "s", &before).await.unwrap();

        let mut deleted = before.clone();
        deleted.clips.remove(&id);
        save_score(&mut connection, "s", &deleted).await.unwrap();
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), deleted);

        assert_eq!(
            save_score(&mut connection, "s", &before).await.unwrap(),
            Changed {
                inserted: 1,
                updated: 0,
                deleted: 0
            }
        );
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM clips WHERE label = 'drop'")
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        assert_eq!(ids, [id]);
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), before);
    }

    /// A clip's id names no score, so the row is found by the id alone and
    /// the document's key is the stored id, unchanged.
    #[tokio::test]
    async fn the_document_key_is_the_row_id() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut score = Score::default();
        score.clips.insert(id.clone(), clip(1, 0.0));
        save_score(&mut connection, "s", &score).await.unwrap();
        let stored: Vec<(String, String)> = sqlx::query_as("SELECT id, score_id FROM clips")
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        assert_eq!(stored, [(id, "s".to_string())]);
    }

    /// A label is stored, changed and cleared like any other column, and one
    /// save can hand it from one clip to another, or to a new clip in place
    /// of a deleted one.
    #[tokio::test]
    async fn a_label_round_trips_and_moves_between_clips() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let mut score = Score::default();
        score.clips.insert("a".into(), labelled(1, 0.0, "intro"));
        score.clips.insert("b".into(), labelled(2, 8.0, "drop"));
        score.clips.insert("c".into(), clip(3, 16.0));
        save_score(&mut connection, "s", &score).await.unwrap();
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), score);

        // a and b swap labels; c goes and a new clip takes nothing of it.
        score.clips.get_mut("a").unwrap().label = Some("drop".into());
        score.clips.get_mut("b").unwrap().label = Some("intro".into());
        save_score(&mut connection, "s", &score).await.unwrap();
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), score);

        score.clips.remove("a");
        score.clips.insert("d".into(), labelled(4, 24.0, "drop"));
        score.clips.get_mut("b").unwrap().label = None;
        save_score(&mut connection, "s", &score).await.unwrap();
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), score);
    }

    /// A save refuses a second clip with a label its score already uses, and
    /// writes nothing; another score may use the same one.
    #[tokio::test]
    async fn a_label_is_unique_in_its_score_only() {
        let (_directory, pool) = seeded().await;
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id) VALUES ('s2', 'alice', 't', 'v')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        let mut score = Score::default();
        score.clips.insert("a".into(), labelled(1, 0.0, "drop"));
        save_score(&mut connection, "s", &score).await.unwrap();
        let mut other = Score::default();
        other.clips.insert("b".into(), labelled(1, 0.0, "drop"));
        save_score(&mut connection, "s2", &other).await.unwrap();

        let saved = score.clone();
        score.clips.insert("c".into(), labelled(2, 8.0, "drop"));
        score.clips.get_mut("a").unwrap().start = 2.0;
        let error = save_score(&mut connection, "s", &score).await.unwrap_err();
        assert!(error.contains("`drop`"), "{error}");
        assert_eq!(load_score(&mut connection, "s").await.unwrap(), saved);
    }

    /// `graph_json` is written when the graph changed, and only then: a
    /// rename writes the name alone.
    #[tokio::test]
    async fn the_graph_column_moves_only_with_the_graph() {
        let stored = ClipRow::of(&clip_with(1, 0.0, "Strobe", 0.9)).unwrap();
        let names = |row: &ClipRow| {
            row.changes(&stored)
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&ClipRow::of(&clip_with(1, 0.0, "Burst", 0.9)).unwrap()),
            vec!["name"]
        );
        assert_eq!(
            names(&ClipRow::of(&clip_with(1, 0.0, "Strobe", 0.5)).unwrap()),
            vec!["graph_json"]
        );
        assert!(names(&ClipRow::of(&clip_with(1, 0.0, "Strobe", 0.9)).unwrap()).is_empty());
    }

    /// The score row is what "last worked on" reads, so it moves when — and
    /// only when — something actually changed.
    #[tokio::test]
    async fn saving_touches_the_score_row_only_on_a_real_change() {
        let (_directory, pool) = seeded().await;
        let mut connection = pool.acquire().await.unwrap();
        let stamp = |pool: sqlx::SqlitePool| async move {
            sqlx::query_scalar::<_, String>("SELECT updated_at FROM scores WHERE id = 's'")
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let before = stamp(pool.clone()).await;
        let mut score = Score::default();
        score.clips.insert("a".into(), clip(1, 0.0));
        save_score(&mut connection, "s", &score).await.unwrap();
        let after = stamp(pool.clone()).await;
        assert_ne!(after, before);
        save_score(&mut connection, "s", &score).await.unwrap();
        assert_eq!(stamp(pool).await, after);
    }
}
