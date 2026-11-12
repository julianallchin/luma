//! Business logic for track operations.
//!
//! The database layer (database/local/tracks.rs) is pure SQL/CRUD. All
//! filesystem work, hashing, audio workers, and orchestration live here.

use lofty::picture::PictureType;
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::database::local::tracks as tracks_db;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read as VenueRead, VenueAccess, VenueResource,
};
use crate::events::{Event, Events};
use crate::models::tracks::{
    ImportRejection, StageOutcome, TrackBrowserRow, TrackImportFailure, TrackImportPhase,
    TrackImportProgress, TrackImportResult, TrackSummary, MAX_TRACK_DURATION_SECS,
};
use crate::preprocessing::concurrency::{analysis_worker_count, run_bounded_track_jobs};
use crate::preprocessing::{scheduler, AnalysisGuard, AnalysisTaskGroup, WorkerEnvironment};
use crate::storage::StorageRoot;

/// Why one song was not imported: the reason the dialog words, and the cause
/// for the log. An error with no reason of its own is [`ImportRejection::Other`].
#[derive(Debug)]
pub struct Rejected {
    pub reason: ImportRejection,
    pub cause: String,
}

impl From<String> for Rejected {
    fn from(cause: String) -> Self {
        Self {
            reason: ImportRejection::Other,
            cause,
        }
    }
}

fn within_duration_limit(seconds: f64) -> Result<(), Rejected> {
    if seconds <= MAX_TRACK_DURATION_SECS {
        return Ok(());
    }
    Err(Rejected {
        reason: ImportRejection::TooLong,
        cause: format!(
            "Track is too long ({} min). Maximum duration is {} minutes.",
            (seconds / 60.0).ceil() as u32,
            (MAX_TRACK_DURATION_SECS / 60.0) as u32
        ),
    })
}

/// A filesystem artifact created before its catalog row commits. Dropping the
/// guard removes only that newly-created file. Once SQLite owns the path, `keep` transfers cleanup to the normal
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

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

/// List all tracks with enriched metadata for the browser view. Album art is
/// not inlined — callers load it from `album_art_path` so payloads stay small
/// and GPUI can decode images lazily.
pub async fn list_tracks_enriched(
    pool: &SqlitePool,
    venue_id: Option<&str>,
) -> Result<Vec<TrackBrowserRow>, String> {
    match venue_id {
        Some(venue_id) => {
            let mut access =
                VenueAccess::<VenueRead>::read(pool, VenueResource::Venue(venue_id)).await?;
            tracks_db::list_tracks_enriched_for_connection(access.connection(), Some(venue_id))
                .await
        }
        None => tracks_db::list_tracks_enriched(pool, None).await,
    }
}

/// Fast import for a file from disk — copies file, reads metadata, inserts DB record.
/// No analysis is run. Returns (track_id, is_new). Deduplicates by content hash.
pub async fn file_fast_import(
    pool: &SqlitePool,
    storage: &StorageRoot,
    file_path: &str,
    uid: &str,
) -> Result<(String, bool), Rejected> {
    let source_path = Path::new(file_path);
    if !source_path.exists() {
        return Err(Rejected {
            reason: ImportRejection::Missing,
            cause: format!("File does not exist: {}", file_path),
        });
    }

    let track_hash = compute_track_hash(source_path)?;
    if let Some(id) = already_imported(pool, uid, &track_hash, None).await? {
        return Ok((id, false));
    }
    let mut stored = StoredAudio::from_source(storage, source_path)?;

    // Tags are a courtesy: a song without readable ones is named for its file.
    let tagged_file = read_tagged(source_path).ok();
    let primary_tag = tagged_file.as_ref().and_then(|file| file.primary_tag());
    let title = primary_tag
        .and_then(|tag| tag.title().map(|s| s.to_string()))
        .filter(|title| !title.is_empty())
        .or_else(|| file_stem(source_path));
    let artist = primary_tag.and_then(|tag| tag.artist().map(|s| s.to_string()));
    let album = primary_tag.and_then(|tag| tag.album().map(|s| s.to_string()));
    let track_number = primary_tag.and_then(|tag| tag.track()).map(|n| n as i64);
    let disc_number = primary_tag.and_then(|tag| tag.disk()).map(|n| n as i64);
    let (album_art_path, album_art_mime) = extract_album_art(storage, source_path)?;
    let mut pending_art = PendingImportFile::from_optional(album_art_path.as_deref());

    let basename = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

    let (id, is_new) = tracks_db::insert_track_record(
        pool,
        &track_hash,
        &title,
        &artist,
        &album,
        track_number,
        disc_number,
        Some(stored.duration_seconds),
        &stored.path.to_string_lossy(),
        &album_art_path,
        &album_art_mime,
        uid,
        Some("file"),
        None,
        basename.as_deref(),
    )
    .await?;
    if is_new {
        stored.file.keep();
        if let Some(art) = &mut pending_art {
            art.keep();
        }
    }

    Ok((id, is_new))
}

