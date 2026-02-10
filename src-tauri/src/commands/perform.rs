use serde::Serialize;
use tauri::{AppHandle, State};
use ts_rs::TS;

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::render_engine::RenderEngine;
use crate::stagelinq_manager::StageLinqManager;

// Re-export stagelinq types with ts-rs bindings

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/perform.ts")]
pub struct DeckState {
    pub id: u8,
    pub title: String,
    pub artist: String,
    pub bpm: f64,
    pub playing: bool,
    pub volume: f64,
    pub fader: f64,
    pub master: bool,
    pub song_loaded: bool,
    pub track_length: f64,
    pub sample_rate: f64,
    pub track_network_path: String,
    pub beat: f64,
    pub total_beats: f64,
    pub beat_bpm: f64,
    pub samples: f64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/perform.ts")]
pub struct DeckSnapshot {
    pub decks: Vec<DeckState>,
    pub crossfader: f64,
    pub master_tempo: f64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/perform.ts")]
#[serde(tag = "type")]
pub enum DeckEvent {
    DeviceDiscovered {
        address: String,
        name: String,
        version: String,
    },
    Connected {
        address: String,
    },
    StateChanged(DeckSnapshot),
    Disconnected {
        address: String,
    },
    Error {
        message: String,
    },
}

#[derive(Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/perform.ts")]
#[serde(rename_all = "camelCase")]
pub struct PerformTrackMatch {
    #[ts(as = "Option<f64>")]
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
