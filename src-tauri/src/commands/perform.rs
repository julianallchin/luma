use serde::Serialize;
use tauri::{AppHandle, State};

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::render_engine::RenderEngine;
use crate::stagelinq_manager::StageLinqManager;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformTrackMatch {
    pub track_id: Option<i64>,
    pub has_annotations: bool,
    pub filename: String,
}

#[tauri::command]
pub async fn stagelinq_connect(
    app: AppHandle,
    manager: State<'_, StageLinqManager>,
) -> Result<(), String> {
    manager.start(app).await
}

#[tauri::command]
pub async fn stagelinq_disconnect(manager: State<'_, StageLinqManager>) -> Result<(), String> {
    manager.stop().await
}

#[tauri::command]
pub async fn perform_match_track(
    db: State<'_, Db>,
    track_network_path: String,
) -> Result<PerformTrackMatch, String> {
    let filename = match stagelinq::extract_filename_from_network_path(&track_network_path) {
        Some(f) => f.to_string(),
        None => {
            return Ok(PerformTrackMatch {
                track_id: None,
                has_annotations: false,
                filename: String::new(),
            });
        }
    };

    let tracks =
        crate::database::local::tracks::get_tracks_by_source_filename(&db.0, &filename).await?;

    let track = match tracks.first() {
        Some(t) => t,
        None => {
            return Ok(PerformTrackMatch {
                track_id: None,
                has_annotations: false,
                filename,
            });
        }
    };

    let scores = crate::database::local::scores::get_scores_for_track(&db.0, track.id).await?;

    Ok(PerformTrackMatch {
        track_id: Some(track.id),
        has_annotations: !scores.is_empty(),
        filename,
    })
}

/// Composite a track's light show and assign the result to a specific perform deck.
#[tauri::command]
pub async fn render_composite_deck(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    deck_id: u8,
    track_id: i64,
    venue_id: i64,
) -> Result<(), String> {
    // Composite track (writes to render_engine.active_layer)
    crate::compositor::composite_track(
        app,
        db,
        render_engine.clone(),
        stem_cache,
        fft_service,
        track_id,
        venue_id,
        None,
    )
    .await?;
    // Move result to perform deck slot
    render_engine.promote_active_layer_to_deck(deck_id);
    Ok(())
}
