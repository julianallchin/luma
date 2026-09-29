//! A score is rows: one `clips` row per clip, and the `scores` row itself.
//!
//! A clip key is unique inside its score, not across the library — two
//! scores may each have a `flash` — so the stored id is `score_id:key` and
//! the key is read back off it. A score id is a uuid and
//! carries no colon, so the split is unambiguous however the key is spelled.
//!
//! [`save_score`] writes only what differs. There is no revision token: a
//! stale candidate overwrites the rows it touches one column at a time, so two
//! people editing different clips of one score do not collide.

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
/// `inputs_json` are still in the table until a later migration drops them;
/// nothing reads them, and a new row writes them empty.
struct ClipRow {
    name: String,
    graph_json: String,
    start: f64,
    duration: f64,
    seed: String,
    selection_seed: Option<String>,
    selection_json: String,
    z_index: i64,
    blend_mode: String,
}

impl ClipRow {
    fn of(clip: &Clip) -> Result<Self, String> {
        Ok(Self {
            name: clip.name.clone(),
            graph_json: json(&clip.graph)?,
            start: clip.start,
            duration: clip.duration,
            seed: clip.seed.to_string(),
            selection_seed: clip.selection_seed.map(|seed| seed.to_string()),
            selection_json: json(&clip.selection)?,
            z_index: clip.z_index,
            blend_mode: clip.blend_mode.name().to_owned(),
        })
    }

