use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

#[derive(FromRow, Clone)]
pub struct TrackRow {
    pub id: i64,
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    pub track_hash: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_seconds: Option<f64>,
    pub file_path: String,
    pub storage_path: Option<String>,
    pub album_art_path: Option<String>,
    pub album_art_mime: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// -----------------------------------------------------------------------------
// Track records
// -----------------------------------------------------------------------------

pub async fn list_tracks(pool: &SqlitePool) -> Result<Vec<TrackRow>, String> {
    sqlx::query_as::<_, TrackRow>(
        "SELECT id, remote_id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list tracks: {}", e))
}

pub async fn get_track_by_hash(
    pool: &SqlitePool,
    track_hash: &str,
) -> Result<Option<TrackRow>, String> {
    sqlx::query_as::<_, TrackRow>(
        "SELECT id, remote_id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks WHERE track_hash = ?",
    )
    .bind(track_hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch track by hash: {}", e))
}

pub async fn get_track_by_id(pool: &SqlitePool, track_id: i64) -> Result<Option<TrackRow>, String> {
    sqlx::query_as::<_, TrackRow>(
        "SELECT id, remote_id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks WHERE id = ?",
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
) -> Result<i64, String> {
    let remote_id = Uuid::new_v4().to_string();
    let result = sqlx::query(
        "INSERT INTO tracks (remote_id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, uid) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(remote_id)
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
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to insert track: {}", e))?;

    Ok(result.last_insert_rowid())
}

pub async fn get_track_file_info(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<Option<(String, Option<String>, String)>, String> {
    sqlx::query_as("SELECT file_path, album_art_path, track_hash FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to fetch track info: {}", e))
}

pub async fn delete_track_record(pool: &SqlitePool, track_id: i64) -> Result<u64, String> {
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
    track_id: i64,
) -> Result<(String, String), String> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to fetch track path: {}", e))?;

    row.ok_or_else(|| format!("Track {} not found", track_id))
}

pub async fn get_track_duration(pool: &SqlitePool, track_id: i64) -> Result<Option<f64>, String> {
    let row: Option<(Option<f64>,)> =
        sqlx::query_as("SELECT duration_seconds FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to get track duration: {}", e))?;

    Ok(row.and_then(|(d,)| d))
}

// -----------------------------------------------------------------------------
// Beats / Roots / Stems presence checks
// -----------------------------------------------------------------------------

pub async fn track_has_beats(pool: &SqlitePool, track_id: i64) -> Result<bool, String> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_beats WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect beat cache: {}", e))?;
    Ok(exists.is_some())
}

pub async fn track_has_roots(pool: &SqlitePool, track_id: i64) -> Result<bool, String> {
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_roots WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect root cache: {}", e))?;
    Ok(exists.is_some())
}

pub async fn track_has_stems(pool: &SqlitePool, track_id: i64) -> Result<bool, String> {
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

pub async fn upsert_track_beats(
    pool: &SqlitePool,
    track_id: i64,
    beats_json: &str,
    downbeats_json: &str,
    bpm: Option<f64>,
    downbeat_offset: Option<f64>,
    beats_per_bar: Option<i64>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO track_beats (track_id, beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            beats_json = excluded.beats_json,
            downbeats_json = excluded.downbeats_json,
            bpm = excluded.bpm,
            downbeat_offset = excluded.downbeat_offset,
            beats_per_bar = excluded.beats_per_bar,
            updated_at = datetime('now')",
    )
    .bind(track_id)
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
    track_id: i64,
    sections_json: &str,
    logits_path: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO track_roots (track_id, sections_json, logits_path)
         VALUES (?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            sections_json = excluded.sections_json,
            logits_path = excluded.logits_path,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(sections_json)
    .bind(logits_path)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist root data: {}", e))?;

    Ok(())
}

pub async fn upsert_track_stem(
    pool: &SqlitePool,
    track_id: i64,
    stem_name: &str,
    file_path: &str,
    storage_path: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO track_stems (track_id, stem_name, file_path, storage_path)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(track_id, stem_name) DO UPDATE SET
            file_path = excluded.file_path,
            storage_path = excluded.storage_path,
            updated_at = datetime('now')",
    )
    .bind(track_id)
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

pub async fn get_track_stems(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<Vec<(String, String)>, String> {
    let stems: Vec<(String, String)> =
        sqlx::query_as("SELECT stem_name, file_path FROM track_stems WHERE track_id = ?")
            .bind(track_id)
            .fetch_all(pool)
            .await
            .map_err(|e| format!("Failed to load stems for track {}: {}", track_id, e))?;

    Ok(stems)
}

pub async fn get_track_roots(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<Option<(String, Option<String>)>, String> {
    sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT sections_json, logits_path FROM track_roots WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to load track roots: {}", e))
}

pub async fn get_track_beats_raw(
    pool: &SqlitePool,
    track_id: i64,
) -> Result<Option<(String, String, Option<f64>, Option<f64>, Option<i64>)>, String> {
    sqlx::query_as::<_, (String, String, Option<f64>, Option<f64>, Option<i64>)>(
        "SELECT beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar FROM track_beats WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch beat data: {}", e))
}

pub async fn get_logits_path(pool: &SqlitePool, track_id: i64) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT logits_path FROM track_roots WHERE track_id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to fetch logits path: {}", e))
}
