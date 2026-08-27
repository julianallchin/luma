//! Where a fixture's light comes from, and where it points.
//!
//! One answer to two questions that used to have three drifting answers
//! (`fixtures::layout::head_world_position` in millimetres, the renderer's
//! `pixel_positions` in three.js space, and a partial `layout_of` beside it):
//!
//! - [`rig_position`] — the **static** point a fixture cell occupies in the rig.
//!   This is what pattern space, spatial selection and the eval engine mean by
//!   "where is this primitive". It takes no [`Articulation`] *by construction*,
//!   so no caller downstream of it can accidentally make pattern space depend on
//!   where a head happens to be pointing this frame.
//! - [`beam_ray`] — the **animated** ray a lit cell actually emits, for the
//!   renderer's cones, lights and shadows.
//!
//! # Space
//!
//! Everything here is the stored data space: **Z-up, right-handed, metres**,
//! `+X` stage right, `+Y` upstage/back, `+Z` up. A fixture at rest points along
//! [`Vec3::NEG_Z`] — straight down.
//!
//! The renderer works in a three.js-derived Y-up space that reaches data space
//! through `S = swap(Y, Z)`, and `det(S) = -1`. That mirror is why the rotation
//! signs below look asymmetric: conjugating a rotation by a reflection negates
//! its angle, and swapping two axes also swaps which stored Euler term drives
//! which axis. It is not a fudge factor, and the signs are pinned by
//! `CONTRACT_VECTORS` and by the tests in `tests/`.
//!
//! # The chain
//!
//! ```text
//! origin = mount.position
//!        + rot_data · (pivot_offset + articulation · (aperture_offset + cell))
//!
//! rot_data     = Rx(-rot[0]) · Rz(-rot[2]) · Ry(-rot[1])
//! articulation = Rz(-pan) · Rx(+tilt)
//! ```
//!
//! `rot` is the fixture's stored Euler triple; `pan`/`tilt` are the head's
//! articulation. Pan negates in data space for the mirror reason above.
//!
//! ```
//! use fixture_kinematics::{beam_ray, rig_position, Articulation, FixtureGeometry, Mount};
//! use glam::Vec3;
//!
//! let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]);
//! let mount = Mount::new(Vec3::new(0.0, 0.0, 4.0), [0.0; 3]);
//!
//! // A fixture at rest emits from its rig position, straight down.
//! let ray = beam_ray(&geom, &mount, &Articulation::REST, 0);
//! assert_eq!(ray.origin, rig_position(&geom, &mount, 0));
//! assert_eq!(ray.direction, Vec3::NEG_Z);
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use glam::{Mat3, Vec3};

/// The emission axis of a fixture with no articulation applied: straight down.
///
/// Public because a caller that needs "which way would this fixture point if it
/// were parked" should read it from here rather than re-deriving a sign.
pub const REST_AXIS: Vec3 = Vec3::NEG_Z;

/// A fixture's optical class, which is the only thing that sets how far the
/// aperture sits from the pivot.
///
/// This is *not* the renderer's `ModelKind` (which picks a mesh) and not QLC+'s
/// `Type` string (which is free text). When the defaults here are switched on,
/// the mapping from a definition to a class must be written once, in this crate,
/// and `ModelKind` lowered onto it — not a third classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FixtureClass {
    /// Flat LED panel, bar or matrix: the emitters *are* the surface.
    Panel,
    /// Wide fixed or zoom wash.
    Wash,
    /// Narrow-angle beam.
    Beam,
    /// Spot with a gobo train.
    Spot,
    /// Profile with framing shutters — the deepest optical train.
    Profile,
}

impl FixtureClass {
    /// Measured distance, in metres, from the head's pivot down to the front of
    /// the lens.
    ///
    /// These are the numbers that make a beam start at the glass instead of
    /// inside the housing. They are **not** switched on anywhere yet:
    /// [`FixtureGeometry::unauthored`] is what current callers build, and moving
    /// a caller onto [`FixtureGeometry::from_class`] moves every beam origin, so
    /// it is a deliberate golden-recapture event, not a drive-by change.
    #[must_use]
    pub fn aperture_depth_m(self) -> f32 {
        match self {
            Self::Panel => 0.06,
            Self::Wash => 0.10,
            Self::Beam => 0.20,
            Self::Spot => 0.27,
            Self::Profile => 0.31,
        }
    }
}

/// A fixture's internal geometry: where its pivot and aperture sit relative to
/// the mounting origin, and where its cells sit on the aperture plane.
///
/// A "cell" is one addressable emitter — a head of a moving fixture (usually
/// exactly one), or one pixel of a bar or matrix. Cell offsets are in the
/// fixture's own frame, metres, and are measured from the aperture, which is
/// why a bar's pixels are a flat grid regardless of how the bar is hung.
///
/// Fields are private: a `FixtureGeometry` always has at least one cell, and
/// callers index it with a plain `usize` that can never be out of range.
#[derive(Debug, Clone, PartialEq)]
pub struct FixtureGeometry {
    pivot_offset: Vec3,
    aperture_offset: Vec3,
    cells: Vec<Vec3>,
}

