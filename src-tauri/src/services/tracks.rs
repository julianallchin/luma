//! Business logic for track operations.
//!
//! The database layer (database/local/tracks.rs) is pure SQL/CRUD. All
//! filesystem work, hashing, audio workers, and orchestration live here.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use sha2::{Digest, Sha256};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::audio::{
    generate_melspec, load_or_decode_audio, FftService, StemCache, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH,
};
use crate::database::local::tracks as tracks_db;
use crate::database::local::tracks::ArtifactVersions;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read as VenueRead, VenueAccess, VenueResource,
};
use crate::engine_dj::types::EngineDjTrack;
use crate::models::tracks::{MelSpec, TrackBrowserRow, TrackSummary};
use crate::node_graph::BeatGrid;
use crate::preprocessing::{registry, scheduler, AnalysisEpoch, AnalysisGuard, AnalysisTaskGroup};
use crate::storage::StorageRoot;

pub const TARGET_SAMPLE_RATE: u32 = 48_000;

/// Maximum track duration allowed for import (10 minutes).
const MAX_TRACK_DURATION_SECS: f64 = 600.0;

/// Source metadata for tracks imported from DJ libraries.
pub struct TrackSourceInfo {
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub source_filename: Option<String>,
}

/// A filesystem artifact created before its catalog row commits. Dropping the
/// guard removes only that newly-created file; reused source audio is never
/// wrapped. Once SQLite owns the path, `keep` transfers cleanup to the normal
/// track deletion lifecycle.
struct PendingImportFile {
    path: Option<PathBuf>,
}

impl PendingImportFile {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn from_optional(path: Option<&str>) -> Option<Self> {
        path.map(|path| Self::new(PathBuf::from(path)))
    }

    fn keep(&mut self) {
        self.path = None;
    }
}

impl Drop for PendingImportFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::error!(
                        "[tracks] failed to roll back uncommitted import file {}: {error}",
                        path.display()
                    );
                }
            }
        }
    }
}

fn emit_import_progress(app_handle: &AppHandle, track_id: &str, step: &str) {
    let _ = app_handle.emit("track-import-progress", (track_id, step));
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

/// List all tracks. Album art is not inlined — callers load it from
/// `album_art_path` via Tauri's asset protocol (`convertFileSrc`) so the
/// payload stays small and the browser can decode images lazily.
pub async fn list_tracks(pool: &SqlitePool) -> Result<Vec<TrackSummary>, String> {
    tracks_db::list_tracks(pool).await
}

/// List all tracks with enriched metadata for the browser view. See
/// [`list_tracks`] for why album art bytes aren't inlined.
pub async fn list_tracks_enriched(
    pool: &SqlitePool,
    venue_id: Option<&str>,
) -> Result<Vec<TrackBrowserRow>, String> {
    let versions = current_artifact_versions();
    match venue_id {
        Some(venue_id) => {
            let mut access =
                VenueAccess::<VenueRead>::read(pool, VenueResource::Venue(venue_id)).await?;
            tracks_db::list_tracks_enriched_for_connection(
                access.connection(),
                Some(venue_id),
                versions,
            )
            .await
        }
        None => tracks_db::list_tracks_enriched(pool, None, versions).await,
    }
}

/// Resolve current preprocessor versions for the artifact tables surfaced in
/// the track browser. A missing entry means "no registered preprocessor for
/// that table"; we fall back to 0 so the EXISTS check matches any row.
fn current_artifact_versions() -> ArtifactVersions {
    let map = registry::current_artifact_versions();
    let v = |table: &'static str| map.get(table).copied().unwrap_or(0) as i64;
    ArtifactVersions {
        beats: v("track_beats"),
        stems: v("track_stems"),
        roots: v("track_roots"),
        drum_onsets: v("track_drum_onsets"),
        bar_classifications: v("track_bar_classifications"),
        genres: v("track_genres"),
    }
}

/// Import a new track from the filesystem.
pub async fn import_track(
    pool: &SqlitePool,
    app_handle: AppHandle,
    stem_cache: &StemCache,
    analysis_tasks: &AnalysisTaskGroup,
    file_path: String,
    uid: Option<String>,
) -> Result<TrackSummary, String> {
    let analysis_epoch = analysis_tasks.current_epoch()?;
    let basename = Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());
    let source = TrackSourceInfo {
        source_type: Some("file".to_string()),
        source_id: None,
        source_filename: basename,
    };
    import_track_with_source(
        pool,
        app_handle,
        stem_cache,
        analysis_tasks,
        analysis_epoch,
        file_path,
        uid,
        Some(source),
    )
    .await
}