/// The song this import would only repeat, so it can stop before converting
/// anything. See [`tracks_db::existing_track`].
async fn already_imported(
    pool: &SqlitePool,
    uid: &str,
    track_hash: &str,
    source: Option<(&str, &str)>,
) -> Result<Option<String>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to look up track: {error}"))?;
    tracks_db::existing_track(&mut connection, uid, track_hash, source).await
}

/// A song's audio brought into the library's storage in the one format Luma
/// keeps ([`crate::audio::stored`]), and how long it runs. The file is removed
/// again unless the import that asked for it keeps it.
struct StoredAudio {
    path: PathBuf,
    duration_seconds: f64,
    file: PendingImportFile,
}

impl StoredAudio {
    /// Convert `source` into storage. Audio ffmpeg cannot decode and audio
    /// over the length limit are both turned away here, measured on the
    /// converted file: what the source's own header claims is not trusted.
    fn from_source(storage: &StorageRoot, source: &Path) -> Result<Self, Rejected> {
        storage.ensure_track_storage()?;
        let path = storage.tracks_dir().join(format!(
            "{}.{}",
            Uuid::new_v4(),
            crate::audio::stored::EXTENSION
        ));
        // One second past the limit is enough to measure as over it.
        let cap = MAX_TRACK_DURATION_SECS + 1.0;
        crate::audio::stored::store(source, &path, Some(cap)).map_err(|cause| Rejected {
            reason: ImportRejection::Unreadable,
            cause,
        })?;
        let file = PendingImportFile::new(path.clone());
        let duration_seconds = read_tagged(&path)
            .map_err(|error| format!("Failed to measure stored audio: {error}"))?
            .properties()
            .duration()
            .as_secs_f64();
        within_duration_limit(duration_seconds)?;
        Ok(Self {
            path,
            duration_seconds,
            file,
        })
    }
}

/// What an untitled song is called: its file's name without the extension.
fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
}

/// An audio file's tags and properties, with the format taken from what the
/// file contains. Its extension is only the fallback: downloads are often AAC
/// or Opus under an `.mp3` name, and lofty parses by extension unless asked
/// to look.
fn read_tagged(path: &Path) -> lofty::error::Result<lofty::file::TaggedFile> {
    Probe::open(path)?.guess_file_type()?.read()
}

/// The cover embedded in an audio file's primary tag: the front cover, else
/// the back, else one marked "other". `None` when there is none or the file
/// cannot be parsed — a file without art is not an error.
pub(crate) fn embedded_picture(path: &Path) -> Option<lofty::picture::Picture> {
    let tagged_file = read_tagged(path).ok()?;
    tagged_file.primary_tag().and_then(|tag| {
        tag.pictures()
            .iter()
            .find(|pic| {
                matches!(
                    pic.pic_type(),
                    PictureType::CoverFront | PictureType::CoverBack | PictureType::Other
                )
            })
            .cloned()
    })
}

/// Extract album art from an audio file and save it to the art directory.
/// Returns `(art_path, art_mime)`; both `None` when no embedded artwork is
/// present or the file can't be parsed.
fn extract_album_art(
    storage: &StorageRoot,
    source_path: &Path,
) -> Result<(Option<String>, Option<String>), String> {
    let art_dir = storage.art_dir();
    match embedded_picture(source_path) {
        Some(picture) => {
            let mime = picture
                .mime_type()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "application/octet-stream".into());
            let art_file_name = format!(
                "{}.{}",
                Uuid::new_v4(),
                crate::storage::art_extension(Some(&mime))
            );
            let art_path = art_dir.join(&art_file_name);
            std::fs::write(&art_path, picture.data())
                .map_err(|e| format!("Failed to write album art: {}", e))?;
            Ok((Some(art_path.to_string_lossy().into_owned()), Some(mime)))
        }
        None => Ok((None, None)),
    }
}

