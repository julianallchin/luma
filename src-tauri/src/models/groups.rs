use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

use super::fixtures::{ChannelType, FixtureDefinition, Mode};

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

/// A fixture group within a venue
#[derive(Debug, Serialize, Deserialize, Clone, TS, FromRow)]
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
    pub display_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A group with its computed fixture type
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct GroupWithType {
    pub group: FixtureGroup,
    pub fixture_type: FixtureType,
    pub fixture_count: usize,
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

// =============================================================================
// Selection Query Types
// =============================================================================

/// Axis for spatial filtering
#[derive(Debug, Serialize, Deserialize, Clone, TS, PartialEq)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// Left-Right axis
    Lr,
    /// Front-Back axis
    Fb,
    /// Above-Below axis
    Ab,
    /// Axis with largest fixture spread
    MajorAxis,
    /// Axis with smallest fixture spread
    MinorAxis,
    /// Any axis that has fixtures on both sides
    AnyOpposing,
}

/// Position predicate for spatial filtering
#[derive(Debug, Serialize, Deserialize, Clone, TS, PartialEq)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "snake_case")]
pub enum AxisPosition {
    /// Positive side of axis (right, back, above)
    Positive,
    /// Negative side of axis (left, front, below)
    Negative,
    /// Both sides (for alternating effects)
    Both,
    /// Near center of axis
    Center,
}

/// Type filter with XOR and fallback logic
#[derive(Debug, Serialize, Deserialize, Clone, TS, Default)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct TypeFilter {
    /// Types to randomly choose between (XOR)
    pub xor: Vec<FixtureType>,
    /// Fallback types if XOR options not available
    pub fallback: Vec<FixtureType>,
}

/// Spatial filter for selection
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct SpatialFilter {
    pub axis: Axis,
    pub position: AxisPosition,
}

/// Amount specifier for selection
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "snake_case", tag = "mode", content = "value")]
pub enum AmountFilter {
    Percent(f64),
    Count(usize),
    EveryOther,
    All,
}

impl Default for AmountFilter {
    fn default() -> Self {
        AmountFilter::All
    }
}

/// Complete selection query
#[derive(Debug, Serialize, Deserialize, Clone, TS, Default)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct SelectionQuery {
    pub type_filter: Option<TypeFilter>,
    pub spatial_filter: Option<SpatialFilter>,
    pub amount: Option<AmountFilter>,
}

/// Selection configuration for the select node
/// Supports both legacy ID-based and new query-based selection
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum SelectionConfig {
    /// Legacy: select by explicit fixture/head IDs
    #[serde(rename = "ids")]
    Ids { ids: Vec<String> },
    /// New: select by query
    #[serde(rename = "query")]
    Query(SelectionQuery),
}

impl Default for SelectionConfig {
    fn default() -> Self {
        SelectionConfig::Ids { ids: vec![] }
    }
}
