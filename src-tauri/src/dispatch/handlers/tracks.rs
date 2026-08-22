use std::collections::HashMap;

use serde::Serialize;

use crate::database::local::tracks as tracks_db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};
use crate::models::node_graph::BeatGrid;
use crate::models::tracks::{
    TrackBrowserRow, TrackImportFailure, TrackImportPhase, TrackImportProgress, TrackImportResult,
    TrackSummary,
};
use crate::preprocessing::scheduler::ImportEventContext;
use crate::services::tracks::{self as track_service, TrackBarClassifications};

/// The whole library, unscoped and without album-art bytes.
pub async fn list_tracks(services: &AppServices) -> Result<Vec<TrackSummary>, CommandError> {
    Ok(track_service::list_tracks(&services.db.0).await?)
}

/// Clip counts for one venue, keyed by track id. Sparse — see
/// [`tracks_db::get_venue_annotation_counts`].
pub async fn get_venue_annotation_counts(
    services: &AppServices,
    venue_id: String,
) -> Result<HashMap<String, i64>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(tracks_db::get_venue_annotation_counts(&mut access).await?)
}

/// `None` when beat detection has not run for this track.
pub async fn get_track_beats(
    services: &AppServices,
    track_id: String,
) -> Result<Option<BeatGrid>, CommandError> {
    Ok(track_service::get_track_beats(&services.db.0, &track_id).await?)
}

/// `None` when bar classification has not run. Both fields are opaque JSON —
/// there is no Rust-side schema for the classification shape.
pub async fn get_track_bar_classifications(
    services: &AppServices,
    track_id: String,
) -> Result<Option<TrackBarClassifications>, CommandError> {
    Ok(track_service::get_track_bar_classifications(&services.db.0, &track_id).await?)
}

/// Per-class drum onset timestamps (seconds), keyed by the n2n class names
/// `kick`, `snare`, `hat`, `cymbal`. `None` when transcription has not run, and
/// individual classes may still be absent.
pub async fn get_track_drum_onsets(
    services: &AppServices,
    track_id: String,
) -> Result<Option<HashMap<String, Vec<f32>>>, CommandError> {
    Ok(tracks_db::get_track_drum_onsets(&services.db.0, &track_id).await?)
}

/// Per-tag F1-optimal suggestion thresholds bundled with the classifier
/// weights, as `tag_name -> threshold`. Callers use these in place of a flat
/// 0.5 cutoff so rare tags (e.g. `vocal_chop` at 0.165) surface at the
/// calibration the model was tuned for.
pub async fn get_classifier_thresholds(
    _services: &AppServices,
) -> Result<HashMap<String, f64>, CommandError> {
    Ok(track_service::classifier_thresholds()?)
}

/// Delete a track's row and its files. A hard delete, not an archive; the
/// `sync_delete_tracks` trigger enqueues the committed row deletion for push.
pub async fn delete_track(services: &AppServices, track_id: String) -> Result<(), CommandError> {
    let principal = services.session_user_id().await?;
    track_service::delete_track(
        &services.db.0,
        &services.storage,
        &services.stem_cache,
        &track_id,
        principal.as_deref(),
    )
    .await?;
    Ok(())
}

/// A track's audio as standard base64 plus a MIME type inferred from the file
/// extension.
///
/// Only justified because the bytes are fed to an LLM as a file part — this
/// reads the whole file into memory and crosses the wire as a string. Playback
/// must not use it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAudioBase64 {
    pub data: String,
    pub mime_type: String,
}

pub async fn get_track_audio_base64(
    services: &AppServices,
    track_id: String,
) -> Result<TrackAudioBase64, CommandError> {
    let (data, mime_type) =
        track_service::get_track_audio_base64(&services.db.0, &track_id).await?;
    Ok(TrackAudioBase64 { data, mime_type })
}