/// Import a track with optional source metadata from a DJ library.
pub async fn import_track_with_source(
    pool: &SqlitePool,
    app_handle: AppHandle,
    stem_cache: &StemCache,
    analysis_tasks: &AnalysisTaskGroup,
    analysis_epoch: AnalysisEpoch,
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

    // Check duration before copying/hashing to reject long tracks early
    if let Ok(probe) = Probe::open(source_path) {
        if let Ok(tagged) = probe.read() {
            let dur = tagged.properties().duration().as_secs_f64();
            if dur > MAX_TRACK_DURATION_SECS {
                let mins = (dur / 60.0).ceil() as u32;
                return Err(format!(
                    "Track is too long ({mins} min). Maximum duration is {} minutes.",
                    (MAX_TRACK_DURATION_SECS / 60.0) as u32
                ));
            }
        }
    }

    log_import_stage("computing track hash");
    let track_hash = compute_track_hash(source_path)?;
    let hash_match = match uid.as_deref() {
        Some(u) => tracks_db::get_own_track_by_hash(pool, &track_hash, u).await?,
        None => tracks_db::get_track_by_hash(pool, &track_hash).await?,
    };
    if let Some(existing) = hash_match {
        run_import_pipeline(
            pool,
            &existing.id,
            &app_handle,
            stem_cache,
            analysis_tasks,
            analysis_epoch,
        )
        .await?;
        return Ok(existing);
    }

    emit_import_progress(&app_handle, "new", "Copying file…");
    log_import_stage("copying track file");
    let extension = source_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("bin");
    let dest_file_name = format!("{}.{}", Uuid::new_v4(), extension);
    let dest_path = tracks_dir.join(&dest_file_name);
    std::fs::copy(&source_path, &dest_path)
        .map_err(|e| format!("Failed to copy track file: {}", e))?;
    let mut pending_audio = PendingImportFile::new(dest_path.clone());
    emit_import_progress(&app_handle, "new", "Reading metadata…");
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
    let (album_art_path, album_art_mime) = extract_album_art(&app_handle, &dest_path)?;
    let mut pending_art = PendingImportFile::from_optional(album_art_path.as_deref());

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
    pending_audio.keep();
    if let Some(art) = &mut pending_art {
        art.keep();
    }

    let row = tracks_db::get_track_by_id(pool, &id)
        .await?
        .ok_or_else(|| format!("Failed to fetch imported track {}", id))?;

    run_import_pipeline(
        pool,
        &id,
        &app_handle,
        stem_cache,
        analysis_tasks,
        analysis_epoch,
    )
    .await?;

    log_import_stage("finished import");
    Ok(row)
}

/// Fast import for a file from disk — copies file, reads metadata, inserts DB record.
/// No analysis is run. Returns (track_id, is_new). Deduplicates by content hash.
pub async fn file_fast_import(
    pool: &SqlitePool,
    app_handle: &AppHandle,
    file_path: &str,
    uid: Option<String>,
) -> Result<(String, bool), String> {
    ensure_storage(app_handle)?;
    let (tracks_dir, _, _) = storage_dirs(app_handle)?;

    let source_path = Path::new(file_path);
    if !source_path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }

    // Check duration before copying
    if let Ok(probe) = Probe::open(source_path) {
        if let Ok(tagged) = probe.read() {
            let dur = tagged.properties().duration().as_secs_f64();
            if dur > MAX_TRACK_DURATION_SECS {
                let mins = (dur / 60.0).ceil() as u32;
                return Err(format!(
                    "Track is too long ({mins} min). Maximum duration is {} minutes.",
                    (MAX_TRACK_DURATION_SECS / 60.0) as u32
                ));
            }
        }
    }

    let track_hash = compute_track_hash(source_path)?;
    let hash_match = match uid.as_deref() {
        Some(u) => tracks_db::get_own_track_by_hash(pool, &track_hash, u).await?,
        None => tracks_db::get_track_by_hash(pool, &track_hash).await?,
    };
    if let Some(existing) = hash_match {
        return Ok((existing.id, false));
    }

    let extension = source_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("bin");
    let dest_file_name = format!("{}.{}", Uuid::new_v4(), extension);
    let dest_path = tracks_dir.join(&dest_file_name);
    std::fs::copy(source_path, &dest_path)
        .map_err(|e| format!("Failed to copy track file: {}", e))?;
    let mut pending_audio = PendingImportFile::new(dest_path.clone());

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
    let (album_art_path, album_art_mime) = extract_album_art(app_handle, &dest_path)?;
    let mut pending_art = PendingImportFile::from_optional(album_art_path.as_deref());

    let basename = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

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
        Some("file"),
        None,
        basename.as_deref(),
    )
    .await?;
    pending_audio.keep();
    if let Some(art) = &mut pending_art {
        art.keep();
    }

    Ok((id, true))
}

/// Extract album art from an audio file and save it to the art directory.
/// Returns `(art_path, art_mime)`; both `None` when no embedded artwork is
/// present or the file can't be parsed.
fn extract_album_art(
    app_handle: &AppHandle,
    source_path: &Path,
) -> Result<(Option<String>, Option<String>), String> {
    let (_, art_dir, _) = storage_dirs(app_handle)?;

    let tagged_file = match Probe::open(source_path) {
        Ok(probe) => match probe.read() {
            Ok(tf) => tf,
            Err(_) => return Ok((None, None)),
        },
        Err(_) => return Ok((None, None)),
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
            Ok((Some(art_path.to_string_lossy().into_owned()), Some(mime)))
        }
        None => Ok((None, None)),
    }
}

