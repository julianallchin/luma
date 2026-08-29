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
//! `+X` stage right, `+Y` upstage/back, `+Z` up.
//!
//! # Rest is the mount normal
//!
//! A fixture at rest emits along the **outward normal of whatever it is mounted
//! on**, and pan/tilt are measured from there. Hung under a truss it points
//! down; standing on the floor it points up; clamped to a truss's downstage face
//! it points at the house. There is no per-fixture-type rest axis — a moving
//! head, a par and an LED bar with the same mount all rest the same way, and a
//! fixture that must point elsewhere says so with its mount, not with its class.
//!
//! In the mount's own frame that normal is [`REST_AXIS`], `-Z`. The mount frame
//! comes from [`Mount::from_stored`] today and from the venue graph's socket
//! frame at phase 3; nothing else in this crate changes when it does.
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
//! let mount = Mount::from_stored(Vec3::new(0.0, 0.0, 4.0), [0.0; 3]);
//!
//! // A fixture at rest emits from its rig position, straight down.
//! let ray = beam_ray(&geom, &mount, &Articulation::REST, 0);
//! assert_eq!(ray.origin, rig_position(&geom, &mount, 0));
//! assert_eq!(ray.direction, Vec3::NEG_Z);
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use glam::{Mat3, Vec3};

/// The mount normal, in the mount's own frame — the axis a parked fixture emits
/// along before the mount is oriented.
///
/// Public because a caller that needs "which way would this fixture point if it
/// were parked" should read it from here rather than re-deriving a sign. What it
/// means in world terms is [`Mount::normal`]; this constant is only ever the
/// mount-local half.
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
    /// Derive the mount frame from a stored fixture row: position in metres,
    /// `rot` the stored Euler triple in radians.
    ///
    /// The stored triple is interpreted here and nowhere else. Callers pass what
    /// the database holds; the axis remap and sign flips that make it a data
    /// space rotation are this crate's business.
    ///
    /// The stored triple is a *serialization* of the frame [`Self::from_frame`]
    /// takes; this constructor is the one place it is read. It survives the
    /// venue graph because `scene_desc::Fixture` and the golden vectors still
    /// carry a triple — what went away is the idea that a fixture *owns* one.
    #[must_use]
    pub fn from_stored(position: Vec3, rot: [f32; 3]) -> Self {
        // Conjugating the renderer's `Rx(r0)·Ry(r2)·Rz(r1)` by the Y/Z swap:
        // each factor keeps its angle's magnitude, changes sign, and moves to
        // the swapped axis.
        Self::from_frame(
            position,
            Mat3::from_rotation_x(-rot[0])
                * Mat3::from_rotation_z(-rot[2])
                * Mat3::from_rotation_y(-rot[1]),
        )
    }

    /// The mount frame directly: `rotation` is a data-space basis whose
    /// [`REST_AXIS`] column is the direction a parked head emits along.
    ///
    /// This is what the venue graph hands over — a fixture has no independent
    /// pose, it hangs off a socket, and the socket's frame *is* the mount
    /// frame (`luma_scene::venue::NodePose::data_basis`). Taking the basis
    /// rather than a triple keeps the pose out of
    /// `luma_scene::coords::euler_xyz_of`'s gimbal clamp, which a socket
    /// pointing straight down would otherwise land in.
    #[must_use]
    pub fn from_frame(position: Vec3, rotation: Mat3) -> Self {
        Self { position, rotation }
    }

    /// The mounting origin — where the clamp is, not where the light is.
    #[must_use]
    pub fn position(self) -> Vec3 {
        self.position
    }

    /// The outward normal of the surface this fixture is mounted on, in data
    /// space: the direction a parked head emits along.
    ///
    /// Equal to `beam_ray(.., Articulation::REST, ..).direction`, and cheaper
    /// when the question is only "which way is this thing facing" — the agent
    /// bindings and [`StageDirection::of`] want exactly that and have no cell to
    /// speak of.
    #[must_use]
    pub fn normal(self) -> Vec3 {
        self.rotation * REST_AXIS
    }
}

/// A direction reduced to the one stage word that best describes it.
///
/// The vocabulary a lighting designer uses, and the only place a vector becomes
/// a word: an agent asking "which fixtures point at the house" and a UI badge
/// must not disagree about where stage left is. Data space is `+X` stage right,
/// `+Y` upstage, `+Z` up, so the house is `-Y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StageDirection {
    /// `-Y`: out over the audience.
    House,
    /// `+Y`: into the back wall.
    Upstage,
    /// `+X`.
    StageRight,
    /// `-X`.
    StageLeft,
    /// `+Z`.
    Up,
    /// `-Z`.
    Down,
}

