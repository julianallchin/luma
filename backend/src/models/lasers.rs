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

impl LaserZone {
    /// The name trimmed, when `name`, `turn` and `tilt` make a zone: a name
    /// that is not empty, and each range in degrees, min below max, turn
    /// within a half turn either way and tilt within a quarter.
    ///
    /// # Errors
    /// Says which rule failed.
    pub fn check<'a>(name: &'a str, turn: [f64; 2], tilt: [f64; 2]) -> Result<&'a str, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("A zone name cannot be empty".into());
        }
        let span = |[min, max]: [f64; 2], reach: f64| {
            min.is_finite() && max.is_finite() && -reach <= min && min < max && max <= reach
        };
        if !span(turn, 180.) || !span(tilt, 90.) {
            return Err(format!(
                "A zone turns between -180° and 180° and tilts between -90° and 90°, each min below max; got turn {turn:?} and tilt {tilt:?}"
            ));
        }
        Ok(name)
    }
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