/// Fast import for Engine DJ tracks — inserts DB record using Engine DJ metadata directly.
/// Computes real hash but skips analysis workers (those run in background).
/// Deduplicates by source_id first, then by content hash. Returns (id, is_new).
pub async fn engine_dj_fast_import(
    pool: &SqlitePool,
    app_handle: &AppHandle,
    engine_track: &EngineDjTrack,
    audio_path: &Path,
    source_id: &str,
    uid: Option<String>,
) -> Result<(String, bool), String> {
    // Dedup by source_id — no file I/O needed
    if let Some(existing) = tracks_db::get_track_by_source_id(pool, "engine_dj", source_id).await? {
        return Ok((existing.id, false));
    }

    if let Some(dur) = engine_track.length {
        if dur > MAX_TRACK_DURATION_SECS {
            return Err(format!(
                "Track is too long ({} min). Maximum duration is {} minutes.",
                (dur / 60.0).ceil() as u32,
                (MAX_TRACK_DURATION_SECS / 60.0) as u32
            ));
        }
    }

    ensure_storage(app_handle)?;

    let track_hash = compute_track_hash(audio_path)?;

    // Fallback dedup by hash — catches re-imports after deletion
    let hash_match = match uid.as_deref() {
        Some(u) => tracks_db::get_own_track_by_hash(pool, &track_hash, u).await?,
        None => tracks_db::get_track_by_hash(pool, &track_hash).await?,
    };
    if let Some(existing) = hash_match {
        return Ok((existing.id, false));
    }

    // Extract album art (only file I/O — reads just the tag header)
    let (album_art_path, album_art_mime) = extract_album_art(app_handle, audio_path)?;
    let mut pending_art = PendingImportFile::from_optional(album_art_path.as_deref());

    let id = tracks_db::insert_track_record(
        pool,
        &track_hash,
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
    if let Some(art) = &mut pending_art {
        art.keep();
    }

    Ok((id, true))
}

/// Generic fast import for any DJ library source (Rekordbox, Engine DJ, etc.).
/// Inserts DB record using metadata directly — computes real hash, no analysis.
/// Deduplicates by source_id first, then by content hash. Returns (id, is_new).
pub async fn dj_fast_import(
    pool: &SqlitePool,
    app_handle: &AppHandle,
    source_type: &str,
    source_id: &str,
    title: &Option<String>,
    artist: &Option<String>,
    album: &Option<String>,
    duration_seconds: Option<f64>,
    filename: Option<&str>,
    audio_path: &Path,
    uid: Option<String>,
) -> Result<(String, bool), String> {
    // Dedup by source_id
    if let Some(existing) = tracks_db::get_track_by_source_id(pool, source_type, source_id).await? {
        return Ok((existing.id, false));
    }

    if let Some(dur) = duration_seconds {
        if dur > MAX_TRACK_DURATION_SECS {
            return Err(format!(
                "Track is too long ({} min). Maximum duration is {} minutes.",
                (dur / 60.0).ceil() as u32,
                (MAX_TRACK_DURATION_SECS / 60.0) as u32
            ));
        }
    }

    ensure_storage(app_handle)?;

    let track_hash = compute_track_hash(audio_path)?;

    // Fallback dedup by hash — catches re-imports after deletion
    let hash_match = match uid.as_deref() {
        Some(u) => tracks_db::get_own_track_by_hash(pool, &track_hash, u).await?,
        None => tracks_db::get_track_by_hash(pool, &track_hash).await?,
    };
    if let Some(existing) = hash_match {
        return Ok((existing.id, false));
    }

    // Extract album art (reads just the tag header)
    let (album_art_path, album_art_mime) = extract_album_art(app_handle, audio_path)?;
    let mut pending_art = PendingImportFile::from_optional(album_art_path.as_deref());

    let id = tracks_db::insert_track_record(
        pool,
        &track_hash,
        title,
        artist,
        album,
        None, // track_number
        None, // disc_number
        duration_seconds,
        &audio_path.to_string_lossy(),
        &album_art_path,
        &album_art_mime,
        uid,
        Some(source_type),
        Some(source_id),
        filename,
    )
    .await?;
    if let Some(art) = &mut pending_art {
        art.keep();
    }

    Ok((id, true))
}

/// Determine how many tracks to analyze in parallel based on available system memory.
/// Reserves 4 GB for the OS/app, then allocates ~3 GB per worker (stems + beats overhead).
pub(crate) fn analysis_worker_count() -> usize {
    let ram_gb = total_system_memory_gb();
    let workers = ((ram_gb as i64 - 4) / 3).clamp(1, 6) as usize;
    eprintln!("[background_analysis] {ram_gb} GB RAM → {workers} parallel workers");
    workers
}

#[cfg(target_os = "macos")]
fn total_system_memory_gb() -> u64 {
    use std::mem;
    let mut memsize: u64 = 0;
    let mut size = mem::size_of::<u64>();
    let mut mib = [libc::CTL_HW, libc::HW_MEMSIZE];
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            &mut memsize as *mut u64 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 {
        memsize / (1024 * 1024 * 1024)
    } else {
        8 // conservative fallback
    }
}

#[cfg(target_os = "linux")]
fn total_system_memory_gb() -> u64 {
    use std::fs;
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
                .map(|kb| kb / (1024 * 1024))
        })
        .unwrap_or(8)
}