impl FixtureGeometry {
    /// Geometry for a definition that carries no pivot or aperture data — which
    /// is every definition today, because Luma's fixture library is QLC+ `.qxf`
    /// and QLC+ describes a housing, a pixel layout and a lens angle but never
    /// where inside the housing the light leaves from.
    ///
    /// Both offsets are zero, so a cell's beam starts exactly at the mounting
    /// origin rotated into place. That reproduces today's behaviour.
    ///
    /// An empty `cells` yields a single cell at the origin: a fixture with no
    /// addressable emitter is not a thing, and returning a `Result` here would
    /// push a `?` into every call site for a case that cannot happen.
    #[must_use]
    pub fn unauthored(cells: Vec<Vec3>) -> Self {
        Self {
            pivot_offset: Vec3::ZERO,
            aperture_offset: Vec3::ZERO,
            cells: Self::at_least_one(cells),
        }
    }

    /// Geometry using the measured per-class aperture depth from
    /// [`FixtureClass::aperture_depth_m`], with a zero pivot offset.
    ///
    /// See that method for why nothing calls this yet.
    #[must_use]
    pub fn from_class(class: FixtureClass, cells: Vec<Vec3>) -> Self {
        Self {
            pivot_offset: Vec3::ZERO,
            aperture_offset: REST_AXIS * class.aperture_depth_m(),
            cells: Self::at_least_one(cells),
        }
    }

    /// Offset from the mounting origin to the pan/tilt pivot, for fixtures whose
    /// yoke does not pivot about where they bolt on.
    ///
    /// The seam for imported geometry (GDTF `Position` matrices, if that import
    /// is ever written); nothing in the QLC+ library can fill it in.
    #[must_use]
    pub fn with_pivot_offset(mut self, offset: Vec3) -> Self {
        self.pivot_offset = offset;
        self
    }

    /// How many addressable cells this fixture has. Always at least 1.
    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    fn at_least_one(cells: Vec<Vec3>) -> Vec<Vec3> {
        if cells.is_empty() {
            vec![Vec3::ZERO]
        } else {
            cells
        }
    }

    /// Cell offset, clamped to the last cell. Total by construction.
    fn cell(&self, index: usize) -> Vec3 {
        self.cells[index.min(self.cells.len() - 1)]
    }
}

/// Where a fixture is bolted and how it is turned — its pose in the rig,
/// independent of anything the fixture does with its own head.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mount {
    position: Vec3,
    rotation: Mat3,
}

impl Mount {
    /// Build from a stored fixture row: position in metres, `rot` the stored
    /// Euler triple in radians.
    ///
    /// The stored triple is interpreted here and nowhere else. Callers pass what
    /// the database holds; the axis remap and sign flips that make it a data
    /// space rotation are this crate's business.
    #[must_use]
    pub fn new(position: Vec3, rot: [f32; 3]) -> Self {
        Self {
            position,
            // Conjugating the renderer's `Rx(r0)·Ry(r2)·Rz(r1)` by the Y/Z swap:
            // each factor keeps its angle's magnitude, changes sign, and moves
            // to the swapped axis.
            rotation: Mat3::from_rotation_x(-rot[0])
                * Mat3::from_rotation_z(-rot[2])
                * Mat3::from_rotation_y(-rot[1]),
        }
    }

    /// The mounting origin — where the clamp is, not where the light is.
    #[must_use]
    pub fn position(self) -> Vec3 {
        self.position
    }
}

/// A head's pan and tilt for one instant.
///
/// Radians internally; [`Articulation::from_degrees`] exists because DMX-derived
/// state is quoted in degrees everywhere upstream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Articulation {
    pan: f32,
    tilt: f32,
}

impl Articulation {
    /// Parked: no pan, no tilt. A fixture here emits along [`REST_AXIS`].
    pub const REST: Self = Self {
        pan: 0.0,
        tilt: 0.0,
    };

    /// From radians.
    #[must_use]
    pub fn from_radians(pan: f32, tilt: f32) -> Self {
        Self { pan, tilt }
    }

    /// From degrees, which is how head state is quoted upstream.
    #[must_use]
    pub fn from_degrees(pan: f32, tilt: f32) -> Self {
        Self {
            pan: pan.to_radians(),
            tilt: tilt.to_radians(),
        }
    }

    /// The rotation this articulation applies, in data space.
    fn rotation(self) -> Mat3 {
        Mat3::from_rotation_z(-self.pan) * Mat3::from_rotation_x(self.tilt)
    }
}

