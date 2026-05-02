use std::collections::HashSet;

use serde::Serialize;
use tauri::{AppHandle, State};
use ts_rs::TS;

use crate::audio::{FftService, StemCache};
use crate::database::Db;
use crate::prodjlink_manager::ProDJLinkManager;
use crate::render_engine::RenderEngine;
use crate::stagelinq_manager::StageLinqManager;

// Re-export perform types with ts-rs bindings

#[allow(dead_code)]
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

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/perform.ts")]
pub struct DeckSnapshot {
    pub decks: Vec<DeckState>,
    pub crossfader: f64,
    pub master_tempo: f64,
}

#[allow(dead_code)]
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
    pub track_id: Option<String>,
    pub has_annotations: bool,
    pub filename: String,
}

// ── StageLinQ commands ────────────────────────────────────────────────────────

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

// ── Pro DJ Link commands ──────────────────────────────────────────────────────

/// Passively listen for CDJ keepalives for 3 seconds and return discovered devices.
/// Safe to call while not connected — does not perform a device-number claim.
#[tauri::command]
pub async fn prodjlink_discover() -> Result<Vec<prodjlink::DiscoveredDevice>, String> {
    Ok(prodjlink::discover_cdjs(3000).await)
}

#[tauri::command]
pub async fn prodjlink_connect(
    app: AppHandle,
    manager: State<'_, ProDJLinkManager>,
    device_num: u8,
) -> Result<(), String> {
    manager.start(app, device_num).await
}

#[tauri::command]
pub async fn prodjlink_disconnect(manager: State<'_, ProDJLinkManager>) -> Result<(), String> {
    manager.stop().await
}

// ── Track matching ────────────────────────────────────────────────────────────

/// Match a track loaded on a StageLinQ (Denon) deck by its network path / filename.
#[tauri::command]
pub async fn perform_match_track(
    db: State<'_, Db>,
    track_network_path: String,
    venue_id: String,
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

    let scores =
        crate::database::local::scores::get_scores_for_track(&db.0, &track.id, &venue_id).await?;

    Ok(PerformTrackMatch {
        track_id: Some(track.id.clone()),
        has_annotations: !scores.is_empty(),
        filename,
    })
}

/// Match a track by metadata (title, artist, BPM, duration) for Pro DJ Link decks.
///
/// Strategy:
///   1. Filter by duration ±5s
///   2. Filter by BPM match (exact, ×2, ÷2) with 5% tolerance
///   3. Rank survivors by bigram similarity of combined title+artist string
///   4. Return best match above a minimum similarity threshold
#[tauri::command]
pub async fn perform_match_track_by_metadata(
    db: State<'_, Db>,
    title: String,
    artist: String,
    bpm: f64,
    duration_secs: f64,
    venue_id: String,
) -> Result<PerformTrackMatch, String> {
    if title.is_empty() && artist.is_empty() {
        return Ok(PerformTrackMatch {
            track_id: None,
            has_annotations: false,
            filename: String::new(),
        });
    }

    let candidates =
        crate::database::local::tracks::get_tracks_by_duration(&db.0, duration_secs, 5.0).await?;

    // BPM filter — skip if no BPM data on either side
    let bpm_filtered: Vec<_> = candidates
        .into_iter()
        .filter(|t| bpm_matches(t.bpm.unwrap_or(0.0), bpm))
        .collect();

    if bpm_filtered.is_empty() {
        return Ok(PerformTrackMatch {
            track_id: None,
            has_annotations: false,
            filename: String::new(),
        });
    }

    // Fuzzy sort by combined title + artist bigram similarity
    let query = normalize_for_match(&format!("{title} {artist}"));
    let mut scored: Vec<_> = bpm_filtered
        .iter()
        .map(|t| {
            let lib = normalize_for_match(&format!(
                "{} {}",
                t.title.as_deref().unwrap_or(""),
                t.artist.as_deref().unwrap_or("")
            ));
            let score = bigram_similarity(&query, &lib);
            (t, score)
        })
        .filter(|(_, score)| *score >= 0.25)
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let track = match scored.first().map(|(t, _)| *t) {
        Some(t) => t,
        None => {
            return Ok(PerformTrackMatch {
                track_id: None,
                has_annotations: false,
                filename: String::new(),
            });
        }
    };

    let filename = track.source_filename.clone().unwrap_or_else(|| {
        track
            .file_path
            .split('/')
            .next_back()
            .unwrap_or("")
            .to_string()
    });

    let scores =
        crate::database::local::scores::get_scores_for_track(&db.0, &track.id, &venue_id).await?;

    Ok(PerformTrackMatch {
        track_id: Some(track.id.clone()),
        has_annotations: !scores.is_empty(),
        filename,
    })
}

// ── BPM + fuzzy matching helpers ──────────────────────────────────────────────

/// Returns true if `lib_bpm` and `src_bpm` are within 5% of each other,
/// accounting for harmonic BPM analysis differences (×2 or ÷2).
fn bpm_matches(lib_bpm: f64, src_bpm: f64) -> bool {
    // If either side has no BPM data, pass through (don't filter out)
    if lib_bpm <= 0.0 || src_bpm <= 0.0 {
        return true;
    }
    let tolerance = 0.05;
    for &ratio in &[1.0f64, 2.0, 0.5] {
        let adjusted = lib_bpm * ratio;
        if (adjusted - src_bpm).abs() / src_bpm <= tolerance {
            return true;
        }
    }
    false
}

/// Normalize a string for fuzzy comparison: lowercase + keep only alphanumeric + spaces.
fn normalize_for_match(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Bigram (character pair) Jaccard similarity in [0, 1].
fn bigram_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let bigrams_a: HashSet<(char, char)> = a.chars().zip(a.chars().skip(1)).collect();
    let bigrams_b: HashSet<(char, char)> = b.chars().zip(b.chars().skip(1)).collect();
    if bigrams_a.is_empty() || bigrams_b.is_empty() {
        return 0.0;
    }
    let intersection = bigrams_a.intersection(&bigrams_b).count();
    let union = bigrams_a.len() + bigrams_b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

// ── Composite deck commands ────────────────────────────────────────────────────

/// Composite a track's light show and assign the result to a specific perform deck.
#[tauri::command]
pub async fn render_composite_deck(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    deck_id: u8,
    track_id: String,
    venue_id: String,
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

/// Compile MIDI cues for a deck that has no matching track in Luma.
///
/// Constructs a beat grid from the CDJ's live BPM and beat-in-bar so that
/// beat-reactive cue patterns stay in phase with the playing music.
/// `beat_number` is 1-indexed (1–4 for 4/4); `position_secs` is the playback
/// position at the moment the track was loaded.
#[tauri::command]
pub async fn render_composite_deck_unmatched(
    app: AppHandle,
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    stem_cache: State<'_, StemCache>,
    fft_service: State<'_, FftService>,
    deck_id: u8,
    bpm: f64,
    beat_number: u8,
    position_secs: f64,
    duration_secs: f64,
    venue_id: String,
) -> Result<(), String> {
    let resource_path = crate::services::fixtures::resolve_fixtures_root(&app).ok();
    crate::controller_compositor::compile_cues_for_unmatched_deck(
        &db.0,
        &stem_cache,
        &fft_service,
        resource_path,
        &render_engine,
        deck_id,
        bpm as f32,
        beat_number,
        position_secs as f32,
        duration_secs as f32,
        &venue_id,
    )
    .await
}