#[cfg(target_os = "windows")]
fn total_system_memory_gb() -> u64 {
    use std::mem;
    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }
    extern "system" {
        fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status: MemoryStatusEx = unsafe { mem::zeroed() };
    status.dw_length = mem::size_of::<MemoryStatusEx>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 {
        status.ull_total_phys / (1024 * 1024 * 1024)
    } else {
        8
    }
}

/// Run background analysis for a batch of tracks. Thin wrapper around the
/// preprocessing DAG scheduler — kept under this name so existing callers
/// don't need to change.
pub async fn run_background_analysis(
    pool: SqlitePool,
    app_handle: AppHandle,
    stem_cache: StemCache,
    track_ids: Vec<String>,
    analysis: AnalysisGuard,
) {
    if analysis.checkpoint().is_err() {
        return;
    }
    // Pre-pipeline metadata gap-fill happens before the scheduler — it's a
    // cheap track-row population step, not a preprocessor.
    for track_id in &track_ids {
        if analysis.checkpoint().is_err() {
            return;
        }
        if let Err(e) = backfill_metadata_gaps(&pool, track_id).await {
            eprintln!("[preprocessing] metadata gap-fill failed for {track_id}: {e}");
        }
    }
    if analysis.checkpoint().is_err() {
        return;
    }
    // Waveform generation is an independent peer to the typed artifact DAG;
    // the identity task group owns and drains both branches together.
    let waveforms = run_waveform_jobs(&pool, &track_ids, &analysis);
    let preprocessing = scheduler::run_for_tracks(
        pool.clone(),
        app_handle,
        stem_cache,
        track_ids.clone(),
        analysis.clone(),
    );
    tokio::join!(waveforms, preprocessing);
}

/// Backfill missing track metadata from file tags. Runs before any
/// preprocessor; not part of the DAG.
async fn backfill_metadata_gaps(pool: &SqlitePool, track_id: &str) -> Result<(), String> {
    let track = tracks_db::get_track_by_id(pool, track_id)
        .await?
        .ok_or_else(|| format!("Track {track_id} not found"))?;
    let file_path = Path::new(&track.file_path);
    if !file_path.exists() {
        return Ok(());
    }
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
    Ok(())
}

/// Fire off waveform generation for each track in parallel with the DAG.
/// The waveform pipeline is independent of the preprocessor DAG and will be
/// migrated in a follow-up PR.
async fn run_waveform_jobs(pool: &SqlitePool, track_ids: &[String], analysis: &AnalysisGuard) {
    let mut jobs = tokio::task::JoinSet::new();
    for track_id in track_ids {
        if analysis.checkpoint().is_err() {
            break;
        }
        let Ok(Some(track)) = tracks_db::get_track_by_id(pool, track_id).await else {
            continue;
        };
        if !Path::new(&track.file_path).exists() {
            continue;
        }
        let pool = pool.clone();
        let track_id = track_id.clone();
        let analysis = analysis.clone();
        jobs.spawn(async move {
            if let Err(e) =
                crate::services::waveforms::ensure_track_waveform(&pool, &track_id, &analysis).await
            {
                if analysis.is_cancelled() {
                    return;
                }
                eprintln!("[preprocessing] waveform failed for {track_id}: {e}");
            }
        });
    }
    while let Some(result) = jobs.join_next().await {
        if let Err(error) = result {
            log::error!("[preprocessing] waveform task panicked: {error}");
        }
    }
}

