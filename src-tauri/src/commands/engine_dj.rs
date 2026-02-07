use tauri::{AppHandle, State};

use crate::audio::StemCache;
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::tracks as tracks_db;
use crate::database::Db;
use crate::engine_dj;
use crate::engine_dj::types::{
    EngineDjLibraryInfo, EngineDjPlaylist, EngineDjSyncResult, EngineDjTrack,
};
use crate::models::tracks::TrackSummary;
use crate::services::tracks::{self as track_service, TrackSourceInfo};

#[tauri::command]
pub async fn engine_dj_open_library(library_path: String) -> Result<EngineDjLibraryInfo, String> {
    let pool = engine_dj::db::open_engine_db(&library_path).await?;
    let info = engine_dj::db::get_library_info(&pool, &library_path).await?;
    pool.close().await;
    Ok(info)
}

#[tauri::command]
pub async fn engine_dj_list_playlists(
    library_path: String,
) -> Result<Vec<EngineDjPlaylist>, String> {
    let pool = engine_dj::db::open_engine_db(&library_path).await?;
    let playlists = engine_dj::db::list_playlists(&pool).await?;
    pool.close().await;
    Ok(playlists)
}

#[tauri::command]
pub async fn engine_dj_list_tracks(library_path: String) -> Result<Vec<EngineDjTrack>, String> {
    let pool = engine_dj::db::open_engine_db(&library_path).await?;
    let tracks = engine_dj::db::list_tracks(&pool).await?;
    pool.close().await;
    Ok(tracks)
}

#[tauri::command]
pub async fn engine_dj_get_playlist_tracks(
    library_path: String,
    playlist_id: i64,
) -> Result<Vec<EngineDjTrack>, String> {
    let pool = engine_dj::db::open_engine_db(&library_path).await?;
    let tracks = engine_dj::db::get_playlist_tracks(&pool, playlist_id).await?;
    pool.close().await;
    Ok(tracks)
}

#[tauri::command]
pub async fn engine_dj_search_tracks(
    library_path: String,
    query: String,
) -> Result<Vec<EngineDjTrack>, String> {
    let pool = engine_dj::db::open_engine_db(&library_path).await?;
    let tracks = engine_dj::db::search_tracks(&pool, &query).await?;
    pool.close().await;
    Ok(tracks)
}

#[tauri::command]
pub async fn engine_dj_import_tracks(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    library_path: String,
    track_ids: Vec<i64>,
) -> Result<Vec<TrackSummary>, String> {
    let engine_pool = engine_dj::db::open_engine_db(&library_path).await?;
    let info = engine_dj::db::get_library_info(&engine_pool, &library_path).await?;
    let db_uuid = info.database_uuid;

    let uid = auth::get_current_user_id(&state_db.0).await?;

    // Fetch all engine tracks in one query
    let all_engine_tracks = engine_dj::db::list_tracks(&engine_pool).await?;
    engine_pool.close().await;

    let mut imported = Vec::new();

    for engine_track_id in track_ids {
        let engine_track = all_engine_tracks
            .iter()
            .find(|t| t.id == engine_track_id)
            .ok_or_else(|| format!("Engine DJ track {} not found", engine_track_id))?;

        let source_id = format!("{}:{}", db_uuid, engine_track.id);

        // Check if already imported
        if let Some(existing) =
            tracks_db::get_track_by_source_id(&db.0, "engine_dj", &source_id).await?
        {
            imported.push(existing);
            continue;
        }

        // Resolve audio file path
        let audio_path = engine_dj::resolve_engine_path(&library_path, &engine_track.path);
        if !audio_path.exists() {
            return Err(format!(
                "Audio file not found: {} (resolved from {})",
                audio_path.display(),
                engine_track.path
            ));
        }

        let source = TrackSourceInfo {
            source_type: Some("engine_dj".to_string()),
            source_id: Some(source_id),
            source_filename: Some(engine_track.filename.clone()),
        };

        let track = track_service::import_track_with_source(
            &db.0,
            app_handle.clone(),
            &stem_cache,
            audio_path.to_string_lossy().to_string(),
            uid.clone(),
            Some(source),
        )
        .await?;

        imported.push(track);
    }

    Ok(imported)
}

#[tauri::command]
pub async fn engine_dj_sync_library(
    db: State<'_, Db>,
    library_path: String,
) -> Result<EngineDjSyncResult, String> {
    let engine_pool = engine_dj::db::open_engine_db(&library_path).await?;
    let info = engine_dj::db::get_library_info(&engine_pool, &library_path).await?;
    let db_uuid = info.database_uuid;

    let engine_tracks = engine_dj::db::list_tracks(&engine_pool).await?;
    engine_pool.close().await;

    let mut updated: i64 = 0;
    let mut missing: i64 = 0;
    let mut new_count: i64 = 0;

    for engine_track in &engine_tracks {
        let source_id = format!("{}:{}", db_uuid, engine_track.id);

        match tracks_db::get_track_by_source_id(&db.0, "engine_dj", &source_id).await? {
            Some(existing) => {
                // Check if metadata changed
                let title_changed = existing.title != engine_track.title;
                let artist_changed = existing.artist != engine_track.artist;
                let filename_changed = existing
                    .source_filename
                    .as_deref()
                    .map(|f| f != engine_track.filename)
                    .unwrap_or(true);

                if title_changed || artist_changed || filename_changed {
                    tracks_db::update_track_source_metadata(
                        &db.0,
                        existing.id,
                        &engine_track.title,
                        &engine_track.artist,
                        Some(&engine_track.filename),
                    )
                    .await?;
                    updated += 1;
                }
            }
            None => {
                // Track exists in Engine DJ but not imported — count as new
                new_count += 1;
            }
        }
    }

    // Check for tracks that were imported from this library but no longer exist in Engine DJ
    let luma_tracks = tracks_db::list_tracks(&db.0).await?;
    let prefix = format!("{}:", db_uuid);
    for track in &luma_tracks {
        if track.source_type.as_deref() == Some("engine_dj") {
            if let Some(sid) = &track.source_id {
                if sid.starts_with(&prefix) {
                    let engine_id: Option<i64> = sid
                        .strip_prefix(&prefix)
                        .and_then(|s| s.parse().ok());
                    if let Some(eid) = engine_id {
                        if !engine_tracks.iter().any(|et| et.id == eid) {
                            missing += 1;
                        }
                    }
                }
            }
        }
    }

    Ok(EngineDjSyncResult {
        updated,
        missing,
        new_count,
    })
}

#[tauri::command]
pub async fn engine_dj_default_library_path() -> Result<String, String> {
    let path = engine_dj::default_library_path();
    Ok(path.to_string_lossy().to_string())
}
