use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

use crate::audio::{
    generate_melspec, load_or_decode_audio, StemCache, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH,
};
use crate::beat_worker::{self, BeatAnalysis};
use crate::database::Db;
use crate::models::tracks::{MelSpec, TrackSummary};
use crate::root_worker::{self, RootAnalysis};
use crate::schema::BeatGrid;
use crate::stem_worker;

pub const TARGET_SAMPLE_RATE: u32 = 48_000;

static STEMS_IN_PROGRESS: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static ROOTS_IN_PROGRESS: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));

impl TrackSummary {
    fn from_row(row: TrackRow, album_art_data: Option<String>) -> Self {
        TrackSummary {
            id: row.id,
            track_hash: row.track_hash,
            title: row.title,
            artist: row.artist,
            album: row.album,
            track_number: row.track_number,
            disc_number: row.disc_number,
            duration_seconds: row.duration_seconds,
            file_path: row.file_path,
            album_art_path: row.album_art_path,
            album_art_mime: row.album_art_mime,
            album_art_data,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(FromRow)]
struct TrackRow {
    id: i64,
    track_hash: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    duration_seconds: Option<f64>,
    file_path: String,
    album_art_path: Option<String>,
    album_art_mime: Option<String>,
    created_at: String,
    updated_at: String,
}

fn album_art_for_row(row: &TrackRow) -> Option<String> {
    match (&row.album_art_path, &row.album_art_mime) {
        (Some(path), Some(mime)) => read_album_art(path, mime),
        _ => None,
    }
}

pub fn ensure_storage(app: &AppHandle) -> Result<(), String> {
    let (tracks_dir, art_dir, stems_dir) = storage_dirs(app)?;
    std::fs::create_dir_all(&tracks_dir)
        .map_err(|e| format!("Failed to create tracks directory: {}", e))?;
    std::fs::create_dir_all(&art_dir)
        .map_err(|e| format!("Failed to create album art directory: {}", e))?;
    std::fs::create_dir_all(&stems_dir)
        .map_err(|e| format!("Failed to create stems directory: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<TrackSummary>, String> {
    let rows = sqlx::query_as::<_, TrackRow>(
        "SELECT id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks ORDER BY created_at DESC",
    )
    .fetch_all(&db.0)
    .await
    .map_err(|e| format!("Failed to list tracks: {}", e))?;

    let mut tracks = Vec::with_capacity(rows.len());
    for row in rows {
        let album_art_data = album_art_for_row(&row);
        tracks.push(TrackSummary::from_row(row, album_art_data));
    }

    Ok(tracks)
}

#[tauri::command]
pub async fn import_track(
    db: State<'_, Db>,
    app_handle: AppHandle,
    file_path: String,
) -> Result<TrackSummary, String> {
    log_import_stage("setup storage");
    ensure_storage(&app_handle)?;
    let (tracks_dir, art_dir, _) = storage_dirs(&app_handle)?;

    let source_path = Path::new(&file_path);
    if !source_path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }

    log_import_stage("computing track hash");
    let track_hash = compute_track_hash(source_path)?;
    if let Some(existing) = sqlx::query_as::<_, TrackRow>(
        "SELECT id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks WHERE track_hash = ?",
    )
    .bind(&track_hash)
    .fetch_optional(&db.0)
    .await
    .map_err(|e| format!("Failed to inspect existing tracks: {}", e))?
    {
        run_import_workers(
            &db.0,
            existing.id,
            &existing.track_hash,
            Path::new(&existing.file_path),
            &app_handle,
            existing.duration_seconds.unwrap_or(0.0),
        )
        .await?;
        let album_art_data = album_art_for_row(&existing);
        return Ok(TrackSummary::from_row(existing, album_art_data));
    }

    log_import_stage("copying track file");
    let extension = source_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("bin");
    let dest_file_name = format!("{}.{}", Uuid::new_v4(), extension);
    let dest_path = tracks_dir.join(&dest_file_name);
    std::fs::copy(&source_path, &dest_path)
        .map_err(|e| format!("Failed to copy track file: {}", e))?;
    log_import_stage("probing metadata");

    let tagged_file = Probe::open(&dest_path)
        .map_err(|e| format!("Failed to probe track file: {}", e))?
        .read()
        .map_err(|e| format!("Failed to read track metadata: {}", e))?;

    let primary_tag = tagged_file.primary_tag();
    let title = primary_tag.and_then(|tag| tag.title().map(|s| s.to_string()));
    let artist = primary_tag.and_then(|tag| tag.artist().map(|s| s.to_string()));
    let album = primary_tag.and_then(|tag| tag.album().map(|s| s.to_string()));
    let track_number = primary_tag.and_then(|tag| tag.track()).map(|n| n as i64);
    let disc_number = primary_tag.and_then(|tag| tag.disk()).map(|n| n as i64);

    let duration_seconds = Some(tagged_file.properties().duration().as_secs_f64());
    let (album_art_path, album_art_mime, album_art_data) = match primary_tag.and_then(|tag| {
        tag.pictures()
            .iter()
            .find(|pic| {
                matches!(
                    pic.pic_type(),
                    PictureType::CoverFront | PictureType::CoverBack | PictureType::Other
                )
            })
            .cloned()
    }) {
        Some(picture) => {
            let mime = picture
                .mime_type()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "application/octet-stream".into());
            let art_extension = match mime.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/bmp" => "bmp",
                _ => "bin",
            };
            let art_file_name = format!("{}.{}", Uuid::new_v4(), art_extension);
            let art_path = art_dir.join(&art_file_name);
            std::fs::write(&art_path, picture.data())
                .map_err(|e| format!("Failed to write album art: {}", e))?;
            let data = format!("data:{};base64,{}", mime, STANDARD.encode(picture.data()));
            (
                Some(art_path.to_string_lossy().into_owned()),
                Some(mime),
                Some(data),
            )
        }
        None => (None, None, None),
    };

    let query = sqlx::query(
        "INSERT INTO tracks (track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&track_hash)
    .bind(&title)
    .bind(&artist)
    .bind(&album)
    .bind(track_number)
    .bind(disc_number)
    .bind(duration_seconds)
    .bind(dest_path.to_string_lossy().into_owned())
    .bind(album_art_path.clone())
    .bind(album_art_mime.clone())
    .execute(&db.0)
    .await
    .map_err(|e| format!("Failed to insert track: {}", e))?;

    let id = query.last_insert_rowid();

    let row = sqlx::query_as::<_, TrackRow>(
        "SELECT id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch imported track: {}", e))?;

    run_import_workers(
        &db.0,
        id,
        &track_hash,
        &dest_path,
        &app_handle,
        duration_seconds.unwrap_or(0.0),
    )
    .await?;

    let album_data = album_art_data.or_else(|| album_art_for_row(&row));

    log_import_stage("finished import");
    Ok(TrackSummary::from_row(row, album_data))
}

#[tauri::command]
pub async fn get_melspec(
    db: State<'_, Db>,
    fft_service: State<'_, crate::audio::FftService>,
    track_id: i64,
) -> Result<MelSpec, String> {
    let (file_path, track_hash): (String, String) =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(&db.0)
            .await
            .map_err(|e| format!("Failed to load track path: {}", e))?;

    let path = PathBuf::from(&file_path);
    let width = MEL_SPEC_WIDTH;
    let height = MEL_SPEC_HEIGHT;

    // Clone the service to move into the blocking task
    let fft = fft_service.inner().clone();

    let data = tauri::async_runtime::spawn_blocking(move || {
        let (samples, sample_rate) = load_or_decode_audio(&path, &track_hash, TARGET_SAMPLE_RATE)?;
        Ok::<_, String>(generate_melspec(&fft, &samples, sample_rate, width, height))
    })
    .await
    .map_err(|e| format!("Mel spec worker failed: {}", e))??;

    Ok(MelSpec {
        width,
        height,
        data,
        beat_grid: None,
    })
}

/// Get beat grid data for a track
#[tauri::command]
pub async fn get_track_beats(db: State<'_, Db>, track_id: i64) -> Result<Option<BeatGrid>, String> {
    let row: Option<(String, String, Option<f64>, Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar FROM track_beats WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch beat data: {}", e))?;

    match row {
        Some((beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)) => {
            let beats: Vec<f32> = serde_json::from_str(&beats_json)
                .map_err(|e| format!("Failed to parse beats: {}", e))?;
            let downbeats: Vec<f32> = serde_json::from_str(&downbeats_json)
                .map_err(|e| format!("Failed to parse downbeats: {}", e))?;
            let (fallback_bpm, fallback_offset, fallback_bpb) =
                infer_grid_metadata(&beats, &downbeats);
            let bpm_value = bpm.unwrap_or(fallback_bpm as f64) as f32;
            let offset_value = downbeat_offset.unwrap_or(fallback_offset as f64) as f32;
            let bpb_value = beats_per_bar.unwrap_or(fallback_bpb as i64) as i32;
            Ok(Some(BeatGrid {
                beats,
                downbeats,
                bpm: bpm_value,
                downbeat_offset: offset_value,
                beats_per_bar: bpb_value,
            }))
        }
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn delete_track(
    db: State<'_, Db>,
    app_handle: AppHandle,
    stem_cache: State<'_, crate::audio::StemCache>,
    track_id: i64,
) -> Result<(), String> {
    // Fetch track info before deletion
    let track_row: Option<(String, Option<String>, String)> =
        sqlx::query_as("SELECT file_path, album_art_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&db.0)
            .await
            .map_err(|e| format!("Failed to fetch track info: {}", e))?;

    let Some((file_path, album_art_path, track_hash)) = track_row else {
        return Err(format!("Track {} not found", track_id));
    };

    // Fetch logits_path if it exists
    let logits_path: Option<Option<String>> =
        sqlx::query_scalar("SELECT logits_path FROM track_roots WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(&db.0)
            .await
            .map_err(|e| format!("Failed to fetch logits path: {}", e))?;

    // Clean up stem cache
    stem_cache.remove_track(track_id);

    // Remove from in-progress sets
    {
        let mut guard = STEMS_IN_PROGRESS.lock().await;
        guard.remove(&track_id);
    }
    {
        let mut guard = ROOTS_IN_PROGRESS.lock().await;
        guard.remove(&track_id);
    }

    // Delete the track (CASCADE will handle related records)
    let result = sqlx::query("DELETE FROM tracks WHERE id = ?")
        .bind(track_id)
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to delete track: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("Track {} not found", track_id));
    }

    // Delete physical files
    let track_path = Path::new(&file_path);
    if track_path.exists() {
        std::fs::remove_file(track_path).map_err(|e| {
            format!(
                "Failed to delete track file {}: {}",
                track_path.display(),
                e
            )
        })?;
    }

    if let Some(art_path) = album_art_path {
        let art_path = Path::new(&art_path);
        if art_path.exists() {
            let _ = std::fs::remove_file(art_path); // Ignore errors for album art
        }
    }

    // Delete stems directory
    let (_, _, stems_dir) = storage_dirs(&app_handle)?;
    let stems_path = stems_dir.join(&track_hash);
    if stems_path.exists() {
        let _ = std::fs::remove_dir_all(&stems_path); // Ignore errors for stems
    }

    // Delete logits file if it exists
    if let Some(Some(logits_path_str)) = logits_path {
        let logits_path = Path::new(&logits_path_str);
        if logits_path.exists() {
            let _ = std::fs::remove_file(logits_path); // Ignore errors for logits
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn wipe_tracks(db: State<'_, Db>, app_handle: AppHandle) -> Result<(), String> {
    sqlx::query("DELETE FROM track_beats")
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to clear track beats: {}", e))?;
    sqlx::query("DELETE FROM track_roots")
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to clear track roots: {}", e))?;

    sqlx::query("DELETE FROM tracks")
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to clear tracks: {}", e))?;

    let (tracks_dir, _, _) = storage_dirs(&app_handle)?;
    if tracks_dir.exists() {
        std::fs::remove_dir_all(&tracks_dir).map_err(|e| {
            format!(
                "Failed to remove tracks directory {}: {}",
                tracks_dir.display(),
                e
            )
        })?;
    }
    ensure_storage(&app_handle)?;
    Ok(())
}

async fn ensure_track_beats_for_path(
    pool: &SqlitePool,
    track_id: i64,
    track_path: &Path,
    app_handle: &AppHandle,
) -> Result<(), String> {
    log_import_stage(&format!("checking beat cache for track {}", track_id));
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_beats WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect beat cache: {}", e))?;

    if existing.is_some() {
        log_import_stage(&format!("beat cache present for track {}", track_id));
        return Ok(());
    }

    let handle = app_handle.clone();
    let path = track_path.to_path_buf();
    log_import_stage(&format!("running beat worker for track {}", track_id));
    let beat_data =
        tauri::async_runtime::spawn_blocking(move || beat_worker::compute_beats(&handle, &path))
            .await
            .map_err(|e| format!("Beat worker task failed: {}", e))??;

    log_import_stage(&format!("beat worker completed for track {}", track_id));
    log_import_stage(&format!("persisting beat data for track {}", track_id));
    persist_track_beats(pool, track_id, &beat_data).await
}

async fn ensure_track_roots_for_path(
    pool: &SqlitePool,
    track_id: i64,
    audio_paths: &[PathBuf],
    app_handle: &AppHandle,
) -> Result<(), String> {
    log_import_stage(&format!("checking root-prob cache for track {}", track_id));
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_roots WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect root cache: {}", e))?;

    if existing.is_some() {
        log_import_stage(&format!("root cache present for track {}", track_id));
        return Ok(());
    }

    loop {
        let should_run = {
            let mut guard = ROOTS_IN_PROGRESS.lock().await;
            if guard.contains(&track_id) {
                false
            } else {
                guard.insert(track_id);
                true
            }
        };

        if !should_run {
            sleep(Duration::from_millis(250)).await;
            continue;
        }

        let handle = app_handle.clone();
        let paths = audio_paths.to_vec();
        log_import_stage(&format!("running root worker for track {}", track_id));
        let root_data = tauri::async_runtime::spawn_blocking(move || {
            root_worker::compute_roots(&handle, &paths)
        })
        .await
        .map_err(|e| format!("Root worker task failed: {}", e))??;

        log_import_stage(&format!("root worker completed for track {}", track_id));

        let persist_result = persist_track_roots(pool, track_id, &root_data).await;

        {
            let mut guard = ROOTS_IN_PROGRESS.lock().await;
            guard.remove(&track_id);
        }

        return persist_result;
    }
}

async fn persist_track_beats(
    pool: &SqlitePool,
    track_id: i64,
    beat_data: &BeatAnalysis,
) -> Result<(), String> {
    let beats_json = serde_json::to_string(&beat_data.beats)
        .map_err(|e| format!("Failed to serialize beats: {}", e))?;
    let downbeats_json = serde_json::to_string(&beat_data.downbeats)
        .map_err(|e| format!("Failed to serialize downbeats: {}", e))?;

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
    .bind(beat_data.bpm)
    .bind(beat_data.downbeat_offset)
    .bind(beat_data.beats_per_bar)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist beat data: {}", e))?;

    Ok(())
}

pub(crate) fn infer_grid_metadata(beats: &[f32], downbeats: &[f32]) -> (f32, f32, i64) {
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

async fn persist_track_roots(
    pool: &SqlitePool,
    track_id: i64,
    root_data: &RootAnalysis,
) -> Result<(), String> {
    let sections_json = serde_json::to_string(&root_data.sections)
        .map_err(|e| format!("Failed to serialize chord sections: {}", e))?;

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
    .bind(&root_data.logits_path)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist root data: {}", e))?;

    Ok(())
}

pub async fn ensure_track_stems(
    pool: &SqlitePool,
    track_id: i64,
    track_hash: &str,
    track_path: &Path,
    app_handle: &AppHandle,
) -> Result<(), String> {
    ensure_storage(app_handle)?;
    let (_, _, stems_dir) = storage_dirs(app_handle)?;
    ensure_track_stems_for_path(
        pool, track_id, track_hash, track_path, &stems_dir, app_handle,
    )
    .await
}

async fn run_import_workers(
    pool: &SqlitePool,
    track_id: i64,
    track_hash: &str,
    track_path: &Path,
    app_handle: &AppHandle,
    duration_seconds: f64,
) -> Result<(), String> {
    ensure_storage(app_handle)?;
    let (_, _, stems_dir) = storage_dirs(app_handle)?;

    let beats = ensure_track_beats_for_path(pool, track_id, track_path, app_handle);
    let stems = ensure_track_stems_for_path(
        pool, track_id, track_hash, track_path, &stems_dir, app_handle,
    );
    let waveforms =
        crate::waveforms::ensure_track_waveform(pool, track_id, track_path, duration_seconds);

    // 1. Run non-dependent workers + stem separation
    tokio::try_join!(beats, stems, waveforms).map(|_| ())?;

    // 2. Run roots worker using the generated stems (bass + other) for cleaner harmony analysis
    let track_stems_dir = stems_dir.join(track_hash);
    let bass_path = track_stems_dir.join("bass.wav");
    let other_path = track_stems_dir.join("other.wav");

    let root_sources = if bass_path.exists() && other_path.exists() {
        vec![bass_path, other_path]
    } else {
        // Fallback to original track if stems failed (shouldn't happen if stems task succeeded)
        eprintln!("[import] Warning: Stems missing for track {}, falling back to full mix for harmony analysis", track_id);
        vec![track_path.to_path_buf()]
    };

    ensure_track_roots_for_path(pool, track_id, &root_sources, app_handle).await
}

async fn ensure_track_stems_for_path(
    pool: &SqlitePool,
    track_id: i64,
    track_hash: &str,
    track_path: &Path,
    stems_dir: &Path,
    app_handle: &AppHandle,
) -> Result<(), String> {
    loop {
        log_import_stage(&format!("checking stem cache for track {}", track_id));
        let existing: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM track_stems WHERE track_id = ? LIMIT 1")
                .bind(track_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("Failed to inspect stem cache: {}", e))?;

        if existing.is_some() {
            return Ok(());
        }

        let should_run = {
            let mut guard = STEMS_IN_PROGRESS.lock().await;
            if guard.contains(&track_id) {
                false
            } else {
                guard.insert(track_id);
                true
            }
        };

        if !should_run {
            sleep(Duration::from_millis(250)).await;
            continue;
        }

        let handle = app_handle.clone();
        let path = track_path.to_path_buf();
        let stems_root = stems_dir.join(track_hash);
        log_import_stage(&format!("running stem worker for track {}", track_id));
        let stem_files = tauri::async_runtime::spawn_blocking(move || {
            stem_worker::separate_stems(&handle, &path, &stems_root)
        })
        .await
        .map_err(|e| format!("Stem worker task failed: {}", e))??;

        log_import_stage(&format!("stem worker completed for track {}", track_id));

        let persist_result = persist_track_stems(pool, track_id, &stem_files).await;

        {
            let mut guard = STEMS_IN_PROGRESS.lock().await;
            guard.remove(&track_id);
        }

        return persist_result;
    }
}

async fn persist_track_stems(
    pool: &SqlitePool,
    track_id: i64,
    stems: &[stem_worker::StemFile],
) -> Result<(), String> {
    log_import_stage(&format!(
        "persisting {} stems for track {}",
        stems.len(),
        track_id
    ));
    for stem in stems {
        sqlx::query(
            "INSERT INTO track_stems (track_id, stem_name, file_path)
             VALUES (?, ?, ?)
             ON CONFLICT(track_id, stem_name) DO UPDATE SET
                file_path = excluded.file_path,
                updated_at = datetime('now')",
        )
        .bind(track_id)
        .bind(&stem.name)
        .bind(stem.path.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to persist stem data: {}", e))?;
    }

    log_import_stage(&format!("stored stems for track {}", track_id));
    Ok(())
}

fn log_import_stage(stage: &str) {
    eprintln!("[import_track] {}", stage);
}

fn storage_dirs(
    app: &AppHandle,
) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf), String> {
    let app_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to locate app config dir: {}", e))?;
    let tracks_dir = app_dir.join("tracks");
    let art_dir = tracks_dir.join("art");
    let stems_dir = tracks_dir.join("stems");
    Ok((tracks_dir, art_dir, stems_dir))
}

fn compute_track_hash(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|e| format!("Failed to open track for hashing: {}", e))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|e| format!("Failed to hash track: {}", e))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_album_art(path: &str, mime: &str) -> Option<String> {
    std::fs::read(path).ok().and_then(|data| {
        if mime.is_empty() {
            None
        } else {
            Some(format!("data:{};base64,{}", mime, STANDARD.encode(data)))
        }
    })
}