/// Get mel spectrogram for a track.
pub async fn get_melspec(
    pool: &SqlitePool,
    fft_service: &FftService,
    track_id: &str,
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
pub async fn get_track_beats(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<BeatGrid>, String> {
    let row = tracks_db::get_track_beats_raw(pool, track_id).await?;
    parse_track_beats(row)
}

pub(crate) async fn get_track_beats_for_connection(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<Option<BeatGrid>, String> {
    let row = tracks_db::get_track_beats_raw_for_connection(connection, track_id).await?;
    parse_track_beats(row)
}

fn parse_track_beats(
    row: Option<crate::models::tracks::TrackBeats>,
) -> Result<Option<BeatGrid>, String> {
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

/// Per-bar tag classifications for a track, with the tag display order the
/// classifier emitted. `None` when classification hasn't run.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackBarClassifications {
    pub classifications: serde_json::Value,
    pub tag_order: serde_json::Value,
}

pub async fn get_track_bar_classifications(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<TrackBarClassifications>, String> {
    let raw = tracks_db::get_track_bar_classifications_raw(pool, track_id).await?;
    let Some((classifications_json, tag_order_json)) = raw else {
        return Ok(None);
    };
    let classifications: serde_json::Value = serde_json::from_str(&classifications_json)
        .map_err(|e| format!("Failed to parse classifications JSON: {e}"))?;
    let tag_order: serde_json::Value = serde_json::from_str(&tag_order_json)
        .map_err(|e| format!("Failed to parse tag order JSON: {e}"))?;
    Ok(Some(TrackBarClassifications {
        classifications,
        tag_order,
    }))
}

/// Per-tag F1-optimal suggestion thresholds bundled with the classifier
/// weights. Returns `tag_name -> threshold`. The frontend uses these in place
/// of a flat 0.5 cutoff so rare tags (e.g. `vocal_chop` at 0.165) surface at
/// the calibration the model was tuned for.
pub fn classifier_thresholds() -> Result<std::collections::HashMap<String, f64>, String> {
    let payload: serde_json::Value =
        serde_json::from_str(crate::classifier_worker::bundled_thresholds())
            .map_err(|e| format!("Failed to parse bundled thresholds JSON: {e}"))?;
    let map = payload
        .get("thresholds")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "Bundled thresholds JSON missing `thresholds` object".to_string())?;
    let mut out = std::collections::HashMap::with_capacity(map.len());
    for (k, v) in map {
        let f = v
            .as_f64()
            .ok_or_else(|| format!("Threshold for `{k}` is not a number"))?;
        out.insert(k.clone(), f);
    }
    Ok(out)
}

/// Delete a track and its derived data.
pub async fn delete_track(
    pool: &SqlitePool,
    app_handle: AppHandle,
    stem_cache: &StemCache,
    track_id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    recover_track_deletions(pool, &app_handle).await?;
    let storage = StorageRoot::from_app(&app_handle)?;
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin track deletion: {error}"))?;
    let Some(plan) =
        tracks_db::prepare_track_deletion(&mut transaction, track_id, owner_user_id).await?
    else {
        transaction
            .rollback()
            .await
            .map_err(|error| format!("Failed to close missing track deletion: {error}"))?;
        return Err(format!("Track {} not found", track_id));
    };
    let staged = match stage_track_files_for_deletion(&storage, track_id, &plan) {
        Ok(staged) => staged,
        Err(error) => {
            transaction.rollback().await.map_err(|rollback| {
                format!("{error}; failed to roll back track deletion: {rollback}")
            })?;
            return Err(error);
        }
    };

    let rows =
        match tracks_db::delete_prepared_track_record(&mut transaction, track_id, owner_user_id)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                let database_rollback = transaction.rollback().await;
                let filesystem_rollback = rollback_staged_track_files(&storage, &staged);
                if let Err(rollback) = database_rollback {
                    return Err(format!(
                        "{error}; failed to roll back track database deletion: {rollback}"
                    ));
                }
                filesystem_rollback?;
                return Err(error);
            }
        };
    if rows == 0 {
        transaction
            .rollback()
            .await
            .map_err(|error| format!("Failed to roll back missing track deletion: {error}"))?;
        rollback_staged_track_files(&storage, &staged)?;
        return Err(format!("Track {} not found", track_id));
    }
    if let Err(commit_error) = transaction.commit().await {
        let row_exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(pool)
            .await
            .map_err(|inspect_error| {
                format!(
                    "Failed to commit track deletion: {commit_error}; could not determine outcome: {inspect_error}"
                )
            })?
            != 0;
        if row_exists {
            rollback_staged_track_files(&storage, &staged)?;
            return Err(format!("Failed to commit track deletion: {commit_error}"));
        }
        eprintln!(
            "[tracks] track {track_id} deletion committed despite an uncertain commit response: {commit_error}"
        );
    }
    stem_cache.remove_track(track_id);

    // SQLite is committed, so an interrupted cleanup is completed by the
    // startup reaper instead of surfacing a false failure after deletion.
    if let Err(error) = std::fs::remove_dir_all(&staged.directory) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "[tracks] staged deletion for {track_id} will be retried at startup: {error}"
            );
        }
    }

    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TrackDeletionManifest {
    track_id: String,
    entries: Vec<TrackDeletionEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TrackDeletionEntry {
    original: String,
    staged_name: String,
}

struct StagedTrackDeletion {
    directory: PathBuf,
    manifest: TrackDeletionManifest,
}

fn validate_track_deletion_manifest(manifest: &TrackDeletionManifest) -> Result<(), String> {
    if manifest.track_id.is_empty()
        || manifest.track_id.len() > 128
        || !manifest
            .track_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || manifest.entries.len() > 32
    {
        return Err("Invalid track deletion manifest identity or entry count".into());
    }
    let mut originals = BTreeSet::new();
    for (index, entry) in manifest.entries.iter().enumerate() {
        if entry.staged_name != index.to_string()
            || !originals.insert(entry.original.as_str())
            || !Path::new(&entry.original).is_absolute()
        {
            return Err("Invalid track deletion manifest entry".into());
        }
    }
    Ok(())
}

fn stage_track_files_for_deletion(
    storage: &StorageRoot,
    track_id: &str,
    plan: &tracks_db::TrackDeletionPlan,
) -> Result<StagedTrackDeletion, String> {
    if track_id.is_empty()
        || track_id.len() > 128
        || !track_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Invalid track deletion identity".into());
    }
    let candidates = managed_track_deletion_candidates(
        storage,
        &plan.file_path,
        plan.album_art_path.as_deref(),
        &plan.track_hash,
        plan.delete_audio,
        plan.delete_album_art,
        plan.delete_hash_artifacts,
    )?;

    let entries = candidates
        .into_iter()
        .enumerate()
        .map(|(index, original)| {
            let original = original.to_str().ok_or_else(|| {
                format!(
                    "Managed track artifact path is not valid UTF-8: {}",
                    original.display()
                )
            })?;
            Ok(TrackDeletionEntry {
                original: original.to_owned(),
                staged_name: index.to_string(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let directory =
        storage
            .track_deletion_trash_dir()
            .join(format!("{}-{}", track_id, Uuid::new_v4()));
    std::fs::create_dir_all(&directory).map_err(|e| {
        format!(
            "Failed to create deletion staging {}: {e}",
            directory.display()
        )
    })?;
    let manifest = TrackDeletionManifest {
        track_id: track_id.to_owned(),
        entries,
    };
    let manifest_path = directory.join("manifest.json");
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|e| format!("Failed to serialize track deletion manifest: {e}"))?;
    let mut manifest_file = std::fs::File::create(&manifest_path).map_err(|e| {
        format!(
            "Failed to create track deletion manifest {}: {e}",
            manifest_path.display()
        )
    })?;
    use std::io::Write;
    manifest_file
        .write_all(&manifest_bytes)
        .map_err(|e| format!("Failed to write track deletion manifest: {e}"))?;
    manifest_file
        .sync_all()
        .map_err(|e| format!("Failed to sync track deletion manifest: {e}"))?;

    let staged = StagedTrackDeletion {
        directory,
        manifest,
    };
    for entry in &staged.manifest.entries {
        let original = Path::new(&entry.original);
        let metadata = match std::fs::symlink_metadata(original) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                rollback_staged_track_files(storage, &staged)?;
                return Err(format!(
                    "Failed to inspect managed track artifact {}: {error}",
                    original.display()
                ));
            }
        };
        let is_expected_directory =
            metadata.is_dir() && original == storage.stems_dir(&plan.track_hash);
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !is_expected_directory) {
            rollback_staged_track_files(storage, &staged)?;
            return Err(format!(
                "Refusing to stage unsupported track artifact {}",
                original.display()
            ));
        }
        let target = staged.directory.join(&entry.staged_name);
        if let Err(error) = std::fs::rename(original, &target) {
            rollback_staged_track_files(storage, &staged)?;
            return Err(format!(
                "Failed to stage track artifact {}: {error}",
                original.display()
            ));
        }
    }
    Ok(staged)
}

fn managed_track_deletion_candidates(
    storage: &StorageRoot,
    audio_path: &str,
    album_art_path: Option<&str>,
    track_hash: &str,
    delete_audio: bool,
    delete_album_art: bool,
    delete_hash_artifacts: bool,
) -> Result<BTreeSet<PathBuf>, String> {
    if track_hash.is_empty()
        || track_hash.len() > 128
        || !track_hash
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Invalid managed track hash".into());
    }
    let mut candidates = BTreeSet::new();
    if delete_audio {
        add_managed_deletion_candidate(
            &mut candidates,
            Path::new(audio_path),
            &storage.tracks_dir(),
        )?;
    }
    if delete_album_art {
        if let Some(path) = album_art_path {
            add_managed_deletion_candidate(&mut candidates, Path::new(path), &storage.art_dir())?;
        }
    }
    if delete_hash_artifacts {
        for path in [
            storage.stems_dir(track_hash),
            storage.mix_pcm_path(track_hash),
            storage.eval_mono_pcm_path(track_hash),
            storage.mert_fullmix_path(track_hash),
            storage.mert_drum_path(track_hash),
        ] {
            add_managed_deletion_candidate(&mut candidates, &path, storage.path())?;
        }
    }

    Ok(candidates)
}