/// A beam: where it starts and which way it goes. `direction` is unit length.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Ray {
    /// Emission point in data space, metres.
    pub origin: Vec3,
    /// Unit direction in data space.
    pub direction: Vec3,
}

/// The pan/tilt pivot of a mounted fixture, in data space.
///
/// Exposed as a named point because a caller that genuinely wants the pivot —
/// drawing a yoke, say — must ask for it, rather than calling [`rig_position`]
/// and subtracting terms back off. Subtraction at a call site is how the three
/// implementations this crate replaced drifted apart in the first place.
#[must_use]
pub fn pivot_point(geom: &FixtureGeometry, mount: &Mount) -> Vec3 {
    mount.position + mount.rotation * geom.pivot_offset
}

/// The static rig position of one cell: where that emitter *is*, with the head
/// parked.
///
/// This is the position pattern space is built on. It deliberately has no
/// [`Articulation`] parameter, so spatial selection, gradients and chases cannot
/// become frame-dependent by accident.
///
/// The aperture offset **is** included: the light of a parked fixture comes out
/// of its lens, not out of its yoke, and pattern space should agree with what a
/// person sees. `cell` is clamped to the fixture's last cell, so this is total.
///
/// ```
/// use fixture_kinematics::{rig_position, FixtureGeometry, FixtureClass, Mount};
/// use glam::Vec3;
///
/// // A profile hung 6 m up emits 0.31 m below its clamp.
/// let geom = FixtureGeometry::from_class(FixtureClass::Profile, vec![Vec3::ZERO]);
/// let mount = Mount::new(Vec3::new(0.0, 0.0, 6.0), [0.0; 3]);
/// assert_eq!(rig_position(&geom, &mount, 0), Vec3::new(0.0, 0.0, 5.69));
/// ```
#[must_use]
pub fn rig_position(geom: &FixtureGeometry, mount: &Mount, cell: usize) -> Vec3 {
    beam_ray(geom, mount, &Articulation::REST, cell).origin
}

