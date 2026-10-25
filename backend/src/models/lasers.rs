use serde::{Deserialize, Serialize};

/// A named window of directions in the room that laser clips draw in
/// (`docs/design/lasers.md`). Every projector shares it: a clip's canvas
/// runs from −1 to 1 across `turn` and up `tilt`, so its middle is the same
/// direction from each projector.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LaserZone {
    pub id: String,
    pub venue_id: String,
    pub name: String,
    /// Degrees round the vertical: 0 toward the house, positive toward
    /// stage right. `[min, max]`, min below max.
    pub turn: [f64; 2],
    /// Degrees from level: positive up. `[min, max]`, min below max.
    pub tilt: [f64; 2],
}

/// Where one projector may send light: a polygon in its own view, galvo
/// coordinates −1 to 1 across and up its scan field. Light outside it is
/// blanked.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LaserLimit {
    pub fixture_id: String,
    pub points: Vec<[f64; 2]>,
}