fn add_managed_deletion_candidate(
    candidates: &mut BTreeSet<PathBuf>,
    path: &Path,
    allowed_root: &Path,
) -> Result<(), String> {
    if !path.is_absolute() {
        return Ok(());
    }
    if !validate_managed_deletion_path(path, allowed_root)? {
        return Ok(());
    }
    candidates.insert(path.to_path_buf());
    Ok(())
}

fn validate_managed_deletion_path(path: &Path, allowed_root: &Path) -> Result<bool, String> {
    let Ok(relative) = path.strip_prefix(allowed_root) else {
        return Ok(false);
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "Refusing unsafe managed track deletion path {}",
            path.display()
        ));
    }
    let canonical_root = allowed_root.canonicalize().map_err(|e| {
        format!(
            "Failed to resolve managed track root {}: {e}",
            allowed_root.display()
        )
    })?;
    let mut cursor = allowed_root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            unreachable!("components were validated above")
        };
        cursor.push(component);
        match std::fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(format!(
                        "Refusing symlinked managed track path {}",
                        cursor.display()
                    ));
                }
                let resolved = cursor.canonicalize().map_err(|e| {
                    format!(
                        "Failed to resolve managed track path {}: {e}",
                        cursor.display()
                    )
                })?;
                if !resolved.starts_with(&canonical_root) {
                    return Err(format!(
                        "Managed track path escapes its root: {}",
                        path.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "Failed to inspect managed track path {}: {error}",
                    cursor.display()
                ));
            }
        }
    }
    Ok(true)
}

