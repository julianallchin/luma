//! Two-phase file sync for audio and stems.
//!
//! **Writer path**: Upload binary to Supabase Storage first, then update
//! local `storage_path` (which marks the metadata dirty for push).
//!
//! **Reader path**: After pull, download files for tracks with a
//! `storage_path` but no local file (stub tracks).

use sqlx::SqlitePool;
use tauri::AppHandle;

use super::error::SyncError;
use super::traits::RemoteClient;

/// Stats from a file sync operation.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FileSyncStats {
    pub audio_uploaded: usize,
    pub stems_uploaded: usize,
    pub audio_downloaded: usize,
    pub stems_downloaded: usize,
    pub errors: Vec<String>,
}

fn audio_content_type(ext: &str) -> &'static str {
    match ext {
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

fn stem_content_type(ext: &str) -> &'static str {
    match ext {
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "mp3" => "audio/mpeg",
        _ => "audio/wav",
    }
}

// ============================================================================
// Upload
// ============================================================================

#[derive(sqlx::FromRow)]
struct PendingAudioUpload {
    id: String,
    track_hash: String,
    file_path: String,
}

/// Upload audio files that have a local file but no storage_path.
pub async fn upload_pending_audio(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    uid: &str,
    token: &str,
    stats: &mut FileSyncStats,
) -> Result<(), SyncError> {
    let rows = sqlx::query_as::<_, PendingAudioUpload>(
        "SELECT id, track_hash, file_path FROM tracks
         WHERE uid = ? AND storage_path IS NULL AND file_path NOT LIKE '%.stub'",
    )
    .bind(uid)
    .fetch_all(pool)
    .await?;

    for row in &rows {
        let file_path = std::path::Path::new(&row.file_path);
        if !file_path.exists() {
            continue;
        }

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let storage_path = format!("{uid}/{}/audio.{ext}", row.track_hash);
        let content_type = audio_content_type(ext);

        let bytes = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(e) => {
                stats.errors.push(format!("read audio {}: {e}", row.id));
                continue;
            }
        };

        // Phase 1: Upload binary
        match remote
            .upload_file("track-audio", &storage_path, bytes, content_type, token)
            .await
        {
            Ok(full_path) => {
                // Phase 2: Update local metadata (marks record dirty for push)
                sqlx::query("UPDATE tracks SET storage_path = ? WHERE id = ?")
                    .bind(&full_path)
                    .bind(&row.id)
                    .execute(pool)
                    .await?;
                stats.audio_uploaded += 1;
            }
            Err(e) => {
                stats.errors.push(format!("upload audio {}: {e}", row.id));
            }
        }
    }

    Ok(())
}

#[derive(sqlx::FromRow)]
struct PendingStemUpload {
    track_id: String,
    track_hash: String,
    stem_name: String,
    stem_file_path: String,
}

/// Upload stem files that have a local file but no storage_path.
pub async fn upload_pending_stems(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    uid: &str,
    token: &str,
    stats: &mut FileSyncStats,
) -> Result<(), SyncError> {
    let rows = sqlx::query_as::<_, PendingStemUpload>(
        "SELECT ts.track_id, t.track_hash, ts.stem_name, ts.file_path AS stem_file_path
         FROM track_stems ts
         JOIN tracks t ON ts.track_id = t.id
         WHERE t.uid = ? AND ts.storage_path IS NULL",
    )
    .bind(uid)
    .fetch_all(pool)
    .await?;

    for row in &rows {
        let file_path = std::path::Path::new(&row.stem_file_path);
        if !file_path.exists() {
            continue;
        }

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("wav");
        let storage_path = format!("{uid}/{}/stems/{}.{ext}", row.track_hash, row.stem_name);
        let content_type = stem_content_type(ext);

        let bytes = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(e) => {
                stats
                    .errors
                    .push(format!("read stem {}/{}: {e}", row.track_id, row.stem_name));
                continue;
            }
        };

        match remote
            .upload_file("track-stems", &storage_path, bytes, content_type, token)
            .await
        {
            Ok(full_path) => {
                sqlx::query(
                    "UPDATE track_stems SET storage_path = ? WHERE track_id = ? AND stem_name = ?",
                )
                .bind(&full_path)
                .bind(&row.track_id)
                .bind(&row.stem_name)
                .execute(pool)
                .await?;
                stats.stems_uploaded += 1;
            }
            Err(e) => {
                stats.errors.push(format!(
                    "upload stem {}/{}: {e}",
                    row.track_id, row.stem_name
                ));
            }
        }
    }

    Ok(())
}

// ============================================================================
// Download
// ============================================================================

#[derive(sqlx::FromRow)]
struct PendingAudioDownload {
    id: String,
    track_hash: String,
    storage_path: String,
}

