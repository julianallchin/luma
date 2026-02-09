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
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

use crate::audio::{
    generate_melspec, load_or_decode_audio, FftService, StemCache, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH,
};
use crate::beat_worker::{self, BeatAnalysis};
use crate::database::local::tracks as tracks_db;
use crate::engine_dj::types::EngineDjTrack;
use crate::models::tracks::{MelSpec, TrackBrowserRow, TrackSummary};
use crate::node_graph::BeatGrid;
use crate::root_worker::{self, RootAnalysis};
use crate::stem_worker;

pub const TARGET_SAMPLE_RATE: u32 = 48_000;

/// Source metadata for tracks imported from DJ libraries.
pub struct TrackSourceInfo {
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub source_filename: Option<String>,
}

static STEMS_IN_PROGRESS: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static ROOTS_IN_PROGRESS: Lazy<Mutex<HashSet<i64>>> = Lazy::new(|| Mutex::new(HashSet::new()));

fn emit_track_status_changed(app_handle: &AppHandle, track_id: i64) {
    let _ = app_handle.emit("track-status-changed", track_id);
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

/// List all tracks with album art data inlined.
pub async fn list_tracks(pool: &SqlitePool) -> Result<Vec<TrackSummary>, String> {
    let rows = tracks_db::list_tracks(pool).await?;
    let mut tracks = Vec::with_capacity(rows.len());
    for mut row in rows {
        row.album_art_data = album_art_for_row(&row);
        tracks.push(row);
    }
    Ok(tracks)
}

/// List all tracks with enriched metadata for the browser view.
pub async fn list_tracks_enriched(pool: &SqlitePool) -> Result<Vec<TrackBrowserRow>, String> {
    let rows = tracks_db::list_tracks_enriched(pool).await?;
    let mut tracks = Vec::with_capacity(rows.len());
    for mut row in rows {
        row.album_art_data = match (&row.album_art_path, &row.album_art_mime) {
            (Some(path), Some(mime)) => read_album_art(path, mime),
            _ => None,
        };
        tracks.push(row);
    }
    Ok(tracks)
}

/// Import a new track from the filesystem.
pub async fn import_track(
    pool: &SqlitePool,
    app_handle: AppHandle,
    stem_cache: &StemCache,
    file_path: String,
    uid: Option<String>,
) -> Result<TrackSummary, String> {
    let basename = Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());
    let source = TrackSourceInfo {
        source_type: Some("file".to_string()),
        source_id: None,
        source_filename: basename,
    };
    import_track_with_source(pool, app_handle, stem_cache, file_path, uid, Some(source)).await
}

/// Import a track with optional source metadata from a DJ library.
pub async fn import_track_with_source(
    pool: &SqlitePool,
    app_handle: AppHandle,
    stem_cache: &StemCache,
    file_path: String,
    uid: Option<String>,
    source: Option<TrackSourceInfo>,
) -> Result<TrackSummary, String> {
    log_import_stage("setup storage");
    ensure_storage(&app_handle)?;
    let (tracks_dir, _, _) = storage_dirs(&app_handle)?;

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
        let mut track_summary = existing;
        track_summary.album_art_data = album_art_for_row(&track_summary);
        return Ok(track_summary);
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
    let (album_art_path, album_art_mime, album_art_data) =
        extract_album_art(&app_handle, &dest_path)?;

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
        uid,
        source.as_ref().and_then(|s| s.source_type.as_deref()),
        source.as_ref().and_then(|s| s.source_id.as_deref()),
        source.as_ref().and_then(|s| s.source_filename.as_deref()),
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

    let mut track_summary = row;
    track_summary.album_art_data = album_art_data.or_else(|| album_art_for_row(&track_summary));

    log_import_stage("finished import");
    Ok(track_summary)
}

/// Extract album art from an audio file and save it to the art directory.
/// Returns (art_path, art_mime, art_data_uri).
fn extract_album_art(
    app_handle: &AppHandle,
    source_path: &Path,
) -> Result<(Option<String>, Option<String>, Option<String>), String> {
    let (_, art_dir, _) = storage_dirs(app_handle)?;

    let tagged_file = match Probe::open(source_path) {
        Ok(probe) => match probe.read() {
            Ok(tf) => tf,
            Err(_) => return Ok((None, None, None)),
        },
        Err(_) => return Ok((None, None, None)),
    };

    let primary_tag = tagged_file.primary_tag();
    let picture = primary_tag.and_then(|tag| {
        tag.pictures()
            .iter()
            .find(|pic| {
                matches!(
                    pic.pic_type(),
                    PictureType::CoverFront | PictureType::CoverBack | PictureType::Other
                )
            })
            .cloned()
    });

    match picture {
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
            Ok((
                Some(art_path.to_string_lossy().into_owned()),
                Some(mime),
                Some(data),
            ))
        }
        None => Ok((None, None, None)),
    }
}