/// The track browser's rows: the whole library, plus the per-venue annotation
/// counts and coverage when `venue_id` is given.
///
/// `album_art_path` is a path on disk, not bytes — a bulk response stays small
/// and the client lazy-loads the image itself.
pub async fn list_tracks_enriched(
    services: &AppServices,
    venue_id: Option<String>,
) -> Result<Vec<TrackBrowserRow>, CommandError> {
    Ok(track_service::list_tracks_enriched(&services.db.0, venue_id.as_deref()).await?)
}

/// Import local files in two phases: this command owns only durable insertion;
/// the task group owns analysis after the result future is gone.
pub async fn import_tracks(
    services: &AppServices,
    file_paths: Vec<String>,
) -> Result<TrackImportResult, CommandError> {
    let import_id = uuid::Uuid::new_v4().to_string();
    let source = "file";
    let total = file_paths.len();
    let principal = services.session_user_id().await?;
    let epoch = services.analysis_tasks.current_epoch()?;
    let lease = services.analysis_tasks.lease(epoch)?;
    let guard = lease.guard();
    let mut tracks = Vec::new();
    let mut failures = Vec::new();
    let mut new_ids = Vec::new();

    for (done, path) in file_paths.iter().enumerate() {
        if let Err(error) = guard.checkpoint() {
            return Err(
                rollback_after_failure(services, &new_ids, principal.as_deref(), error).await,
            );
        }
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("track")
            .to_string();
        emit_import_progress(
            services,
            &import_id,
            source,
            TrackImportPhase::Importing,
            done,
            total,
            None,
            Some(name.clone()),
            None,
        );
        match track_service::file_fast_import(
            &services.db.0,
            &services.storage,
            path,
            principal.clone(),
        )
        .await
        {
            Ok((track_id, is_new)) => {
                if let Err(error) = guard.checkpoint() {
                    if is_new {
                        new_ids.push(track_id);
                    }
                    return Err(rollback_after_failure(
                        services,
                        &new_ids,
                        principal.as_deref(),
                        error,
                    )
                    .await);
                }
                if is_new {
                    new_ids.push(track_id.clone());
                }
                if let Some(track) = tracks_db::get_track_by_id(&services.db.0, &track_id).await? {
                    push_unique(&mut tracks, track);
                }
                emit_import_progress(
                    services,
                    &import_id,
                    source,
                    TrackImportPhase::Importing,
                    done + 1,
                    total,
                    Some(track_id),
                    None,
                    None,
                );
            }
            Err(message) => {
                emit_import_progress(
                    services,
                    &import_id,
                    source,
                    TrackImportPhase::Importing,
                    done + 1,
                    total,
                    None,
                    Some(name),
                    Some(message.clone()),
                );
                failures.push(TrackImportFailure {
                    source_id: path.clone(),
                    message,
                });
            }
        }
    }

    finish_fast_import(
        services,
        epoch,
        guard,
        lease,
        &import_id,
        source,
        total,
        new_ids,
        principal.as_deref(),
    )
    .await?;
    Ok(TrackImportResult {
        import_id,
        tracks,
        failures,
    })
}

pub async fn reprocess_track(services: &AppServices, track_id: String) -> Result<(), CommandError> {
    let epoch = services.analysis_tasks.current_epoch()?;
    let pool = services.db.0.clone();
    let storage = services.storage.clone();
    let workers = services.workers.clone();
    let events = services.events.clone();
    let cache = services.stem_cache.clone();
    let context = ImportEventContext {
        import_id: uuid::Uuid::new_v4().to_string(),
        source: "reprocess".to_string(),
        done: 0,
        total: 1,
    };
    services
        .analysis_tasks
        .spawn(epoch, move |analysis| async move {
            track_service::run_background_analysis(
                pool,
                storage,
                workers,
                events,
                cache,
                vec![track_id],
                analysis,
                context,
            )
            .await;
        })?;
    Ok(())
}