/// Download audio for tracks that have a storage_path but a .stub local file.
pub async fn download_pending_audio(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    app_handle: &AppHandle,
    token: &str,
    stats: &mut FileSyncStats,
) -> Result<(), SyncError> {
    let rows = sqlx::query_as::<_, PendingAudioDownload>(
        "SELECT id, track_hash, storage_path FROM tracks
         WHERE storage_path IS NOT NULL AND file_path LIKE '%.stub'",
    )
    .fetch_all(pool)
    .await?;

    let storage_dir =
        crate::services::tracks::storage_dirs(app_handle).map_err(SyncError::Local)?;

    for row in &rows {
        let (bucket, path) = match row.storage_path.split_once('/') {
            Some(bp) => bp,
            None => continue,
        };

        let bytes = match remote.download_file(bucket, path, token).await {
            Ok(b) => b,
            Err(e) => {
                stats.errors.push(format!("download audio {}: {e}", row.id));
                continue;
            }
        };

        let ext = path.rsplit('.').next().unwrap_or("bin");
        let dest = storage_dir.0.join(format!("{}.{ext}", row.track_hash));

        if let Err(e) = std::fs::write(&dest, &bytes) {
            stats.errors.push(format!("write audio {}: {e}", row.id));
            continue;
        }

        sqlx::query("UPDATE tracks SET file_path = ?, version = version + 1 WHERE id = ?")
            .bind(dest.to_string_lossy().as_ref())
            .bind(&row.id)
            .execute(pool)
            .await?;

        stats.audio_downloaded += 1;
    }

    Ok(())
}

#[derive(sqlx::FromRow)]
struct TrackNeedingStems {
    id: String,
    track_hash: String,
}

#[derive(serde::Deserialize)]
struct RemoteStemRow {
    stem_name: String,
    storage_path: Option<String>,
}

/// Download stems for tracks that have cloud stems but no local files.
pub async fn download_pending_stems(
    pool: &SqlitePool,
    remote: &dyn RemoteClient,
    app_handle: &AppHandle,
    token: &str,
    stats: &mut FileSyncStats,
) -> Result<(), SyncError> {
    let tracks = sqlx::query_as::<_, TrackNeedingStems>(
        "SELECT DISTINCT t.id, t.track_hash FROM tracks t
         WHERE t.storage_path IS NOT NULL
         AND NOT EXISTS (
             SELECT 1 FROM track_stems ts
             WHERE ts.track_id = t.id AND ts.file_path IS NOT NULL AND ts.file_path != ''
         )",
    )
    .fetch_all(pool)
    .await?;

    let storage_dir =
        crate::services::tracks::storage_dirs(app_handle).map_err(SyncError::Local)?;

    for track in &tracks {
        let remote_stems: Vec<RemoteStemRow> = remote
            .select_json(
                "track_stems",
                &format!(
                    "track_id=eq.{}&select=track_id,stem_name,storage_path",
                    track.id
                ),
                token,
            )
            .await
            .and_then(|rows| {
                serde_json::from_value(serde_json::Value::Array(rows))
                    .map_err(|e| SyncError::Parse(e.to_string()))
            })?;

        let stems_dir = storage_dir.2.join(&track.track_hash);
        if let Err(e) = std::fs::create_dir_all(&stems_dir) {
            stats.errors.push(format!("mkdir stems {}: {e}", track.id));
            continue;
        }

        for stem in &remote_stems {
            let Some(ref spath) = stem.storage_path else {
                continue;
            };

            let (bucket, path) = match spath.split_once('/') {
                Some(bp) => bp,
                None => continue,
            };

            let bytes = match remote.download_file(bucket, path, token).await {
                Ok(b) => b,
                Err(e) => {
                    stats.errors.push(format!(
                        "download stem {}/{}: {e}",
                        track.id, stem.stem_name
                    ));
                    continue;
                }
            };

            let ext = path.rsplit('.').next().unwrap_or("wav");
            let dest = stems_dir.join(format!("{}.{ext}", stem.stem_name));
            if let Err(e) = std::fs::write(&dest, &bytes) {
                stats
                    .errors
                    .push(format!("write stem {}/{}: {e}", track.id, stem.stem_name));
                continue;
            }

            sqlx::query(
                "INSERT INTO track_stems (track_id, uid, stem_name, file_path, storage_path)
                 VALUES (?, (SELECT uid FROM tracks WHERE id = ?), ?, ?, ?)
                 ON CONFLICT(track_id, stem_name) DO UPDATE SET
                   file_path = excluded.file_path, version = version + 1,
                   storage_path = excluded.storage_path",
            )
            .bind(&track.id)
            .bind(&track.id)
            .bind(&stem.stem_name)
            .bind(dest.to_string_lossy().as_ref())
            .bind(spath)
            .execute(pool)
            .await?;

            stats.stems_downloaded += 1;
        }
    }

    Ok(())
}
