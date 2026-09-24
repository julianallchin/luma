use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrimitiveState {
    pub dimmer: f32,        // 0.0 - 1.0
    pub color: [f32; 3],    // RGB [0.0 - 1.0]
    pub strobe: f32,        // 0.0 (off) - 1.0 (fastest)
    pub position: [f32; 2], // [PanDeg, TiltDeg]
    pub speed: f32,         // 0.0 (frozen) or 1.0 (fast) - binary
    /// Where the head points in the room, from `aim@1` clips. Never pan or
    /// tilt: the solver turns it into pan and tilt. `None` is no aim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aim: Option<HeadAim>,
}

/// A head's aim: a unit direction in U (stage right), V (downstage), Z (up),
/// and how much of it the clips set. The head points at
/// `slerp(home, direction, weight)`, with home its pan and tilt at mid-range.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HeadAim {
    pub direction: [f32; 3],
    pub weight: f32,
}
impl HeadAim {
    pub fn from_aim(aim: luma_patterns::Aim) -> Self {
        Self {
            direction: aim.direction.map(|v| v as f32),
            weight: aim.weight as f32,
        }
    }
    pub fn to_aim(self) -> luma_patterns::Aim {
        luma_patterns::Aim {
            direction: self.direction.map(f64::from),
            weight: f64::from(self.weight),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UniverseState {
    // Key: "fixture-uuid" OR "fixture-uuid:head-index"
    pub primitives: HashMap<String, PrimitiveState>,
}
