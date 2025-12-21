//! Tauri commands for track operations

use tauri::{AppHandle, State};

use crate::audio::{FftService, StemCache};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::models::tracks::{MelSpec, TrackSummary};
use crate::schema::BeatGrid;
use crate::services::tracks as track_service;
use crate::services::sync;

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<TrackSummary>, String> {
    track_service::list_tracks(&db.0).await
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
    let track = track_service::import_track(&db.0, app_handle, &stem_cache, file_path, uid).await?;

    if let Ok(Some(token)) = auth::get_current_access_token(&state_db.0).await {
        let track_clone = track.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = sync::push_track(&track_clone, &token).await {
                eprintln!("[sync] Failed to push track: {}", e);
            }
        });
    }

    Ok(track)
}

#[tauri::command]
pub async fn get_melspec(
    db: State<'_, Db>,
    fft_service: State<'_, FftService>,
    track_id: i64,
) -> Result<MelSpec, String> {
    track_service::get_melspec(&db.0, &fft_service, track_id).await
}

#[tauri::command]
pub async fn get_track_beats(db: State<'_, Db>, track_id: i64) -> Result<Option<BeatGrid>, String> {
    track_service::get_track_beats(&db.0, track_id).await
}

#[tauri::command]
pub async fn delete_track(
    db: State<'_, Db>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    track_id: i64,
) -> Result<(), String> {
    track_service::delete_track(&db.0, app_handle, &stem_cache, track_id).await
}

#[tauri::command]
pub async fn wipe_tracks(db: State<'_, Db>, app_handle: AppHandle) -> Result<(), String> {
    track_service::wipe_tracks(&db.0, app_handle).await
}