/// Fast import for Engine DJ tracks — inserts DB record using Engine DJ metadata directly.
/// No file copy, no hash, no tag reading (except album art), no analysis workers.
/// Returns the new track ID (or existing ID if already imported via source_id dedup).
pub async fn engine_dj_fast_import(
    pool: &SqlitePool,
    app_handle: &AppHandle,
    engine_track: &EngineDjTrack,
    audio_path: &Path,
    source_id: &str,
    uid: Option<String>,
) -> Result<(i64, bool), String> {
    // Dedup by source_id — no file I/O needed
    if let Some(existing) = tracks_db::get_track_by_source_id(pool, "engine_dj", source_id).await? {
        return Ok((existing.id, false));
    }

    ensure_storage(app_handle)?;

    // Extract album art (only file I/O — reads just the tag header)
    let (album_art_path, album_art_mime, _album_art_data) =
        extract_album_art(app_handle, audio_path)?;

    // Placeholder hash satisfies NOT NULL UNIQUE constraint
    let placeholder_hash = format!("pending:{}", Uuid::new_v4());

    let id = tracks_db::insert_track_record(
        pool,
        &placeholder_hash,
        &engine_track.title,
        &engine_track.artist,
        &engine_track.album,
        None, // track_number
        None, // disc_number
        engine_track.length,
        &audio_path.to_string_lossy(),
        &album_art_path,
        &album_art_mime,
        uid,
        Some("engine_dj"),
        Some(source_id),
        Some(&engine_track.filename),
    )
    .await?;

    Ok((id, true))
}

/// Run background analysis for a batch of tracks (hash, metadata gap-fill, workers).
pub async fn run_background_analysis(
    pool: SqlitePool,
    app_handle: AppHandle,
    stem_cache: StemCache,
    track_ids: Vec<i64>,
) {
    for track_id in track_ids {
        if let Err(e) = run_single_track_analysis(&pool, &app_handle, &stem_cache, track_id).await {
            eprintln!("[background_analysis] track {} failed: {}", track_id, e);
        }
    }
    eprintln!("[background_analysis] finished all tracks");
}

async fn run_single_track_analysis(
    pool: &SqlitePool,
    app_handle: &AppHandle,
    stem_cache: &StemCache,
    track_id: i64,
) -> Result<(), String> {
    let track = tracks_db::get_track_by_id(pool, track_id)
        .await?
        .ok_or_else(|| format!("Track {} not found", track_id))?;

    let file_path = Path::new(&track.file_path);
    if !file_path.exists() {
        return Err(format!("File not found: {}", track.file_path));
    }

    // Compute real SHA256 hash
    let real_hash = compute_track_hash(file_path)?;

    // Check for hash collision (same file imported via regular import too)
    let hash_in_use = tracks_db::get_track_by_hash(pool, &real_hash).await?;
    if hash_in_use.is_none() {
        // No collision — update the placeholder hash
        tracks_db::update_track_hash(pool, track_id, &real_hash).await?;
    }
    // If there IS a collision, keep the placeholder to avoid UNIQUE violation.
    // The track is still usable — it just has a synthetic hash.

    // Resolve the final hash for this track (may still be placeholder)
    let current = tracks_db::get_track_path_and_hash(pool, track_id).await?;
    let track_hash = &current.track_hash;

    // Fill metadata gaps from file tags as fallback
    let tagged_file = Probe::open(file_path).ok().and_then(|p| p.read().ok());
    if let Some(tf) = &tagged_file {
        let primary_tag = tf.primary_tag();
        let tag_title = primary_tag.and_then(|t| t.title().map(|s| s.to_string()));
        let tag_artist = primary_tag.and_then(|t| t.artist().map(|s| s.to_string()));
        let tag_album = primary_tag.and_then(|t| t.album().map(|s| s.to_string()));
        let tag_duration = Some(tf.properties().duration().as_secs_f64());

        tracks_db::fill_track_metadata_gaps(
            pool,
            track_id,
            &tag_title,
            &tag_artist,
            &tag_album,
            tag_duration,
        )
        .await?;
    }

    // Get duration for waveform worker
    let duration = tracks_db::get_track_duration(pool, track_id)
        .await?
        .unwrap_or(0.0);

    ensure_storage(app_handle)?;
    let (_, _, stems_dir) = storage_dirs(app_handle)?;

    // Priority pass: beats + waveforms first (what the user sees immediately)
    let beats = ensure_track_beats_for_path(pool, track_id, file_path, app_handle);
    let waveforms =
        crate::services::waveforms::ensure_track_waveform(pool, track_id, file_path, duration);
    tokio::try_join!(beats, waveforms)?;

    // Second pass: stems + roots (heavy, less immediately visible)
    ensure_track_stems_for_path(
        pool, track_id, track_hash, file_path, &stems_dir, app_handle, stem_cache,
    )
    .await?;

    let track_stems_dir = stems_dir.join(track_hash);
    let bass_path = track_stems_dir.join("bass.wav");
    let other_path = track_stems_dir.join("other.wav");
    let root_sources = if bass_path.exists() && other_path.exists() {
        vec![bass_path, other_path]
    } else {
        vec![file_path.to_path_buf()]
    };
    ensure_track_roots_for_path(pool, track_id, &root_sources, app_handle).await?;

    Ok(())
}

