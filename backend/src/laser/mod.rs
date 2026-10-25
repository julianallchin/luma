//! Laser projectors (`docs/design/lasers.md`): where a projector sits and
//! which way it scans, a zone's window of directions, and the solver that
//! turns the composited beams into each projector's point stream.
//!
//! ```text
//! UniverseState → LaserRig::solve (zone, scan plan, limit) → LaserFrame
//!                                                             ├→ visualizer
//!                                                             └→ DAC driver
//! ```
//!
//! The visualizer draws the same frames a DAC would play, so what the
//! screen shows is what the projector would send.
mod scan;

pub use scan::{LaserFrame, LaserPoint};

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::models::lasers::LaserZone;
use crate::models::universe::UniverseState;
use fixture_kinematics::Mount;
use glam::DVec3;
use luma_scene::venue::ResolvedVenue;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// How many colour heads a projector gives a Color, Strobe or Aim clip: the
/// colour along a laser clip's order, sampled. A beam takes the colour at
/// its own place `s`, so a gradient never depends on the beam count.
pub const COLOR_SAMPLES: usize = 32;

/// How far apart a projector's first and last head sit, in metres, across
/// its scan. A Space or Mirror node reads heads by where they are, so a
/// projector's heads spread a little either side of it: enough for a
/// gradient to run through its beams, little enough that the projector still
/// sits where it hangs.
pub const HEAD_SPREAD_M: f64 = 0.5;

/// How many points a second a projector draws: its scan rate.
pub const POINT_RATE: u32 = 30_000;

/// The window a laser clip draws in when the venue has no zone: ahead into
/// the house, a little above level.
pub const DEFAULT_ZONE: Zone = Zone {
    turn: [-30., 30.],
    tilt: [5., 25.],
};

/// A projector's scan frame in data space (X stage right, Y upstage, Z up):
/// where its light leaves, the direction its middle points, and the two axes
/// its mirrors move along. The mirrors are taken to sit level: `right` is
/// horizontal, `up` above it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "[[f64; 3]; 4]", into = "[[f64; 3]; 4]")]
pub struct Basis {
    pub origin: DVec3,
    pub forward: DVec3,
    pub right: DVec3,
    pub up: DVec3,
}

/// Origin, forward, right and up: glam's vectors carry no serde here.
impl From<[[f64; 3]; 4]> for Basis {
    fn from([origin, forward, right, up]: [[f64; 3]; 4]) -> Self {
        Basis {
            origin: origin.into(),
            forward: forward.into(),
            right: right.into(),
            up: up.into(),
        }
    }
}

impl From<Basis> for [[f64; 3]; 4] {
    fn from(basis: Basis) -> Self {
        [basis.origin, basis.forward, basis.right, basis.up].map(Into::into)
    }
}

impl Basis {
    /// The frame of a projector bolted at `mount`: it scans around the mount
    /// normal, the way a parked head emits.
    pub fn of(mount: &Mount) -> Self {
        let forward = mount.normal().as_dvec3().normalize();
        let right = DVec3::Z.cross(forward);
        let right = if right.length_squared() > 1e-9 {
            right.normalize()
        } else {
            // Straight up or down: the mount's own X is as good as any.
            DVec3::X
        };
        Basis {
            origin: mount.position().as_dvec3(),
            forward,
            right,
            up: forward.cross(right),
        }
    }

    /// Where head `index` of `count` sits: spread across the scan around the
    /// projector ([`HEAD_SPREAD_M`]).
    pub fn head(&self, index: usize, count: usize) -> DVec3 {
        let s = luma_patterns::laser::along(index, count);
        self.origin + self.right * ((s - 0.5) * HEAD_SPREAD_M)
    }
}

/// A projector as the solver sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Projector {
    pub id: String,
    pub basis: Basis,
    /// The full scan angle across and up, in degrees.
    pub field: [f64; 2],
    /// Points a second.
    pub rate: u32,
    /// Where it may send light, in galvo coordinates; `None` anywhere.
    pub limit: Option<Vec<[f64; 2]>>,
}

