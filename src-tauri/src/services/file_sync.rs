//! File sync service — handles audio and stem file uploads/downloads
//! separately from record sync.
//!
//! Record sync (cloud_sync) is metadata-only and fast.
//! File sync runs independently afterward, uploading/downloading
//! audio files and stems to/from Supabase storage.

use serde::Deserialize;
use sqlx::SqlitePool;
use tauri::AppHandle;

use crate::database::local::tracks as local_tracks;
use crate::database::remote::common::SupabaseClient;

// ============================================================================
// Stats
// ============================================================================

#[derive(Debug, Default)]
pub struct FileSyncStats {
    pub audio_uploaded: usize,
    pub stems_uploaded: usize,
    pub audio_downloaded: usize,
    pub stems_downloaded: usize,
    pub errors: Vec<String>,
}

// ============================================================================
// Content type helpers
// ============================================================================

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
// Upload: pending audio files
// ============================================================================

/// Row type for tracks needing audio upload.
#[derive(sqlx::FromRow)]
struct PendingAudioUpload {
    id: String,
    track_hash: String,
    file_path: String,
}

/// Upload pending audio files to Supabase storage.
/// Finds tracks owned by the current user that have a local file but no storage_path.
pub async fn upload_pending_audio(
    pool: &SqlitePool,
    client: &SupabaseClient,
    uid: &str,
    access_token: &str,
) -> Result<FileSyncStats, String> {
    let mut stats = FileSyncStats::default();

    let rows = sqlx::query_as::<_, PendingAudioUpload>(
        "SELECT id, track_hash, file_path FROM tracks
         WHERE uid = ? AND storage_path IS NULL AND file_path NOT LIKE '%.stub'",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query pending audio uploads: {}", e))?;

    for row in &rows {
        let file_path = std::path::Path::new(&row.file_path);
        if !file_path.exists() {
            continue;
        }

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let storage_path = format!("{}/{}/audio.{}", uid, row.track_hash, ext);
        let content_type = audio_content_type(ext);

        let bytes = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(e) => {
                stats.errors.push(format!("Read audio {}: {}", row.id, e));
                continue;
            }
        };

        match client
            .upload_file(
                "track-audio",
                &storage_path,
                bytes,
                content_type,
                access_token,
            )
            .await
        {
            Ok(full_path) => {
                if let Err(e) = local_tracks::set_storage_path(pool, &row.id, &full_path).await {
                    stats
                        .errors
                        .push(format!("Set storage_path {}: {}", row.id, e));
                } else {
                    stats.audio_uploaded += 1;
                }
            }
            Err(e) => {
                stats
                    .errors
                    .push(format!("Upload audio {}: {:?}", row.id, e));
            }
        }
    }

    Ok(stats)
}

// ============================================================================
// Upload: pending stem files
// ============================================================================

/// Row type for stems needing upload.
#[derive(sqlx::FromRow)]
struct PendingStemUpload {
    track_id: String,
    track_hash: String,
    stem_name: String,
    stem_file_path: String,
}

/// Upload pending stem files to Supabase storage.
pub async fn upload_pending_stems(
    pool: &SqlitePool,
    client: &SupabaseClient,
    uid: &str,
    access_token: &str,
) -> Result<usize, String> {
    let rows = sqlx::query_as::<_, PendingStemUpload>(
        "SELECT ts.track_id, t.track_hash, ts.stem_name, ts.file_path AS stem_file_path
         FROM track_stems ts
         JOIN tracks t ON ts.track_id = t.id
         WHERE t.uid = ? AND ts.storage_path IS NULL",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query pending stem uploads: {}", e))?;

    let mut count = 0;
    for row in &rows {
        let file_path = std::path::Path::new(&row.stem_file_path);
        if !file_path.exists() {
            continue;
        }

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("wav");
        let storage_path = format!("{}/{}/stems/{}.{}", uid, row.track_hash, row.stem_name, ext);
        let content_type = stem_content_type(ext);

        let bytes = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "[file_sync] Failed to read stem {}/{}: {}",
                    row.track_id, row.stem_name, e
                );
                continue;
            }
        };

        match client
            .upload_file(
                "track-stems",
                &storage_path,
                bytes,
                content_type,
                access_token,
            )
            .await
        {
            Ok(full_path) => {
                let _ = local_tracks::set_stem_storage_path(
                    pool,
                    &row.track_id,
                    &row.stem_name,
                    &full_path,
                )
                .await;
                count += 1;
            }
            Err(e) => {
                eprintln!(
                    "[file_sync] Failed to upload stem {}/{}: {:?}",
                    row.track_id, row.stem_name, e
                );
            }
        }
    }

    Ok(count)
}

// ============================================================================
// Download: pending audio files
// ============================================================================

/// Row type for tracks needing audio download (stub files with a storage_path).
#[derive(sqlx::FromRow)]
struct PendingAudioDownload {
    id: String,
    track_hash: String,
    storage_path: String,
}