pub(crate) fn emit_import_progress(
    services: &AppServices,
    import_id: &str,
    source: &str,
    phase: TrackImportPhase,
    done: usize,
    total: usize,
    track_id: Option<String>,
    current_track: Option<String>,
    error: Option<String>,
) {
    services.events.emit(
        "track-import-state",
        TrackImportProgress {
            import_id: import_id.to_string(),
            source: source.to_string(),
            phase,
            done,
            total,
            track_id,
            current_track,
            step: None,
            error,
        },
    );
}

pub(crate) fn push_unique(tracks: &mut Vec<TrackSummary>, track: TrackSummary) {
    if tracks.iter().all(|existing| existing.id != track.id) {
        tracks.push(track);
    }
}

pub(crate) async fn finish_fast_import(
    services: &AppServices,
    epoch: crate::preprocessing::task_group::AnalysisEpoch,
    guard: crate::preprocessing::AnalysisGuard,
    lease: crate::preprocessing::task_group::AnalysisLease,
    import_id: &str,
    source: &str,
    total: usize,
    new_ids: Vec<String>,
    principal: Option<&str>,
) -> Result<(), CommandError> {
    if let Err(error) = guard.checkpoint() {
        return Err(rollback_after_failure(services, &new_ids, principal, error).await);
    }
    emit_import_progress(
        services,
        import_id,
        source,
        TrackImportPhase::Importing,
        total,
        total,
        None,
        None,
        None,
    );
    services.events.emit("library-changed", ());
    if new_ids.is_empty() {
        drop(lease);
        emit_import_progress(
            services,
            import_id,
            source,
            TrackImportPhase::Complete,
            total,
            total,
            None,
            None,
            None,
        );
        return Ok(());
    }
    let pool = services.db.0.clone();
    let storage = services.storage.clone();
    let workers = services.workers.clone();
    let events = services.events.clone();
    let cache = services.stem_cache.clone();
    let context = ImportEventContext {
        import_id: import_id.to_string(),
        source: source.to_string(),
        done: total.saturating_sub(new_ids.len()),
        total,
    };
    let rollback_ids = new_ids.clone();
    if let Err(error) = services
        .analysis_tasks
        .spawn(epoch, move |analysis| async move {
            track_service::run_background_analysis(
                pool, storage, workers, events, cache, new_ids, analysis, context,
            )
            .await;
        })
    {
        let error = rollback_after_failure(services, &rollback_ids, principal, error).await;
        services.events.emit("library-changed", ());
        return Err(error);
    }
    drop(lease);
    Ok(())
}