/// The ray one cell emits, given where the head is pointing.
///
/// `cell` is clamped to the fixture's last cell, so this is total.
///
/// Guaranteed: `beam_ray(g, m, &Articulation::REST, c).origin == rig_position(g, m, c)`.
///
/// ```
/// use fixture_kinematics::{beam_ray, Articulation, FixtureGeometry, Mount};
/// use glam::Vec3;
///
/// // Tilt 90 degrees swings a downward beam out to the horizontal, upstage.
/// let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]);
/// let mount = Mount::new(Vec3::new(0.0, 0.0, 5.0), [0.0; 3]);
/// let dir = beam_ray(&geom, &mount, &Articulation::from_degrees(0.0, 90.0), 0).direction;
/// assert!(dir.abs_diff_eq(Vec3::Y, 1e-6));
/// ```
#[must_use]
pub fn beam_ray(geom: &FixtureGeometry, mount: &Mount, art: &Articulation, cell: usize) -> Ray {
    let articulation = art.rotation();
    let head = articulation * (geom.aperture_offset + geom.cell(cell));
    Ray {
        origin: mount.position + mount.rotation * (geom.pivot_offset + head),
        direction: (mount.rotation * articulation * REST_AXIS).normalize(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn close(got: Vec3, want: Vec3) {
        assert!(got.abs_diff_eq(want, 1e-5), "got {got:?}, want {want:?}");
    }

    #[test]
    fn rest_origin_is_the_rig_position() {
        // The invariant the whole crate exists to keep: two entry points, one
        // point in space when nothing is moving.
        let geom = FixtureGeometry::from_class(FixtureClass::Spot, vec![Vec3::ZERO])
            .with_pivot_offset(Vec3::new(0.0, 0.02, -0.15));
        let mount = Mount::new(Vec3::new(1.0, -2.0, 5.0), [0.3, -0.7, 1.1]);
        for cell in 0..3 {
            assert_eq!(
                beam_ray(&geom, &mount, &Articulation::REST, cell).origin,
                rig_position(&geom, &mount, cell)
            );
        }
    }

    #[test]
    fn rig_position_of_an_unrotated_panel_is_base_plus_cell() {
        let geom = FixtureGeometry::unauthored(vec![Vec3::new(0.1, 0.0, 0.2)]);
        let mount = Mount::new(Vec3::new(1.0, 2.0, 3.0), [0.0; 3]);
        close(rig_position(&geom, &mount, 0), Vec3::new(1.1, 2.0, 3.2));
    }

    #[test]
    fn stored_roll_rotates_about_data_x_with_a_negated_angle() {
        // rot[0] = +90 deg. Under `Rx(-rot[0])`, a cell on +Y goes to -Z.
        let geom = FixtureGeometry::unauthored(vec![Vec3::Y]);
        let mount = Mount::new(Vec3::ZERO, [FRAC_PI_2, 0.0, 0.0]);
        close(rig_position(&geom, &mount, 0), Vec3::NEG_Z);
    }

    #[test]
    fn stored_second_euler_term_drives_data_yaw() {
        // rot[1] is the *three.js* Y term; the swap sends it to data Y as well,
        // but negated. A cell on +X goes to +Z under `Ry(-90 deg)`.
        let geom = FixtureGeometry::unauthored(vec![Vec3::X]);
        let mount = Mount::new(Vec3::ZERO, [0.0, FRAC_PI_2, 0.0]);
        close(rig_position(&geom, &mount, 0), Vec3::Z);
    }

    #[test]
    fn stored_third_euler_term_drives_data_z() {
        // rot[2] lands on data Z, negated: +X goes to -Y.
        let geom = FixtureGeometry::unauthored(vec![Vec3::X]);
        let mount = Mount::new(Vec3::ZERO, [0.0, 0.0, FRAC_PI_2]);
        close(rig_position(&geom, &mount, 0), Vec3::NEG_Y);
    }

    #[test]
    fn pan_negates_and_tilt_does_not() {
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]);
        let mount = Mount::new(Vec3::ZERO, [0.0; 3]);

        // Tilt alone: +90 deg about +X sends -Z to +Y (upstage, horizontal).
        let tilted = beam_ray(&geom, &mount, &Articulation::from_degrees(0.0, 90.0), 0);
        close(tilted.direction, Vec3::Y);

        // Pan then applies about -Z: panning +90 deg sends +Y to +X.
        let panned = beam_ray(&geom, &mount, &Articulation::from_degrees(90.0, 90.0), 0);
        close(panned.direction, Vec3::X);
    }

    #[test]
    fn pan_alone_cannot_move_a_centred_beam() {
        // A single-cell fixture with no aperture depth pivots in place; only the
        // direction may change, and with zero tilt not even that.
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]);
        let mount = Mount::new(Vec3::new(0.0, 0.0, 4.0), [0.0; 3]);
        let ray = beam_ray(&geom, &mount, &Articulation::from_degrees(137.0, 0.0), 0);
        close(ray.origin, Vec3::new(0.0, 0.0, 4.0));
        close(ray.direction, REST_AXIS);
    }

    #[test]
    fn aperture_offset_swings_with_the_head() {
        // The reason `rig_position` takes no articulation: with a real aperture
        // depth, the emission point *moves* when the head tilts. Pattern space
        // must not see that.
        let geom = FixtureGeometry::from_class(FixtureClass::Beam, vec![Vec3::ZERO]);
        let mount = Mount::new(Vec3::ZERO, [0.0; 3]);
        close(rig_position(&geom, &mount, 0), Vec3::new(0.0, 0.0, -0.2));
        let tilted = beam_ray(&geom, &mount, &Articulation::from_degrees(0.0, 90.0), 0);
        close(tilted.origin, Vec3::new(0.0, 0.2, 0.0));
    }

    #[test]
    fn cell_index_is_total() {
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO, Vec3::X]);
        let mount = Mount::new(Vec3::ZERO, [0.0; 3]);
        assert_eq!(geom.cell_count(), 2);
        assert_eq!(rig_position(&geom, &mount, 99), Vec3::X);
    }

    #[test]
    fn an_empty_cell_list_still_has_one_cell() {
        let geom = FixtureGeometry::unauthored(Vec::new());
        assert_eq!(geom.cell_count(), 1);
        assert_eq!(
            rig_position(&geom, &Mount::new(Vec3::ZERO, [0.0; 3]), 0),
            Vec3::ZERO
        );
    }

    #[test]
    fn pivot_point_is_the_rig_position_of_a_bare_pivot() {
        let pivot = Vec3::new(0.0, 0.03, -0.1);
        let mount = Mount::new(Vec3::new(2.0, 1.0, 4.0), [0.2, 0.4, -0.9]);
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]).with_pivot_offset(pivot);
        close(pivot_point(&geom, &mount), rig_position(&geom, &mount, 0));
    }

    #[test]
    fn mount_rotation_is_a_rotation() {
        // No scale, no mirror: `det = +1` and columns orthonormal. If this ever
        // fails, a sign flip has been written as a reflection by mistake.
        let m = Mount::new(Vec3::ZERO, [0.3, -1.2, 0.8]);
        assert!((m.rotation.determinant() - 1.0).abs() < 1e-5);
        let ray = beam_ray(
            &FixtureGeometry::unauthored(vec![Vec3::ZERO]),
            &m,
            &Articulation::from_degrees(31.0, -12.0),
            0,
        );
        assert!((ray.direction.length() - 1.0).abs() < 1e-6);
    }
}
