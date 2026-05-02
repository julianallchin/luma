//! Tauri commands for track operations

use tauri::{AppHandle, Emitter, State};

use crate::audio::{FftService, StemCache};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::tracks as tracks_db;
use crate::database::Db;
use crate::engine_dj::types::ImportProgressEvent;
use crate::models::tracks::{MelSpec, TrackBrowserRow, TrackSummary};
use crate::node_graph::BeatGrid;
use serde::Serialize;

use crate::services::tracks as track_service;
use std::collections::HashMap;

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<TrackSummary>, String> {
    track_service::list_tracks(&db.0).await
}

#[tauri::command]
pub async fn list_tracks_enriched(
    db: State<'_, Db>,
    venue_id: Option<String>,
) -> Result<Vec<TrackBrowserRow>, String> {
    track_service::list_tracks_enriched(&db.0, venue_id.as_deref()).await
}

/// Fast query: just the annotation counts per track for a venue
#[tauri::command]
pub async fn get_venue_annotation_counts(
    db: State<'_, Db>,
    venue_id: String,
) -> Result<HashMap<String, i64>, String> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT s.track_id, COUNT(tsc.id) as cnt
         FROM scores s
         JOIN track_scores tsc ON tsc.score_id = s.id
         WHERE s.venue_id = ?
         GROUP BY s.track_id",
    )
    .bind(&venue_id)
    .fetch_all(&db.0)
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
    file_path: String,
) -> Result<TrackSummary, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    track_service::import_track(&db.0, app_handle, &stem_cache, file_path, uid).await
}

#[tauri::command]
pub async fn import_tracks(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    file_paths: Vec<String>,
) -> Result<Vec<TrackSummary>, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;

    let total = file_paths.len();
    let mut imported = Vec::new();
    let mut new_track_ids = Vec::new();

    // Phase 1: Fast import — copy files + DB inserts, no analysis
    for (i, file_path) in file_paths.iter().enumerate() {
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
        let pool = db.0.clone();
        let handle = app_handle.clone();
        let cache = stem_cache.inner().clone();
        tokio::spawn(async move {
            track_service::run_background_analysis(pool, handle, cache, new_track_ids).await;
        });
    }

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackBarClassifications {
    pub classifications: serde_json::Value,
    pub tag_order: serde_json::Value,
}

#[tauri::command]
pub async fn get_track_bar_classifications(
    db: State<'_, Db>,
    track_id: String,
) -> Result<Option<TrackBarClassifications>, String> {
    let raw = tracks_db::get_track_bar_classifications_raw(&db.0, &track_id).await?;
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
/// weights. Returns `tag_name -> threshold`. The frontend uses these in
/// place of a flat 0.5 cutoff so rare tags (e.g. `vocal_chop` at 0.165)
/// surface at the calibration the model was tuned for.
#[tauri::command]
pub fn get_classifier_thresholds() -> Result<HashMap<String, f64>, String> {
    let payload: serde_json::Value =
        serde_json::from_str(crate::classifier_worker::bundled_thresholds())
            .map_err(|e| format!("Failed to parse bundled thresholds JSON: {e}"))?;
    let map = payload
        .get("thresholds")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "Bundled thresholds JSON missing `thresholds` object".to_string())?;
    let mut out = HashMap::with_capacity(map.len());
    for (k, v) in map {
        let f = v
            .as_f64()
            .ok_or_else(|| format!("Threshold for `{k}` is not a number"))?;
        out.insert(k.clone(), f);
    }
    Ok(out)
}

#[tauri::command]
pub async fn delete_track(
    db: State<'_, Db>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    track_id: String,
) -> Result<(), String> {
    track_service::delete_track(&db.0, app_handle, &stem_cache, &track_id).await?;

    // Soft-delete is auto-enqueued by the sync_delete_tracks SQLite trigger
    // when the row is deleted in delete_track_record().

    Ok(())
}

#[tauri::command]
pub async fn reprocess_track(
    db: State<'_, Db>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    track_id: String,
) -> Result<(), String> {
    let pool = db.0.clone();
    let handle = app_handle.clone();
    let cache = stem_cache.inner().clone();
    tokio::spawn(async move {
        track_service::run_background_analysis(pool, handle, cache, vec![track_id]).await;
    });
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

#[tauri::command]
pub async fn wipe_tracks(db: State<'_, Db>, app_handle: AppHandle) -> Result<(), String> {
    track_service::wipe_tracks(&db.0, app_handle).await
}
