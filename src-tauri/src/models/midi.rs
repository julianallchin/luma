use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::node_graph::BlendMode;

// ============================================================================
// MIDI Input Types
// ============================================================================

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
pub enum MidiInput {
    /// Pad/button — note on/off on a channel
    Note { channel: u8, note: u8 },
    /// Button CC — any value > 0 = pressed, 0 = released
    ControlChange { channel: u8, cc: u8 },
    /// Continuous CC — maps 0–127 → 0.0–1.0
    ControlChangeValue { channel: u8, cc: u8 },
}

// ============================================================================
// Target
// ============================================================================

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
pub enum Target {
    /// All fixtures
    All,
    /// Specific groups baked in at binding time
    Explicit { groups: Vec<String> },
    /// Resolved from held modifiers at fire time (union of their groups)
    FromModifiers,
}

// ============================================================================
// Cue Execution Mode
// ============================================================================

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
pub enum CueExecutionMode {
    /// Compile N bars at track BPM; loop `elapsed % loop_duration`
    Loop { bars: u8 },
    /// Compile full track duration; sample at current deck playback time.
    /// Auto-detected when graph contains harmony_analysis / audio_input /
    /// frequency_amplitude / beat_input nodes.
    TrackTime,
}

impl Default for CueExecutionMode {
    fn default() -> Self {
        CueExecutionMode::Loop { bars: 4 }
    }
}

// ============================================================================
// Cue
// ============================================================================

/// A named pre-configured pattern instance — the live equivalent of a score annotation.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct Cue {
    pub id: String,
    pub venue_id: String,
    pub name: String,
    pub pattern_id: String,
    #[ts(type = "Record<string, unknown>")]
    pub args: Value,
    #[ts(type = "number")]
    pub z_index: i64,
    pub blend_mode: BlendMode,
    pub default_target: Target,
    pub execution_mode: CueExecutionMode,
    #[ts(type = "number")]
    pub display_x: i64,
    #[ts(type = "number")]
    pub display_y: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateCueInput {
    pub venue_id: String,
    pub name: String,
    pub pattern_id: String,
    #[serde(default)]
    #[ts(type = "Record<string, unknown> | undefined")]
    pub args: Option<Value>,
    #[serde(default)]
    #[ts(type = "number | undefined")]
    pub z_index: Option<i64>,
    pub blend_mode: Option<BlendMode>,
    pub default_target: Option<Target>,
    pub execution_mode: Option<CueExecutionMode>,
    #[serde(default)]
    #[ts(type = "number | undefined")]
    pub display_x: Option<i64>,
    #[serde(default)]
    #[ts(type = "number | undefined")]
    pub display_y: Option<i64>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct UpdateCueInput {
    pub id: String,
    pub name: Option<String>,
    pub pattern_id: Option<String>,
    #[serde(default)]
    #[ts(type = "Record<string, unknown> | undefined")]
    pub args: Option<Value>,
    #[ts(type = "number | undefined")]
    pub z_index: Option<i64>,
    pub blend_mode: Option<BlendMode>,
    pub default_target: Option<Target>,
    pub execution_mode: Option<CueExecutionMode>,
    #[ts(type = "number | undefined")]
    pub display_x: Option<i64>,
    #[ts(type = "number | undefined")]
    pub display_y: Option<i64>,
}

// ============================================================================
// ModifierDef
// ============================================================================

/// A held input that routes subsequent pad presses to specific groups.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct ModifierDef {
    pub id: String,
    pub venue_id: String,
    pub name: String,
    pub input: MidiInput,
    /// None = named modifier with no group association
    pub groups: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateModifierInput {
    pub venue_id: String,
    pub name: String,
    pub input: MidiInput,
    pub groups: Option<Vec<String>>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct UpdateModifierInput {
    pub id: String,
    pub name: Option<String>,
    pub input: Option<MidiInput>,
    pub groups: Option<Option<Vec<String>>>,
}

// ============================================================================
// MidiBinding
// ============================================================================

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
pub enum TriggerMode {
    Toggle,
    /// On while held (note-on / cc>0), off on release
    Flash,
    /// Tap = latch toggle; hold ≥ threshold = flash. Default 300ms.
    TapToggleHoldFlash {
        #[ts(type = "number")]
        threshold_ms: u64,
    },
}

impl Default for TriggerMode {
    fn default() -> Self {
        TriggerMode::TapToggleHoldFlash { threshold_ms: 300 }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
pub enum MidiAction {
    FireCue {
        cue_id: String,
    },
    /// Continuous: CC value 0–127 → 0.0–1.0 intensity. group_id=None = master.
    SetIntensity {
        group_id: Option<String>,
    },
    Blackout,
    /// Toggle ManualLayerState::active
    ControllerActive,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct MidiBinding {
    pub id: String,
    pub venue_id: String,
    pub trigger: MidiInput,
    /// Modifier names; all must be held for this binding to match
    pub required_modifiers: Vec<String>,
    /// If true: no other modifiers may be held (exact match)
    pub exclusive: bool,
    pub mode: TriggerMode,
    pub action: MidiAction,
    /// Overrides cue's default_target if set
    pub target_override: Option<Target>,
    #[ts(type = "number")]
    pub display_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct CreateBindingInput {
    pub venue_id: String,
    pub trigger: MidiInput,
    #[serde(default)]
    pub required_modifiers: Vec<String>,
    #[serde(default)]
    pub exclusive: bool,
    pub mode: Option<TriggerMode>,
    pub action: MidiAction,
    pub target_override: Option<Target>,
    #[serde(default)]
    #[ts(type = "number")]
    pub display_order: i64,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct UpdateBindingInput {
    pub id: String,
    pub trigger: Option<MidiInput>,
    pub required_modifiers: Option<Vec<String>>,
    pub exclusive: Option<bool>,
    pub mode: Option<TriggerMode>,
    pub action: Option<MidiAction>,
    pub target_override: Option<Option<Target>>,
    #[ts(type = "number | undefined")]
    pub display_order: Option<i64>,
}

// ============================================================================
// Frontend state events
// ============================================================================

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct ControllerState {
    pub active: bool,
    pub master_intensity: f32,
    /// Latched cue IDs
    pub active_cue_ids: Vec<String>,
    /// Flash (held) cue IDs
    pub flash_cue_ids: Vec<String>,
    /// Currently held modifier names
    pub held_modifiers: Vec<String>,
    /// Per-group intensity values (group_id → 0.0–1.0). Only groups with non-default intensity included.
    pub group_intensities: std::collections::HashMap<String, f32>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/midi.ts")]
#[ts(rename_all = "camelCase")]
pub struct ControllerStatus {
    pub connected: bool,
    pub port_name: Option<String>,
    pub available_ports: Vec<String>,
}
