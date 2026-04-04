use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::models::tracks::{TrackBeats, TrackBrowserRow, TrackRoots, TrackStem, TrackSummary};

// Helper structs for internal queries
#[derive(FromRow)]
pub struct TrackPathAndHash {
    pub file_path: String,
    pub track_hash: String,
}

#[derive(FromRow)]
pub struct TrackFileInfo {
    pub file_path: String,
    pub album_art_path: Option<String>,
    pub track_hash: String,
}

// -----------------------------------------------------------------------------
// Track records
// -----------------------------------------------------------------------------

pub async fn list_tracks(pool: &SqlitePool) -> Result<Vec<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, album_art_storage_path, source_type, source_id, source_filename, created_at, updated_at FROM tracks ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list tracks: {}", e))
}

pub async fn list_tracks_enriched(
    pool: &SqlitePool,
    venue_id: Option<&str>,
) -> Result<Vec<TrackBrowserRow>, String> {
    let vid = venue_id.unwrap_or("");
    sqlx::query_as::<_, TrackBrowserRow>(
        "SELECT
            t.id, t.uid, t.title, t.artist, t.album, t.duration_seconds,
            t.album_art_path, t.album_art_mime, t.source_type, t.file_path, t.created_at,
            tb.bpm,
            COALESCE(ac.cnt, 0) AS annotation_count,
            COALESCE(vac.cnt, 0) AS venue_annotation_count,
            (t.storage_path IS NOT NULL) AS has_storage,
            (tb.track_id IS NOT NULL) AS has_beats,
            (st.track_id IS NOT NULL) AS has_stems,
            (tr.track_id IS NOT NULL) AS has_roots
         FROM tracks t
         LEFT JOIN track_beats tb ON tb.track_id = t.id
         LEFT JOIN track_roots tr ON tr.track_id = t.id
         LEFT JOIN (SELECT track_id FROM track_stems GROUP BY track_id) st ON st.track_id = t.id
         LEFT JOIN (
             SELECT s.track_id, COUNT(tsc.id) AS cnt
             FROM scores s
             JOIN track_scores tsc ON tsc.score_id = s.id
             GROUP BY s.track_id
         ) ac ON ac.track_id = t.id
         LEFT JOIN (
             SELECT s.track_id, COUNT(tsc.id) AS cnt
             FROM scores s
             JOIN track_scores tsc ON tsc.score_id = s.id
             WHERE s.venue_id = ?
             GROUP BY s.track_id
         ) vac ON vac.track_id = t.id
         ORDER BY t.created_at DESC",
    )
    .bind(vid)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list enriched tracks: {}", e))
}

pub async fn get_track_by_hash(
    pool: &SqlitePool,
    track_hash: &str,
) -> Result<Option<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, album_art_storage_path, source_type, source_id, source_filename, created_at, updated_at FROM tracks WHERE track_hash = ?",
    )
    .bind(track_hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch track by hash: {}", e))
}

pub async fn get_own_track_by_hash(
    pool: &SqlitePool,
    track_hash: &str,
    uid: &str,
) -> Result<Option<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, album_art_storage_path, source_type, source_id, source_filename, created_at, updated_at FROM tracks WHERE track_hash = ? AND uid = ?",
    )
    .bind(track_hash)
    .bind(uid)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch track by hash: {}", e))
}

pub async fn get_track_by_id(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, album_art_storage_path, source_type, source_id, source_filename, created_at, updated_at FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch track by id: {}", e))
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
    uid: Option<String>,
    source_type: Option<&str>,
    source_id: Option<&str>,
    source_filename: Option<&str>,
) -> Result<String, String> {
    let id = Uuid::new_v4().to_string();

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
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to insert track: {}", e))?;

    Ok(id)
}

pub async fn get_track_file_info(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<TrackFileInfo>, String> {
    sqlx::query_as::<_, TrackFileInfo>(
        "SELECT file_path, album_art_path, track_hash FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch track info: {}", e))
}