pub(crate) async fn rollback_imports(
    services: &AppServices,
    track_ids: &[String],
    principal: Option<&str>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    for track_id in track_ids.iter().rev() {
        if let Err(error) = track_service::delete_track(
            &services.db.0,
            &services.storage,
            &services.stem_cache,
            track_id,
            principal,
        )
        .await
        {
            failures.push(format!("{track_id}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "failed to roll back cancelled import: {}",
            failures.join("; ")
        ))
    }
}

pub(crate) async fn rollback_after_failure(
    services: &AppServices,
    track_ids: &[String],
    principal: Option<&str>,
    cause: String,
) -> CommandError {
    match rollback_imports(services, track_ids, principal).await {
        Ok(()) => cause.into(),
        Err(rollback) => CommandError::Internal(format!("{cause}; {rollback}")),
    }
}

/// The write is committed before the event goes out, so a lost `library-changed`
/// is not a failed rename and must not be reported as one.
pub async fn update_track_metadata(
    services: &AppServices,
    track_id: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
) -> Result<(), CommandError> {
    tracks_db::update_track_metadata(
        &services.db.0,
        &track_id,
        title.as_deref(),
        artist.as_deref(),
        album.as_deref(),
    )
    .await?;
    services.events.emit("library-changed", ());
    Ok(())
}

#[cfg(test)]
mod import_tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier};

    use serde_json::Value;

    use super::*;
    use crate::database::local::{auth, database, state};
    use crate::dispatch::{EventSink, Events};

    struct ChannelEvents {
        tx: tokio::sync::mpsc::UnboundedSender<(String, Value)>,
        inserted: Arc<Barrier>,
    }

    impl EventSink for ChannelEvents {
        fn emit(&self, event: &str, payload: Value) {
            let inserted = event == "track-import-state"
                && payload.get("trackId").is_some_and(|value| !value.is_null());
            let _ = self.tx.send((event.to_string(), payload));
            if inserted {
                self.inserted.wait();
            }
        }
    }

    fn write_wav(path: &Path) {
        let frames = 8_000_u32;
        let data_len = frames * 2;
        let mut bytes = Vec::with_capacity((44 + data_len) as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&8_000_u32.to_le_bytes());
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.resize((44 + data_len) as usize, 0);
        std::fs::write(path, bytes).unwrap();
    }

    async fn services(directory: &Path, events: Events) -> Arc<AppServices> {
        let db = database::init_app_db_at(directory).await.unwrap();
        let state_db = state::init_state_db_at(directory).await.unwrap();
        auth::bootstrap_host_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        Arc::new(
            AppServices::headless(db, state_db, storage, directory.to_path_buf(), workspaces)
                .with_events(events),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identity_epoch_cancellation_rolls_back_catalog_and_managed_audio() {
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("source.wav");
        write_wav(&audio);
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let inserted = Arc::new(Barrier::new(2));
        let services = services(
            directory.path(),
            Events::new(ChannelEvents {
                tx: events_tx,
                inserted: Arc::clone(&inserted),
            }),
        )
        .await;
        let task_services = Arc::clone(&services);
        let import = tokio::spawn(async move {
            import_tracks(&task_services, vec![audio.to_string_lossy().into_owned()]).await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let (name, payload) = events_rx.recv().await.unwrap();
                if name == "track-import-state"
                    && payload.get("phase").and_then(Value::as_str) == Some("importing")
                    && payload.get("trackId").is_some_and(|value| !value.is_null())
                {
                    break;
                }
            }
        })
        .await
        .expect("import never reached the inserted-row barrier");
        // The import has already resolved its principal, matching an in-flight
        // operation when auth freezes StateDb for the identity transition.
        let mut session_guard = services.state_db.0.acquire().await.unwrap();
        let admission = auth::capture_write_admission(&services.db.0, &mut session_guard)
            .await
            .unwrap();
        let tasks = services.analysis_tasks.clone();
        let mut suspended = tasks.subscribe_identity_suspension();
        let transition_tasks = tasks.clone();
        let pool = services.db.0.clone();
        let transition = tokio::spawn(async move {
            // Production auth order: cancel/drain import compensation while
            // the old principal is still admitted, then close admission.
            let barrier = transition_tasks
                .suspend_for_identity_switch()
                .await
                .unwrap();
            auth::suspend_write_admission(&pool, &admission)
                .await
                .unwrap();
            drop(barrier);
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), suspended.changed())
            .await
            .expect("identity transition never closed analysis admission")
            .expect("identity suspension signal closed");
        assert!(tasks.current_epoch().is_err());
        inserted.wait();

        let error = import.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("identity transition"));
        transition.await.unwrap();
        let admission: (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT accepting, maintenance, active_uid
             FROM auth_write_admission WHERE singleton = 1",
        )
        .fetch_one(&services.db.0)
        .await
        .unwrap();
        assert_eq!(
            admission,
            (0, 0, None),
            "production transition did not close admission"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(&services.db.0)
            .await
            .unwrap();
        assert_eq!(count, 0, "cancelled import left catalog ownership");
        let audio_files = std::fs::read_dir(services.storage.tracks_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(audio_files, 0, "cancelled import left managed audio");
    }
}
