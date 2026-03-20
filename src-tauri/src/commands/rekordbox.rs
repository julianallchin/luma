use std::path::Path;

use tauri::{AppHandle, Emitter, State};

use crate::audio::StemCache;
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::tracks as tracks_db;
use crate::database::Db;
use crate::engine_dj::types::ImportProgressEvent;
use crate::models::tracks::TrackSummary;
use crate::rekordbox::subprocess;
use crate::rekordbox::types::{RekordboxLibraryInfo, RekordboxPlaylist, RekordboxTrack};
use crate::services::tracks as track_service;

#[tauri::command]
pub async fn rekordbox_open_library() -> Result<RekordboxLibraryInfo, String> {
    tokio::task::spawn_blocking(subprocess::get_library_info)
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

#[tauri::command]
pub async fn rekordbox_list_tracks() -> Result<Vec<RekordboxTrack>, String> {
    tokio::task::spawn_blocking(subprocess::list_tracks)
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

#[tauri::command]
pub async fn rekordbox_list_playlists() -> Result<Vec<RekordboxPlaylist>, String> {
    tokio::task::spawn_blocking(subprocess::list_playlists)
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

#[tauri::command]
pub async fn rekordbox_get_playlist_tracks(
    playlist_id: String,
) -> Result<Vec<RekordboxTrack>, String> {
    let pid = playlist_id.clone();
    tokio::task::spawn_blocking(move || subprocess::get_playlist_tracks(&pid))
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

#[tauri::command]
pub async fn rekordbox_search_tracks(query: String) -> Result<Vec<RekordboxTrack>, String> {
    let q = query.clone();
    tokio::task::spawn_blocking(move || subprocess::search_tracks(&q))
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

#[tauri::command]
pub async fn rekordbox_import_tracks(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    track_uuids: Vec<String>,
) -> Result<Vec<TrackSummary>, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;

    // Fetch all rekordbox tracks in one subprocess call
    let all_rb_tracks: Vec<RekordboxTrack> = tokio::task::spawn_blocking(subprocess::list_tracks)
        .await
        .map_err(|e| format!("Task join error: {}", e))??;

    let total = track_uuids.len();
    let mut imported = Vec::new();
    let mut new_track_ids = Vec::new();

    // Phase 1: Fast import — DB inserts only, no analysis
    for (i, rb_uuid) in track_uuids.iter().enumerate() {
        let rb_track = all_rb_tracks
            .iter()
            .find(|t| &t.uuid == rb_uuid)
            .ok_or_else(|| format!("Rekordbox track with UUID {} not found", rb_uuid))?;

        let source_id = &rb_track.uuid; // Rekordbox UUID is the stable source_id
        let track_name = rb_track
            .title
            .clone()
            .or_else(|| rb_track.filename.clone())
            .unwrap_or_default();

        // Emit progress
        let _ = app_handle.emit(
            "rekordbox-import-progress",
            ImportProgressEvent {
                done: i,
                total,
                current_track: Some(track_name),
                phase: "importing".into(),
                error: None,
            },
        );

        // Resolve audio file path
        let audio_path_str = rb_track
            .file_path
            .as_deref()
            .ok_or_else(|| format!("No file path for Rekordbox track {}", rb_uuid))?;
        let audio_path = Path::new(audio_path_str);
        if !audio_path.exists() {
            return Err(format!("Audio file not found: {}", audio_path.display()));
        }

        let (track_id, is_new) = track_service::dj_fast_import(
            &db.0,
            &app_handle,
            "rekordbox",
            source_id,
            &rb_track.title,
            &rb_track.artist,
            &rb_track.album,
            rb_track.duration_seconds,
            rb_track.filename.as_deref(),
            audio_path,
            uid.clone(),
        )
        .await?;

        if is_new {
            new_track_ids.push(track_id);
        }

        let track = tracks_db::get_track_by_id(&db.0, track_id)
            .await?
            .ok_or_else(|| format!("Failed to fetch imported track {}", track_id))?;
        imported.push(track);
    }

    // Emit completion of Phase 1
    let _ = app_handle.emit(
        "rekordbox-import-progress",
        ImportProgressEvent {
            done: total,
            total,
            current_track: None,
            phase: "importing".into(),
            error: None,
        },
    );

    // Phase 2: Spawn background analysis for newly imported tracks
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
