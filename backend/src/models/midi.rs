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
// Target
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Target {
    /// All fixtures
    All,
    /// Specific groups baked in at binding time
    Explicit { groups: Vec<String> },
    /// Resolved from held modifiers at fire time (union of their groups)
    FromModifiers,
}

// ============================================================================
// ModifierDef
// ============================================================================

/// A held input that routes subsequent pad presses to specific groups.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModifierDef {
    pub id: String,
    pub uid: Option<String>,
    pub venue_id: String,
    pub name: String,
    pub input: MidiInput,
    /// None = named modifier with no group association
    pub groups: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateModifierInput {
    pub venue_id: String,
    pub name: String,
    pub input: MidiInput,
    pub groups: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdateModifierInput {
    pub id: String,
    pub name: Option<String>,
    pub input: Option<MidiInput>,
    pub groups: Option<Option<Vec<String>>>,
}

// ============================================================================
// MidiBinding
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TriggerMode {
    Toggle,
    /// On while held (note-on / cc>0), off on release
    Flash,
    /// Tap = latch toggle; hold ≥ threshold = flash. Default 300ms.
    TapToggleHoldFlash {
        threshold_ms: u64,
    },
}

impl Default for TriggerMode {
    fn default() -> Self {
        TriggerMode::TapToggleHoldFlash { threshold_ms: 300 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MidiAction {
    /// Continuous: CC value 0–127 → 0.0–1.0 intensity. group_id=None = master.
    SetIntensity { group_id: Option<String> },
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
    pub mode: TriggerMode,
    pub action: MidiAction,
    /// Stored for a future action that fires at a target; no action reads it now.
    pub target_override: Option<Target>,
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
    pub mode: Option<TriggerMode>,
    pub action: MidiAction,
    pub target_override: Option<Target>,
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
    pub mode: Option<TriggerMode>,
    pub action: Option<MidiAction>,
    pub target_override: Option<Option<Target>>,
    pub display_order: Option<i64>,
}

// ============================================================================
// Frontend state events
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControllerState {
    pub active: bool,
    pub master_intensity: f32,
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