/// A DJ-library row to import: the library's own metadata, and the audio
/// wherever the library keeps it.
pub struct LibraryTrack {
    pub source_id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub filename: Option<String>,
    /// The audio's path, or why the library cannot give one.
    pub audio_path: Result<PathBuf, String>,
}

/// One item an import was asked for.
pub enum ImportRequest {
    /// An audio file on disk, tagged from its own metadata.
    File(String),
    /// A DJ-library row; `source` on [`Importer::import`] names the library.
    Library(LibraryTrack),
}

impl ImportRequest {
    fn source_id(&self) -> &str {
        match self {
            Self::File(path) => path,
            Self::Library(track) => &track.source_id,
        }
    }

    fn name(&self) -> Option<String> {
        match self {
            Self::File(path) => Some(
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("track")
                    .to_string(),
            ),
            Self::Library(track) => track.title.clone().or_else(|| track.filename.clone()),
        }
    }
}

/// What an import writes to and reports through.
pub struct Importer<'a> {
    pub pool: &'a SqlitePool,
    pub storage: &'a StorageRoot,
    pub workers: &'a WorkerEnvironment,
    pub events: &'a Events,
    pub tasks: &'a AnalysisTaskGroup,
}

impl Importer<'_> {
    /// Import in two phases: this call owns durable insertion — hash, copy,
    /// metadata, catalog row — and the analysis task group owns analysis after
    /// it returns. An `Err` request is a row its library does not have,
    /// recorded as a failure without progress.
    ///
    /// An identity transition mid-batch rolls back every row this call
    /// inserted.
    pub async fn import(
        &self,
        principal: String,
        source: &str,
        requests: Vec<Result<ImportRequest, TrackImportFailure>>,
    ) -> Result<TrackImportResult, String> {
        let mut import = scheduler::ImportEventContext {
            import_id: Uuid::new_v4().to_string(),
            source: source.to_string(),
            done: 0,
            total: requests.len(),
        };
        let epoch = self.tasks.current_epoch()?;
        let lease = self.tasks.lease(epoch)?;
        let guard = lease.guard();
        let mut tracks: Vec<TrackSummary> = Vec::new();
        let mut failures = Vec::new();
        let mut new_ids = Vec::new();

        for (done, request) in requests.into_iter().enumerate() {
            if let Err(cause) = guard.checkpoint() {
                return Err(self.rollback(&new_ids, &principal, cause).await);
            }
            let request = match request {
                Ok(request) => request,
                Err(failure) => {
                    failures.push(failure);
                    continue;
                }
            };
            let name = request.name();
            self.progress(&import, done, None, name.clone(), None);
            let imported = match &request {
                ImportRequest::File(path) => {
                    file_fast_import(self.pool, self.storage, path, &principal).await
                }
                ImportRequest::Library(track) => {
                    library_fast_import(self.pool, self.storage, source, track, &principal).await
                }
            };
            match imported {
                Ok((track_id, is_new)) => {
                    if is_new {
                        new_ids.push(track_id.clone());
                    }
                    if let Err(cause) = guard.checkpoint() {
                        return Err(self.rollback(&new_ids, &principal, cause).await);
                    }
                    if let Some(track) = tracks_db::get_track_by_id(self.pool, &track_id).await? {
                        if tracks.iter().all(|existing| existing.id != track.id) {
                            tracks.push(track);
                        }
                    }
                    self.progress(&import, done + 1, Some(track_id), None, None);
                }
                Err(Rejected { reason, cause }) => {
                    self.progress(&import, done + 1, None, name.clone(), Some(cause.clone()));
                    failures.push(TrackImportFailure {
                        source_id: request.source_id().to_string(),
                        name,
                        reason,
                        message: cause,
                    });
                }
            }
        }

        if let Err(cause) = guard.checkpoint() {
            return Err(self.rollback(&new_ids, &principal, cause).await);
        }
        let total = import.total;
        self.progress(&import, total, None, None, None);
        let import_id = import.import_id.clone();
        if new_ids.is_empty() {
            drop(lease);
            let summary = scheduler::AnalysisSummary {
                done: total,
                errors: Vec::new(),
            };
            import.complete(self.events, summary);
        } else {
            import.done = total - new_ids.len();
            let pool = self.pool.clone();
            let storage = self.storage.clone();
            let workers = self.workers.clone();
            let events = self.events.clone();
            let analysed = new_ids.clone();
            let spawned = self.tasks.spawn(epoch, move |analysis| async move {
                run_background_analysis(pool, storage, workers, events, analysed, analysis, import)
                    .await;
            });
            if let Err(cause) = spawned {
                return Err(self.rollback(&new_ids, &principal, cause).await);
            }
            drop(lease);
        }
        Ok(TrackImportResult {
            import_id,
            tracks,
            failures,
        })
    }

    fn progress(
        &self,
        import: &scheduler::ImportEventContext,
        done: usize,
        track_id: Option<String>,
        current_track: Option<String>,
        error: Option<String>,
    ) {
        self.events
            .emit(Event::TrackImportState(TrackImportProgress {
                import_id: import.import_id.clone(),
                source: import.source.clone(),
                phase: TrackImportPhase::Importing,
                done,
                total: import.total,
                track_id,
                current_track,
                analysis: None,
                error,
            }));
    }

    /// Delete what this import inserted, newest first, and report `cause`
    /// with whatever could not be undone.
    async fn rollback(&self, track_ids: &[String], principal: &str, cause: String) -> String {
        let mut failures = Vec::new();
        for track_id in track_ids.iter().rev() {
            if let Err(error) = delete_track(self.pool, self.storage, track_id, principal).await {
                failures.push(format!("{track_id}: {error}"));
            }
        }
        if failures.is_empty() {
            cause
        } else {
            format!(
                "{cause}; failed to roll back cancelled import: {}",
                failures.join("; ")
            )
        }
    }
}

