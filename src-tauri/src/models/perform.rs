//! Wire types for the perform surface: live deck telemetry pushed on the
//! `perform_event` channel, and the result of matching a deck's loaded track
//! against the local library.
//!
//! `DeckState`/`DeckSnapshot`/`DeckEvent` are deliberately **snake_case on the
//! wire** — no `rename_all` — because the frontend store reads `beat_bpm` and
//! `track_network_path` directly. `PerformTrackMatch` is camelCase. Changing
//! either casing is a frontend-visible break.

use serde::Serialize;
use ts_rs::TS;

/// One deck's state, normalized across StageLinQ (Denon) and Pro DJ Link
/// (Pioneer). Fields a given protocol cannot report are filled with neutral
/// defaults rather than made optional — see `prodjlink_manager`.
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

/// Every deck plus the mixer state, as of one telemetry frame.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/perform.ts")]
pub struct DeckSnapshot {
    pub decks: Vec<DeckState>,
    pub crossfader: f64,
    pub master_tempo: f64,
}

/// The `perform_event` payload. Tagged by `type`.
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

/// Outcome of matching a deck's loaded track against the library.
///
/// A miss is not an error: `track_id` is the only nullability signal, and
/// `filename` is `""` rather than null when nothing could be parsed or matched.
#[derive(Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/perform.ts")]
#[serde(rename_all = "camelCase")]
pub struct PerformTrackMatch {
    pub track_id: Option<String>,
    pub has_annotations: bool,
    pub filename: String,
}

impl PerformTrackMatch {
    /// A miss carrying whatever filename was recovered, if any.
    pub(crate) fn miss(filename: String) -> Self {
        Self {
            track_id: None,
            has_annotations: false,
            filename,
        }
    }
}
