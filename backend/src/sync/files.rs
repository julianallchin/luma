//! Two-phase file sync for audio, stems, and album art.
//!
//! **Writer path**: Upload binary to Supabase Storage first, then update
//! local `storage_path` (which marks the metadata dirty for push).
//!
//! **Reader path**: Audio is downloaded on demand and retained locally.
//! Sync downloads album art; stems follow tracks with local audio.
//! Downloads go to a temp file first and are atomically renamed on success.

use sqlx::SqlitePool;
use std::process::Command;

use super::error::SyncError;
use super::host::SyncHost;
use crate::database::remote::common::SupabaseClient;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadProgressStart {
    count: usize,
}

fn emit_upload_start(host: &SyncHost, count: usize) {
    if count > 0 {
        host.events
            .emit("upload-progress-start", UploadProgressStart { count });
    }
}

fn emit_upload_tick(host: &SyncHost) {
    host.events.emit("upload-progress-tick", ());
}

/// Stats from a file sync operation.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FileSyncStats {
    pub audio_uploaded: usize,
    pub stems_uploaded: usize,
    pub art_uploaded: usize,
    pub stems_downloaded: usize,
    pub art_downloaded: usize,
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

/// Write bytes to a temp file alongside `dest`, then atomically rename.
/// This prevents partial/corrupt files if the process is interrupted.
fn atomic_write(dest: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

// ============================================================================
// Upload
// ============================================================================

/// Transcode an audio file to OGG Opus using the bundled ffmpeg.
/// Returns the compressed bytes, or None if transcoding fails.
fn transcode_to_ogg_opus(source: &std::path::Path) -> Option<Vec<u8>> {
    let tmp = tempfile::Builder::new().suffix(".ogg").tempfile().ok()?;
    let tmp_path = tmp.path().to_path_buf();

    let ffmpeg = crate::ffmpeg_env::ffmpeg_path();
    let mut cmd = Command::new(&ffmpeg);
    crate::cmd_util::no_window(&mut cmd);
    let output = cmd
        .args([
            "-i",
            source.to_str()?,
            "-c:a",
            "libopus",
            "-b:a",
            "96k",
            "-vn",
            "-y",
            tmp_path.to_str()?,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        eprintln!(
            "[file-sync] ffmpeg transcode failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }

    std::fs::read(&tmp_path).ok()
}

#[derive(sqlx::FromRow)]
struct PendingAudioUpload {
    id: String,
    track_hash: String,
    file_path: String,
}

/// Upload audio files that have a local file but no storage_path.
/// Audio is transcoded to OGG Opus before upload to reduce storage/bandwidth.
pub async fn upload_pending_audio(
    pool: &SqlitePool,
    remote: &SupabaseClient,
    uid: &str,
    token: &str,
    stats: &mut FileSyncStats,
    host: &SyncHost,
    progress: &super::progress::Progress,
) -> Result<(), SyncError> {
    let rows = sqlx::query_as::<_, PendingAudioUpload>(
        "SELECT id, track_hash, file_path FROM tracks
         WHERE uid = ? AND storage_path IS NULL AND file_path NOT LIKE '%.stub'",
    )
    .bind(uid)
    .fetch_all(pool)
    .await?;

    emit_upload_start(host, rows.len());

    progress.phase("Uploading audio", Some(rows.len()), "files");
    for row in &rows {
        let _item = progress.item();
        let file_path = std::path::Path::new(&row.file_path);
        if !file_path.exists() {
            continue;
        }

        // Transcode to OGG Opus; fall back to original if ffmpeg fails
        let (bytes, storage_path, content_type) =
            if let Some(compressed) = transcode_to_ogg_opus(file_path) {
                (
                    compressed,
                    format!("{uid}/{}/audio.ogg", row.track_hash),
                    "audio/ogg",
                )
            } else {
                let ext = file_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("bin");
                let bytes = match std::fs::read(file_path) {
                    Ok(b) => b,
                    Err(e) => {
                        stats.errors.push(format!("read audio {}: {e}", row.id));
                        continue;
                    }
                };
                (
                    bytes,
                    format!("{uid}/{}/audio.{ext}", row.track_hash),
                    audio_content_type(ext),
                )
            };

        // Phase 1: Upload binary
        match remote
            .upload_file("track-audio", &storage_path, bytes, content_type, token)
            .await
        {
            Ok(full_path) => {
                // Phase 2: Update local metadata (marks record dirty for push).
                // Log-and-continue on DB failure so remaining uploads aren't skipped.
                if let Err(e) = sqlx::query("UPDATE tracks SET storage_path = ? WHERE id = ?")
                    .bind(&full_path)
                    .bind(&row.id)
                    .execute(pool)
                    .await
                {
                    stats
                        .errors
                        .push(format!("db update audio {}: {e}", row.id));
                    continue;
                }
                stats.audio_uploaded += 1;
                emit_upload_tick(host);
            }
            Err(e) => {
                let msg = format!("upload audio {}: {e}", row.id);
                stats.errors.push(msg.clone());
                sentry::capture_message(&msg, sentry::Level::Error);
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
    remote: &SupabaseClient,
    uid: &str,
    token: &str,
    stats: &mut FileSyncStats,
    host: &SyncHost,
    progress: &super::progress::Progress,
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

    emit_upload_start(host, rows.len());

    progress.phase("Uploading stems", Some(rows.len()), "files");
    for row in &rows {
        let _item = progress.item();
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
                if let Err(e) = sqlx::query(
                    "UPDATE track_stems SET storage_path = ? WHERE track_id = ? AND stem_name = ?",
                )
                .bind(&full_path)
                .bind(&row.track_id)
                .bind(&row.stem_name)
                .execute(pool)
                .await
                {
                    stats.errors.push(format!(
                        "db update stem {}/{}: {e}",
                        row.track_id, row.stem_name
                    ));
                    continue;
                }
                stats.stems_uploaded += 1;
                emit_upload_tick(host);
            }
            Err(e) => {
                let msg = format!("upload stem {}/{}: {e}", row.track_id, row.stem_name);
                stats.errors.push(msg.clone());
                sentry::capture_message(&msg, sentry::Level::Error);
            }
        }
    }

    Ok(())
}

// ============================================================================
// Upload: Album Art
// ============================================================================

#[derive(sqlx::FromRow)]
struct PendingArtUpload {
    id: String,
    track_hash: String,
    album_art_path: String,
    album_art_mime: String,
}

/// Upload album art that has a local file but no album_art_storage_path.
pub async fn upload_pending_album_art(
    pool: &SqlitePool,
    remote: &SupabaseClient,
    uid: &str,
    token: &str,
    stats: &mut FileSyncStats,
    host: &SyncHost,
    progress: &super::progress::Progress,
) -> Result<(), SyncError> {
    let rows = sqlx::query_as::<_, PendingArtUpload>(
        "SELECT id, track_hash, album_art_path, album_art_mime FROM tracks
         WHERE uid = ? AND album_art_storage_path IS NULL
           AND album_art_path IS NOT NULL AND album_art_path != ''",
    )
    .bind(uid)
    .fetch_all(pool)
    .await?;

    emit_upload_start(host, rows.len());

    progress.phase("Uploading cover art", Some(rows.len()), "files");
    for row in &rows {
        let _item = progress.item();
        let file_path = std::path::Path::new(&row.album_art_path);
        if !file_path.exists() {
            continue;
        }

        let ext = match row.album_art_mime.as_str() {
            "image/png" => "png",
            "image/gif" => "gif",
            _ => "jpg",
        };
        let storage_path = format!("{uid}/{}/cover.{ext}", row.track_hash);

        let bytes = match std::fs::read(file_path) {
            Ok(b) => b,
            Err(e) => {
                stats.errors.push(format!("read art {}: {e}", row.id));
                continue;
            }
        };

        match remote
            .upload_file(
                "track-audio",
                &storage_path,
                bytes,
                &row.album_art_mime,
                token,
            )
            .await
        {
            Ok(full_path) => {
                if let Err(e) =
                    sqlx::query("UPDATE tracks SET album_art_storage_path = ? WHERE id = ?")
                        .bind(&full_path)
                        .bind(&row.id)
                        .execute(pool)
                        .await
                {
                    stats.errors.push(format!("db update art {}: {e}", row.id));
                    continue;
                }
                stats.art_uploaded += 1;
                emit_upload_tick(host);
            }
            Err(e) => {
                let msg = format!("upload art {}: {e}", row.id);
                stats.errors.push(msg.clone());
                sentry::capture_message(&msg, sentry::Level::Error);
            }
        }
    }

    Ok(())
}

// ============================================================================
// Download
// ============================================================================

/// Downloaded metadata can belong to another principal, just like pulled rows.
/// Keep admission inside the database transaction and outside the network/file
/// work so failures roll it back without exposing it to ordinary local edits.
async fn apply_download_metadata(
    pool: &SqlitePool,
    query: sqlx::query::Query<'_, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
) -> Result<u64, SyncError> {
    use crate::database::local::write_admission::{enter_remote_writes, leave_remote_writes};

    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    enter_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    let changed = query.execute(&mut *transaction).await?.rows_affected();
    leave_remote_writes(&mut transaction)
        .await
        .map_err(SyncError::Local)?;
    transaction.commit().await?;
    Ok(changed)
}

#[derive(sqlx::FromRow)]
struct PendingStemDownload {
    track_id: String,
    track_hash: String,
    stem_name: String,
    file_path: String,
    storage_path: String,
}

/// Only download missing stem files for tracks whose main audio is local.
pub async fn download_pending_stems(
    pool: &SqlitePool,
    remote: &SupabaseClient,
    host: &SyncHost,
    token: &str,
    stats: &mut FileSyncStats,
    progress: &super::progress::Progress,
) -> Result<(), SyncError> {
    let rows = sqlx::query_as::<_, PendingStemDownload>(
        "SELECT ts.track_id, t.track_hash, ts.stem_name, ts.file_path, ts.storage_path
         FROM track_stems ts JOIN tracks t ON t.id = ts.track_id
         WHERE ts.storage_path IS NOT NULL AND t.file_path NOT LIKE '%.stub'
         ORDER BY ts.track_id, ts.stem_name",
    )
    .fetch_all(pool)
    .await?;
    let pending: Vec<_> = rows
        .into_iter()
        .filter(|row| !std::path::Path::new(&row.file_path).is_file())
        .collect();
    progress.phase("Downloading stems", Some(pending.len()), "files");
    for row in pending {
        let _item = progress.item();
        let Some((bucket, path)) = row.storage_path.split_once('/') else {
            continue;
        };
        let bytes = match remote.download_file(bucket, path, token).await {
            Ok(bytes) => bytes,
            Err(error) => {
                stats.errors.push(format!(
                    "download stem {}/{}: {error}",
                    row.track_id, row.stem_name
                ));
                continue;
            }
        };
        let stems_dir = host.storage.stems_root().join(&row.track_hash);
        if let Err(error) = std::fs::create_dir_all(&stems_dir) {
            stats
                .errors
                .push(format!("mkdir stems {}: {error}", row.track_id));
            continue;
        }
        let ext = path.rsplit('.').next().unwrap_or("wav");
        let dest = stems_dir.join(format!("{}.{ext}", row.stem_name));
        if let Err(error) = atomic_write(&dest, &bytes) {
            stats.errors.push(format!(
                "write stem {}/{}: {error}",
                row.track_id, row.stem_name
            ));
            continue;
        }
        match apply_download_metadata(
            pool,
            sqlx::query(
                "UPDATE track_stems SET file_path = ?, version = version + 1
             WHERE track_id = ? AND stem_name = ? AND storage_path = ?",
            )
            .bind(dest.to_string_lossy().as_ref())
            .bind(&row.track_id)
            .bind(&row.stem_name)
            .bind(&row.storage_path),
        )
        .await
        {
            Ok(0) => continue,
            Ok(_) => stats.stems_downloaded += 1,
            Err(error) => stats.errors.push(format!(
                "db update stem {}/{}: {error}",
                row.track_id, row.stem_name
            )),
        }
    }
    Ok(())
}

// ============================================================================
// Download: Album Art
// ============================================================================

#[derive(sqlx::FromRow)]
struct PendingArtDownload {
    id: String,
    track_hash: String,
    album_art_storage_path: String,
    album_art_mime: Option<String>,
}

/// Download album art for tracks that have a storage path but no local file.
pub async fn download_pending_album_art(
    pool: &SqlitePool,
    remote: &SupabaseClient,
    host: &SyncHost,
    token: &str,
    stats: &mut FileSyncStats,
    progress: &super::progress::Progress,
) -> Result<(), SyncError> {
    let rows = sqlx::query_as::<_, PendingArtDownload>(
        "SELECT id, track_hash, album_art_storage_path, album_art_mime FROM tracks
         WHERE album_art_storage_path IS NOT NULL
           AND (album_art_path IS NULL OR album_art_path = '')",
    )
    .fetch_all(pool)
    .await?;

    let art_dir = host.storage.art_dir();

    progress.phase("Downloading cover art", Some(rows.len()), "files");
    for row in &rows {
        let _item = progress.item();
        let (bucket, path) = match row.album_art_storage_path.split_once('/') {
            Some(bp) => bp,
            None => continue,
        };

        let bytes = match remote.download_file(bucket, path, token).await {
            Ok(b) => b,
            Err(e) => {
                stats.errors.push(format!("download art {}: {e}", row.id));
                continue;
            }
        };

        let ext = match row.album_art_mime.as_deref() {
            Some("image/png") => "png",
            Some("image/gif") => "gif",
            _ => "jpg",
        };
        let dest = art_dir.join(format!("{}.{ext}", row.track_hash));

        if let Err(e) = atomic_write(&dest, &bytes) {
            stats.errors.push(format!("write art {}: {e}", row.id));
            continue;
        }

        if let Err(e) = apply_download_metadata(
            pool,
            sqlx::query("UPDATE tracks SET album_art_path = ?, version = version + 1 WHERE id = ?")
                .bind(dest.to_string_lossy().as_ref())
                .bind(&row.id),
        )
        .await
        {
            stats.errors.push(format!("db update art {}: {e}", row.id));
            continue;
        }

        stats.art_downloaded += 1;
    }

    Ok(())
}

/// Is this track's audio on this device?
///
/// Phase one is local-only, so this is a check rather than a fetch: there is
/// no record transport to ask for the bytes yet. Phase two restores the
/// download here, behind the same call.
pub async fn ensure_track_audio(pool: &SqlitePool, track_id: &str) -> Result<(), SyncError> {
    use crate::database::local::track_access::{Read, VisibleTrackAccess};

    let mut access = VisibleTrackAccess::<Read>::read(pool, track_id)
        .await
        .map_err(SyncError::Local)?;
    let file_path: String = sqlx::query_scalar("SELECT file_path FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_one(access.connection())
        .await?;
    access.finish().await.map_err(SyncError::Local)?;
    if !file_path.ends_with(".stub") && std::path::Path::new(&file_path).is_file() {
        return Ok(());
    }
    Err(SyncError::Local(
        "This track's audio is not on this device".into(),
    ))
}
