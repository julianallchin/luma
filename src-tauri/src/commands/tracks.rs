//! Track commands that are not yet on the dispatch seam.
//!
//! Both of these spawn background analysis, which threads an `AppHandle` down
//! through `preprocessing::scheduler` for storage paths as well as progress
//! emits. They move once that stack takes `(&StorageRoot, &Events)` — see the
//! port guide's "spawned-progress commands".

use tauri::{AppHandle, Emitter, State};

use crate::audio::StemCache;
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::tracks as tracks_db;
use crate::database::Db;
use crate::engine_dj::types::ImportProgressEvent;
use crate::models::tracks::TrackSummary;
use crate::preprocessing::AnalysisTaskGroup;

use crate::services::tracks as track_service;

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
