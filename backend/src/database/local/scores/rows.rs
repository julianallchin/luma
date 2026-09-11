//! A score is rows: one `clips` row per clip, one `score_definitions` row per
//! local definition, and the `scores` row itself.
//!
//! [`load_score`] reads the three tables into the in-memory
//! [`luma_patterns::Score`]; [`save_score`] diffs a candidate against what is
//! stored and writes only what differs. There is no revision token — a stale
//! candidate simply overwrites the rows it touches, one column at a time, so
//! two people editing different clips of one score do not collide.

use luma_patterns::{BlendMode, Clip, Definition, Score, Selection};
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
struct ClipRow {
    graph: String,
    start: f64,
    duration: f64,
    seed: String,
    selection_seed: Option<String>,
    selection_json: String,
    z_index: i64,
    blend_mode: String,
    inputs_json: String,
}

impl ClipRow {
    fn of(clip: &Clip) -> Result<Self, String> {
        Ok(Self {
            graph: clip.graph.clone(),
            start: clip.start,
            duration: clip.duration,
            seed: clip.seed.to_string(),
            selection_seed: clip.selection_seed.map(|seed| seed.to_string()),
            selection_json: json(&clip.selection)?,
            z_index: clip.z_index,
            blend_mode: clip.blend_mode.name().to_owned(),
            inputs_json: json(&clip.inputs)?,
        })
    }