/// Insert a DJ-library row from the library's metadata, with its audio
/// converted into storage so the song no longer depends on where the library
/// keeps it. Runs no analysis. Deduplicates by source id, then by content hash.
/// Returns `(id, is_new)`.
async fn library_fast_import(
    pool: &SqlitePool,
    storage: &StorageRoot,
    source_type: &str,
    track: &LibraryTrack,
    uid: &str,
) -> Result<(String, bool), Rejected> {
    let audio_path = match &track.audio_path {
        Ok(path) if path.exists() => path,
        Ok(path) => {
            return Err(Rejected {
                reason: ImportRejection::Missing,
                cause: format!("Audio file not found: {}", path.display()),
            })
        }
        Err(message) => return Err(message.clone().into()),
    };
    let track_hash = compute_track_hash(audio_path)?;
    let source = Some((source_type, track.source_id.as_str()));
    if let Some(id) = already_imported(pool, uid, &track_hash, source).await? {
        return Ok((id, false));
    }
    let mut stored = StoredAudio::from_source(storage, audio_path)?;

    // Extract album art (reads just the tag header)
    let (album_art_path, album_art_mime) = extract_album_art(storage, audio_path)?;
    let mut pending_art = PendingImportFile::from_optional(album_art_path.as_deref());

    let (id, is_new) = tracks_db::insert_track_record(
        pool,
        &track_hash,
        &track
            .title
            .clone()
            .filter(|title| !title.is_empty())
            .or_else(|| {
                track
                    .filename
                    .as_deref()
                    .and_then(|name| file_stem(Path::new(name)))
            }),
        &track.artist,
        &track.album,
        None, // track_number
        None, // disc_number
        Some(stored.duration_seconds),
        &stored.path.to_string_lossy(),
        &album_art_path,
        &album_art_mime,
        uid,
        Some(source_type),
        Some(&track.source_id),
        track.filename.as_deref(),
    )
    .await?;
    if is_new {
        stored.file.keep();
        if let Some(art) = &mut pending_art {
            art.keep();
        }
    }

    Ok((id, is_new))
}

/// The stage the waveform branch reports under, beside the DAG's own names.
pub const WAVEFORM_STAGE: &str = "waveform";

/// Every stage an imported track reports, in no particular order: each
/// registered preprocessor, and the waveform branch beside them.
fn analysis_stages() -> Vec<String> {
    crate::preprocessing::registry::dag()
        .preprocessors()
        .map(|p| p.name().to_string())
        .chain(std::iter::once(WAVEFORM_STAGE.to_string()))
        .collect()
}