pub async fn delete_track_record(pool: &SqlitePool, track_id: &str) -> Result<u64, String> {
    let result = sqlx::query("DELETE FROM tracks WHERE id = ?")
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete track: {}", e))?;

    Ok(result.rows_affected())
}

pub async fn wipe_tracks(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("DELETE FROM track_beats")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear track beats: {}", e))?;
    sqlx::query("DELETE FROM track_roots")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear track roots: {}", e))?;
    sqlx::query("DELETE FROM track_waveforms")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear track waveforms: {}", e))?;
    sqlx::query("DELETE FROM track_stems")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear track stems: {}", e))?;
    sqlx::query("DELETE FROM tracks")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear tracks: {}", e))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Track metadata helpers
// -----------------------------------------------------------------------------

pub async fn get_track_path_and_hash(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<TrackPathAndHash, String> {
    sqlx::query_as::<_, TrackPathAndHash>("SELECT file_path, track_hash FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to fetch track path: {}", e))?
        .ok_or_else(|| format!("Track {} not found", track_id))
}

pub async fn get_track_duration(pool: &SqlitePool, track_id: &str) -> Result<Option<f64>, String> {
    sqlx::query_scalar("SELECT duration_seconds FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to get track duration: {}", e))
}

// -----------------------------------------------------------------------------
// Beats / Roots / Stems presence checks
// -----------------------------------------------------------------------------

pub async fn track_has_beats(pool: &SqlitePool, track_id: &str) -> Result<bool, String> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_beats WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect beat cache: {}", e))?;
    Ok(exists.is_some())
}

pub async fn track_has_roots(pool: &SqlitePool, track_id: &str) -> Result<bool, String> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_roots WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect root cache: {}", e))?;
    Ok(exists.is_some())
}

pub async fn track_has_stems(pool: &SqlitePool, track_id: &str) -> Result<bool, String> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_stems WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect stem cache: {}", e))?;
    Ok(exists.is_some())
}

// -----------------------------------------------------------------------------
// Beats / Roots / Stems persistence
// -----------------------------------------------------------------------------

/// Fetch the uid for a track (used to propagate auth.uid() to child rows).
pub(crate) async fn track_uid(pool: &SqlitePool, track_id: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT uid FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to get track uid: {}", e))
}

