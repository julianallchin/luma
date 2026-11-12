use sqlx::{FromRow, SqliteConnection, SqlitePool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::database::local::deletes;
use crate::models::beat_grid::BeatGrid;
use crate::models::tracks::{ChordSection, TrackBeats, TrackBrowserRow, TrackStem, TrackSummary};
use crate::sync::triggers::{self, AttributedWrite, Attribution};

// Helper structs for internal queries
#[derive(FromRow)]
pub struct TrackPathAndHash {
    pub file_path: String,
    pub track_hash: String,
}

#[derive(FromRow)]
pub struct TrackDeletionPlan {
    pub file_path: String,
    pub album_art_path: Option<String>,
    pub track_hash: String,
    pub delete_audio: bool,
    pub delete_album_art: bool,
    pub delete_hash_artifacts: bool,
}

// -----------------------------------------------------------------------------
// Track records
// -----------------------------------------------------------------------------

pub async fn list_tracks_enriched(
    pool: &SqlitePool,
    venue_id: Option<&str>,
) -> Result<Vec<TrackBrowserRow>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open enriched track read: {error}"))?;
    list_tracks_enriched_for_connection(&mut connection, venue_id).await
}

pub async fn list_tracks_enriched_for_connection(
    connection: &mut SqliteConnection,
    venue_id: Option<&str>,
) -> Result<Vec<TrackBrowserRow>, String> {
    let vid = venue_id.unwrap_or("");
    let mut rows = sqlx::query_as::<_, TrackBrowserRow>(concat!(
        "SELECT
            t.id, t.uid, t.title, t.artist, t.album, t.duration_seconds,
            t.album_art_path, t.album_art_mime, t.source_type, t.file_path, t.created_at,
            tb.bpm,
            COALESCE(ac.cnt, 0) AS annotation_count,
            COALESCE(vac.cnt, 0) AS venue_annotation_count,
            COALESCE(vsc.cnt, 0) AS venue_score_count,
            COALESCE(vsc.cnt, 0) > 0 AS is_in_venue,
            (t.storage_path IS NOT NULL) AS has_storage
         FROM tracks t
         JOIN auth_visible_tracks visible ON visible.track_id = t.id
         LEFT JOIN track_beats tb ON tb.id = ",
        current_grid!("t.id"),
        "
         LEFT JOIN (
             SELECT s.track_id,
                 SUM((SELECT COUNT(*) FROM clips WHERE clips.score_id = s.id)) AS cnt
             FROM scores s
             JOIN auth_venue_access access ON access.venue_id = s.venue_id
             GROUP BY s.track_id
         ) ac ON ac.track_id = t.id
         LEFT JOIN (
             SELECT s.track_id,
                 SUM((SELECT COUNT(*) FROM clips WHERE clips.score_id = s.id)) AS cnt
             FROM scores s
             JOIN auth_venue_access access ON access.venue_id = s.venue_id
             WHERE s.venue_id = ?
             GROUP BY s.track_id
         ) vac ON vac.track_id = t.id
         LEFT JOIN (
             SELECT s.track_id, COUNT(s.id) AS cnt
             FROM scores s
             JOIN auth_venue_access access ON access.venue_id = s.venue_id
             WHERE s.venue_id = ?
             GROUP BY s.track_id
         ) vsc ON vsc.track_id = t.id
         ORDER BY t.created_at DESC, t.id DESC",
    ))
    .bind(vid)
    .bind(vid)
    .fetch_all(&mut *connection)
    .await
    .map_err(|e| format!("Failed to list enriched tracks: {}", e))?;

    // Coverage: union of annotation intervals per track, scoped to the
    // active venue. Done in Rust because SQLite gaps-and-islands SQL is
    // noisy and this set is small.
    if !vid.is_empty() {
        let mut intervals: Vec<(String, f64, f64)> = Vec::new();
        let clips: Vec<(String, String, f64, f64)> = sqlx::query_as(
            "SELECT s.id, s.track_id, clip.start, clip.duration
             FROM scores s
             JOIN clips clip ON clip.score_id = s.id
             JOIN auth_venue_access access ON access.venue_id = s.venue_id
             WHERE s.venue_id = ?
             ORDER BY s.track_id, s.id, clip.start",
        )
        .bind(vid)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| format!("Failed to load annotation intervals: {error}"))?;
        let mut clocks = HashMap::new();
        // Clips are in beats of the grid their score was written against.
        for (score_id, track_id, start, duration) in clips {
            if !clocks.contains_key(&score_id) {
                let grid = get_track_beats_for_connection(
                    connection,
                    Grid::of_score(&track_id, &score_id),
                )
                .await?;
                clocks.insert(
                    score_id.clone(),
                    grid.map(|grid| grid.timeline())
                        .transpose()
                        .map_err(|error| error.to_string())?,
                );
            }
            // A score may arrive before its beat metadata. Its clip count is
            // already known; time coverage waits for that grid.
            let Some(clock) = clocks[&score_id].as_ref() else {
                continue;
            };
            intervals.push((
                track_id.clone(),
                clock.seconds_at(start).map_err(|error| error.to_string())?,
                clock
                    .seconds_at(start + duration)
                    .map_err(|error| error.to_string())?,
            ));
        }

        intervals.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.total_cmp(&right.1)));
        let mut coverage: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        let mut cur_track: Option<String> = None;
        let mut cur_start = 0.0_f64;
        let mut cur_end = 0.0_f64;
        let mut acc = 0.0_f64;
        for (track_id, start, end) in intervals {
            if start >= end {
                continue;
            }
            if cur_track.as_deref() != Some(track_id.as_str()) {
                if let Some(prev) = cur_track.take() {
                    acc += cur_end - cur_start;
                    *coverage.entry(prev).or_insert(0.0) += acc;
                }
                cur_track = Some(track_id);
                cur_start = start;
                cur_end = end;
                acc = 0.0;
            } else if start <= cur_end {
                if end > cur_end {
                    cur_end = end;
                }
            } else {
                acc += cur_end - cur_start;
                cur_start = start;
                cur_end = end;
            }
        }
        if let Some(prev) = cur_track.take() {
            acc += cur_end - cur_start;
            *coverage.entry(prev).or_insert(0.0) += acc;
        }

        for row in rows.iter_mut() {
            if let Some(c) = coverage.get(&row.id) {
                row.venue_annotation_coverage_seconds = *c;
            }
        }
    }

    Ok(rows)
}

