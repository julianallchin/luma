use serde::{Deserialize, Serialize};

// ============================================================================
// MIDI Input Types
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MidiInput {
    /// Pad/button — note on/off on a channel
    Note { channel: u8, note: u8 },
    /// Button CC — any value > 0 = pressed, 0 = released
    ControlChange { channel: u8, cc: u8 },
    /// Continuous CC — maps 0–127 → 0.0–1.0
    ControlChangeValue { channel: u8, cc: u8 },
}

// ============================================================================
// ModifierDef
// ============================================================================

/// A named held input. A binding that requires it fires only while it is held.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModifierDef {
    pub id: String,
    pub uid: Option<String>,
    pub venue_id: String,
    pub name: String,
    pub input: MidiInput,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateModifierInput {
    pub venue_id: String,
    pub name: String,
    pub input: MidiInput,
}

// ============================================================================
// MidiBinding
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MidiAction {
    /// Continuous: CC value 0–127 → 0.0–1.0 intensity of one group.
    SetIntensity { group_id: String },
    /// Toggle ManualLayerState::active
    ControllerActive,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MidiBinding {
    pub id: String,
    pub uid: Option<String>,
    pub venue_id: String,
    pub trigger: MidiInput,
    /// Modifier names; all must be held for this binding to match
    pub required_modifiers: Vec<String>,
    /// If true: no other modifiers may be held (exact match)
    pub exclusive: bool,
    pub action: MidiAction,
    pub display_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateBindingInput {
    pub venue_id: String,
    pub trigger: MidiInput,
    #[serde(default)]
    pub required_modifiers: Vec<String>,
    #[serde(default)]
    pub exclusive: bool,
    pub action: MidiAction,
    #[serde(default)]
    pub display_order: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBindingInput {
    pub id: String,
    pub trigger: Option<MidiInput>,
    pub required_modifiers: Option<Vec<String>>,
    pub exclusive: Option<bool>,
    pub action: Option<MidiAction>,
    pub display_order: Option<i64>,
}

// ============================================================================
// Frontend state events
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControllerState {
    pub active: bool,
    /// Currently held modifier names
    pub held_modifiers: Vec<String>,
    /// Per-group intensity values (group_id → 0.0–1.0). Only groups with non-default intensity included.
    pub group_intensities: std::collections::HashMap<String, f32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControllerStatus {
    pub connected: bool,
    pub port_name: Option<String>,
    pub available_ports: Vec<String>,
}