fn rollback_staged_track_files(
    storage: &StorageRoot,
    staged: &StagedTrackDeletion,
) -> Result<(), String> {
    validate_track_deletion_manifest(&staged.manifest)?;
    for entry in staged.manifest.entries.iter().rev() {
        let original = Path::new(&entry.original);
        if !validate_managed_deletion_path(original, storage.path())? {
            return Err(format!(
                "Track deletion manifest contains an unmanaged path: {}",
                original.display()
            ));
        }
        let source = staged.directory.join(&entry.staged_name);
        if !source.exists() {
            continue;
        }
        if original.exists() {
            return Err(format!(
                "Cannot restore staged track artifact because {} now exists",
                original.display()
            ));
        }
        if let Some(parent) = original.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Failed to recreate track artifact directory {}: {e}",
                    parent.display()
                )
            })?;
        }
        std::fs::rename(&source, original).map_err(|e| {
            format!(
                "Failed to restore staged track artifact {}: {e}",
                original.display()
            )
        })?;
    }
    match std::fs::remove_dir_all(&staged.directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to remove track deletion staging {}: {error}",
            staged.directory.display()
        )),
    }
}

/// Recover a crash at either side of the track catalog transaction. If the
/// row remains, put every staged artifact back; otherwise finish deletion.
pub async fn recover_track_deletions(
    pool: &SqlitePool,
    app_handle: &AppHandle,
) -> Result<(), String> {
    let storage = StorageRoot::from_app(app_handle)?;
    let root = storage.track_deletion_trash_dir();
    let directories = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Failed to inspect {}: {error}", root.display())),
    };
    for entry in directories {
        let entry = entry.map_err(|e| format!("Failed to inspect track deletion entry: {e}"))?;
        let metadata = entry
            .file_type()
            .map_err(|e| format!("Failed to inspect track deletion entry type: {e}"))?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return Err(format!(
                "Unexpected entry in track deletion staging: {}",
                entry.path().display()
            ));
        }
        let bytes = match std::fs::read(entry.path().join("manifest.json")) {
            Ok(bytes) => bytes,
            Err(error) => {
                if deletion_stage_has_no_moved_artifacts(&entry.path())? {
                    std::fs::remove_dir_all(entry.path()).map_err(|remove_error| {
                        format!(
                            "Failed to remove incomplete track deletion staging {}: {remove_error}",
                            entry.path().display()
                        )
                    })?;
                    continue;
                }
                return Err(format!(
                    "Failed to read track deletion manifest {}: {error}",
                    entry.path().display()
                ));
            }
        };
        if bytes.len() > 64 * 1024 {
            return Err("Track deletion manifest exceeds 64 KiB".into());
        }
        let manifest: TrackDeletionManifest = match serde_json::from_slice(&bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                if deletion_stage_has_no_moved_artifacts(&entry.path())? {
                    std::fs::remove_dir_all(entry.path()).map_err(|remove_error| {
                        format!(
                            "Failed to remove incomplete track deletion staging {}: {remove_error}",
                            entry.path().display()
                        )
                    })?;
                    continue;
                }
                return Err(format!("Failed to parse track deletion manifest: {error}"));
            }
        };
        validate_track_deletion_manifest(&manifest)?;
        let track: Option<(String, Option<String>, String)> =
            sqlx::query_as("SELECT file_path, album_art_path, track_hash FROM tracks WHERE id = ?")
                .bind(&manifest.track_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("Failed to reconcile track deletion: {e}"))?;
        let staged = StagedTrackDeletion {
            directory: entry.path(),
            manifest,
        };
        if let Some((audio_path, album_art_path, track_hash)) = track {
            let allowed = managed_track_deletion_candidates(
                &storage,
                &audio_path,
                album_art_path.as_deref(),
                &track_hash,
                true,
                true,
                true,
            )?;
            if staged
                .manifest
                .entries
                .iter()
                .any(|entry| !allowed.contains(Path::new(&entry.original)))
            {
                return Err(format!(
                    "Track deletion manifest for {} names an unexpected artifact",
                    staged.manifest.track_id
                ));
            }
            rollback_staged_track_files(&storage, &staged)?;
        } else {
            std::fs::remove_dir_all(&staged.directory).map_err(|e| {
                format!(
                    "Failed to finish track deletion {}: {e}",
                    staged.directory.display()
                )
            })?;
        }
    }
    Ok(())
}