pub async fn upsert_track_beats(
    pool: &SqlitePool,
    track_id: &str,
    beats_json: &str,
    downbeats_json: &str,
    bpm: Option<f64>,
    downbeat_offset: Option<f64>,
    beats_per_bar: Option<i64>,
) -> Result<(), String> {
    let uid = track_uid(pool, track_id).await?;
    sqlx::query(
        "INSERT INTO track_beats (track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            beats_json = excluded.beats_json,
            downbeats_json = excluded.downbeats_json,
            bpm = excluded.bpm,
            downbeat_offset = excluded.downbeat_offset,
            beats_per_bar = excluded.beats_per_bar,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(beats_json)
    .bind(downbeats_json)
    .bind(bpm)
    .bind(downbeat_offset)
    .bind(beats_per_bar)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist beat data: {}", e))?;

    Ok(())
}

pub async fn upsert_track_roots(
    pool: &SqlitePool,
    track_id: &str,
    sections_json: &str,
    logits_path: Option<&str>,
) -> Result<(), String> {
    let uid = track_uid(pool, track_id).await?;
    sqlx::query(
        "INSERT INTO track_roots (track_id, uid, sections_json, logits_path)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            uid = excluded.uid,
            sections_json = excluded.sections_json,
            logits_path = excluded.logits_path,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(sections_json)
    .bind(logits_path)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist root data: {}", e))?;

    Ok(())
}

pub async fn upsert_track_stem(
    pool: &SqlitePool,
    track_id: &str,
    stem_name: &str,
    file_path: &str,
    storage_path: Option<&str>,
) -> Result<(), String> {
    let uid = track_uid(pool, track_id).await?;
    sqlx::query(
        "INSERT INTO track_stems (track_id, uid, stem_name, file_path, storage_path)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(track_id, stem_name) DO UPDATE SET
            uid = excluded.uid,
            file_path = excluded.file_path,
            storage_path = excluded.storage_path,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(&uid)
    .bind(stem_name)
    .bind(file_path)
    .bind(storage_path)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist stem data: {}", e))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Queries used by services
// -----------------------------------------------------------------------------

pub async fn get_track_stems(pool: &SqlitePool, track_id: &str) -> Result<Vec<TrackStem>, String> {
    sqlx::query_as::<_, TrackStem>(
        "SELECT track_id, uid, stem_name, file_path, storage_path, created_at, updated_at FROM track_stems WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load stems for track {}: {}", track_id, e))
}

pub async fn get_track_roots(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<TrackRoots>, String> {
    sqlx::query_as::<_, TrackRoots>(
        "SELECT track_id, uid, sections_json, logits_path, logits_storage_path, created_at, updated_at FROM track_roots WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load track roots: {}", e))
}

pub async fn get_track_beats_raw(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<TrackBeats>, String> {
    sqlx::query_as::<_, TrackBeats>(
        "SELECT track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar, created_at, updated_at FROM track_beats WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch beat data: {}", e))
}

pub async fn get_logits_path(pool: &SqlitePool, track_id: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT logits_path FROM track_roots WHERE track_id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to fetch logits path: {}", e))
}

// -----------------------------------------------------------------------------
// Source-based lookups (DJ library imports)
// -----------------------------------------------------------------------------

pub async fn get_track_by_source_id(
    pool: &SqlitePool,
    source_type: &str,
    source_id: &str,
) -> Result<Option<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, album_art_storage_path, source_type, source_id, source_filename, created_at, updated_at FROM tracks WHERE source_type = ? AND source_id = ?",
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch track by source_id: {}", e))
}

/// Fetch tracks whose duration is within `tolerance_secs` of `target_duration`.
/// Also joins `track_beats` to include BPM for subsequent filtering.
#[derive(sqlx::FromRow)]
pub struct TrackWithBpm {
    pub id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_seconds: Option<f64>,
    pub file_path: String,
    pub source_filename: Option<String>,
    pub bpm: Option<f64>,
}

pub async fn get_tracks_by_duration(
    pool: &SqlitePool,
    target_duration: f64,
    tolerance_secs: f64,
) -> Result<Vec<TrackWithBpm>, String> {
    sqlx::query_as::<_, TrackWithBpm>(
        "SELECT t.id, t.title, t.artist, t.duration_seconds, t.file_path, t.source_filename,
                tb.bpm
         FROM tracks t
         LEFT JOIN track_beats tb ON tb.track_id = t.id
         WHERE ABS(COALESCE(t.duration_seconds, 0) - ?) <= ?",
    )
    .bind(target_duration)
    .bind(tolerance_secs)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to fetch tracks by duration: {}", e))
}

pub async fn get_tracks_by_source_filename(
    pool: &SqlitePool,
    filename: &str,
) -> Result<Vec<TrackSummary>, String> {
    sqlx::query_as::<_, TrackSummary>(
        "SELECT id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, album_art_storage_path, source_type, source_id, source_filename, created_at, updated_at FROM tracks WHERE source_filename = ?",
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
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to fill track metadata gaps: {}", e))?;
    Ok(())
}

pub async fn update_track_source_metadata(
    pool: &SqlitePool,
    track_id: &str,
    title: &Option<String>,
    artist: &Option<String>,
    source_filename: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE tracks SET title = ?, artist = ?, source_filename = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(title)
    .bind(artist)
    .bind(source_filename)
    .bind(track_id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update track source metadata: {}", e))?;
    Ok(())
}