pub async fn get_track_by_id(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT t.id, t.uid, t.track_hash, t.title, t.artist, t.album, t.track_number,
                t.disc_number, t.duration_seconds, t.file_path, t.storage_path,
                t.album_art_path, t.album_art_mime, t.album_art_storage_path,
                t.source_type, t.source_id, t.source_filename, t.created_at, t.updated_at
         FROM tracks t JOIN auth_visible_tracks visible ON visible.track_id = t.id
         WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch track by id: {}", e))
}

/// The track `uid` already has for this audio: the one imported from the
/// same source item, else the one with the same content hash. Ownership is
/// part of both identities: separate principals may legitimately import the
/// same source item or bytes.
///
/// [`insert_track_record`] decides with this inside its transaction. An
/// import may also ask first, to skip work on a song it already has; that
/// answer is a shortcut, and the insert still has the last word.
pub async fn existing_track(
    connection: &mut sqlx::SqliteConnection,
    uid: &str,
    track_hash: &str,
    source: Option<(&str, &str)>,
) -> Result<Option<String>, String> {
    if let Some((source_type, source_id)) = source {
        let source_match = sqlx::query_scalar::<_, String>(
            "SELECT id FROM tracks
             WHERE uid = ? AND source_type = ? AND source_id = ?
             LIMIT 1",
        )
        .bind(uid)
        .bind(source_type)
        .bind(source_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| format!("Failed to resolve track source identity: {error}"))?;
        if source_match.is_some() {
            return Ok(source_match);
        }
    }
    sqlx::query_scalar::<_, String>(
        "SELECT id FROM tracks WHERE uid = ? AND track_hash = ? LIMIT 1",
    )
    .bind(uid)
    .bind(track_hash)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| format!("Failed to resolve track content identity: {error}"))
}

