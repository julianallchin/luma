use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::services::group_derivation::FixtureRole;

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
    pub id: String,
    pub uid: Option<String>,
    pub venue_id: String,
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
    pub group_id: String,
    pub group_name: Option<String>,
    /// The role of the group's first member — what the set is mostly for.
    /// `None` for an empty group.
    pub role: Option<FixtureRole>,
    /// Whether anything in the group aims: a pan or a tilt channel somewhere.
    ///
    /// Not derivable from [`Self::role`] and deliberately beside it: a wash
    /// mover and a par are both [`FixtureRole::Wash`], and only one of them has
    /// a movement pyramid to configure.
    pub moves: bool,
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
    pub role: FixtureRole,
    /// Whether this fixture aims — see [`FixtureGroupNode::moves`].
    pub moves: bool,
    /// Heads of this fixture that belong to the group — all of them for
    /// whole-fixture membership. Empty for fixtures whose mode defines no heads.
    pub heads: Vec<HeadNode>,
    /// Total number of heads the fixture's mode defines (0 if none).
    /// `heads.len() < head_count` ⇒ only part of the fixture is in the group.
    pub head_count: i64,
}

/// A head within a fixture
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct HeadNode {
    /// Format: "fixtureId:headIndex"
    pub id: String,
    pub label: String,
    pub head_index: i64,
    /// World position in meters (Z-up data space), for visualizer bounds.
    pub position: [f32; 3],
}

/// Where a node of the group tree came from.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "snake_case")]
pub enum GroupOrigin {
    /// The rule produced it and nobody has touched it. It re-derives on every
    /// read, so it tracks the rig.
    Derived,
    /// The rule produced it and somebody renamed, moved or merged it. Frozen:
    /// a touched node is never re-derived. Delete the override to get
    /// [`GroupOrigin::Derived`] back.
    Edited,
    /// A `fixture_groups` row someone created outright.
    Manual,
}

/// One node of the group tree — the merged answer: derivation, with overrides
/// on top, plus whatever was authored by hand.
///
/// Flat with a `parent_id` rather than a recursive type, for the same reason
/// [`crate::models::venue_graph::ResolvedVenue`] is: parents come before
/// children, so a consumer can build the tree in one pass, and `ts-rs` can
/// name the type.
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "camelCase")]
pub struct GroupTreeNode {
    pub id: String,
    /// The snake_case name a selection expression uses. Unique in the venue.
    pub name: String,
    /// What the tree shows — one path segment, not the whole path.
    pub label: String,
    pub parent_id: Option<String>,
    pub origin: GroupOrigin,
    /// The role branch this sits under; `None` for an authored group.
    pub role: Option<FixtureRole>,
    /// Fixture ids, in creation order.
    pub fixtures: Vec<String>,
}
