//! Laser projectors (`docs/design/lasers.md`): where a projector sits and
//! which way it scans, what it is ([`Settings`]), the window of directions
//! a venue's laser clips draw in, and the solver that turns the composited
//! elements into each projector's point stream.
//!
//! ```text
//! UniverseState → LaserRig::solve (window, scan plan, limit) → LaserFrame
//!                                                               ├→ visualizer
//!                                                               └→ output (a DAC)
//! ```
//!
//! The visualizer draws the same frames a DAC plays, so what the screen
//! shows is what the projector is sent.
mod scan;
mod settings;

pub use scan::{blank_step, LaserFrame, LaserPoint, FLICKER_RATE, FRAME_RATE};
pub use settings::{Key, Preset, Settings};

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::models::universe::UniverseState;
use fixture_kinematics::Mount;
use glam::DVec3;
use luma_scene::venue::{NodeKind, Params, ResolvedVenue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// How far apart a projector's first and last element sit, in metres,
/// across its scan. A Space or Mirror node reads heads by where they are, so
/// a projector's elements spread a little either side of it: enough for a
/// gradient to run through them, little enough that the projector still
/// sits where it hangs.
pub const HEAD_SPREAD_M: f64 = 0.5;

/// The lowest edge of the window a venue draws in until it sets one, in
/// degrees above level: over the crowd's heads.
const DEFAULT_TILT: f64 = 5.;
/// How much of the narrowest projector's half scan that window spans either
/// side of the house: the canvas's edges stay well inside every scan, so a
/// fan of size 1 neither fires out sideways nor loses its outer beams at the
/// scan's edge.
const DEFAULT_REACH: f64 = 2. / 3.;
/// That window with no projector to fit it to: a 60° scan's.
const FALLBACK_WINDOW: Window = Window {
    turn: [-20., 20.],
    tilt: [DEFAULT_TILT, DEFAULT_TILT + 20.],
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
    /// normal, the way a parked head emits, from its aperture, `aperture`
    /// metres ahead of the mount along that normal.
    pub fn of(mount: &Mount, aperture: f64) -> Self {
        let forward = mount.normal().as_dvec3().normalize();
        let right = DVec3::Z.cross(forward);
        let right = if right.length_squared() > 1e-9 {
            right.normalize()
        } else {
            // Straight up or down: the mount's own X is as good as any.
            DVec3::X
        };
        Basis {
            origin: mount.position().as_dvec3() + forward * aperture,
            forward,
            right,
            up: forward.cross(right),
        }
    }

    /// Where element `index` of `count` sits: spread across the scan around
    /// the projector ([`HEAD_SPREAD_M`]).
    pub fn head(&self, index: usize, count: usize) -> DVec3 {
        let s = luma_patterns::laser::order(index, count);
        self.origin + self.right * ((s - 0.5) * HEAD_SPREAD_M)
    }
}

/// A projector as the solver sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Projector {
    /// Its fixture's id.
    pub id: String,
    pub basis: Basis,
    /// What it is: the venue's kind of projector, or its own, with how its
    /// own picture is trimmed.
    pub settings: Settings,
    /// Whether it holds a kind of its own instead of following the venue's.
    #[serde(default)]
    pub own: bool,
    /// Where it may send light, in galvo coordinates; `None` anywhere.
    pub limit: Option<Vec<[f64; 2]>>,
}

impl Projector {
    /// Galvo coordinates, −1 to 1 across the scan, of the direction `d` in
    /// data space; `None` when the scanner cannot point there.
    ///
    /// The mirrors sit one after the other: the first turns the beam across,
    /// the second tips that whole fan up or down. So the angle up is about
    /// the scan's level axis, and the angle across is measured in the tipped
    /// plane, which is what bows a straight line toward the scan's corners.
    pub fn galvo(&self, d: DVec3) -> Option<[f64; 2]> {
        let [across, up] = self.galvo_of(d)?;
        // A direction on the scan's edge is in it, rounding either way.
        let edge = 1. + 1e-9;
        (across.abs() <= edge && up.abs() <= edge)
            .then_some([across.clamp(-1., 1.), up.clamp(-1., 1.)])
    }

    /// [`Self::galvo`] for any direction ahead of the projector, in its scan
    /// or past its edge: where the direction falls in the scan's own
    /// coordinates. `None` behind it.
    pub fn galvo_of(&self, d: DVec3) -> Option<[f64; 2]> {
        let ahead = d.dot(self.basis.forward);
        if ahead <= 1e-9 {
            return None;
        }
        let (right, up) = (d.dot(self.basis.right), d.dot(self.basis.up));
        let reach = self.settings.reach();
        Some([
            right.atan2(ahead.hypot(up)).to_degrees() / reach,
            up.atan2(ahead).to_degrees() / reach,
        ])
    }

