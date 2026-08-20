//! The one Engine DJ command still bound to Tauri.
//!
//! Every read-only command in this domain is on the dispatch seam
//! (`dispatch::handlers::engine_dj`). `engine_dj_import_tracks` stays here
//! because it threads an `AppHandle` through `services::tracks` and
//! `preprocessing` for storage paths as well as progress emission — see the
//! port guide's "spawned-progress commands" case.

use tauri::{AppHandle, Emitter, State};

use crate::audio::StemCache;
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::tracks as tracks_db;
use crate::database::Db;
use crate::engine_dj;
use crate::engine_dj::types::ImportProgressEvent;
use crate::models::tracks::TrackSummary;
use crate::preprocessing::AnalysisTaskGroup;
use crate::services::tracks as track_service;

#[tauri::command]
pub async fn engine_dj_import_tracks(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    app_handle: AppHandle,
    stem_cache: State<'_, StemCache>,
    analysis_tasks: State<'_, AnalysisTaskGroup>,
    library_path: String,
    track_ids: Vec<i64>,
) -> Result<Vec<TrackSummary>, String> {
    let engine_pool = engine_dj::db::open_engine_db(&library_path).await?;
    let info = engine_dj::db::get_library_info(&engine_pool, &library_path).await?;
    let db_uuid = info.database_uuid;

    let uid = auth::get_current_user_id(&state_db.0).await?;
    let analysis_epoch = analysis_tasks.current_epoch()?;
    let import_lease = analysis_tasks.lease(analysis_epoch)?;
    let import_guard = import_lease.guard();

    // Fetch all engine tracks in one query
    let all_engine_tracks = engine_dj::db::list_tracks(&engine_pool).await?;
    engine_pool.close().await;

    let total = track_ids.len();
    let mut imported = Vec::new();
    let mut new_track_ids = Vec::new();

    // Phase 1: Fast import — DB inserts only, no analysis
    for (i, engine_track_id) in track_ids.iter().enumerate() {
        import_guard.checkpoint()?;
        let engine_track = all_engine_tracks
            .iter()
            .find(|t| t.id == *engine_track_id)
            .ok_or_else(|| format!("Engine DJ track {} not found", engine_track_id))?;

        let source_id = format!("{}:{}", db_uuid, engine_track.id);
        let track_name = engine_track
            .title
            .clone()
            .or_else(|| Some(engine_track.filename.clone()))
            .unwrap_or_default();

        // Emit progress
        let _ = app_handle.emit(
            "engine-dj-import-progress",
            ImportProgressEvent {
                done: i,
                total,
                current_track: Some(track_name),
                phase: "importing".into(),
                error: None,
            },
        );

        // Resolve audio file path
        let audio_path = engine_dj::resolve_engine_path(&library_path, &engine_track.path);
        if !audio_path.exists() {
            return Err(format!(
                "Audio file not found: {} (resolved from {})",
                audio_path.display(),
                engine_track.path
            ));
        }

        let (track_id, is_new) = track_service::engine_dj_fast_import(
            &db.0,
            &app_handle,
            engine_track,
            &audio_path,
            &source_id,
            uid.clone(),
        )
        .await?;
        import_guard.checkpoint()?;

        if is_new {
            new_track_ids.push(track_id.clone());
        }

        let track = tracks_db::get_track_by_id(&db.0, &track_id)
            .await?
            .ok_or_else(|| format!("Failed to fetch imported track {}", track_id))?;
        imported.push(track);
    }

    // Emit completion of Phase 1
    let _ = app_handle.emit(
        "engine-dj-import-progress",
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