impl Projector {
    /// Galvo coordinates, −1 to 1 across the field, of the direction `d` in
    /// data space; `None` when the scanner cannot point there.
    pub fn galvo(&self, d: DVec3) -> Option<[f64; 2]> {
        let ahead = d.dot(self.basis.forward);
        if ahead <= 1e-9 {
            return None;
        }
        let across = d.dot(self.basis.right).atan2(ahead).to_degrees() / (self.field[0] / 2.);
        let up = d.dot(self.basis.up).atan2(ahead).to_degrees() / (self.field[1] / 2.);
        (across.abs() <= 1. && up.abs() <= 1.).then_some([across, up])
    }

    /// The unit direction in data space of galvo coordinates `g`: the
    /// inverse of [`Projector::galvo`].
    pub fn direction(&self, g: [f64; 2]) -> DVec3 {
        let tan = |c: f64, field: f64| (c * field / 2.).to_radians().tan();
        (self.basis.forward
            + self.basis.right * tan(g[0], self.field[0])
            + self.basis.up * tan(g[1], self.field[1]))
        .normalize()
    }
}

/// A zone's window of directions, in degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    /// Round the vertical: 0 toward the house, positive toward stage right.
    pub turn: [f64; 2],
    /// From level, positive up.
    pub tilt: [f64; 2],
}

impl Zone {
    /// The direction in data space of canvas point `(x, y)`, each −1 to 1.
    pub fn direction(&self, x: f64, y: f64) -> DVec3 {
        let at = |[min, max]: [f64; 2], c: f64| (min + max) / 2. + c * (max - min) / 2.;
        let (turn, tilt) = (at(self.turn, x).to_radians(), at(self.tilt, y).to_radians());
        DVec3::new(
            turn.sin() * tilt.cos(),
            -turn.cos() * tilt.cos(),
            tilt.sin(),
        )
    }
}

impl From<&LaserZone> for Zone {
    fn from(zone: &LaserZone) -> Self {
        Zone {
            turn: zone.turn,
            tilt: zone.tilt,
        }
    }
}

/// Every projector in a venue and its zones: what the solver needs to turn
/// beams into point streams.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LaserRig {
    pub projectors: Vec<Projector>,
    /// By id, in the venue's order; the first is where a clip with no zone
    /// draws.
    pub zones: Vec<(String, Zone)>,
}

impl LaserRig {
    /// The venue's projectors, posed as it places them, with their limits,
    /// and its zones.
    ///
    /// # Errors
    /// Fails when the venue cannot be solved or its rows cannot be read. A
    /// projector the solve cannot reach is left out.
    pub async fn load(
        access: &mut impl AuthorizedVenue,
        fixtures_root: &Path,
    ) -> Result<Self, String> {
        let patch = crate::database::local::fixtures::get_patched_fixtures(access).await?;
        let mut definitions = HashMap::new();
        for fixture in &patch {
            definitions
                .entry(fixture.fixture_path.clone())
                .or_insert_with(|| {
                    crate::fixtures::parser::parse_definition(
                        &fixtures_root.join(&fixture.fixture_path),
                    )
                    .ok()
                });
        }
        let any = definitions
            .values()
            .flatten()
            .any(FixtureDefinition::is_laser_projector);
        let venue = if any {
            crate::venue_graph::resolved(access, fixtures_root).await?
        } else {
            ResolvedVenue::default()
        };
        Self::read(
            access,
            &patch,
            |path| definitions.get(path)?.as_ref(),
            &venue,
        )
        .await
    }