    /// The unit direction in data space of galvo coordinates `g`: the
    /// inverse of [`Projector::galvo`].
    pub fn direction(&self, g: [f64; 2]) -> DVec3 {
        let reach = self.settings.reach();
        let (across, up) = ((g[0] * reach).to_radians(), (g[1] * reach).to_radians());
        self.basis.forward * (across.cos() * up.cos())
            + self.basis.right * across.sin()
            + self.basis.up * (across.cos() * up.sin())
    }
}

/// The window of directions a venue's laser clips draw in, in degrees. Every
/// projector shares it: a clip's canvas runs from −1 to 1 across `turn` and
/// up `tilt`, so its middle is the same direction from each projector and
/// six projectors give six parallel fans.
///
/// A venue stores it as four parameters of its root node; one that has not
/// set it draws in [`LaserRig::default_window`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Round the vertical: 0 toward the house, positive toward stage right.
    pub turn: [f64; 2],
    /// From level, positive up.
    pub tilt: [f64; 2],
}

impl Window {
    /// The root node's parameters a window is stored under: turn's and
    /// tilt's lower and upper edge.
    pub const PARAMS: [&str; 4] = [
        "laser_turn_min",
        "laser_turn_max",
        "laser_tilt_min",
        "laser_tilt_max",
    ];

    /// The window `params` holds, when they hold a whole one.
    #[must_use]
    pub fn read(params: &Params) -> Option<Self> {
        let [turn_min, turn_max, tilt_min, tilt_max] = Self::PARAMS.map(|key| params.lookup(key));
        let window = Window {
            turn: [turn_min?, turn_max?],
            tilt: [tilt_min?, tilt_max?],
        };
        window.check().is_ok().then_some(window)
    }

    /// Its four parameters, in [`Self::PARAMS`] order.
    #[must_use]
    pub fn params(&self) -> [(&'static str, f64); 4] {
        let values = [self.turn[0], self.turn[1], self.tilt[0], self.tilt[1]];
        std::array::from_fn(|i| (Self::PARAMS[i], values[i]))
    }

    /// Whether this is a window: each range in degrees, lower edge below
    /// upper, turn within a half turn either way and tilt within a quarter.
    ///
    /// # Errors
    /// Says what a window is and what this one was.
    pub fn check(&self) -> Result<(), String> {
        let span = |[min, max]: [f64; 2], reach: f64| {
            min.is_finite() && max.is_finite() && -reach <= min && min < max && max <= reach
        };
        if span(self.turn, 180.) && span(self.tilt, 90.) {
            return Ok(());
        }
        Err(format!(
            "A laser window turns between -180° and 180° and tilts between -90° and 90°, each lower edge below its upper; got turn {:?} and tilt {:?}",
            self.turn, self.tilt
        ))
    }

    /// The turn and tilt, in degrees, of direction `d` in data space: where
    /// it sits among the directions a window is a range of.
    #[must_use]
    pub fn angles(d: DVec3) -> [f64; 2] {
        let d = d.normalize_or_zero();
        [
            d.x.atan2(-d.y).to_degrees(),
            d.z.clamp(-1., 1.).asin().to_degrees(),
        ]
    }

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

/// Every projector in a venue and the window they draw in: what the solver
/// needs to turn laser elements into point streams.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LaserRig {
    pub projectors: Vec<Projector>,
    /// The venue's window; `None` until it sets one.
    pub window: Option<Window>,
    /// The kind of projector the venue's projectors are, unless one holds
    /// its own: most rigs hang several of one model.
    #[serde(default)]
    pub shared: Settings,
}

impl LaserRig {
    /// The venue's projectors, posed and set up as it has them, with their
    /// limits, and its window.
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
                    crate::fixtures::definition(fixtures_root, &fixture.fixture_path).ok()
                });
        }
        let any = definitions
            .values()
            .flatten()
            .any(|definition| definition.is_laser_projector());
        let venue = if any {
            crate::venue_graph::resolved(access, fixtures_root).await?
        } else {
            ResolvedVenue::default()
        };
        Self::read(
            access,
            &patch,
            |path| definitions.get(path)?.as_deref(),
            &venue,
        )
        .await
    }

    /// [`Self::load`] for a caller that already holds the patch, its
    /// definitions by path and the solved venue: reads only the limits.
    ///
    /// # Errors
    /// Fails when the limits cannot be read.
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
        let root = venue.poses().find(|pose| pose.kind == NodeKind::Venue);
        let shared = root.map_or_else(Settings::default, |root| Settings::shared(&root.params));
        let projectors = patch
            .iter()
            .filter_map(|fixture| {
                let definition = definition(&fixture.fixture_path)
                    .filter(|definition| definition.is_laser_projector())?;
                let aperture = luma_scene::definition::Definition::from(definition).aperture_m();
                let pose = venue.pose(&fixture.id)?;
                Some(Projector {
                    id: fixture.id.clone(),
                    basis: Basis::of(
                        &crate::fixtures::layout::fixture_mount(pose),
                        f64::from(aperture),
                    ),
                    settings: shared.with(&pose.params),
                    own: Settings::has_kind(&pose.params),
                    limit: limits.get(&fixture.id).cloned(),
                })
            })
            .collect();
        Ok(LaserRig {
            projectors,
            window: root.and_then(|root| Window::read(&root.params)),
            shared,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.projectors.is_empty()
    }

    pub fn projector(&self, id: &str) -> Option<&Projector> {
        self.projectors.iter().find(|p| p.id == id)
    }

    /// The window clips draw in: the venue's, or
    /// [`LaserRig::default_window`] until it sets one.
    pub fn window(&self) -> Window {
        self.window.unwrap_or_else(|| self.default_window())
    }

    /// The window of a venue that has not set one: fixed in the room, ahead
    /// into the house from just above level, and [`DEFAULT_REACH`] of the
    /// narrowest projector's half scan either side and up, so it sits well
    /// inside a level projector's scan.
    pub fn default_window(&self) -> Window {
        self.projectors
            .iter()
            .map(|p| p.settings.reach())
            .reduce(f64::min)
            .map_or(FALLBACK_WINDOW, |reach| {
                let reach = reach * DEFAULT_REACH;
                Window {
                    turn: [-reach, reach],
                    tilt: [DEFAULT_TILT, DEFAULT_TILT + reach],
                }
            })
    }

    /// Each projector's point stream for `state`: one scan of its elements,
    /// to play on repeat until the next frame. A projector with nothing to
    /// draw gives no frame.
    pub fn solve(&self, state: &UniverseState) -> Vec<LaserFrame> {
        let window = self.window();
        self.projectors
            .iter()
            .filter_map(|projector| scan::frame(window, projector, state))
            .collect()
    }

    /// [`Self::solve`] while projector `testing` shows its test pattern in
    /// place of what it would play: on a DAC and on screen alike.
    pub fn solve_testing(&self, state: &UniverseState, testing: Option<&str>) -> Vec<LaserFrame> {
        let Some(testing) = testing else {
            return self.solve(state);
        };
        let mut frames = self.solve(&test_pattern(testing));
        frames.extend(
            self.solve(state)
                .into_iter()
                .filter(|frame| frame.projector != testing),
        );
        frames
    }
}

