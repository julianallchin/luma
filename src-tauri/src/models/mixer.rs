use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which MIDI CC to read for a given fader/crossfader.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/mixer.ts")]
#[ts(rename_all = "camelCase")]
pub struct MidiCcSpec {
    pub channel: u8,
    pub cc: u8,
}

/// Mapping from deck faders + crossfader → MIDI CC specs.
/// Serialised as JSON and stored per-venue in the database.
#[derive(Debug, Clone, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/mixer.ts")]
#[ts(rename_all = "camelCase")]
pub struct MixerMapping {
    /// deck_id (1-based) → CC spec
    pub channel_faders: HashMap<u8, MidiCcSpec>,
    pub crossfader: Option<MidiCcSpec>,
}

/// Live fader/crossfader values, emitted as the `mixer_state` Tauri event.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/mixer.ts")]
#[ts(rename_all = "camelCase")]
pub struct MixerState {
    /// deck_id → 0.0–1.0
    pub channel_faders: HashMap<u8, f64>,
    pub crossfader: f64,
}

/// Connection status returned by `mixer_get_status`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/mixer.ts")]
#[ts(rename_all = "camelCase")]
pub struct MixerStatus {
    pub connected: bool,
    pub port_name: Option<String>,
    pub available_ports: Vec<String>,
}