/// Run background analysis for a batch of tracks: the preprocessing DAG and
/// the waveform branch beside it, then the import's terminal event once both
/// have drained — a track is not analysed while its waveform is still being
/// drawn, so neither is the import.
async fn run_background_analysis(
    pool: SqlitePool,
    storage: StorageRoot,
    workers: WorkerEnvironment,
    events: Events,
    track_ids: Vec<String>,
    analysis: AnalysisGuard,
    import: scheduler::ImportEventContext,
) {
    if analysis.checkpoint().is_err() {
        return;
    }
    // Every track is queued before any work starts, so a consumer knows the
    // whole of each track's analysis before the first stage of any of them.
    let stages = analysis_stages();
    for track_id in &track_ids {
        scheduler::StageReport::new(import.clone(), events.clone(), track_id.clone())
            .queued(stages.clone());
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
    let waveforms = run_waveform_jobs(&pool, &track_ids, &analysis, Some((&import, &events)));
    let preprocessing = scheduler::run_for_tracks(
        pool.clone(),
        storage,
        workers,
        events.clone(),
        track_ids.clone(),
        analysis.clone(),
        Some(import.clone()),
    );
    let ((), summary) = tokio::join!(waveforms, preprocessing);
    import.complete(&events, summary);
}

/// Analyse a track again from its audio, in the background: every step
/// reruns, and what the track has stays in place until a step rewrites it.
pub fn reprocess_track(
    pool: &SqlitePool,
    storage: &StorageRoot,
    workers: &WorkerEnvironment,
    tasks: &AnalysisTaskGroup,
    track_id: String,
) -> Result<(), String> {
    let (pool, storage, workers) = (pool.clone(), storage.clone(), workers.clone());
    tasks.spawn(tasks.current_epoch()?, move |analysis| async move {
        if let Err(error) =
            scheduler::rerun_track(&pool, &storage, &workers, &track_id, &analysis).await
        {
            eprintln!("[preprocessing] reprocessing track {track_id} failed: {error}");
        }
    })?;
    Ok(())
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
    let tagged_file = read_tagged(file_path).ok();
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

/// Waveform generation for a batch, running beside — and bounded like — the
/// preprocessor DAG. It is an independent peer of the DAG, not a stage in it,
/// though under an import it reports as one ([`WAVEFORM_STAGE`]).
async fn run_waveform_jobs(
    pool: &SqlitePool,
    track_ids: &[String],
    analysis: &AnalysisGuard,
    import: Option<(&scheduler::ImportEventContext, &Events)>,
) {
    run_bounded_track_jobs(track_ids, analysis, analysis_worker_count(), |track_id| {
        let pool = pool.clone();
        let analysis = analysis.clone();
        let report = import.map(|(import, events)| {
            scheduler::StageReport::new(import.clone(), events.clone(), track_id.clone())
        });
        async move {
            let settle = |outcome| {
                if let Some(report) = &report {
                    report.settle(WAVEFORM_STAGE, outcome);
                }
            };
            let Ok(Some(track)) = tracks_db::get_track_by_id(&pool, &track_id).await else {
                settle(StageOutcome::Skipped);
                return;
            };
            if !Path::new(&track.file_path).exists() {
                settle(StageOutcome::Skipped);
                return;
            }
            if let Some(report) = &report {
                report.started(WAVEFORM_STAGE);
            }
            match crate::services::waveforms::ensure_track_waveform(&pool, &track_id, &analysis)
                .await
            {
                Ok(_) => settle(StageOutcome::Done),
                // A retired identity publishes nothing — see the scheduler.
                Err(_) if analysis.is_cancelled() => {}
                Err(e) => {
                    eprintln!("[preprocessing] waveform failed for {track_id}: {e}");
                    settle(StageOutcome::Failed(e));
                }
            }
        }
    })
    .await;
}

/// Delete a track and its derived data.
pub async fn delete_track(
    pool: &SqlitePool,
    storage: &StorageRoot,
    track_id: &str,
    owner_user_id: &str,
) -> Result<(), String> {
    recover_track_deletions(pool, storage).await?;
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
    let staged = match stage_track_files_for_deletion(storage, track_id, &plan) {
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
                let filesystem_rollback = rollback_staged_track_files(storage, &staged);
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
        rollback_staged_track_files(storage, &staged)?;
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
            rollback_staged_track_files(storage, &staged)?;
            return Err(format!("Failed to commit track deletion: {commit_error}"));
        }
        eprintln!(
            "[tracks] track {track_id} deletion committed despite an uncertain commit response: {commit_error}"
        );
    }

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
async fn recover_track_deletions(pool: &SqlitePool, storage: &StorageRoot) -> Result<(), String> {
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
// Helpers
// -----------------------------------------------------------------------------

/// `(tracks_dir, art_dir, stems_dir)`. A convenience tuple over
/// [`StorageRoot`], which owns the layout.
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

#[cfg(test)]
mod deletion_tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_file_imports_resolve_one_atomic_identity() {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::database::local::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&db.0, Some("alice"))
            .await
            .unwrap();
        let storage = StorageRoot::from_path(directory.path().to_path_buf());
        let source = directory.path().join("race.wav");
        crate::test_support::write_wav(&source, 1);
        let source = source.to_string_lossy().into_owned();

        let (left, right) = tokio::join!(
            file_fast_import(&db.0, &storage, &source, "alice"),
            file_fast_import(&db.0, &storage, &source, "alice"),
        );
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(left.0, right.0);
        assert_ne!(left.1, right.1, "exactly one insert must win the race");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(&db.0)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let managed_files = std::fs::read_dir(storage.tracks_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count();
        assert_eq!(managed_files, 1, "losing import leaked its copied audio");
    }

    /// A download is often one format under another's extension. What the
    /// file contains decides how it is read.
    #[tokio::test]
    async fn a_file_named_for_another_format_imports() {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::database::local::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&db.0, Some("alice"))
            .await
            .unwrap();
        let storage = StorageRoot::from_path(directory.path().to_path_buf());
        let source = directory.path().join("really a wav.mp3");
        crate::test_support::write_wav(&source, 1);

        let (track_id, _) = file_fast_import(&db.0, &storage, &source.to_string_lossy(), "alice")
            .await
            .unwrap();
        let track = tracks_db::get_track_by_id(&db.0, &track_id)
            .await
            .unwrap()
            .unwrap();
        let duration = track.duration_seconds.unwrap();
        assert!((duration - 1.0).abs() < 0.05, "{duration}");
        // Whatever it arrived as, it is kept in the one stored format.
        assert!(track.file_path.ends_with(".ogg"), "{}", track.file_path);
        // It has no title tag, so it is called what its file is.
        assert_eq!(track.title.as_deref(), Some("really a wav"));
    }

    /// Over the limit is refused, and refusing it leaves nothing behind.
    #[tokio::test]
    async fn a_song_over_the_length_limit_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::database::local::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&db.0, Some("alice"))
            .await
            .unwrap();
        let storage = StorageRoot::from_path(directory.path().to_path_buf());
        let source = directory.path().join("mix.wav");
        crate::test_support::write_wav(&source, MAX_TRACK_DURATION_SECS as u32 + 60);

        let refused = file_fast_import(&db.0, &storage, &source.to_string_lossy(), "alice")
            .await
            .unwrap_err();
        assert_eq!(
            refused.reason,
            ImportRejection::TooLong,
            "{}",
            refused.cause
        );
        let left = std::fs::read_dir(storage.tracks_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count();
        assert_eq!(left, 0, "a refused import left audio in storage");
    }

    /// A file that is not audio is refused as such, whatever it is named.
    #[tokio::test]
    async fn a_file_that_is_not_audio_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::database::local::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&db.0, Some("alice"))
            .await
            .unwrap();
        let storage = StorageRoot::from_path(directory.path().to_path_buf());
        let source = directory.path().join("notes.mp3");
        std::fs::write(&source, "not audio at all").unwrap();

        let refused = file_fast_import(&db.0, &storage, &source.to_string_lossy(), "alice")
            .await
            .unwrap_err();
        assert_eq!(
            refused.reason,
            ImportRejection::Unreadable,
            "{}",
            refused.cause
        );
    }

    #[tokio::test]
    async fn concurrent_source_imports_resolve_one_atomic_identity() {
        async fn insert_source(
            pool: &SqlitePool,
            hash: &str,
            path: &str,
        ) -> Result<(String, bool), String> {
            let empty = None;
            tracks_db::insert_track_record(
                pool,
                hash,
                &empty,
                &empty,
                &empty,
                None,
                None,
                None,
                path,
                &empty,
                &empty,
                "alice",
                Some("engine_dj"),
                Some("library:42"),
                None,
            )
            .await
        }

        let directory = tempfile::tempdir().unwrap();
        let db = crate::database::local::database::init_app_db_at(directory.path())
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&db.0, Some("alice"))
            .await
            .unwrap();
        let (left, right) = tokio::join!(
            insert_source(&db.0, "hash-a", "/a.wav"),
            insert_source(&db.0, "hash-b", "/b.wav"),
        );
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(left.0, right.0);
        assert_ne!(left.1, right.1, "exactly one source identity must win");
        let row: (i64, String) = sqlx::query_as("SELECT COUNT(*), track_hash FROM tracks")
            .fetch_one(&db.0)
            .await
            .unwrap();
        assert_eq!(row.0, 1);
        assert!(row.1 == "hash-a" || row.1 == "hash-b");
    }

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