    fn into_clip(self) -> Result<Clip, String> {
        Ok(Clip {
            graph: self.graph,
            start: self.start,
            duration: self.duration,
            seed: parse_seed(&self.seed)?,
            selection_seed: self.selection_seed.as_deref().map(parse_seed).transpose()?,
            selection: from_json::<Selection>(&self.selection_json)?,
            z_index: self.z_index,
            blend_mode: from_json::<BlendMode>(&format!("\"{}\"", self.blend_mode))?,
            inputs: from_json(&self.inputs_json)?,
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
        text("graph", &self.graph, &stored.graph);
        text("selection_json", &self.selection_json, &stored.selection_json);
        text("blend_mode", &self.blend_mode, &stored.blend_mode);
        text("inputs_json", &self.inputs_json, &stored.inputs_json);
        text("seed", &self.seed, &stored.seed);
        if self.selection_seed != stored.selection_seed {
            changes.push(("selection_seed", Field::NullableText(self.selection_seed.as_deref())));
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

/// Read one score's clips and local definitions back into a document.
pub async fn load_score(
    connection: &mut SqliteConnection,
    score_id: &str,
) -> Result<Score, String> {
    let mut score = Score::default();
    let rows = sqlx::query(
        "SELECT id, graph, start, duration, seed, selection_seed, selection_json,
                z_index, blend_mode, inputs_json
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
    let rows = sqlx::query(
        "SELECT id, definition_json FROM score_definitions WHERE score_id = ? ORDER BY id",
    )
    .bind(score_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| format!("failed to read the score's definitions: {error}"))?;
    for row in rows {
        let id: String = row.try_get("id").map_err(|error| error.to_string())?;
        let source: String = row
            .try_get("definition_json")
            .map_err(|error| error.to_string())?;
        score
            .definitions
            .insert(id, from_json::<Definition>(&source)?);
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
                insert_clip(&mut *connection, score_id, uid, id, &row).await?;
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
    for id in stored.clips.keys().filter(|id| !candidate.clips.contains_key(*id)) {
        sqlx::query("DELETE FROM clips WHERE id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(|error| format!("failed to delete clip {id}: {error}"))?;
        changed.deleted += 1;
    }

    for (id, definition) in &candidate.definitions {
        let source = json(definition)?;
        match stored.definitions.get(id) {
            None => {
                sqlx::query(
                    "INSERT INTO score_definitions (id, uid, score_id, definition_json)
                     VALUES (?, ?, ?, ?)",
                )
                .bind(id)
                .bind(uid)
                .bind(score_id)
                .bind(&source)
                .execute(&mut *connection)
                .await
                .map_err(|error| format!("failed to insert definition {id}: {error}"))?;
                changed.inserted += 1;
            }
            Some(current) if json(current)? != source => {
                sqlx::query("UPDATE score_definitions SET definition_json = ? WHERE id = ?")
                    .bind(&source)
                    .bind(id)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| format!("failed to update definition {id}: {error}"))?;
                changed.updated += 1;
            }
            Some(_) => {}
        }
    }
    for id in stored
        .definitions
        .keys()
        .filter(|id| !candidate.definitions.contains_key(*id))
    {
        sqlx::query("DELETE FROM score_definitions WHERE id = ?")
            .bind(id)
            .execute(&mut *connection)
            .await
            .map_err(|error| format!("failed to delete definition {id}: {error}"))?;
        changed.deleted += 1;
    }

    // The score row is what "last worked on" reads, and what the change log
    // records one entry per save under. Only touched when something moved.
    if changed.any() {
        sqlx::query("UPDATE scores SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?")
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
        graph: get(row, "graph")?,
        start: get(row, "start")?,
        duration: get(row, "duration")?,
        seed: get(row, "seed")?,
        selection_seed: get(row, "selection_seed")?,
        selection_json: get(row, "selection_json")?,
        z_index: get(row, "z_index")?,
        blend_mode: get(row, "blend_mode")?,
        inputs_json: get(row, "inputs_json")?,
    })
}

async fn insert_clip(
    connection: &mut SqliteConnection,
    score_id: &str,
    uid: &str,
    id: &str,
    row: &ClipRow,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO clips (id, uid, score_id, graph, start, duration, seed, selection_seed,
                            selection_json, z_index, blend_mode, inputs_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(uid)
    .bind(score_id)
    .bind(&row.graph)
    .bind(row.start)
    .bind(row.duration)
    .bind(&row.seed)
    .bind(row.selection_seed.as_deref())
    .bind(&row.selection_json)
    .bind(row.z_index)
    .bind(&row.blend_mode)
    .bind(&row.inputs_json)
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

/// Finish the row-model cutover for documents the migration could not split.
///
/// SQL cannot do this: a clip's `seed` is a full u64 and `json_extract` returns
/// it through a double, and a document written before the current
/// [`Score::VERSION`] needs the Rust migration in `luma_patterns`. So the
/// migration parked every stored document in `scores_pending_cutover` and this
/// converts them once, on the next startup. A document that cannot be read is
/// left parked rather than dropped — the bytes are the only copy.
pub async fn cutover(pool: &sqlx::SqlitePool) -> Result<(), String> {
    let pending: Vec<(String, String)> =
        sqlx::query_as("SELECT score_id, document_json FROM scores_pending_cutover")
            .fetch_all(pool)
            .await
            .map_err(|error| format!("failed to read the pending score cutover: {error}"))?;
    for (score_id, document) in pending {
        let converted = convert(&document);
        let Some(score) = converted.as_ref().ok() else {
            log::warn!(
                "[cutover] leaving score {score_id} parked: {}",
                converted.unwrap_err()
            );
            continue;
        };
        let uid: Option<String> = sqlx::query_scalar("SELECT uid FROM scores WHERE id = ?")
            .bind(&score_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| error.to_string())?
            .flatten();
        let mut transaction = pool
            .begin()
            .await
            .map_err(|error| format!("failed to open the cutover transaction: {error}"))?;
        save_score(&mut transaction, &score_id, uid.as_deref().unwrap_or(""), score).await?;
        sqlx::query("DELETE FROM scores_pending_cutover WHERE score_id = ?")
            .bind(&score_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit the cutover: {error}"))?;
    }
    Ok(())
}

fn convert(document: &str) -> Result<Score, String> {
    let score: Score = serde_json::from_str(document)
        .map_err(|error| format!("unreadable score document: {error}"))?;
    if score.version() == Score::VERSION {
        return Ok(score);
    }
    luma_patterns::migration::upgrade(&score).map_err(|error| error.to_string())
}