impl StageDirection {
    /// The dominant axis of `v`, as a stage word.
    ///
    /// Ties break toward the vertical and then toward depth, which is what makes
    /// this total: a zero vector reads as `Down`, the rest pose, rather than
    /// forcing every caller to handle an `Option` for a case the geometry cannot
    /// produce.
    #[must_use]
    pub fn of(v: Vec3) -> Self {
        let (ax, ay, az) = (v.x.abs(), v.y.abs(), v.z.abs());
        if az >= ax && az >= ay {
            if v.z > 0.0 {
                Self::Up
            } else {
                Self::Down
            }
        } else if ay >= ax {
            if v.y > 0.0 {
                Self::Upstage
            } else {
                Self::House
            }
        } else if v.x > 0.0 {
            Self::StageRight
        } else {
            Self::StageLeft
        }
    }

    /// The word itself, hyphenated, for prose handed to a model or a person.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::House => "house",
            Self::Upstage => "upstage",
            Self::StageRight => "stage-right",
            Self::StageLeft => "stage-left",
            Self::Up => "up",
            Self::Down => "down",
        }
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
/// let mount = Mount::from_stored(Vec3::new(0.0, 0.0, 6.0), [0.0; 3]);
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
/// let mount = Mount::from_stored(Vec3::new(0.0, 0.0, 5.0), [0.0; 3]);
/// let dir = beam_ray(&geom, &mount, &Articulation::from_degrees(0.0, 90.0), 0).direction;
/// assert!(dir.abs_diff_eq(Vec3::Y, 1e-6));
/// ```
#[must_use]
pub fn beam_ray(geom: &FixtureGeometry, mount: &Mount, art: &Articulation, cell: usize) -> Ray {
    let articulation = art.rotation();
    let head = articulation * (geom.aperture_offset + geom.cell(cell));
    Ray {
        origin: mount.position + mount.rotation * (geom.pivot_offset + head),
        direction: aim(mount, art),
    }
}