/// Download audio files for tracks that have storage_path but local file is a stub.
pub async fn download_pending_audio(
    pool: &SqlitePool,
    client: &SupabaseClient,
    app_handle: &AppHandle,
    access_token: &str,
) -> Result<usize, String> {
    let rows = sqlx::query_as::<_, PendingAudioDownload>(
        "SELECT id, track_hash, storage_path FROM tracks
         WHERE storage_path IS NOT NULL AND file_path LIKE '%.stub'",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query pending audio downloads: {}", e))?;

    let storage_dir = crate::services::tracks::storage_dirs(app_handle)?;
    let mut count = 0;

    for row in &rows {
        let (bucket, path) = match row.storage_path.split_once('/') {
            Some(bp) => bp,
            None => continue,
        };

        let bytes = match client.download_file(bucket, path, access_token).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[file_sync] Failed to download audio {}: {:?}", row.id, e);
                continue;
            }
        };

        let ext = path.rsplit('.').next().unwrap_or("bin");
        let dest = storage_dir.0.join(format!("{}.{}", row.track_hash, ext));

        if let Err(e) = std::fs::write(&dest, &bytes) {
            eprintln!("[file_sync] Failed to write audio {}: {}", row.id, e);
            continue;
        }

        // Update local file_path from .stub to actual file
        sqlx::query("UPDATE tracks SET file_path = ? WHERE id = ?")
            .bind(dest.to_string_lossy().as_ref())
            .bind(&row.id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to update file_path: {}", e))?;

        count += 1;
    }

    Ok(count)
}

// ============================================================================
// Download: pending stem files
// ============================================================================

/// Remote stem row for download queries.
#[derive(Deserialize)]
#[allow(dead_code)]
struct RemoteTrackStem {
    id: String,
    track_id: String,
    stem_name: String,
    storage_path: Option<String>,
}

/// Row type for tracks that have cloud stems but may be missing local files.
#[derive(sqlx::FromRow)]
struct TrackNeedingStems {
    id: String,
    track_hash: String,
}

/// Download stems for tracks that have cloud stems but no local files.
pub async fn download_pending_stems(
    pool: &SqlitePool,
    client: &SupabaseClient,
    app_handle: &AppHandle,
    access_token: &str,
) -> Result<usize, String> {
    // Find tracks that have a storage_path (were pulled from cloud) but might
    // be missing local stem files. We check for tracks whose file_path was a stub
    // OR where stems exist in cloud but not locally.
    let tracks = sqlx::query_as::<_, TrackNeedingStems>(
        "SELECT DISTINCT t.id, t.track_hash FROM tracks t
         WHERE t.storage_path IS NOT NULL
         AND NOT EXISTS (
             SELECT 1 FROM track_stems ts
             WHERE ts.track_id = t.id AND ts.file_path IS NOT NULL AND ts.file_path != ''
         )",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query tracks needing stems: {}", e))?;

    let storage_dir = crate::services::tracks::storage_dirs(app_handle)?;
    let mut total = 0;

    for track in &tracks {
        let remote_stems: Vec<RemoteTrackStem> = match client
            .select(
                "track_stems",
                &format!(
                    "track_id=eq.{}&select=id,track_id,stem_name,storage_path",
                    track.id
                ),
                access_token,
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[file_sync] Failed to fetch stems for {}: {:?}",
                    track.id, e
                );
                continue;
            }
        };

        let stems_dir = storage_dir.2.join(&track.track_hash);
        if let Err(e) = std::fs::create_dir_all(&stems_dir) {
            eprintln!(
                "[file_sync] Failed to create stems dir for {}: {}",
                track.id, e
            );
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

            let bytes = match client.download_file(bucket, path, access_token).await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!(
                        "[file_sync] Failed to download stem {}/{}: {:?}",
                        track.id, stem.stem_name, e
                    );
                    continue;
                }
            };

            let ext = path.rsplit('.').next().unwrap_or("wav");
            let dest = stems_dir.join(format!("{}.{}", stem.stem_name, ext));
            if let Err(e) = std::fs::write(&dest, &bytes) {
                eprintln!(
                    "[file_sync] Failed to write stem {}/{}: {}",
                    track.id, stem.stem_name, e
                );
                continue;
            }

            // Upsert stem record locally
            sqlx::query(
                "INSERT INTO track_stems (track_id, uid, stem_name, file_path, storage_path)
                 VALUES (?, (SELECT uid FROM tracks WHERE id = ?), ?, ?, ?)
                 ON CONFLICT(track_id, stem_name) DO UPDATE SET
                   file_path = excluded.file_path,
                   storage_path = excluded.storage_path",
            )
            .bind(&track.id)
            .bind(&track.id)
            .bind(&stem.stem_name)
            .bind(dest.to_string_lossy().as_ref())
            .bind(spath)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to upsert stem: {}", e))?;

            total += 1;
        }
    }

    Ok(total)
}
