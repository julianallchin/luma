//! Business logic for track operations.
//!
//! The database layer (database/local/tracks.rs) is pure SQL/CRUD. All
//! filesystem work, hashing, audio workers, and orchestration live here.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

use crate::audio::{
    generate_melspec, load_or_decode_audio, FftService, StemCache, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH,
};
use crate::beat_worker::{self, BeatAnalysis};
use crate::database::local::tracks as tracks_db;
use crate::models::tracks::{MelSpec, TrackSummary};
use crate::root_worker::{self, RootAnalysis};
use crate::schema::BeatGrid;
use crate::stem_worker;

pub const TARGET_SAMPLE_RATE: u32 = 48_000;

static STEMS_IN_PROGRESS: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static ROOTS_IN_PROGRESS: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));

fn track_summary_from_row(
    row: tracks_db::TrackRow,
    album_art_data: Option<String>,
) -> TrackSummary {
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

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

/// List all tracks with album art data inlined.
pub async fn list_tracks(pool: &SqlitePool) -> Result<Vec<TrackSummary>, String> {
    let rows = tracks_db::list_tracks(pool).await?;
    let mut tracks = Vec::with_capacity(rows.len());
    for row in rows {
        let album_art_data = album_art_for_row(&row);
        tracks.push(track_summary_from_row(row, album_art_data));
    }
    Ok(tracks)
}

/// Import a new track from the filesystem.
pub async fn import_track(
    pool: &SqlitePool,
    app_handle: AppHandle,
    stem_cache: &StemCache,
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
    if let Some(existing) = tracks_db::get_track_by_hash(pool, &track_hash).await? {
        run_import_workers(
            pool,
            existing.id,
            &existing.track_hash,
            Path::new(&existing.file_path),
            &app_handle,
            stem_cache,
            existing.duration_seconds.unwrap_or(0.0),
        )
        .await?;
        let album_art_data = album_art_for_row(&existing);
        return Ok(track_summary_from_row(existing, album_art_data));
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

    let id = tracks_db::insert_track_record(
        pool,
        &track_hash,
        &title,
        &artist,
        &album,
        track_number,
        disc_number,
        duration_seconds,
        &dest_path.to_string_lossy(),
        &album_art_path,
        &album_art_mime,
    )
    .await?;

    let row = tracks_db::get_track_by_id(pool, id)
        .await?
        .ok_or_else(|| format!("Failed to fetch imported track {}", id))?;

    run_import_workers(
        pool,
        id,
        &track_hash,
        &dest_path,
        &app_handle,
        stem_cache,
        duration_seconds.unwrap_or(0.0),
    )
    .await?;

    let album_data = album_art_data.or_else(|| album_art_for_row(&row));

    log_import_stage("finished import");
    Ok(track_summary_from_row(row, album_data))
}

/// Get mel spectrogram for a track.
pub async fn get_melspec(
    pool: &SqlitePool,
    fft_service: &FftService,
    track_id: i64,
) -> Result<MelSpec, String> {
    let (file_path, track_hash) = tracks_db::get_track_path_and_hash(pool, track_id)
        .await
        .map_err(|e| format!("Failed to load track path: {}", e))?;

    let path = PathBuf::from(&file_path);
    let width = MEL_SPEC_WIDTH;
    let height = MEL_SPEC_HEIGHT;

    let fft = fft_service.clone();

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

/// Get beat grid for a track.
pub async fn get_track_beats(pool: &SqlitePool, track_id: i64) -> Result<Option<BeatGrid>, String> {
    let row = tracks_db::get_track_beats_raw(pool, track_id).await?;

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

/// Delete a track and its derived data.
pub async fn delete_track(
    pool: &SqlitePool,
    app_handle: AppHandle,
    stem_cache: &StemCache,
    track_id: i64,
) -> Result<(), String> {
    let track_row = tracks_db::get_track_file_info(pool, track_id).await?;
    let Some((file_path, album_art_path, track_hash)) = track_row else {
        return Err(format!("Track {} not found", track_id));
    };

    let logits_path = tracks_db::get_logits_path(pool, track_id).await?;

    stem_cache.remove_track(track_id);

    {
        let mut guard = STEMS_IN_PROGRESS.lock().await;
        guard.remove(&track_id);
    }
    {
        let mut guard = ROOTS_IN_PROGRESS.lock().await;
        guard.remove(&track_id);
    }

    let rows = tracks_db::delete_track_record(pool, track_id).await?;
    if rows == 0 {
        return Err(format!("Track {} not found", track_id));
    }

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
            let _ = std::fs::remove_file(art_path);
        }
    }

    let (_, _, stems_dir) = storage_dirs(&app_handle)?;
    let stems_path = stems_dir.join(&track_hash);
    if stems_path.exists() {
        let _ = std::fs::remove_dir_all(&stems_path);
    }

    if let Some(logits_path_str) = logits_path {
        let logits_path = Path::new(&logits_path_str);
        if logits_path.exists() {
            let _ = std::fs::remove_file(logits_path);
        }
    }

    Ok(())
}

/// Wipe all track data from the database and filesystem.
pub async fn wipe_tracks(pool: &SqlitePool, app_handle: AppHandle) -> Result<(), String> {
    tracks_db::wipe_tracks(pool).await?;

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

// -----------------------------------------------------------------------------
// Worker orchestration
// -----------------------------------------------------------------------------

async fn run_import_workers(
    pool: &SqlitePool,
    track_id: i64,
    track_hash: &str,
    track_path: &Path,
    app_handle: &AppHandle,
    stem_cache: &StemCache,
    duration_seconds: f64,
) -> Result<(), String> {
    ensure_storage(app_handle)?;
    let (_, _, stems_dir) = storage_dirs(app_handle)?;

    let beats = ensure_track_beats_for_path(pool, track_id, track_path, app_handle);
    let stems = ensure_track_stems_for_path(
        pool, track_id, track_hash, track_path, &stems_dir, app_handle, stem_cache,
    );
    let waveforms = crate::services::waveforms::ensure_track_waveform(
        pool,
        track_id,
        track_path,
        duration_seconds,
    );

    tokio::try_join!(beats, stems, waveforms).map(|_| ())?;

    let track_stems_dir = stems_dir.join(track_hash);
    let bass_path = track_stems_dir.join("bass.wav");
    let other_path = track_stems_dir.join("other.wav");

    let root_sources = if bass_path.exists() && other_path.exists() {
        vec![bass_path, other_path]
    } else {
        eprintln!("[import] Warning: Stems missing for track {}, falling back to full mix for harmony analysis", track_id);
        vec![track_path.to_path_buf()]
    };

    ensure_track_roots_for_path(pool, track_id, &root_sources, app_handle).await
}

async fn ensure_track_beats_for_path(
    pool: &SqlitePool,
    track_id: i64,
    track_path: &Path,
    app_handle: &AppHandle,
) -> Result<(), String> {
    log_import_stage(&format!("checking beat cache for track {}", track_id));
    if tracks_db::track_has_beats(pool, track_id).await? {
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
    if tracks_db::track_has_roots(pool, track_id).await? {
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

async fn ensure_track_stems_for_path(
    pool: &SqlitePool,
    track_id: i64,
    track_hash: &str,
    track_path: &Path,
    stems_dir: &Path,
    app_handle: &AppHandle,
    stem_cache: &StemCache,
) -> Result<(), String> {
    loop {
        log_import_stage(&format!("checking stem cache for track {}", track_id));
        if tracks_db::track_has_stems(pool, track_id).await? {
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

        for stem in &stem_files {
            let cache_tag = format!("{}_stem_{}", track_hash, stem.name);
            if let Ok((samples, rate)) =
                load_or_decode_audio(&stem.path, &cache_tag, TARGET_SAMPLE_RATE)
            {
                if !samples.is_empty() && rate > 0 {
                    stem_cache.insert(track_id, stem.name.clone(), samples.into(), rate);
                }
            }
        }

        {
            let mut guard = STEMS_IN_PROGRESS.lock().await;
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

    tracks_db::upsert_track_beats(
        pool,
        track_id,
        &beats_json,
        &downbeats_json,
        Some(beat_data.bpm as f64),
        Some(beat_data.downbeat_offset as f64),
        Some(beat_data.beats_per_bar as i64),
    )
    .await
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

    tracks_db::upsert_track_roots(
        pool,
        track_id,
        &sections_json,
        root_data.logits_path.as_deref(),
    )
    .await
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
        tracks_db::upsert_track_stem(
            pool,
            track_id,
            &stem.name,
            &stem.path.to_string_lossy(),
            None,
        )
        .await?;
    }

    log_import_stage(&format!("stored stems for track {}", track_id));
    Ok(())
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

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

fn album_art_for_row(row: &tracks_db::TrackRow) -> Option<String> {
    match (&row.album_art_path, &row.album_art_mime) {
        (Some(path), Some(mime)) => read_album_art(path, mime),
        _ => None,
    }
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