    /// [`Self::load`] for a caller that already holds the patch, its
    /// definitions by path and the solved venue: reads only the limits and
    /// zones.
    ///
    /// # Errors
    /// Fails when the limits or zones cannot be read.
    pub async fn read<'d>(
        access: &mut impl AuthorizedVenue,
        patch: &[PatchedFixture],
        definition: impl Fn(&str) -> Option<&'d FixtureDefinition>,
        venue: &ResolvedVenue,
    ) -> Result<Self, String> {
        let limits: HashMap<String, Vec<[f64; 2]>> =
            crate::database::local::lasers::list_limits(access)
                .await?
                .into_iter()
                .map(|limit| (limit.fixture_id, limit.points))
                .collect();
        let projectors = patch
            .iter()
            .filter_map(|fixture| {
                let definition = definition(&fixture.fixture_path)
                    .filter(|definition| definition.is_laser_projector())?;
                let pose = venue.pose(&fixture.id)?;
                let [across, up] = definition.focus_range();
                Some(Projector {
                    id: fixture.id.clone(),
                    basis: Basis::of(&crate::fixtures::layout::fixture_mount(pose)),
                    field: [f64::from(across), f64::from(up)],
                    rate: POINT_RATE,
                    limit: limits.get(&fixture.id).cloned(),
                })
            })
            .collect();
        let zones = crate::database::local::lasers::list_zones(access)
            .await?
            .iter()
            .map(|zone| (zone.id.clone(), Zone::from(zone)))
            .collect();
        Ok(LaserRig { projectors, zones })
    }

    pub fn is_empty(&self) -> bool {
        self.projectors.is_empty()
    }

    pub fn projector(&self, id: &str) -> Option<&Projector> {
        self.projectors.iter().find(|p| p.id == id)
    }

    /// The zone with id `id`; an empty or unknown id is the venue's first
    /// zone, or [`DEFAULT_ZONE`] when it has none.
    pub fn zone(&self, id: &str) -> Zone {
        self.zones
            .iter()
            .find(|(known, _)| known == id)
            .or_else(|| self.zones.first())
            .map_or(DEFAULT_ZONE, |(_, zone)| *zone)
    }

    /// Each projector's point stream for `state`: one scan of its beams, to
    /// play on repeat until the next frame. A projector with no beams gives
    /// no frame.
    pub fn solve(&self, state: &UniverseState) -> Vec<LaserFrame> {
        self.projectors
            .iter()
            .filter_map(|projector| scan::frame(self, projector, state))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Mat3, Vec3};

    /// A projector on the stage floor at U `x`, scanning out into the house.
    pub(crate) fn facing_house(id: &str, x: f32) -> Projector {
        // The mount normal is the frame's −Z: turn it to point along −Y.
        let rotation = Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let mount = Mount::from_frame(Vec3::new(x, 0., 1.), rotation);
        Projector {
            id: id.into(),
            basis: Basis::of(&mount),
            field: [60., 60.],
            rate: POINT_RATE,
            limit: None,
        }
    }

    #[test]
    fn a_projector_facing_the_house_scans_level_and_upright() {
        let p = facing_house("p", 0.);
        assert!(p.basis.forward.abs_diff_eq(DVec3::NEG_Y, 1e-6), "{p:?}");
        assert!(p.basis.right.abs_diff_eq(DVec3::X, 1e-6));
        assert!(p.basis.up.abs_diff_eq(DVec3::Z, 1e-6));
    }

    #[test]
    fn galvo_and_direction_are_inverse() {
        let p = facing_house("p", 0.);
        for g in [[0., 0.], [0.5, -0.25], [-0.9, 0.8]] {
            let back = p.galvo(p.direction(g)).unwrap();
            assert!((back[0] - g[0]).abs() < 1e-9 && (back[1] - g[1]).abs() < 1e-9);
        }
        assert_eq!(p.galvo(DVec3::Y), None, "behind the projector");
        assert_eq!(p.galvo(DVec3::new(1., -0.1, 0.)), None, "outside the field");
    }

    /// The canvas middle is the same direction from every projector: six
    /// projectors drawing a fan's middle beam point parallel.
    #[test]
    fn a_zone_is_the_same_window_from_every_projector() {
        let zone = Zone {
            turn: [-20., 20.],
            tilt: [0., 20.],
        };
        let middle = zone.direction(0., 0.);
        let expected = DVec3::new(0., -(10f64.to_radians().cos()), 10f64.to_radians().sin());
        assert!(middle.abs_diff_eq(expected, 1e-12));
        let a = facing_house("a", -4.).galvo(middle).unwrap();
        let b = facing_house("b", 4.).galvo(middle).unwrap();
        assert_eq!(a, b);
        // Canvas right is stage right, canvas up is up.
        assert!(zone.direction(1., 0.).x > 0. && zone.direction(0., 1.).z > middle.z);
    }

    #[test]
    fn a_projectors_heads_spread_across_its_scan() {
        let p = facing_house("p", 2.);
        let first = p.basis.head(0, COLOR_SAMPLES);
        let last = p.basis.head(COLOR_SAMPLES - 1, COLOR_SAMPLES);
        assert!((last.x - first.x - HEAD_SPREAD_M).abs() < 1e-6);
        assert!((p.basis.head(0, 1) - p.basis.origin).length() < 1e-9);
    }
}