pub async fn insert_track_record(
    pool: &SqlitePool,
    track_hash: &str,
    title: &Option<String>,
    artist: &Option<String>,
    album: &Option<String>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    duration_seconds: Option<f64>,
    file_path: &str,
    album_art_path: &Option<String>,
    album_art_mime: &Option<String>,
    uid: &str,
    source_type: Option<&str>,
    source_id: Option<&str>,
    source_filename: Option<&str>,
) -> Result<(String, bool), String> {
    let id = Uuid::new_v4().to_string();

    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin track insert: {error}"))?;
    let admitted: Option<String> = sqlx::query_scalar(
        "SELECT active_uid FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND remote_writes = 0 AND active_uid = ?",
    )
    .bind(uid)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize track insert: {error}"))?;
    if admitted.is_none() {
        return Err("Authenticated identity changed while importing track".into());
    }

    // Deduplication belongs to this immediate transaction. The write lock
    // makes the lookup plus insert one atomic conflict-resolution operation,
    // so two concurrent imports cannot both observe absence and publish two
    // rows.
    let existing = existing_track(
        &mut transaction,
        uid,
        track_hash,
        source_type.zip(source_id),
    )
    .await?;
    if let Some(id) = existing {
        transaction
            .rollback()
            .await
            .map_err(|error| format!("Failed to close duplicate track insert: {error}"))?;
        return Ok((id, false));
    }

    // The row is what the import read off the file, not something the
    // person typed, and history says so. Cleared before the commit, so the
    // connection goes back to the pool as the person's.
    triggers::attribute(&mut transaction, Attribution::IMPORT).await?;
    sqlx::query(
        "INSERT INTO tracks (id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, uid, source_type, source_id, source_filename) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(track_hash)
    .bind(title)
    .bind(artist)
    .bind(album)
    .bind(track_number)
    .bind(disc_number)
    .bind(duration_seconds)
    .bind(file_path)
    .bind(album_art_path)
    .bind(album_art_mime)
    .bind(uid)
    .bind(source_type)
    .bind(source_id)
    .bind(source_filename)
    .execute(&mut *transaction)
    .await
    .map_err(|e| format!("Failed to insert track: {}", e))?;
    triggers::unattribute(&mut transaction).await?;

    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit track insert: {error}"))?;

    Ok((id, true))
}

/// Resolve and lock every decision needed to delete one track. Callers must
/// hold an immediate transaction across filesystem staging and the matching
/// row delete so another import cannot acquire a shared hash/path in between.
pub async fn prepare_track_deletion(
    connection: &mut SqliteConnection,
    track_id: &str,
    owner_user_id: &str,
) -> Result<Option<TrackDeletionPlan>, String> {
    let row = sqlx::query_as::<_, TrackDeletionPlan>(
        "SELECT t.file_path, t.album_art_path, t.track_hash,
            NOT EXISTS(
                SELECT 1 FROM tracks other
                WHERE other.id <> t.id AND other.file_path = t.file_path
            ) AS delete_audio,
            CASE WHEN t.album_art_path IS NULL THEN 0 ELSE NOT EXISTS(
                SELECT 1 FROM tracks other
                WHERE other.id <> t.id
                  AND other.album_art_path = t.album_art_path
            ) END AS delete_album_art,
            NOT EXISTS(
                SELECT 1 FROM tracks other
                WHERE other.id <> t.id AND other.track_hash = t.track_hash
            ) AS delete_hash_artifacts
         FROM tracks t WHERE t.id = ? AND t.uid = ?",
    )
    .bind(track_id)
    .bind(owner_user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|e| format!("Failed to fetch track info: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let scores: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scores WHERE track_id = ?")
        .bind(track_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|e| format!("Failed to inspect track scores: {e}"))?;
    if scores != 0 {
        return Err(
            "Track still owns scores; delete only empty, non-authored track containers".into(),
        );
    }
    let threads: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_threads
         WHERE subject_kind = 'track' AND subject_id = ?",
    )
    .bind(track_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|e| format!("Failed to inspect track conversations: {e}"))?;
    if threads != 0 {
        return Err(
            "Track still owns durable conversations; delete those conversations first".into(),
        );
    }
    Ok(Some(row))
}

pub async fn delete_prepared_track_record(
    connection: &mut SqliteConnection,
    track_id: &str,
    owner_user_id: &str,
) -> Result<u64, String> {
    let deleted = deletes::delete_where(
        connection,
        "tracks",
        "id = ? AND uid = ?",
        &[track_id, owner_user_id],
    )
    .await
    .map_err(|e| format!("Failed to delete prepared track: {e}"))?;
    Ok(deleted as u64)
}

// -----------------------------------------------------------------------------
// Track metadata helpers
// -----------------------------------------------------------------------------

pub async fn get_track_path_and_hash(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<TrackPathAndHash, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open track path read: {error}"))?;
    get_track_path_and_hash_for_connection(&mut connection, track_id).await
}

pub async fn get_track_path_and_hash_for_connection(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<TrackPathAndHash, String> {
    sqlx::query_as::<_, TrackPathAndHash>(
        "SELECT track.file_path, track.track_hash
         FROM tracks track
         JOIN auth_visible_tracks visible ON visible.track_id = track.id
         WHERE track.id = ?",
    )
    .bind(track_id)
    .fetch_optional(connection)
    .await
    .map_err(|e| format!("Failed to fetch track path: {}", e))?
    .ok_or_else(|| format!("Track {} not found", track_id))
}

pub async fn get_track_duration(pool: &SqlitePool, track_id: &str) -> Result<Option<f64>, String> {
    sqlx::query_scalar(
        "SELECT track.duration_seconds
         FROM tracks track
         JOIN auth_visible_tracks visible ON visible.track_id = track.id
         WHERE track.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to get track duration: {}", e))
}

// -----------------------------------------------------------------------------
// Beats / Roots / Stems persistence
// -----------------------------------------------------------------------------

/// Fetch the uid for a track (used to propagate auth.uid() to child rows).
pub(crate) async fn track_uid(
    executor: impl sqlx::SqliteExecutor<'_>,
    track_id: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT uid FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_one(executor)
        .await
        .map_err(|e| format!("Failed to get track uid: {}", e))
}

/// The columns of [`TrackBeats`], from a `track_beats` row aliased `beats`.
macro_rules! grid_columns {
    () => {
        "beats.id, beats.track_id, beats.uid, beats.beats_json, beats.downbeats_json,
         beats.bpm, beats.downbeat_offset, beats.beats_per_bar, beats.processor_version,
         beats.created_at, beats.updated_at"
    };
}

/// Record `grid` as `track_id`'s beat grid, as version `processor_version`
/// of the detector found it.
///
/// A grid is written once and never edited (the server refuses an update), and
/// scores name the one they were written against, so this adds a row rather
/// than replacing one. The row it adds is the track's current grid
/// ([`current_grid`]). A detection that finds the grid the track already has
/// writes nothing: analysing a song again must not grow the table, or the
/// server's history, with copies.
pub async fn record_track_beats(
    connection: &mut SqliteConnection,
    track_id: &str,
    grid: &BeatGrid,
    processor_version: u32,
) -> Result<(), String> {
    let current = sqlx::query_as::<_, TrackBeats>(concat!(
        "SELECT ",
        grid_columns!(),
        " FROM track_beats beats WHERE beats.id = ",
        current_grid!("?")
    ))
    .bind(track_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|e| format!("Failed to read the current beat grid: {e}"))?;
    let replaced = match current {
        Some(current) => {
            // A newer detector that agrees still writes its row: the stamp is
            // what tells reconcile the track needs no further run, and the
            // row that carries the old one cannot be edited.
            let stamped = current.processor_version >= i64::from(processor_version);
            let created_at = current.created_at.clone();
            if stamped && beat_grid(current)? == *grid {
                return Ok(());
            }
            Some(created_at)
        }
        None => None,
    };
    let beats_json = serde_json::to_string(&grid.beats)
        .map_err(|e| format!("Failed to serialize beats: {e}"))?;
    let downbeats_json = serde_json::to_string(&grid.downbeats)
        .map_err(|e| format!("Failed to serialize downbeats: {e}"))?;
    // Stamped after the grid it replaces even if this clock is behind the one
    // that wrote that grid, so the row just written is the current one.
    sqlx::query(
        "INSERT INTO track_beats
            (id, track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset,
             beats_per_bar, processor_version, created_at, updated_at)
         SELECT ?1, ?2, (SELECT uid FROM tracks WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8,
                stamp.at, stamp.at
         FROM (SELECT strftime('%Y-%m-%dT%H:%M:%fZ', MAX(
                   julianday('now'),
                   COALESCE(julianday(?9) + 2.0 / 86400000.0, 0))) AS at) stamp",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(track_id)
    .bind(beats_json)
    .bind(downbeats_json)
    .bind(f64::from(grid.bpm))
    .bind(f64::from(grid.downbeat_offset))
    .bind(i64::from(grid.beats_per_bar))
    .bind(i64::from(processor_version))
    .bind(replaced)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to persist beat data: {}", e))?;

    Ok(())
}

pub async fn upsert_track_roots(
    connection: &mut SqliteConnection,
    track_id: &str,
    sections_json: &str,
    logits_path: Option<&str>,
    processor_version: u32,
) -> Result<(), String> {
    let uid = track_uid(&mut *connection, track_id).await?;
    sqlx::query(
        "INSERT INTO track_roots (track_id, uid, sections_json, logits_path, processor_version)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            sections_json = excluded.sections_json,
            logits_path = excluded.logits_path,
            processor_version = excluded.processor_version,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(sections_json)
    .bind(logits_path)
    .bind(processor_version as i64)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to persist root data: {}", e))?;

    Ok(())
}

pub async fn upsert_track_bar_classifications(
    connection: &mut SqliteConnection,
    track_id: &str,
    classifications_json: &str,
    tag_order_json: &str,
    processor_version: u32,
) -> Result<(), String> {
    let uid = track_uid(&mut *connection, track_id).await?;
    sqlx::query(
        "INSERT INTO track_bar_classifications (track_id, uid, classifications_json, tag_order_json, processor_version)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            classifications_json = excluded.classifications_json,
            tag_order_json = excluded.tag_order_json,
            processor_version = excluded.processor_version,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(classifications_json)
    .bind(tag_order_json)
    .bind(processor_version as i64)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to persist bar classifications: {}", e))?;

    Ok(())
}

pub async fn upsert_track_genres(
    connection: &mut SqliteConnection,
    track_id: &str,
    genres_json: &str,
    labels_json: &str,
    processor_version: u32,
) -> Result<(), String> {
    let uid = track_uid(&mut *connection, track_id).await?;
    sqlx::query(
        "INSERT INTO track_genres (track_id, uid, genres_json, labels_json, processor_version)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            genres_json = excluded.genres_json,
            labels_json = excluded.labels_json,
            processor_version = excluded.processor_version,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(genres_json)
    .bind(labels_json)
    .bind(processor_version as i64)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to persist genres: {}", e))?;

    Ok(())
}

/// Raw `(genres_json, labels_json)` for a track, or `None` when the genre
/// preprocessor hasn't produced a row.
pub async fn get_track_genres_raw(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<(String, String)>, String> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT g.genres_json, g.labels_json
         FROM track_genres g
         JOIN auth_visible_tracks visible ON visible.track_id = g.track_id
         WHERE g.track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch genres: {}", e))?;
    Ok(row)
}

/// Read drum onsets for a track. Returns `None` if no row exists (e.g. drum
/// transcription hasn't run yet). Keys are the n2n class names `kick`, `snare`,
/// `hat`, `cymbal` — note `hat` is singular here, while the bar classifier's
/// hi-hat tag is spelled `hats`; different models, different vocabularies.
/// Values are sorted timestamps in seconds.
pub async fn get_track_drum_onsets(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<std::collections::HashMap<String, Vec<f32>>>, String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT onset.onsets_json
             FROM track_drum_onsets onset
             JOIN auth_visible_tracks visible ON visible.track_id = onset.track_id
             WHERE onset.track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load drum onsets: {}", e))?;

    let Some((onsets_json,)) = row else {
        return Ok(None);
    };

    let parsed = serde_json::from_str(&onsets_json)
        .map_err(|e| format!("Failed to parse drum onsets JSON: {}", e))?;
    Ok(Some(parsed))
}

pub async fn upsert_track_drum_onsets(
    connection: &mut SqliteConnection,
    track_id: &str,
    onsets_json: &str,
    processor_version: u32,
) -> Result<(), String> {
    let uid = track_uid(&mut *connection, track_id).await?;
    sqlx::query(
        "INSERT INTO track_drum_onsets (track_id, uid, onsets_json, processor_version)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            onsets_json = excluded.onsets_json,
            processor_version = excluded.processor_version,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(onsets_json)
    .bind(processor_version as i64)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to persist drum onsets: {}", e))?;

    Ok(())
}

pub async fn upsert_track_mert(
    pool: &SqlitePool,
    track_id: &str,
    fullmix_path: &str,
    drum_path: &str,
    processor_version: u32,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO track_mert (track_id, file_path, drum_path, processor_version)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            file_path = excluded.file_path,
            drum_path = excluded.drum_path,
            processor_version = excluded.processor_version,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(fullmix_path)
    .bind(drum_path)
    .bind(processor_version as i64)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist MERT cache row: {}", e))?;

    Ok(())
}

/// Returns `(fullmix_path, drum_path)` for a track's MERT cache, or `None`
/// if no row exists. Both paths are populated by the `mert` preprocessor.
pub async fn get_track_mert_paths(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<(String, String)>, String> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT mert.file_path, mert.drum_path
             FROM track_mert mert
             JOIN auth_visible_tracks visible ON visible.track_id = mert.track_id
             WHERE mert.track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch MERT cache row: {}", e))?;
    Ok(row)
}

pub async fn upsert_track_stem(
    connection: &mut SqliteConnection,
    track_id: &str,
    stem_name: &str,
    file_path: &str,
    storage_path: Option<&str>,
    processor_version: u32,
) -> Result<(), String> {
    let uid = track_uid(&mut *connection, track_id).await?;
    sqlx::query(
        "INSERT INTO track_stems (track_id, uid, stem_name, file_path, storage_path, processor_version)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id, stem_name) DO UPDATE SET
            uid = excluded.uid,
            file_path = excluded.file_path,
            storage_path = excluded.storage_path,
            processor_version = excluded.processor_version,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(stem_name)
    .bind(file_path)
    .bind(storage_path)
    .bind(processor_version as i64)
    .execute(&mut *connection)
    .await
    .map_err(|e| format!("Failed to persist stem data: {}", e))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Queries used by services
// -----------------------------------------------------------------------------

pub async fn get_track_stems(pool: &SqlitePool, track_id: &str) -> Result<Vec<TrackStem>, String> {
    sqlx::query_as::<_, TrackStem>(
        "SELECT stem.track_id, stem.uid, stem.stem_name, stem.file_path,
                stem.storage_path, stem.created_at, stem.updated_at
         FROM track_stems stem
         JOIN auth_visible_tracks visible ON visible.track_id = stem.track_id
         WHERE stem.track_id = ?",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load stems for track {}: {}", track_id, e))
}

/// Parsed chord sections from `track_roots.sections_json`, ordered as stored.
///
/// `Ok(None)` means the track has no roots row at all (analysis hasn't run) —
/// distinct from `Ok(Some(vec![]))`, which means the analysis produced nothing.
/// Malformed JSON or a malformed entry is skipped rather than failing the load:
/// harmony is an optional enrichment, and one bad section must not take down a
/// graph evaluation.
pub async fn get_track_chord_sections(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<Vec<ChordSection>>, String> {
    let json: Option<String> = sqlx::query_scalar(
        "SELECT roots.sections_json
             FROM track_roots roots
             JOIN auth_visible_tracks visible ON visible.track_id = roots.track_id
             WHERE roots.track_id = ? LIMIT 1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch chord sections: {e}"))?;
    let Some(json) = json else {
        return Ok(None);
    };
    Ok(Some(parse_chord_sections(&json).unwrap_or_else(|| {
        log::warn!("[tracks] unparsable sections_json for track {track_id}");
        Vec::new()
    })))
}

/// Parse `track_roots.sections_json`. `None` when the blob isn't a JSON array;
/// an entry that is not a section (no `start`/`end`) is dropped on its own, so
/// one bad entry costs one section rather than the track's chords.
fn parse_chord_sections(json: &str) -> Option<Vec<ChordSection>> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    Some(
        entries
            .into_iter()
            .filter_map(|entry| serde_json::from_value(entry).ok())
            .collect(),
    )
}

/// The id of a track's current beat grid, as SQL: its newest. `$track` is
/// the SQL for the track's id.
///
/// `julianday()` rather than the raw column because one instant is spelled
/// more than one way (a row this device wrote, a row another wrote, a row
/// from before the stamps were ISO-8601), and text order files `T` above a
/// space whatever the clock says. `rowid` settles two grids stamped in the
/// same millisecond as the one this database learned of last.
///
/// "Newest" is defined here and nowhere else. A macro so the statements that
/// use it stay one `concat!`-ed literal.
macro_rules! current_grid {
    ($track:literal) => {
        concat!(
            "(SELECT newest.id FROM track_beats newest WHERE newest.track_id = ",
            $track,
            " ORDER BY julianday(newest.created_at) DESC, newest.rowid DESC LIMIT 1)"
        )
    };
}
pub(crate) use current_grid;

/// Which of a track's beat grids a read means. A track has one per detection
/// that found something new, and every read of `track_beats` says which.
#[derive(Clone, Copy, Debug)]
pub struct Grid<'a> {
    track_id: &'a str,
    score_id: Option<&'a str>,
}

impl<'a> Grid<'a> {
    /// The track's current grid: its newest. What a read with no score in
    /// hand means.
    pub fn current(track_id: &'a str) -> Self {
        Self {
            track_id,
            score_id: None,
        }
    }

    /// The grid `score_id` was written against (`scores.grid_id`), so its
    /// clips stay where they were put when the song is analysed again. A
    /// score that names no grid, or one this device does not hold, reads its
    /// track's current grid.
    pub fn of_score(track_id: &'a str, score_id: &'a str) -> Self {
        Self {
            track_id,
            score_id: Some(score_id),
        }
    }

    /// [`Grid::of_score`] when a score is in scope, else [`Grid::current`].
    pub fn in_scope(track_id: &'a str, score_id: Option<&'a str>) -> Self {
        Self { track_id, score_id }
    }

    pub fn track_id(&self) -> &'a str {
        self.track_id
    }
}

pub async fn get_track_beats_raw(
    pool: &SqlitePool,
    grid: Grid<'_>,
) -> Result<Option<TrackBeats>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open beat data read: {error}"))?;
    get_track_beats_raw_for_connection(&mut connection, grid).await
}

async fn get_track_beats_raw_for_connection(
    connection: &mut SqliteConnection,
    grid: Grid<'_>,
) -> Result<Option<TrackBeats>, String> {
    sqlx::query_as::<_, TrackBeats>(concat!(
        "SELECT ",
        grid_columns!(),
        " FROM track_beats beats
         JOIN auth_visible_tracks visible ON visible.track_id = beats.track_id
         WHERE beats.id = COALESCE(
             (SELECT written.id FROM scores score
                JOIN track_beats written ON written.id = score.grid_id
               WHERE score.id = ?2 AND written.track_id = ?1),
             ",
        current_grid!("?1"),
        ")"
    ))
    .bind(grid.track_id)
    .bind(grid.score_id)
    .fetch_optional(connection)
    .await
    .map_err(|e| format!("Failed to fetch beat data: {}", e))
}

/// One of a track's beat grids; `None` when beat detection has not run for
/// the track.
pub async fn get_track_beats(
    pool: &SqlitePool,
    grid: Grid<'_>,
) -> Result<Option<BeatGrid>, String> {
    get_track_beats_raw(pool, grid)
        .await?
        .map(beat_grid)
        .transpose()
}

/// [`get_track_beats`] through a connection the caller already holds.
pub(crate) async fn get_track_beats_for_connection(
    connection: &mut SqliteConnection,
    grid: Grid<'_>,
) -> Result<Option<BeatGrid>, String> {
    get_track_beats_raw_for_connection(connection, grid)
        .await?
        .map(beat_grid)
        .transpose()
}

/// The id of `track_id`'s current grid, which a score made now is written
/// against. `None` until beat detection has run.
pub(crate) async fn current_grid_id(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar(concat!("SELECT ", current_grid!("?")))
        .bind(track_id)
        .fetch_one(connection)
        .await
        .map_err(|e| format!("Failed to find the track's current beat grid: {e}"))
}

/// The detector version that found `grid`, when it is one of `track_id`'s
/// grids: the newest that matches. A person reviews the grid on their screen,
/// which is their score's and not always the track's current one.
pub(crate) async fn grid_version_for_connection(
    connection: &mut SqliteConnection,
    track_id: &str,
    grid: &BeatGrid,
) -> Result<Option<i64>, String> {
    let rows = sqlx::query_as::<_, TrackBeats>(concat!(
        "SELECT ",
        grid_columns!(),
        " FROM track_beats beats WHERE beats.track_id = ?
         ORDER BY julianday(beats.created_at) DESC, beats.rowid DESC"
    ))
    .bind(track_id)
    .fetch_all(connection)
    .await
    .map_err(|e| format!("Failed to fetch the track's beat grids: {e}"))?;
    for row in rows {
        let version = row.processor_version;
        if beat_grid(row)? == *grid {
            return Ok(Some(version));
        }
    }
    Ok(None)
}

/// The grid a row stores, with any metadata it lacks inferred from its beats.
fn beat_grid(track_beats: TrackBeats) -> Result<BeatGrid, String> {
    let beats: Vec<f32> = serde_json::from_str(&track_beats.beats_json)
        .map_err(|e| format!("Failed to parse beats: {}", e))?;
    let downbeats: Vec<f32> = serde_json::from_str(&track_beats.downbeats_json)
        .map_err(|e| format!("Failed to parse downbeats: {}", e))?;
    let (fallback_bpm, fallback_offset, fallback_bpb) = infer_grid_metadata(&beats, &downbeats);
    let bpm_value = track_beats.bpm.unwrap_or(fallback_bpm as f64) as f32;
    let offset_value = track_beats
        .downbeat_offset
        .unwrap_or(fallback_offset as f64) as f32;
    let bpb_value = track_beats.beats_per_bar.unwrap_or(fallback_bpb as i64) as i32;
    Ok(BeatGrid {
        beats,
        downbeats,
        bpm: bpm_value,
        downbeat_offset: offset_value,
        beats_per_bar: bpb_value,
    })
}

fn infer_grid_metadata(beats: &[f32], downbeats: &[f32]) -> (f32, f32, i64) {
    if beats.len() < 2 {
        let offset = downbeats.first().cloned().unwrap_or(0.0);
        return (0.0, offset, 4);
    }
    let mut intervals: Vec<f32> = beats.windows(2).map(|w| (w[1] - w[0]).max(1e-6)).collect();
    intervals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = intervals[intervals.len() / 2];
    let bpm = if median > 0.0 { 60.0 / median } else { 0.0 };
    let offset = downbeats
        .first()
        .copied()
        .unwrap_or_else(|| beats.first().copied().unwrap_or(0.0));
    let beats_per_bar = if downbeats.len() >= 2 && median > 0.0 {
        let bar_intervals: Vec<f32> = downbeats
            .windows(2)
            .map(|w| (w[1] - w[0]).max(1e-6))
            .collect();
        let mut sorted = bar_intervals.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let bar_med = sorted[sorted.len() / 2];
        let est = (bar_med / median).round().clamp(1.0, 16.0);
        est as i64
    } else {
        4
    };
    (bpm, offset, beats_per_bar)
}

pub async fn get_track_bar_classifications_raw(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<(String, String)>, String> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT bars.classifications_json, bars.tag_order_json
         FROM track_bar_classifications bars
         JOIN auth_visible_tracks visible ON visible.track_id = bars.track_id
         WHERE bars.track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch bar classifications: {}", e))?;
    Ok(row)
}

/// A visible track with its BPM, as the live-follow matcher compares them.
#[derive(sqlx::FromRow)]
pub struct TrackWithBpm {
    pub id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub bpm: Option<f64>,
}

/// Visible tracks to match a deck's track against: every one, or with
/// `duration` as `(seconds, tolerance)`, those within the tolerance.
pub async fn get_match_candidates(
    pool: &SqlitePool,
    duration: Option<(f64, f64)>,
) -> Result<Vec<TrackWithBpm>, String> {
    let (target, tolerance) = duration.unzip();
    sqlx::query_as::<_, TrackWithBpm>(concat!(
        "SELECT t.id, t.title, t.artist, tb.bpm
         FROM tracks t
         JOIN auth_visible_tracks visible ON visible.track_id = t.id
         LEFT JOIN track_beats tb ON tb.id = ",
        current_grid!("t.id"),
        "
         WHERE ?1 IS NULL OR ABS(COALESCE(t.duration_seconds, 0) - ?1) <= ?2",
    ))
    .bind(target)
    .bind(tolerance)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to fetch tracks to match: {e}"))
}

pub async fn get_tracks_by_source_filename(
    pool: &SqlitePool,
    filename: &str,
) -> Result<Vec<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT t.id, t.uid, t.track_hash, t.title, t.artist, t.album, t.track_number,
                t.disc_number, t.duration_seconds, t.file_path, t.storage_path,
                t.album_art_path, t.album_art_mime, t.album_art_storage_path,
                t.source_type, t.source_id, t.source_filename, t.created_at, t.updated_at
         FROM tracks t JOIN auth_visible_tracks visible ON visible.track_id = t.id
         WHERE t.source_filename = ?",
    )
    .bind(filename)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to fetch tracks by source_filename: {}", e))
}

pub async fn fill_track_metadata_gaps(
    pool: &SqlitePool,
    track_id: &str,
    title: &Option<String>,
    artist: &Option<String>,
    album: &Option<String>,
    duration_seconds: Option<f64>,
) -> Result<(), String> {
    // What the file's tags say, filled in by the import rather than typed.
    let mut write = AttributedWrite::begin(pool, Attribution::IMPORT).await?;
    sqlx::query(
        "UPDATE tracks SET
            title = COALESCE(title, ?),
            artist = COALESCE(artist, ?),
            album = COALESCE(album, ?),
            duration_seconds = COALESCE(duration_seconds, ?),
            updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(title)
    .bind(artist)
    .bind(album)
    .bind(duration_seconds)
    .bind(track_id)
    .execute(write.connection())
    .await
    .map_err(|e| format!("Failed to fill track metadata gaps: {}", e))?;
    write.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tracks.db");
        let migration_pool = SqlitePoolOptions::new()
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
            .run(&migration_pool)
            .await
            .unwrap();
        migration_pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        (directory, pool)
    }

    /// Four beats to the bar from zero at `bpm`, for one bar and a beat.
    fn grid_at(bpm: f32) -> BeatGrid {
        let beat = 60. / bpm;
        BeatGrid {
            beats: (0..5).map(|i| i as f32 * beat).collect(),
            downbeats: vec![0., 4. * beat],
            bpm,
            downbeat_offset: 0.,
            beats_per_bar: 4,
        }
    }

    async fn grid_count(pool: &SqlitePool, track_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM track_beats WHERE track_id = ?")
            .bind(track_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Alice's track `song` in her venue `room`, with her admitted.
    async fn song_in_a_room(pool: &SqlitePool) {
        sqlx::query(
            "INSERT INTO venues (id, uid, name) VALUES ('room', 'alice', 'Room');
             INSERT INTO tracks (id, uid, track_hash, file_path)
             VALUES ('song', 'alice', 'hash', '/song.mp3')",
        )
        .execute(pool)
        .await
        .unwrap();
        crate::database::local::auth::arm_write_admission(pool, Some("alice"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn analysing_again_adds_a_grid_only_when_it_finds_another() {
        let (_directory, pool) = test_pool().await;
        song_in_a_room(&pool).await;
        let current = || async { get_track_beats(&pool, Grid::current("song")).await.unwrap() };
        assert_eq!(current().await, None);

        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            7,
        )
        .await
        .unwrap();
        let first: String = sqlx::query_scalar("SELECT id FROM track_beats")
            .fetch_one(&pool)
            .await
            .unwrap();

        // The same detector finding the same grid writes nothing.
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            7,
        )
        .await
        .unwrap();
        assert_eq!(grid_count(&pool, "song").await, 1);

        // Another grid becomes the current one beside the first, unedited.
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(60.),
            7,
        )
        .await
        .unwrap();
        assert_eq!(grid_count(&pool, "song").await, 2);
        assert_eq!(current().await, Some(grid_at(60.)));
        let kept: (String, f64) =
            sqlx::query_as("SELECT beats_json, bpm FROM track_beats WHERE id = ?")
                .bind(&first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (serde_json::from_str::<Vec<f32>>(&kept.0).unwrap(), kept.1),
            (grid_at(120.).beats, 120.)
        );

        // Going back to an earlier grid is a change too.
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            7,
        )
        .await
        .unwrap();
        assert_eq!(grid_count(&pool, "song").await, 3);
        assert_eq!(current().await, Some(grid_at(120.)));

        // The song still lists once, at its current tempo.
        let listed = list_tracks_enriched(&pool, None).await.unwrap();
        assert_eq!(
            listed.iter().map(|row| row.bpm).collect::<Vec<_>>(),
            [Some(120.)]
        );
    }

    /// A grid row cannot be edited, so a newer detector that finds the same
    /// grid writes its own: otherwise the track would look unanalysed by it
    /// at every launch.
    #[tokio::test]
    async fn a_newer_detector_that_agrees_still_stamps_the_track() {
        let (_directory, pool) = test_pool().await;
        song_in_a_room(&pool).await;
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            7,
        )
        .await
        .unwrap();
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            8,
        )
        .await
        .unwrap();
        let versions: Vec<i64> = sqlx::query_scalar(
            "SELECT processor_version FROM track_beats WHERE track_id = 'song'
             ORDER BY processor_version",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(versions, [7, 8]);

        // And an older one that agrees has nothing to add.
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            7,
        )
        .await
        .unwrap();
        assert_eq!(grid_count(&pool, "song").await, 2);
    }

    /// The grid written last is current even when the one before it carries
    /// a later stamp, as a grid from a device whose clock runs ahead does.
    #[tokio::test]
    async fn the_grid_just_written_is_current_whatever_the_clocks_say() {
        let (_directory, pool) = test_pool().await;
        song_in_a_room(&pool).await;
        sqlx::query(
            "INSERT INTO track_beats (id, track_id, uid, beats_json, downbeats_json, created_at)
             VALUES ('ahead', 'song', 'alice', '[0,1,2,3,4]', '[0,4]', '2999-01-01 00:00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            7,
        )
        .await
        .unwrap();
        assert_eq!(
            get_track_beats(&pool, Grid::current("song")).await.unwrap(),
            Some(grid_at(120.))
        );
    }

    #[tokio::test]
    async fn a_score_reads_the_grid_it_was_written_against() {
        use crate::services::catalog;
        let (_directory, pool) = test_pool().await;
        song_in_a_room(&pool).await;
        let request = |n: u8| format!("00000000-0000-4000-8000-00000000000{n}");
        let beats = |score: &str| {
            let score = score.to_owned();
            let pool = pool.clone();
            async move {
                get_track_beats(&pool, Grid::of_score("song", &score))
                    .await
                    .unwrap()
            }
        };

        // A score made before the song is analysed names no grid, and reads
        // whichever is current.
        let early = catalog::create_score(
            &pool,
            &request(1),
            "song",
            "room",
            None,
            crate::sync::triggers::Attribution::PERSON,
        )
        .await
        .unwrap();
        assert_eq!(early.grid_id, None);
        assert_eq!(beats(&early.id).await, None);

        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(120.),
            7,
        )
        .await
        .unwrap();
        let written = catalog::create_score(
            &pool,
            &request(2),
            "song",
            "room",
            None,
            crate::sync::triggers::Attribution::PERSON,
        )
        .await
        .unwrap();
        assert!(written.grid_id.is_some());
        // Four beats of it, which the song list measures in seconds.
        sqlx::query(
            "INSERT INTO clips
             (id, uid, score_id, graph_json, graph, start, duration, seed, selection_json,
              blend_mode)
             VALUES (?2, 'alice', ?1, '{\"version\":1,\"nodes\":{}}', '', 0, 4, '0',
                     '{\"all\":true}', 'replace')",
        )
        .bind(&written.id)
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();
        let coverage = || async {
            list_tracks_enriched(&pool, Some("room")).await.unwrap()[0]
                .venue_annotation_coverage_seconds
        };
        let before = coverage().await;
        assert!(before > 0.);

        // The song is analysed again and found at half the tempo.
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "song",
            &grid_at(60.),
            7,
        )
        .await
        .unwrap();
        assert_eq!(beats(&written.id).await, Some(grid_at(120.)));
        assert_eq!(coverage().await, before);
        assert_eq!(beats(&early.id).await, Some(grid_at(60.)));

        // A copy holds the same clips, so it names the same grid; a new
        // score is written against the current one.
        let copy = catalog::duplicate_score(&pool, &request(3), &written.id, "Copy")
            .await
            .unwrap();
        assert_eq!(copy.grid_id, written.grid_id);
        assert_eq!(beats(&copy.id).await, Some(grid_at(120.)));
        let late = catalog::create_score(
            &pool,
            &request(4),
            "song",
            "room",
            None,
            crate::sync::triggers::Attribution::PERSON,
        )
        .await
        .unwrap();
        assert_ne!(late.grid_id, written.grid_id);
        assert_eq!(beats(&late.id).await, Some(grid_at(60.)));

        // A grid this device no longer holds leaves its scores on the
        // current one, not without beats.
        sqlx::query("DELETE FROM track_beats WHERE id = ?")
            .bind(&written.grid_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(beats(&written.id).await, Some(grid_at(60.)));
    }

    #[tokio::test]
    async fn beat_validation_snapshots_survive_reanalysis_and_reject_stale_approval() {
        use super::super::beat_validations;
        use crate::models::tracks::{
            BeatValidation, BeatValidationReason as Reason, BeatValidationVerdict as Verdict,
        };
        let (_directory, pool) = test_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('reviewed', 'alice', 'audio-hash', '/audio.ogg')")
            .execute(&pool).await.unwrap();
        let grid = grid_at(120.);
        record_track_beats(&mut *pool.acquire().await.unwrap(), "reviewed", &grid, 7)
            .await
            .unwrap();
        assert_eq!(
            get_track_beats(&pool, Grid::current("reviewed"))
                .await
                .unwrap(),
            Some(grid.clone())
        );
        assert!(beat_validations::get(&pool, "reviewed")
            .await
            .unwrap()
            .is_none());
        beat_validations::set(&pool, "reviewed", &grid, Verdict::Correct, None)
            .await
            .unwrap();
        assert_eq!(
            beat_validations::get(&pool, "reviewed").await.unwrap(),
            Some(BeatValidation {
                grid: grid.clone(),
                verdict: Verdict::Correct,
                reason: None
            })
        );
        beat_validations::set(
            &pool,
            "reviewed",
            &grid,
            Verdict::Incorrect,
            Some(Reason::Drift),
        )
        .await
        .unwrap();
        assert_eq!(
            beat_validations::get(&pool, "reviewed").await.unwrap(),
            Some(BeatValidation {
                grid: grid.clone(),
                verdict: Verdict::Incorrect,
                reason: Some(Reason::Drift)
            })
        );
        assert!(beat_validations::set(
            &pool,
            "reviewed",
            &grid,
            Verdict::Correct,
            Some(Reason::Drift)
        )
        .await
        .is_err());
        beat_validations::set(&pool, "reviewed", &grid, Verdict::Unreviewed, None)
            .await
            .unwrap();
        assert!(beat_validations::get(&pool, "reviewed")
            .await
            .unwrap()
            .is_none());
        beat_validations::set(&pool, "reviewed", &grid, Verdict::Correct, None)
            .await
            .unwrap();
        record_track_beats(
            &mut *pool.acquire().await.unwrap(),
            "reviewed",
            &grid_at(60.),
            8,
        )
        .await
        .unwrap();
        assert_eq!(
            beat_validations::get(&pool, "reviewed").await.unwrap(),
            Some(BeatValidation {
                grid: grid.clone(),
                verdict: Verdict::Correct,
                reason: None
            })
        );
        // A grid the track never had is not one a person can have reviewed.
        assert!(
            beat_validations::set(&pool, "reviewed", &grid_at(90.), Verdict::Correct, None)
                .await
                .unwrap_err()
                .contains("changed")
        );
        // The earlier grid is still the track's, on the screen of anyone whose
        // score was written against it, and is stamped with its own detector.
        beat_validations::set(&pool, "reviewed", &grid, Verdict::Correct, None)
            .await
            .unwrap();
        let snapshot: (String, i64, String) = sqlx::query_as("SELECT track_hash, processor_version, verdict FROM track_beat_validations WHERE track_id = 'reviewed'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(snapshot, ("audio-hash".into(), 7, "correct".into()));
        crate::database::local::auth::arm_write_admission(&pool, Some("another-user"))
            .await
            .unwrap();
        assert!(
            beat_validations::set(&pool, "reviewed", &grid, Verdict::Correct, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn shared_track_artifacts_are_deleted_only_by_the_last_owner() {
        let (_directory, pool) = test_pool().await;
        for (id, uid) in [("one", "alice"), ("two", "bob")] {
            sqlx::query(
                "INSERT INTO tracks
                 (id, uid, track_hash, file_path, album_art_path)
                 VALUES (?, ?, 'shared-hash', '/managed/shared.mp3', '/managed/shared.jpg')",
            )
            .bind(id)
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
        }

        let mut first = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let plan = prepare_track_deletion(&mut first, "one", "alice")
            .await
            .unwrap()
            .unwrap();
        assert!(!plan.delete_audio);
        assert!(!plan.delete_album_art);
        assert!(!plan.delete_hash_artifacts);
        assert_eq!(
            delete_prepared_track_record(&mut first, "one", "alice")
                .await
                .unwrap(),
            1
        );
        first.commit().await.unwrap();

        let mut last = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let plan = prepare_track_deletion(&mut last, "two", "bob")
            .await
            .unwrap()
            .unwrap();
        assert!(plan.delete_audio);
        assert!(plan.delete_album_art);
        assert!(plan.delete_hash_artifacts);
        last.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn an_empty_score_is_durable_venue_membership() {
        let (_directory, pool) = test_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'Venue')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path)
             VALUES ('track', 'alice', 'hash', '/track.mp3')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('empty-score', 'alice', 'track', 'venue', 'Score')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = list_tracks_enriched(&pool, Some("venue")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].venue_annotation_count, 0);
        assert_eq!(rows[0].venue_score_count, 1);
        assert!(rows[0].is_in_venue);

        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('duplicate', 'alice', 'track', 'venue', 'Duplicate')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let rows = list_tracks_enriched(&pool, Some("venue")).await.unwrap();
        assert_eq!(rows[0].venue_score_count, 2);
        assert!(rows[0].is_in_venue);
    }

    #[tokio::test]
    async fn venue_aggregates_do_not_leak_across_principals() {
        let (_directory, pool) = test_pool().await;
        for (id, uid) in [("alice-venue", "alice"), ("bob-venue", "bob")] {
            sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, ?, ?)")
                .bind(id)
                .bind(uid)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path)
             VALUES ('alice-track', 'alice', 'hash', '/track.mp3')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('bob-score', 'bob', 'alice-track', 'bob-venue', 'Private')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO clips
             (id, uid, score_id, graph_json, graph, start, duration, seed, selection_json,
              blend_mode)
             VALUES ('bob-score:bob-clip', 'bob', 'bob-score', '{\"version\":1,\"nodes\":{\"strobe1\":{\"kind\":\"strobe\"}}}', '', 0, 1, '0',
                     '{\"all\":true}', 'replace')",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();

        let rows = list_tracks_enriched(&pool, Some("bob-venue"))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "Alice's own track remains visible");
        assert_eq!(rows[0].venue_score_count, 0);
        assert_eq!(rows[0].venue_annotation_count, 0);
        assert_eq!(rows[0].venue_annotation_coverage_seconds, 0.0);
        assert!(!rows[0].is_in_venue);
    }

    /// Shape taken verbatim from a real `track_roots.sections_json`: a labelled
    /// chord, a null-root "no chord" section, and an entry with no label at all.
    #[test]
    fn parses_chord_sections_with_null_root_and_labels() {
        let json = r#"[
            {"start":2.716,"end":4.318,"root":9,"label":"A:(1)"},
            {"start":4.318,"end":6.0,"root":null,"label":"N"},
            {"start":6.0,"end":8.5,"root":7},
            {"start":8.5}
        ]"#;
        let got = parse_chord_sections(json).unwrap();
        assert_eq!(
            got,
            vec![
                ChordSection {
                    start_s: 2.716,
                    end_s: 4.318,
                    root_pitch_class: Some(9),
                    label: Some("A:(1)".into()),
                },
                ChordSection {
                    start_s: 4.318,
                    end_s: 6.0,
                    root_pitch_class: None,
                    label: Some("N".into()),
                },
                ChordSection {
                    start_s: 6.0,
                    end_s: 8.5,
                    root_pitch_class: Some(7),
                    label: None,
                },
            ]
        );
    }

    #[test]
    fn rejects_non_array_sections_json() {
        assert!(parse_chord_sections("not json").is_none());
        assert!(parse_chord_sections("{}").is_none());
        assert_eq!(parse_chord_sections("[]").unwrap(), vec![]);
    }

    /// What the roots worker writes is the shape every stored row has, so a
    /// row written today and one synced from an older build read the same.
    #[test]
    fn written_sections_keep_the_stored_shape() {
        let stored = r#"[{"start":2.716,"end":4.318,"root":9,"label":"A:(1)"}]"#;
        let sections = parse_chord_sections(stored).unwrap();
        assert_eq!(serde_json::to_string(&sections).unwrap(), stored);
    }
}