/// Get mel spectrogram for a track.
pub async fn get_melspec(
    pool: &SqlitePool,
    fft_service: &FftService,
    track_id: i64,
) -> Result<MelSpec, String> {
    let info = tracks_db::get_track_path_and_hash(pool, track_id)
        .await
        .map_err(|e| format!("Failed to load track path: {}", e))?;
    let file_path = info.file_path;
    let track_hash = info.track_hash;

    let path = PathBuf::from(&file_path);
    let width = MEL_SPEC_WIDTH;
    let height = MEL_SPEC_HEIGHT;

    let fft = fft_service.clone();

    let data = tauri::async_runtime::spawn_blocking(move || {
        let audio = load_or_decode_audio(&path, &track_hash, TARGET_SAMPLE_RATE)?;
        // Convert stereo to mono for mel spectrogram analysis
        let mono_samples = audio.to_mono();
        Ok::<_, String>(generate_melspec(
            &fft,
            &mono_samples,
            audio.sample_rate,
            width,
            height,
        ))
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
        Some(track_beats) => {
            let beats: Vec<f32> = serde_json::from_str(&track_beats.beats_json)
                .map_err(|e| format!("Failed to parse beats: {}", e))?;
            let downbeats: Vec<f32> = serde_json::from_str(&track_beats.downbeats_json)
                .map_err(|e| format!("Failed to parse downbeats: {}", e))?;
            let (fallback_bpm, fallback_offset, fallback_bpb) =
                infer_grid_metadata(&beats, &downbeats);
            let bpm_value = track_beats.bpm.unwrap_or(fallback_bpm as f64) as f32;
            let offset_value = track_beats
                .downbeat_offset
                .unwrap_or(fallback_offset as f64) as f32;
            let bpb_value = track_beats.beats_per_bar.unwrap_or(fallback_bpb as i64) as i32;
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
    let Some(track_info) = tracks_db::get_track_file_info(pool, track_id).await? else {
        return Err(format!("Track {} not found", track_id));
    };
    let file_path = track_info.file_path;
    let album_art_path = track_info.album_art_path;
    let track_hash = track_info.track_hash;

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

    // Only delete the audio file if it lives inside the app's managed tracks/ directory.
    // Engine DJ imports point file_path at the user's original music file — we must not delete those.
    let track_path = Path::new(&file_path);
    let (tracks_dir, _, _) = storage_dirs(&app_handle)?;
    if track_path.starts_with(&tracks_dir) && track_path.exists() {
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
    persist_track_beats(pool, track_id, &beat_data).await?;
    emit_track_status_changed(app_handle, track_id);
    Ok(())
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

        if persist_result.is_ok() {
            emit_track_status_changed(app_handle, track_id);
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
            if let Ok(audio) = load_or_decode_audio(&stem.path, &cache_tag, TARGET_SAMPLE_RATE) {
                if !audio.samples.is_empty() && audio.sample_rate > 0 {
                    // Store stereo samples for stems (same format as main audio)
                    stem_cache.insert(
                        track_id,
                        stem.name.clone(),
                        audio.samples.into(),
                        audio.sample_rate,
                    );
                }
            }
        }

        {
            let mut guard = STEMS_IN_PROGRESS.lock().await;
            guard.remove(&track_id);
        }

        if persist_result.is_ok() {
            emit_track_status_changed(app_handle, track_id);
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

fn album_art_for_row(row: &TrackSummary) -> Option<String> {
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