    fn into_clip(self) -> Result<Clip, String> {
        Ok(Clip {
            name: self.name,
            graph: from_json(&self.graph_json)?,
            start: self.start,
            duration: self.duration,
            seed: parse_seed(&self.seed)?,
            selection_seed: self.selection_seed.as_deref().map(parse_seed).transpose()?,
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
        if self.selection_seed != stored.selection_seed {
            changes.push((
                "selection_seed",
                Field::NullableText(self.selection_seed.as_deref()),
            ));
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
    NullableText(Option<&'a str>),
    Real(f64),
    Integer(i64),
}

/// The globally unique id of a row whose key is unique only inside its score.
#[must_use]
pub fn row_id(score_id: &str, key: &str) -> String {
    format!("{score_id}:{key}")
}

fn key_of(score_id: &str, id: &str) -> Result<String, String> {
    id.strip_prefix(score_id)
        .and_then(|rest| rest.strip_prefix(':'))
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("row {id} does not belong to score {score_id}"))
}

/// Read one score's clips back into a document.
pub async fn load_score(
    connection: &mut SqliteConnection,
    score_id: &str,
) -> Result<Score, String> {
    let mut score = Score::default();
    let rows = sqlx::query(
        "SELECT id, name, graph_json, start, duration, seed, selection_seed, selection_json,
                z_index, blend_mode
         FROM clips WHERE score_id = ? ORDER BY id",
    )
    .bind(score_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| format!("failed to read the score's clips: {error}"))?;
    for row in rows {
        let id: String = row.try_get("id").map_err(|error| error.to_string())?;
        score
            .clips
            .insert(key_of(score_id, &id)?, read_clip(&row)?.into_clip()?);
    }
    Ok(score)
}

/// Write `candidate` onto the score's rows, touching only what differs.
///
/// Runs in the caller's transaction and takes the owner explicitly: the rows a
/// score owns carry the score's `uid`, not the session's.
pub async fn save_score(
    connection: &mut SqliteConnection,
    score_id: &str,
    uid: &str,
    candidate: &Score,
) -> Result<Changed, String> {
    let mut changed = Changed::default();
    let stored = load_score(&mut *connection, score_id).await?;

    for (id, clip) in &candidate.clips {
        let row = ClipRow::of(clip)?;
        match stored.clips.get(id) {
            None => {
                insert_clip(&mut *connection, score_id, uid, &row_id(score_id, id), &row).await?;
                changed.inserted += 1;
            }
            Some(current) => {
                let updates = row.changes(&ClipRow::of(current)?);
                if !updates.is_empty() {
                    update_clip(&mut *connection, &row_id(score_id, id), &updates).await?;
                    changed.updated += 1;
                }
            }
        }
    }
    for id in stored
        .clips
        .keys()
        .filter(|id| !candidate.clips.contains_key(*id))
    {
        sqlx::query("DELETE FROM clips WHERE id = ?")
            .bind(row_id(score_id, id))
            .execute(&mut *connection)
            .await
            .map_err(|error| format!("failed to delete clip {id}: {error}"))?;
        changed.deleted += 1;
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

fn read_clip(row: &sqlx::sqlite::SqliteRow) -> Result<ClipRow, String> {
    fn get<'r, T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>>(
        row: &'r sqlx::sqlite::SqliteRow,
        name: &str,
    ) -> Result<T, String> {
        row.try_get(name).map_err(|error| error.to_string())
    }
    Ok(ClipRow {
        name: get(row, "name")?,
        graph_json: get(row, "graph_json")?,
        start: get(row, "start")?,
        duration: get(row, "duration")?,
        seed: get(row, "seed")?,
        selection_seed: get(row, "selection_seed")?,
        selection_json: get(row, "selection_json")?,
        z_index: get(row, "z_index")?,
        blend_mode: get(row, "blend_mode")?,
    })
}

async fn insert_clip(
    connection: &mut SqliteConnection,
    score_id: &str,
    uid: &str,
    id: &str,
    row: &ClipRow,
) -> Result<(), String> {
    // `graph` is the old form id column: NOT NULL with no default, and
    // unread. It goes when the form columns are dropped.
    sqlx::query(
        "INSERT INTO clips (id, uid, score_id, name, graph_json, graph, start, duration, seed,
                            selection_seed, selection_json, z_index, blend_mode)
         VALUES (?, ?, ?, ?, ?, '', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(uid)
    .bind(score_id)
    .bind(&row.name)
    .bind(&row.graph_json)
    .bind(row.start)
    .bind(row.duration)
    .bind(&row.seed)
    .bind(row.selection_seed.as_deref())
    .bind(&row.selection_json)
    .bind(row.z_index)
    .bind(&row.blend_mode)
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
            Field::NullableText(text) => statement.bind(*text),
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
            "graph": {"version": 1, "nodes": {
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
        save_score(&mut connection, "s", "alice", &score)
            .await
            .unwrap();
        let read = load_score(&mut connection, "s").await.unwrap();
        assert_eq!(read.clips["c"].seed, 14_772_579_305_790_499_209);
        assert_eq!(read, score);
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
            save_score(&mut connection, "s", "alice", &score)
                .await
                .unwrap(),
            Changed {
                inserted: 2,
                updated: 0,
                deleted: 0
            }
        );
        assert_eq!(
            save_score(&mut connection, "s", "alice", &score)
                .await
                .unwrap(),
            Changed::default(),
            "re-sending an unchanged document must write nothing"
        );

        score.clips.get_mut("a").unwrap().start = 2.0;
        score.clips.remove("b");
        score.clips.insert("c".into(), clip(3, 16.0));
        assert_eq!(
            save_score(&mut connection, "s", "alice", &score)
                .await
                .unwrap(),
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
            "graph": {"version": 1, "nodes": {
                "clock1": {"kind": "clock", "inputs": {"every": 2}},
                "time1": {"kind": "time", "inputs": {"clock": {"node": "clock1"}}},
                "curve1": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time1"},
                                      "shape": {"points": [[0, 0], [1, 1]]},
                                      "low": -0.2, "high": 1}},
                "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"},
                           "inputs": {"offset": {"node": "curve1"}, "width": 0.2}},
                "curve2": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "space1"},
                                      "shape": {"points": [[0, 1], [1, 1]]}}},
                "color1": {"kind": "color",
                           "inputs": {"color": [1, 1, 1], "brightness": {"node": "curve2"}}}}},
        }))
        .expect("the spec's Chase clip");
        let mut score = Score::default();
        score.clips.insert("chase".into(), chase);
        save_score(&mut connection, "s", "alice", &score)
            .await
            .unwrap();
        let (name, graph): (String, String) =
            sqlx::query_as("SELECT name, graph_json FROM clips WHERE id = 's:chase'")
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
        save_score(&mut connection, "s", "alice", &score)
            .await
            .unwrap();
        let after = stamp(pool.clone()).await;
        assert_ne!(after, before);
        save_score(&mut connection, "s", "alice", &score)
            .await
            .unwrap();
        assert_eq!(stamp(pool).await, after);
    }
}
