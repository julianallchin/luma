use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::fixtures::{ChannelType, FixtureDefinition, Mode};

/// Movement pyramid configuration for a fixture group.
/// Defines the base aim direction and angular extents for UV perturbation.
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct MovementConfig {
    /// Base direction unit vector (Z-up coordinate system)
    pub base_dir_x: f64,
    pub base_dir_y: f64,
    pub base_dir_z: f64,
    /// Angular extent along primary axis (degrees, half-width)
    pub extent_u: f64,
    /// Angular extent along secondary axis (degrees, half-width)
    pub extent_v: f64,
    /// Rotation of the UV plane around the base direction (degrees)
    pub uv_rotation: f64,
}

impl Default for MovementConfig {
    fn default() -> Self {
        Self {
            base_dir_x: 0.0,
            base_dir_y: 0.0,
            base_dir_z: -1.0, // straight down
            extent_u: 30.0,
            extent_v: 30.0,
            uv_rotation: 0.0,
        }
    }
}

/// Auto-detected fixture type based on fixture definition capabilities
#[derive(Debug, Serialize, Deserialize, Clone, TS, PartialEq, Eq, Hash)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "snake_case")]
pub enum FixtureType {
    MovingHead,
    PixelBar,
    ParWash,
    Scanner,
    Strobe,
    Static,
    Unknown,
}

impl FixtureType {
    /// Detect fixture type from its definition and selected mode
    pub fn detect(definition: &FixtureDefinition, mode: &Mode) -> Self {
        let mut has_pan = false;
        let mut has_tilt = false;
        let mut has_rgb = false;
        let mut has_dimmer = false;
        let mut has_pixels = false;

        // Check mode channels against definition channels
        for mode_channel in &mode.channels {
            if let Some(channel) = definition
                .channels
                .iter()
                .find(|c| c.name == mode_channel.name)
            {
                let ch_type = channel.get_type();
                match ch_type {
                    ChannelType::Pan => has_pan = true,
                    ChannelType::Tilt => has_tilt = true,
                    ChannelType::Intensity => {
                        let colour = channel.get_colour();
                        if colour == super::fixtures::ChannelColour::None {
                            has_dimmer = true;
                        } else {
                            has_rgb = true;
                        }
                    }
                    ChannelType::Colour => has_rgb = true,
                    _ => {}
                }
            }
        }

        // Check for pixel bar (multiple heads in layout)
        if let Some(physical) = &definition.physical {
            if let Some(layout) = &physical.layout {
                if layout.width > 1 || layout.height > 1 {
                    has_pixels = true;
                }
            }
        }
        // Also check if mode has multiple heads
        if mode.heads.len() > 2 {
            has_pixels = true;
        }

        // Determine type based on capabilities
        match (has_pan, has_tilt, has_rgb, has_pixels, has_dimmer) {
            (true, true, _, _, _) => FixtureType::MovingHead,
            (_, _, _, true, _) => FixtureType::PixelBar,
            (true, false, _, _, _) | (false, true, _, _, _) => FixtureType::Scanner,
            (false, false, true, false, _) => FixtureType::ParWash,
            (false, false, false, false, true) => FixtureType::Static,
            _ => FixtureType::Unknown,
        }
    }
}

/// Normalize a group name to snake_case (lowercase, spaces/hyphens to underscores,
/// strip non-alphanumeric/underscore, collapse consecutive underscores, trim trailing underscores)
pub fn normalize_group_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else if c == ' ' || c == '-' {
                '_'
            } else {
                '\0' // strip
            }
        })
        .filter(|c| *c != '\0')
        .collect();

    // Collapse consecutive underscores and trim leading/trailing underscores
    let mut result = String::new();
    let mut prev_underscore = true; // treat start as underscore to trim leading
    for c in s.chars() {
        if c == '_' {
            if !prev_underscore {
                result.push('_');
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
            result.push(c);
        }
    }
    // Trim trailing underscore
    if result.ends_with('_') {
        result.pop();
    }
    result
}

/// Validate that a normalized group name is a valid identifier
pub fn validate_group_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Group name cannot be empty".into());
    }
    if name == "all" {
        return Err("Group name cannot be 'all' (reserved keyword)".into());
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return Err("Group name must start with a lowercase letter".into()),
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' {
            return Err(format!("Group name contains invalid character: '{}'", c));
        }
    }
    Ok(())
}

/// A fixture group within a venue
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct FixtureGroup {
    #[ts(type = "number")]
    pub id: i64,
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    #[ts(type = "number")]
    pub venue_id: i64,
    pub name: Option<String>,
    /// Left (-1) to Right (+1) axis position
    pub axis_lr: Option<f64>,
    /// Front (-1) to Back (+1) axis position
    pub axis_fb: Option<f64>,
    /// Below (-1) to Above (+1) axis position
    pub axis_ab: Option<f64>,
    /// Movement pyramid config (only relevant for groups with movers)
    pub movement_config: Option<MovementConfig>,
    pub display_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Hierarchy node for displaying groups in the UI
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct FixtureGroupNode {
    #[ts(type = "number")]
    pub group_id: i64,
    pub group_name: Option<String>,
    pub fixture_type: FixtureType,
    pub axis_lr: Option<f64>,
    pub axis_fb: Option<f64>,
    pub axis_ab: Option<f64>,
    pub movement_config: Option<MovementConfig>,
    pub fixtures: Vec<GroupedFixtureNode>,
}

/// A fixture within a group hierarchy
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct GroupedFixtureNode {
    pub id: String,
    pub label: String,
    pub fixture_type: FixtureType,
    pub heads: Vec<HeadNode>,
}

/// A head within a fixture
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct HeadNode {
    /// Format: "fixtureId:headIndex"
    pub id: String,
    pub label: String,
}
