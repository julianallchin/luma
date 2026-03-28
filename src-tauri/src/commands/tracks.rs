//! Tauri commands for track operations

use tauri::{AppHandle, State};

use crate::audio::{FftService, StemCache};
use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::remote::common::SupabaseClient;
use crate::database::Db;
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
pub async fn delete_track(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    track_id: String,
) -> Result<(), String> {
    track_service::delete_track(&db.0, app_handle, &stem_cache, &track_id).await?;

    // Delete from cloud so it doesn't come back on next pull
    if let Ok(Some(token)) = auth::get_current_access_token(&state_db.0).await {
        let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
        if let Err(e) = client.delete("tracks", &track_id, &token).await {
            eprintln!("[delete_track] Failed to delete track from cloud: {}", e);
        }
    }

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

#[tauri::command]
pub async fn repair_album_art(db: State<'_, Db>, app_handle: AppHandle) -> Result<usize, String> {
    track_service::repair_album_art(&db.0, &app_handle).await
}