fn deletion_stage_has_no_moved_artifacts(directory: &Path) -> Result<bool, String> {
    for entry in std::fs::read_dir(directory)
        .map_err(|e| format!("Failed to inspect {}: {e}", directory.display()))?
    {
        let entry = entry.map_err(|e| format!("Failed to inspect deletion staging: {e}"))?;
        if entry.file_name() != "manifest.json" {
            return Ok(false);
        }
    }
    Ok(true)
}

// -----------------------------------------------------------------------------
// Import orchestration — runs the preprocessing DAG plus waveform generation
// for a single freshly-imported (or re-encountered) track.
// -----------------------------------------------------------------------------

async fn run_import_pipeline(
    pool: &SqlitePool,
    track_id: &str,
    app_handle: &AppHandle,
    stem_cache: &StemCache,
    analysis_tasks: &AnalysisTaskGroup,
    analysis_epoch: AnalysisEpoch,
) -> Result<(), String> {
    ensure_storage(app_handle)?;
    let lease = analysis_tasks.lease(analysis_epoch)?;
    let analysis = lease.guard();

    // Waveform generation is an independent peer to the typed artifact DAG;
    // run both branches under the same generation lease.
    let preprocessors = crate::preprocessing::registry::registered_preprocessors();
    let waveform = crate::services::waveforms::ensure_track_waveform(pool, track_id, &analysis);
    let preprocessing = scheduler::run_for_track(
        pool,
        app_handle,
        stem_cache,
        track_id,
        &preprocessors,
        &analysis,
    );
    tokio::try_join!(waveform, preprocessing).map(|_| ())
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

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn log_import_stage(stage: &str) {
    eprintln!("[import_track] {}", stage);
}

/// `(tracks_dir, art_dir, stems_dir)`. A convenience tuple over
/// [`StorageRoot`], which owns the layout.
pub fn storage_dirs(
    app: &AppHandle,
) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf), String> {
    let root = StorageRoot::from_app(app)?;
    Ok((root.tracks_dir(), root.art_dir(), root.stems_root()))
}

pub fn ensure_storage(app: &AppHandle) -> Result<(), String> {
    StorageRoot::from_app(app)?.ensure_track_storage()
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

/// Read a track's audio file and return it as base64 + MIME type.
pub async fn get_track_audio_base64(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<(String, String), String> {
    let info = tracks_db::get_track_path_and_hash(pool, track_id).await?;
    let path = Path::new(&info.file_path);

    let mime_type = match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/mp4",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
    .to_string();

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Failed to read audio file: {}", e))?;
    let data = STANDARD.encode(&bytes);

    Ok((data, mime_type))
}

#[cfg(test)]
mod deletion_tests {
    use super::*;

    #[test]
    fn staged_track_deletion_is_reversible_before_the_database_commit() {
        let directory = tempfile::tempdir().unwrap();
        let storage = StorageRoot::from_path(directory.path().join("luma"));
        storage.ensure_track_storage().unwrap();
        let audio = storage.tracks_dir().join("hash.mp3");
        let art = storage.art_dir().join("hash.jpg");
        let stem = storage.stems_dir("hash").join("drums.ogg");
        let cache = storage.mix_pcm_path("hash");
        for (path, contents) in [
            (&audio, b"audio".as_slice()),
            (&art, b"art".as_slice()),
            (&stem, b"stem".as_slice()),
            (&cache, b"cache".as_slice()),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        let staged = stage_track_files_for_deletion(
            &storage,
            "track",
            &tracks_db::TrackDeletionPlan {
                file_path: audio.to_str().unwrap().to_owned(),
                album_art_path: Some(art.to_str().unwrap().to_owned()),
                track_hash: "hash".into(),
                delete_audio: true,
                delete_album_art: true,
                delete_hash_artifacts: true,
            },
        )
        .unwrap();
        for path in [&audio, &art, &stem, &cache] {
            assert!(!path.exists());
        }
        rollback_staged_track_files(&storage, &staged).unwrap();
        for path in [&audio, &art, &stem, &cache] {
            assert!(path.exists());
        }
        assert!(!staged.directory.exists());
    }

    #[cfg(unix)]
    #[test]
    fn staged_track_deletion_rejects_symlinked_managed_paths() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let storage = StorageRoot::from_path(directory.path().join("luma"));
        storage.ensure_track_storage().unwrap();
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("song.mp3"), b"keep").unwrap();
        symlink(&outside, storage.tracks_dir().join("escape")).unwrap();
        let escaped = storage.tracks_dir().join("escape/song.mp3");

        let error = stage_track_files_for_deletion(
            &storage,
            "track",
            &tracks_db::TrackDeletionPlan {
                file_path: escaped.to_str().unwrap().to_owned(),
                album_art_path: None,
                track_hash: "hash".into(),
                delete_audio: true,
                delete_album_art: true,
                delete_hash_artifacts: true,
            },
        )
        .err()
        .expect("symlinked managed path must fail");
        assert!(error.contains("symlinked managed track path"));
        assert_eq!(std::fs::read(outside.join("song.mp3")).unwrap(), b"keep");
    }
}