/// Which way a head points, without reference to its geometry.
///
/// Every cell of a fixture aims the same way — a bar's pixels are parallel, a
/// head has one cell — so aim is a property of the mount and the articulation
/// alone. Callers that only want the direction (a renderer siting a cone, an
/// agent asking which fixtures face the house) take this rather than building a
/// [`FixtureGeometry`] to throw away.
///
/// `aim(m, &Articulation::REST) == m.normal()`, which is the "rest is the mount
/// normal" rule written as an equation.
///
/// ```
/// use fixture_kinematics::{aim, Articulation, Mount};
/// use glam::Vec3;
///
/// // Hung square, tilted 90 degrees: the beam swings from down to upstage.
/// let mount = Mount::from_stored(Vec3::new(0.0, 0.0, 5.0), [0.0; 3]);
/// assert_eq!(aim(&mount, &Articulation::REST), mount.normal());
/// assert!(aim(&mount, &Articulation::from_degrees(0.0, 90.0)).abs_diff_eq(Vec3::Y, 1e-6));
/// ```
#[must_use]
pub fn aim(mount: &Mount, art: &Articulation) -> Vec3 {
    (mount.rotation * art.rotation() * REST_AXIS).normalize()
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
        let mount = Mount::from_stored(Vec3::new(1.0, -2.0, 5.0), [0.3, -0.7, 1.1]);
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
        let mount = Mount::from_stored(Vec3::new(1.0, 2.0, 3.0), [0.0; 3]);
        close(rig_position(&geom, &mount, 0), Vec3::new(1.1, 2.0, 3.2));
    }

    #[test]
    fn stored_roll_rotates_about_data_x_with_a_negated_angle() {
        // rot[0] = +90 deg. Under `Rx(-rot[0])`, a cell on +Y goes to -Z.
        let geom = FixtureGeometry::unauthored(vec![Vec3::Y]);
        let mount = Mount::from_stored(Vec3::ZERO, [FRAC_PI_2, 0.0, 0.0]);
        close(rig_position(&geom, &mount, 0), Vec3::NEG_Z);
    }

    #[test]
    fn stored_second_euler_term_drives_data_yaw() {
        // rot[1] is the *three.js* Y term; the swap sends it to data Y as well,
        // but negated. A cell on +X goes to +Z under `Ry(-90 deg)`.
        let geom = FixtureGeometry::unauthored(vec![Vec3::X]);
        let mount = Mount::from_stored(Vec3::ZERO, [0.0, FRAC_PI_2, 0.0]);
        close(rig_position(&geom, &mount, 0), Vec3::Z);
    }

    #[test]
    fn stored_third_euler_term_drives_data_z() {
        // rot[2] lands on data Z, negated: +X goes to -Y.
        let geom = FixtureGeometry::unauthored(vec![Vec3::X]);
        let mount = Mount::from_stored(Vec3::ZERO, [0.0, 0.0, FRAC_PI_2]);
        close(rig_position(&geom, &mount, 0), Vec3::NEG_Y);
    }

    #[test]
    fn pan_negates_and_tilt_does_not() {
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]);
        let mount = Mount::from_stored(Vec3::ZERO, [0.0; 3]);

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
        let mount = Mount::from_stored(Vec3::new(0.0, 0.0, 4.0), [0.0; 3]);
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
        let mount = Mount::from_stored(Vec3::ZERO, [0.0; 3]);
        close(rig_position(&geom, &mount, 0), Vec3::new(0.0, 0.0, -0.2));
        let tilted = beam_ray(&geom, &mount, &Articulation::from_degrees(0.0, 90.0), 0);
        close(tilted.origin, Vec3::new(0.0, 0.2, 0.0));
    }

    #[test]
    fn cell_index_is_total() {
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO, Vec3::X]);
        let mount = Mount::from_stored(Vec3::ZERO, [0.0; 3]);
        assert_eq!(geom.cell_count(), 2);
        assert_eq!(rig_position(&geom, &mount, 99), Vec3::X);
    }

    #[test]
    fn an_empty_cell_list_still_has_one_cell() {
        let geom = FixtureGeometry::unauthored(Vec::new());
        assert_eq!(geom.cell_count(), 1);
        assert_eq!(
            rig_position(&geom, &Mount::from_stored(Vec3::ZERO, [0.0; 3]), 0),
            Vec3::ZERO
        );
    }

    #[test]
    fn pivot_point_is_the_rig_position_of_a_bare_pivot() {
        let pivot = Vec3::new(0.0, 0.03, -0.1);
        let mount = Mount::from_stored(Vec3::new(2.0, 1.0, 4.0), [0.2, 0.4, -0.9]);
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]).with_pivot_offset(pivot);
        close(pivot_point(&geom, &mount), rig_position(&geom, &mount, 0));
    }

    #[test]
    fn the_mount_normal_is_where_a_parked_head_points() {
        // The rule, stated three ways, because the three mounts are the ones a
        // rig actually has. No fixture *type* appears in any of them.
        let hung = Mount::from_stored(Vec3::new(0.0, 0.0, 6.0), [0.0; 3]);
        close(hung.normal(), Vec3::NEG_Z);
        assert_eq!(StageDirection::of(hung.normal()), StageDirection::Down);

        let floor = Mount::from_stored(Vec3::new(0.0, -2.0, 0.1), [std::f32::consts::PI, 0.0, 0.0]);
        close(floor.normal(), Vec3::Z);
        assert_eq!(StageDirection::of(floor.normal()), StageDirection::Up);

        let downstage_face = Mount::from_stored(Vec3::new(0.0, 4.0, 3.0), [FRAC_PI_2, 0.0, 0.0]);
        close(downstage_face.normal(), Vec3::NEG_Y);
        assert_eq!(
            StageDirection::of(downstage_face.normal()),
            StageDirection::House
        );
    }

    #[test]
    fn the_mount_normal_is_the_rest_beam() {
        // `normal` is a shortcut, not a second derivation: it must agree with
        // the ray for every mount, or "rest = mount normal" is only a slogan.
        let geom = FixtureGeometry::unauthored(vec![Vec3::ZERO]);
        for rot in [[0.0; 3], [0.4, -1.3, 0.9], [FRAC_PI_2, 0.0, 0.0]] {
            let mount = Mount::from_stored(Vec3::new(1.0, -2.0, 3.0), rot);
            close(
                beam_ray(&geom, &mount, &Articulation::REST, 0).direction,
                mount.normal(),
            );
        }
    }

    #[test]
    fn stage_words_name_the_dominant_axis() {
        for (v, want) in [
            (Vec3::NEG_Y, StageDirection::House),
            (Vec3::Y, StageDirection::Upstage),
            (Vec3::X, StageDirection::StageRight),
            (Vec3::NEG_X, StageDirection::StageLeft),
            (Vec3::Z, StageDirection::Up),
            (Vec3::NEG_Z, StageDirection::Down),
            // Mostly at the house, slightly stage left and slightly up.
            (Vec3::new(-0.2, -0.9, 0.3), StageDirection::House),
            // Total: a direction that is not a direction still reads.
            (Vec3::ZERO, StageDirection::Down),
        ] {
            assert_eq!(StageDirection::of(v), want, "{v:?}");
        }
    }

    #[test]
    fn mount_rotation_is_a_rotation() {
        // No scale, no mirror: `det = +1` and columns orthonormal. If this ever
        // fails, a sign flip has been written as a reflection by mistake.
        let m = Mount::from_stored(Vec3::ZERO, [0.3, -1.2, 0.8]);
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