/// How bright the test pattern is, of the projector's capped power: enough
/// to see where the window falls, with the room's lights up.
pub const TEST_LEVEL: f64 = 0.25;

/// What a projector draws to show where it points: the edge of the venue's
/// window, and a cross at its middle.
fn test_pattern(projector: &str) -> UniverseState {
    use luma_patterns::laser::{frame as turned, Element, Outline, Sample};
    let element = |outline, size: f64, rotation: f64, s: f64| Element {
        projector: projector.to_owned(),
        layer: 0,
        outline,
        s,
        samples: vec![Sample {
            at: [0., 0.],
            frame: turned(size, rotation),
            rgb: [1.; 3],
        }],
        intensity: TEST_LEVEL,
    };
    UniverseState {
        lasers: vec![
            // A square's corners are √2 from its middle.
            element(Outline::Polygon(4), 2f64.sqrt(), 45., 0.),
            element(Outline::Line, 0.15, 0., 0.5),
            element(Outline::Line, 0.15, 90., 1.),
        ],
        ..UniverseState::default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use glam::{Mat3, Vec3};

    /// A projector on the stage floor at U `x`, scanning out into the house.
    pub(crate) fn facing_house(id: &str, x: f32) -> Projector {
        // The mount normal is the frame's −Z: turn it to point along −Y.
        let rotation = Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let mount = Mount::from_frame(Vec3::new(x, 0., 1.), rotation);
        Projector {
            id: id.into(),
            basis: Basis::of(&mount, 0.),
            settings: Settings::default(),
            own: false,
            limit: None,
        }
    }

    /// The scan leaves from the aperture, ahead of where the box is hung,
    /// along the way it faces.
    #[test]
    fn a_scan_leaves_from_the_aperture() {
        let rotation = Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let mount = Mount::from_frame(Vec3::new(1., 2., 3.), rotation);
        let (hung, aperture) = (Basis::of(&mount, 0.), Basis::of(&mount, 0.25));
        assert_eq!(aperture.forward, hung.forward);
        assert!(aperture
            .origin
            .abs_diff_eq(hung.origin + hung.forward * 0.25, 1e-9));
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
        for g in [[0., 0.], [0.5, -0.25], [-0.9, 0.8], [1., 1.]] {
            let back = p.galvo(p.direction(g)).unwrap();
            assert!((back[0] - g[0]).abs() < 1e-9 && (back[1] - g[1]).abs() < 1e-9);
        }
        assert_eq!(p.galvo(DVec3::Y), None, "behind the projector");
        assert_eq!(p.galvo(DVec3::new(1., -0.1, 0.)), None, "outside the scan");
    }

    /// The second mirror tips the first one's fan, so a level line across
    /// the room, off the scan's middle, takes a different mirror angle up at
    /// its ends than in its middle.
    #[test]
    fn a_level_line_bows_toward_the_scans_corners() {
        let p = facing_house("p", 0.);
        let level = |turn: f64| {
            let (turn, tilt) = (turn.to_radians(), 20f64.to_radians());
            DVec3::new(
                turn.sin() * tilt.cos(),
                -turn.cos() * tilt.cos(),
                tilt.sin(),
            )
        };
        let middle = p.galvo(level(0.)).unwrap();
        let end = p.galvo(level(25.)).unwrap();
        assert!(end[1] > middle[1] + 0.01, "{middle:?} {end:?}");
    }

    /// The canvas middle is the same direction from every projector: six
    /// projectors drawing a fan's middle beam point parallel.
    #[test]
    fn the_window_is_the_same_from_every_projector() {
        let window = Window {
            turn: [-20., 20.],
            tilt: [0., 20.],
        };
        let middle = window.direction(0., 0.);
        let expected = DVec3::new(0., -(10f64.to_radians().cos()), 10f64.to_radians().sin());
        assert!(middle.abs_diff_eq(expected, 1e-12));
        let a = facing_house("a", -4.).galvo(middle).unwrap();
        let b = facing_house("b", 4.).galvo(middle).unwrap();
        assert_eq!(a, b);
        // Canvas right is stage right, canvas up is up.
        assert!(window.direction(1., 0.).x > 0. && window.direction(0., 1.).z > middle.z);
    }

    #[test]
    fn a_window_is_stored_as_four_parameters_of_the_venues_root() {
        let window = Window {
            turn: [-30., 30.],
            tilt: [5., 25.],
        };
        let params: Params = window
            .params()
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        assert_eq!(Window::read(&params), Some(window));
        assert_eq!(Window::read(&Params::default()), None);
        // Upside down is no window.
        let mut flipped = params.clone();
        flipped.set(Window::PARAMS[2], 40.);
        assert_eq!(Window::read(&flipped), None);
    }

    #[test]
    fn a_projectors_heads_spread_across_its_scan() {
        let p = facing_house("p", 2.);
        let first = p.basis.head(0, 16);
        let last = p.basis.head(15, 16);
        assert!((last.x - first.x - HEAD_SPREAD_M).abs() < 1e-6);
        assert!((p.basis.head(0, 1) - p.basis.origin).length() < 1e-9);
    }

    /// Until a venue sets its window, the canvas's corners are ahead into
    /// the house and inside a level projector's scan, so a fan of size 1
    /// keeps its outer beams; the canvas middle is ahead of the house from
    /// every projector.
    #[test]
    fn a_venue_with_no_window_draws_ahead_of_the_house_inside_every_scan() {
        let rig = LaserRig {
            projectors: vec![facing_house("a", -2.), facing_house("b", 2.)],
            window: None,
            shared: Settings::default(),
        };
        let window = rig.window();
        for p in &rig.projectors {
            for corner in [[-1., -1.], [-1., 1.], [1., -1.], [1., 1.]] {
                let d = window.direction(corner[0], corner[1]);
                let g = p.galvo(d).expect("the canvas corner is in the scan");
                assert!(g[0].abs() < 0.9 && g[1].abs() < 0.9, "{g:?}");
                assert!(d.y < 0. && d.z > 0., "toward the house and up: {d}");
            }
        }
        let middle = window.direction(0., 0.);
        assert!(middle.x.abs() < 1e-12 && middle.y < 0. && middle.z > 0.);
    }

    /// A direction exactly on the scan's edge is in the scan, either side.
    #[test]
    fn the_scans_edge_is_in_the_scan() {
        let p = facing_house("p", 0.);
        assert!(p.galvo(p.direction([-1., 0.])).is_some());
        assert!(p.galvo(p.direction([1., 1.])).is_some());
    }
}
