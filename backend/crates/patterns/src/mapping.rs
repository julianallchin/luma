use serde::{Deserialize, Serialize};

/// Host-resolved cell geometry. U/V/Z is supplied by the venue's existing
/// coordinate convention; it is not silently renamed world X/Y/Z here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub id: String,
    pub group: String,
    pub world: [f64; 3],
    pub uvz: [f64; 3],
}

/// The fixture part of a head identity (`fixture:head`).
pub(crate) fn fixture_of(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(fixture, _)| fixture)
}

/// The head number of a head identity (`fixture:3`), when it has one.
pub(crate) fn head_of(id: &str) -> Option<u64> {
    id.rsplit_once(':').and_then(|(_, head)| head.parse().ok())
}

impl Cell {
    /// Venue coordinates are X stage right, Y upstage, Z up. The authored
    /// stage convention is U right, V downstage, Z up, independent of rig fits.
    pub fn stage_coordinates(world: [f64; 3]) -> [f64; 3] {
        [world[0], -world[1], world[2]]
    }
}
