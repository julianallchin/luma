//! Tauri commands for track operations

use tauri::{AppHandle, Emitter, State};

use crate::audio::{FftService, StemCache};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::tracks as tracks_db;
use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::database::Db;
use crate::engine_dj::types::ImportProgressEvent;
use crate::models::tracks::{MelSpec, TrackSummary};
use crate::node_graph::BeatGrid;
use crate::preprocessing::AnalysisTaskGroup;
use serde::Serialize;

use crate::services::tracks::{self as track_service, TrackBarClassifications};
use std::collections::HashMap;

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<TrackSummary>, String> {
    track_service::list_tracks(&db.0).await
}

/// Fast query: just the annotation counts per track for a venue
#[tauri::command]
pub async fn get_venue_annotation_counts(
    db: State<'_, Db>,
    venue_id: String,
) -> Result<HashMap<String, i64>, String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT s.track_id, COUNT(tsc.id) as cnt
         FROM scores s
         JOIN track_scores tsc ON tsc.score_id = s.id
         WHERE s.venue_id = ?
         GROUP BY s.track_id",
    )
    .bind(&venue_id)
    .fetch_all(access.connection())
    .await
    .map_err(|e| format!("Failed to get venue annotation counts: {}", e))?;

    Ok(rows.into_iter().collect())
}

#[tauri::command]
pub async fn import_track(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    analysis_tasks: State<'_, AnalysisTaskGroup>,
    file_path: String,
) -> Result<TrackSummary, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    track_service::import_track(
        &db.0,
        app_handle,
        &stem_cache,
        &analysis_tasks,
        file_path,
        uid,
    )
    .await
}

#[tauri::command]
pub async fn import_tracks(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    analysis_tasks: State<'_, AnalysisTaskGroup>,
    file_paths: Vec<String>,
) -> Result<Vec<TrackSummary>, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    let analysis_epoch = analysis_tasks.current_epoch()?;
    let import_lease = analysis_tasks.lease(analysis_epoch)?;
    let import_guard = import_lease.guard();

    let total = file_paths.len();
    let mut imported = Vec::new();
    let mut new_track_ids = Vec::new();

    // Phase 1: Fast import — copy files + DB inserts, no analysis
    for (i, file_path) in file_paths.iter().enumerate() {
        import_guard.checkpoint()?;
        let track_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("track")
            .to_string();

        let _ = app_handle.emit(
            "file-import-progress",
            ImportProgressEvent {
                done: i,
                total,
                current_track: Some(track_name),
                phase: "importing".into(),
                error: None,
            },
        );

        match track_service::file_fast_import(&db.0, &app_handle, file_path, uid.clone()).await {
            Ok((track_id, is_new)) => {
                import_guard.checkpoint()?;
                if is_new {
                    new_track_ids.push(track_id.clone());
                }
                if let Ok(Some(track)) = tracks_db::get_track_by_id(&db.0, &track_id).await {
                    imported.push(track);
                }
            }
            Err(e) => {
                eprintln!("[import_tracks] failed to import {}: {}", file_path, e);
                let _ = app_handle.emit(
                    "file-import-progress",
                    ImportProgressEvent {
                        done: i,
                        total,
                        current_track: None,
                        phase: "importing".into(),
                        error: Some(e),
                    },
                );
            }
        }
    }

    // Emit completion of Phase 1
    let _ = app_handle.emit(
        "file-import-progress",
        ImportProgressEvent {
            done: total,
            total,
            current_track: None,
            phase: "importing".into(),
            error: None,
        },
    );

    // Phase 2: Spawn background analysis for newly imported tracks (parallel)
    if !new_track_ids.is_empty() {
        import_guard.checkpoint()?;
        let pool = db.0.clone();
        let handle = app_handle.clone();
        let cache = stem_cache.inner().clone();
        analysis_tasks.spawn(analysis_epoch, move |analysis| async move {
            track_service::run_background_analysis(pool, handle, cache, new_track_ids, analysis)
                .await;
        })?;
    }

    import_guard.checkpoint()?;
    Ok(imported)
}

#[tauri::command]
pub async fn get_melspec(
    db: State<'_, Db>,
    fft_service: State<'_, FftService>,
    track_id: String,
) -> Result<MelSpec, String> {
    track_service::get_melspec(&db.0, &fft_service, &track_id).await
}

#[tauri::command]
pub async fn get_track_beats(
    db: State<'_, Db>,
    track_id: String,
) -> Result<Option<BeatGrid>, String> {
    track_service::get_track_beats(&db.0, &track_id).await
}

#[tauri::command]
pub async fn get_track_bar_classifications(
    db: State<'_, Db>,
    track_id: String,
) -> Result<Option<TrackBarClassifications>, String> {
    track_service::get_track_bar_classifications(&db.0, &track_id).await
}

/// Per-class drum onset timestamps (seconds). Keys are the n2n class names
/// `kick`, `snare`, `hat`, `cymbal`. Returns `None` if drum transcription
/// hasn't run for this track.
#[tauri::command]
pub async fn get_track_drum_onsets(
    db: State<'_, Db>,
    track_id: String,
) -> Result<Option<HashMap<String, Vec<f32>>>, String> {
    tracks_db::get_track_drum_onsets(&db.0, &track_id).await
}

/// Per-tag F1-optimal suggestion thresholds bundled with the classifier
/// weights. Returns `tag_name -> threshold`. The frontend uses these in
/// place of a flat 0.5 cutoff so rare tags (e.g. `vocal_chop` at 0.165)
/// surface at the calibration the model was tuned for.
#[tauri::command]
pub fn get_classifier_thresholds() -> Result<HashMap<String, f64>, String> {
    track_service::classifier_thresholds()
}

#[tauri::command]
pub async fn delete_track(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    track_id: String,
) -> Result<(), String> {
    let principal = auth::get_current_user_id(&state_db.0).await?;
    track_service::delete_track(
        &db.0,
        app_handle,
        &stem_cache,
        &track_id,
        principal.as_deref(),
    )
    .await?;

    // The sync_delete_tracks SQLite trigger enqueues the committed row deletion.

    Ok(())
}

#[tauri::command]
pub async fn reprocess_track(
    db: State<'_, Db>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    analysis_tasks: State<'_, AnalysisTaskGroup>,
    track_id: String,
) -> Result<(), String> {
    let analysis_epoch = analysis_tasks.current_epoch()?;
    let pool = db.0.clone();
    let handle = app_handle.clone();
    let cache = stem_cache.inner().clone();
    analysis_tasks.spawn(analysis_epoch, move |analysis| async move {
        track_service::run_background_analysis(pool, handle, cache, vec![track_id], analysis).await;
    })?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAudioBase64 {
    pub data: String,
    pub mime_type: String,
}

#[tauri::command]
pub async fn get_track_audio_base64(
    db: State<'_, Db>,
    track_id: String,
) -> Result<TrackAudioBase64, String> {
    let (data, mime_type) = track_service::get_track_audio_base64(&db.0, &track_id).await?;
    Ok(TrackAudioBase64 { data, mime_type })
}
