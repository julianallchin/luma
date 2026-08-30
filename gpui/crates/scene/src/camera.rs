//! The camera: spherical, Z-up, reverse-Z with an infinite far plane.
//!
//! Spherical because that is what the orbit controller manipulates and what
//! gets persisted per venue — storing `position + target` and deriving the
//! angles back is how a camera drifts.

use crate::bvh::Ray;
use crate::framing::{Framing, Viewfinder};
use glam::{Mat4, Vec2, Vec3};

/// Keeps the view direction off the poles, where `look_at` has no up vector.
const POLAR_LIMIT: f32 = 1e-3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: Vec3,
    pub radius: f32,
    /// Rotation about +Z, measured from +X.
    pub azimuth: f32,
    /// Angle from +Z. Clamped away from the poles.
    pub polar: f32,
    pub fov_y_deg: f32,
    pub znear: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            radius: 8.0,
            azimuth: std::f32::consts::FRAC_PI_4,
            polar: std::f32::consts::FRAC_PI_3,
            fov_y_deg: 50.0,
            znear: 0.1,
        }
    }
}

impl Camera {
    /// Smallest radius a derived camera may have — four near planes, so a booth
    /// standing on the framing target still has a frustum in front of it.
    /// See [`Self::looking_from`].
    pub const MIN_RADIUS: f32 = 4.0 * DEFAULT_ZNEAR;

    pub fn position(&self) -> Vec3 {
        let (sp, cp) = self.clamped_polar().sin_cos();
        let (sa, ca) = self.azimuth.sin_cos();
        self.target + self.radius * Vec3::new(sp * ca, sp * sa, cp)
    }

    fn clamped_polar(&self) -> f32 {
        self.polar
            .clamp(POLAR_LIMIT, std::f32::consts::PI - POLAR_LIMIT)
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.position(), self.target, Vec3::Z)
    }

    /// Reverse-Z, infinite far: clip `z ∈ [1, 0]`, compared with
    /// `CompareFunction::Greater`. Removes every depth-precision question over
    /// the 0.1 m – 50 m stage for the price of one matrix constant.
    pub fn projection(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(self.fov_y_deg.to_radians(), aspect, self.znear)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection(aspect) * self.view()
    }

    /// Picking ray through a normalized device coordinate (`-1..1`, +Y up).
    pub fn ray(&self, ndc: Vec2, aspect: f32) -> Ray {
        let inv = self.view_projection(aspect).inverse();
        // Reverse-Z: the near plane is z = 1 and z = 0 is infinitely far, so
        // the second point is taken at a finite depth (z = 0.5, i.e. twice
        // znear) rather than on the far plane.
        let near = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
        let ahead = inv.project_point3(Vec3::new(ndc.x, ndc.y, 0.5));
        Ray::new(near, ahead - near)
    }

    /// Project a world point to NDC. Marquee selection is projection-based,
    /// not raycast-based, so this is its primitive.
    pub fn project(&self, world: Vec3, aspect: f32) -> Vec3 {
        self.view_projection(aspect).project_point3(world)
    }

    /// The camera that sits at `eye` and looks at `target`.
    ///
    /// The spherical parameters are *derived*, not stored alongside a pose:
    /// holding both is how a camera drifts (see the module note). Views that
    /// place a physical eye — a body in the audience, a body at the booth —
    /// come through here; views that orbit at a fitted distance set the angles
    /// directly.
    ///
    /// An eye on top of its target has no direction, so the radius is floored
    /// at [`Self::MIN_RADIUS`] and the pose degenerates to looking straight
    /// down rather than to a NaN basis.
    #[must_use]
    pub fn looking_from(eye: Vec3, target: Vec3, fov_y_deg: f32) -> Self {
        let offset = eye - target;
        let radius = offset.length().max(Self::MIN_RADIUS);
        Self {
            target,
            radius,
            azimuth: offset.y.atan2(offset.x),
            polar: (offset.z / radius).clamp(-1.0, 1.0).acos(),
            fov_y_deg,
            znear: DEFAULT_ZNEAR,
        }
    }

    /// The camera for a named [`View`] of a framed rig.
    ///
    /// `view_finder` carries the lens, the frame's shape, and what chrome
    /// covers its edges — none of them optional, because fitting a box to a
    /// 9:16 portrait, to a 16:9 pane, and to a 16:9 pane with a toolbar across
    /// the bottom are three different distances. `booth` is the world position
    /// of the DJ booth, which only [`View::Dj`] reads — see that variant for
    /// what `None` falls back to.
    ///
    /// # Two rules, and where each applies
    ///
    /// The **orbit views** ([`View::Front`], [`View::Overhead`], the two
    /// quarters, and `dj` with no booth) pick a direction and hand it to
    /// [`Framing::fit`]: the eye is free in space, so the closed-form distance
    /// applies directly.
    ///
    /// The **standing views** ([`View::Audience`], and `dj` with a booth) pin
    /// the eye to a *height* — [`EYE_HEIGHT_M`] above [`Framing::floor_z`] —
    /// because they answer "what does someone in the room see", and someone in
    /// the room does not float. That makes the direction a function of the
    /// distance being solved for, so the closed form does not apply; the
    /// distance along the floor is bisected against [`Framing::fits`] instead.
    ///
    /// `dj` stands *at least* as far back as the booth, and further when the
    /// booth is not far enough. A booth is usually a metre from the nearest
    /// fixture and often between them, so taking it literally puts the eye
    /// inside the light field and renders one white rectangle — which is what
    /// both real venues did. Backing off along the booth's own bearing keeps
    /// the operator's angle and gives the picture something to be of.
    ///
    /// Every returned camera has its eye above the floor.
    #[must_use]
    pub fn for_view(
        view: View,
        framing: &Framing,
        booth: Option<Vec3>,
        view_finder: &Viewfinder,
    ) -> Self {
        let orbit = |target: Vec3, direction: Vec3| {
            framing
                .fit(target, direction, view_finder)
                .above_floor(framing.floor_z())
        };
        let spherical = |azimuth: f32, polar: f32| {
            let (sp, cp) = polar.sin_cos();
            let (sa, ca) = azimuth.sin_cos();
            Vec3::new(sp * ca, sp * sa, cp)
        };
        // A body standing on the floor, `at_least` metres out along `azimuth`
        // and further if that is not far enough to see the rig, aimed so the
        // chrome does not eat it.
        let target = framing.target();
        let standing = |azimuth: f32, at_least: f32| {
            let eye_z = framing.floor_z() + EYE_HEIGHT_M;
            let standoff =
                standing_standoff(framing, target, azimuth, eye_z, view_finder).max(at_least);
            let (sa, ca) = azimuth.sin_cos();
            let eye = Vec3::new(target.x + standoff * ca, target.y + standoff * sa, eye_z);
            Self::aimed_from(framing, eye, target, view_finder)
        };
        match view {
            View::Front => orbit(target, FRONT_EYE.normalize()),
            View::Overhead => orbit(target, spherical(front_azimuth(), Framing::MIN_POLAR)),
            View::QuarterLeft => orbit(target, spherical(front_azimuth() - QUARTER, QUARTER_POLAR)),
            View::QuarterRight => {
                orbit(target, spherical(front_azimuth() + QUARTER, QUARTER_POLAR))
            }
            View::Audience => standing(front_azimuth(), 0.0),
            View::Dj => match booth {
                Some(booth) => {
                    let out = (booth - target).truncate();
                    standing(out.y.atan2(out.x), out.length())
                }
                None => orbit(target, reversed(FRONT_EYE).normalize()),
            },
        }
    }

    /// [`Camera::looking_from`], with the aim shifted so the chrome-free part
    /// of the frame is what the rig is centred in.
    ///
    /// The shift is what the two standing views need and [`Framing::fit`] does
    /// for itself: their eye is already placed, so only the aim is left to
    /// choose.
    fn aimed_from(framing: &Framing, eye: Vec3, target: Vec3, view: &Viewfinder) -> Self {
        let offset = eye - target;
        let distance = offset.length().max(Self::MIN_RADIUS);
        let aim = framing.aim(target, offset / distance, view, distance);
        Self::looking_from(eye, aim, view.fov_y_deg)
    }

    /// Tilt the eye down towards the target until it clears the floor.
    ///
    /// Only bites on a rig whose framed target is itself near the floor and
    /// whose fitted direction is shallow; the ordinary case is metres clear.
    fn above_floor(mut self, floor_z: f32) -> Self {
        let lowest = (floor_z + MIN_EYE_Z - self.target.z) / self.radius;
        if lowest < 1.0 {
            self.polar = self.polar.min(lowest.max(-1.0).acos());
        }
        self
    }
}

/// Distance along the floor from `target` at which an eye `eye_z` high frames
/// the whole rig.
///
/// Bisection rather than a formula because the eye is pinned to a plane: moving
/// it back also tilts it up, so the direction the closed form needs is itself a
/// function of the answer. [`Framing::fits`] is monotone in standoff — an eye
/// that has stepped back never loses a corner it had — which is the whole of
/// what a bisection needs. Twenty-four halvings take a bracket of hundreds of
/// metres below a millimetre, and the answer is nudged out by one part in ten
/// thousand: the eye is handed back through [`Camera::looking_from`]'s
/// spherical round-trip, which loses a few ulps, and a standoff solved to the
/// exact boundary would come back a hair inside it.
fn standing_standoff(
    framing: &Framing,
    target: Vec3,
    azimuth: f32,
    eye_z: f32,
    view: &Viewfinder,
) -> f32 {
    let (sa, ca) = azimuth.sin_cos();
    let fits = |s: f32| {
        framing.fits(
            Vec3::new(target.x + s * ca, target.y + s * sa, eye_z),
            target,
            view,
        )
    };
    // A body is at least outside the rig's own footprint.
    let mut lo = framing.radius();
    let mut hi = lo * 2.0;
    for _ in 0..24 {
        if fits(hi) {
            break;
        }
        hi *= 2.0;
    }
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if fits(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi * (1.0 + BISECTION_SLACK)
}

/// See [`standing_standoff`].
const BISECTION_SLACK: f32 = 1e-4;

/// Near plane for every derived camera. The stage is 0.1 m – 50 m and the
/// projection is reverse-Z, so this is a constant rather than a dial.
const DEFAULT_ZNEAR: f32 = 0.1;

/// How high off the floor a standing eye is, in metres.
pub const EYE_HEIGHT_M: f32 = 1.7;

/// Lowest an eye may sit, in metres. Not zero: a camera exactly on the floor
/// plane renders it edge-on as a single row of pixels.
const MIN_EYE_Z: f32 = 0.25;

/// Quarter views swing this far off the front azimuth.
const QUARTER: f32 = std::f32::consts::FRAC_PI_4;

/// …and look down from this far off vertical: high enough to read the plan of
/// the rig, low enough to keep the beams as beams rather than as pools.
const QUARTER_POLAR: f32 = 55.0 * std::f32::consts::PI / 180.0;

/// Where the audience is, as a world-space offset from the framing target.
///
/// This is the one place the "front" of a stage is defined. It is the three.js
/// camera the web opened at, `(0, 1, 3)`, brought into world space — its
/// magnitude is meaningless (a fitted distance replaces it), only the direction
/// and the resulting azimuth/polar carry over.
const FRONT_EYE: Vec3 = Vec3::new(0.0, -3.0, 1.0);

/// The same elevation, half a turn round: what [`View::Dj`] falls back to.
fn reversed(direction: Vec3) -> Vec3 {
    Vec3::new(-direction.x, -direction.y, direction.z)
}

/// Azimuth of [`FRONT_EYE`]: the direction the audience stands in.
fn front_azimuth() -> f32 {
    FRONT_EYE.y.atan2(FRONT_EYE.x)
}

/// A camera position with a name — the vocabulary a human or an agent asks for
/// a picture of the stage in.
///
/// Every variant is defined against a [`Framing`], so "front" means the same
/// distance-to-fit whatever the venue's size. The snake_case names are the
/// public API (`luma.venue.render(view="quarter_left")`), so they are stable:
/// [`View::name`] and the [`FromStr`](std::str::FromStr) impl are inverses over [`View::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum View {
    /// The rig from the audience side, fitted — how a venue opens.
    Front,
    /// Eye height on the floor at the back of the framed region, looking at the
    /// rig: what someone standing in the room sees, beams overhead included.
    Audience,
    /// Plan view from just off vertical, fitted. Reads positions, not beams.
    Overhead,
    /// Front azimuth swung a quarter turn towards the audience's left.
    QuarterLeft,
    /// Front azimuth swung a quarter turn towards the audience's right.
    QuarterRight,
    /// Eye height on the booth's bearing, looking at the rig — the operator's
    /// own view, backed off to the booth's own distance or further if that is
    /// inside the light. With no booth position supplied this is the reverse
    /// of [`View::Front`] (the rig seen from behind), because a booth is
    /// behind the rig far more often than it is in front of it.
    Dj,
}

impl View {
    /// Every view, in the order they are offered.
    pub const ALL: [View; 6] = [
        View::Front,
        View::Audience,
        View::Overhead,
        View::QuarterLeft,
        View::QuarterRight,
        View::Dj,
    ];

    /// The stable snake_case name. Inverse of the [`FromStr`](std::str::FromStr) impl.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            View::Front => "front",
            View::Audience => "audience",
            View::Overhead => "overhead",
            View::QuarterLeft => "quarter_left",
            View::QuarterRight => "quarter_right",
            View::Dj => "dj",
        }
    }
}

impl std::fmt::Display for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a frame is seen from: one of the framed [`View`]s, or one fixture's
/// own head.
///
/// The two are one vocabulary with one spelling — `"front"`, `"pov:mover-3"` —
/// because they answer the same request (`luma.venue.render(view=...)`) and a
/// caller that had to know which kind it was asking for would be holding the
/// renderer's implementation, not its camera.
///
/// [`Display`](std::fmt::Display) and [`FromStr`](std::str::FromStr) are
/// inverses; the fixture id is not validated here, because the venue that would
/// know is not in this crate.
/// Exhaustive on purpose, unlike [`View`]: the two cases are the whole model —
/// a camera that frames the rig, or a camera that *is* one of its lights — and
/// a consumer forced to write an unreachable third arm would be carrying the
/// cost of a variant nobody has.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Viewpoint {
    /// A named camera fitted to the whole rig.
    Framed(View),
    /// One fixture's head, looking along its beam with the head parked.
    Pov(String),
}

/// What a [`Viewpoint::Pov`] name starts with. One spelling, here.
pub const POV_PREFIX: &str = "pov:";

impl Viewpoint {
    /// The name of the fixture this looks through, if any.
    #[must_use]
    pub fn pov_fixture(&self) -> Option<&str> {
        match self {
            Viewpoint::Pov(id) => Some(id),
            Viewpoint::Framed(_) => None,
        }
    }

    /// The name a fixture's POV is asked for by.
    #[must_use]
    pub fn pov_name(fixture_id: &str) -> String {
        format!("{POV_PREFIX}{fixture_id}")
    }
}

impl From<View> for Viewpoint {
    fn from(view: View) -> Self {
        Viewpoint::Framed(view)
    }
}

impl std::fmt::Display for Viewpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Viewpoint::Framed(view) => f.write_str(view.name()),
            Viewpoint::Pov(id) => write!(f, "{POV_PREFIX}{id}"),
        }
    }
}

impl std::str::FromStr for Viewpoint {
    type Err = UnknownView;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_prefix(POV_PREFIX) {
            Some("") | None => View::from_str(s).map(Viewpoint::Framed),
            Some(id) => Ok(Viewpoint::Pov(id.to_string())),
        }
    }
}

/// The name was not one of [`View::ALL`]. Carries the list, because the caller
/// is nearly always about to tell a human what they could have said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownView(pub String);

impl std::fmt::Display for UnknownView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown view {:?}; expected one of ", self.0)?;
        for view in View::ALL {
            write!(f, "{}, ", view.name())?;
        }
        write!(f, "or {POV_PREFIX}<fixture id>")
    }
}

impl std::error::Error for UnknownView {}

impl std::str::FromStr for View {
    type Err = UnknownView;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        View::ALL
            .into_iter()
            .find(|view| view.name() == s)
            .ok_or_else(|| UnknownView(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aabb::Aabb;
    use crate::framing::{Beam, Insets};

    #[test]
    fn spherical_position_is_z_up() {
        let c = Camera {
            target: Vec3::ZERO,
            radius: 2.0,
            azimuth: 0.0,
            polar: 0.0,
            ..Default::default()
        };
        // Straight overhead, clamped just off the pole.
        assert!(c.position().abs_diff_eq(Vec3::new(0.002, 0.0, 2.0), 1e-3));
    }

    #[test]
    fn reverse_z_maps_near_to_one_and_far_to_zero() {
        let c = Camera::default();
        let p = c.projection(16.0 / 9.0);
        let near = p.project_point3(Vec3::new(0.0, 0.0, -c.znear));
        let far = p.project_point3(Vec3::new(0.0, 0.0, -1.0e7));
        assert!((near.z - 1.0).abs() < 1e-5, "near z = {}", near.z);
        assert!(far.z.abs() < 1e-5, "far z = {}", far.z);
    }

    #[test]
    fn centre_ray_points_at_the_target() {
        let c = Camera::default();
        let ray = c.ray(Vec2::ZERO, 1.5);
        let to_target = (c.target - c.position()).normalize();
        assert!(ray.dir.abs_diff_eq(to_target, 1e-4));
        assert!((ray.origin.distance(c.position()) - c.znear).abs() < 1e-3);
    }

    /// A rig that is not centred on the origin and not symmetric, so a view
    /// that quietly assumed either would show up. It has a deck, so its box
    /// reaches the floor.
    fn rig() -> Framing {
        let down = |p| Beam {
            origin: p,
            direction: Vec3::NEG_Z,
        };
        Framing::of(
            [
                down(Vec3::new(-3.0, 6.0, 5.5)),
                down(Vec3::new(1.0, 6.0, 5.5)),
                down(Vec3::new(4.0, 7.5, 5.5)),
                down(Vec3::new(0.5, 9.0, 2.0)),
            ],
            [Aabb::new(
                Vec3::new(-2.0, 8.0, 0.0),
                Vec3::new(2.0, 9.5, 1.2),
            )],
        )
    }

    /// The frames a view has to survive: square, landscape pane, portrait
    /// phone, and a landscape pane with chrome over both its edges.
    fn finders() -> [Viewfinder; 4] {
        [
            Viewfinder::new(50.0, 1.0),
            Viewfinder::new(50.0, 16.0 / 9.0),
            Viewfinder::new(50.0, 9.0 / 16.0),
            Viewfinder::new(50.0, 16.0 / 9.0).inset(Insets::vertical(0.14, 0.10)),
        ]
    }

    /// The usable NDC rectangle a fitted rig has to land inside, insets and
    /// margin included.
    fn usable(view: &Viewfinder) -> (f32, f32, f32, f32) {
        let keep = 1.0 - Framing::MARGIN;
        let (i, hx) = (
            view.insets,
            (1.0 - view.insets.left - view.insets.right) * keep,
        );
        let hy = (1.0 - i.top - i.bottom) * keep;
        (
            i.left - i.right - hx,
            i.left - i.right + hx,
            i.bottom - i.top - hy,
            i.bottom - i.top + hy,
        )
    }

    /// The primary test. Every orbit view, in every frame, puts every framed
    /// point inside the part of that frame no chrome covers.
    ///
    /// The standing views are exempt by construction and say so in
    /// [`Camera::for_view`]: an eye pinned to head height inside the room
    /// frames what is in front of it. `audience` is checked separately below
    /// for the weaker property it does promise.
    #[test]
    fn orbit_views_fit_the_usable_frame() {
        let framing = rig();
        for view in finders() {
            let (xlo, xhi, ylo, yhi) = usable(&view);
            for name in View::ALL {
                if matches!(name, View::Audience | View::Dj) {
                    continue;
                }
                let camera = Camera::for_view(name, &framing, None, &view);
                for point in framing.points() {
                    let ndc = camera.project(point, view.aspect);
                    assert!(
                        (xlo - 1e-3..=xhi + 1e-3).contains(&ndc.x)
                            && (ylo - 1e-3..=yhi + 1e-3).contains(&ndc.y)
                            && (0.0..=1.0).contains(&ndc.z),
                        "{name} in {view:?}: point {point:?} projects to {ndc:?}, \
                         usable x {xlo}..{xhi} y {ylo}..{yhi}"
                    );
                }
            }
        }
    }

    /// The fit is *tight*: pulling the camera 10% closer pushes a framed point
    /// out. A fit that merely satisfied the assertion above could do so from
    /// orbit.
    #[test]
    fn the_fit_is_the_smallest_distance_that_works() {
        let framing = rig();
        for view in finders() {
            let (xlo, xhi, ylo, yhi) = usable(&view);
            let mut camera = Camera::for_view(View::Front, &framing, None, &view);
            camera.radius *= 0.9;
            let escaped = framing.points().any(|c| {
                let ndc = camera.project(c, view.aspect);
                !(xlo..=xhi).contains(&ndc.x) || !(ylo..=yhi).contains(&ndc.y)
            });
            assert!(escaped, "{view:?}: a 10% dolly in lost nothing");
        }
    }

    /// Chrome costs distance, and nothing else. A frame with bands over it
    /// frames the same rig from further back than a bare one of the same shape.
    #[test]
    fn chrome_pushes_the_camera_back() {
        let framing = rig();
        let bare = Camera::for_view(
            View::Front,
            &framing,
            None,
            &Viewfinder::new(50.0, 16.0 / 9.0),
        );
        let chromed = Camera::for_view(
            View::Front,
            &framing,
            None,
            &Viewfinder::new(50.0, 16.0 / 9.0).inset(Insets::vertical(0.14, 0.10)),
        );
        assert!(
            chromed.radius > bare.radius,
            "{} vs {}",
            chromed.radius,
            bare.radius
        );
        // The visible band sits low in a top-heavy pane, so the camera aims
        // *higher* to drop the rig into it.
        assert!(chromed.target.z > bare.target.z - 1e-4);
    }

    #[test]
    fn audience_stands_back_far_enough_to_see_the_rig() {
        let framing = rig();
        for view in finders() {
            let camera = Camera::for_view(View::Audience, &framing, None, &view);
            assert!(
                framing.fits(camera.position(), framing.target(), &view),
                "{view:?}: the audience is too close"
            );
        }
    }

    #[test]
    fn every_view_keeps_its_eye_above_the_floor() {
        let framing = rig();
        for view in finders() {
            for name in View::ALL {
                let camera =
                    Camera::for_view(name, &framing, Some(Vec3::new(0.0, -4.0, 0.0)), &view);
                assert!(
                    camera.position().z > framing.floor_z(),
                    "{name} eye at {:?}",
                    camera.position()
                );
                assert!(camera.radius >= Camera::MIN_RADIUS, "{name} radius");
            }
        }
    }

    /// With no chrome there is no aim shift, so every view looks at a point in
    /// the rig itself.
    #[test]
    fn every_view_targets_a_point_in_the_bounds() {
        let framing = rig();
        let view = Viewfinder::new(50.0, 16.0 / 9.0);
        let (min, max) = (framing.bounds().min, framing.bounds().max);
        for name in View::ALL {
            let t = Camera::for_view(name, &framing, None, &view).target;
            assert!(
                t.cmpge(min - 1e-4).all() && t.cmple(max + 1e-4).all(),
                "{name} target {t:?} outside [{min:?}, {max:?}]"
            );
        }
    }

    #[test]
    fn quarter_views_straddle_the_front() {
        let framing = rig();
        let view = Viewfinder::new(50.0, 16.0 / 9.0);
        let at = |name| Camera::for_view(name, &framing, None, &view).azimuth;
        let (front, left, right) = (
            at(View::Front),
            at(View::QuarterLeft),
            at(View::QuarterRight),
        );
        assert!((front - left - std::f32::consts::FRAC_PI_4).abs() < 1e-4);
        assert!((right - front - std::f32::consts::FRAC_PI_4).abs() < 1e-4);
    }

    #[test]
    fn dj_without_a_booth_is_the_reverse_of_front() {
        let framing = rig();
        let view = Viewfinder::new(50.0, 1.0);
        let front = Camera::for_view(View::Front, &framing, None, &view);
        let dj = Camera::for_view(View::Dj, &framing, None, &view);
        let turn = (dj.azimuth - front.azimuth).abs();
        assert!((turn - std::f32::consts::PI).abs() < 1e-4, "turn {turn}");
    }

    /// A booth far enough out is taken literally: the operator's own eye.
    #[test]
    fn a_distant_booth_puts_the_eye_at_head_height_on_it() {
        let framing = rig();
        let view = Viewfinder::new(50.0, 1.0);
        // Well outside the rig, on the audience side of it.
        let booth = Vec3::new(0.5, -40.0, 0.0);
        let dj = Camera::for_view(View::Dj, &framing, Some(booth), &view);
        let expected = booth.with_z(framing.floor_z() + EYE_HEIGHT_M);
        assert!(
            dj.position().abs_diff_eq(expected, 1e-3),
            "{:?} vs {expected:?}",
            dj.position()
        );
        let audience = Camera::for_view(View::Audience, &framing, None, &view);
        assert!((audience.position().z - framing.floor_z() - EYE_HEIGHT_M).abs() < 1e-4);
    }

    /// The regression both real venues showed: a booth a metre from the rig
    /// puts the eye inside the beams and renders one white rectangle. The
    /// bearing is the booth's, the distance is whatever it takes to see.
    #[test]
    fn a_booth_inside_the_rig_backs_off_along_its_own_bearing() {
        let framing = rig();
        let view = Viewfinder::new(50.0, 16.0 / 9.0);
        let booth = Vec3::new(0.5, 7.0, 0.0);
        let dj = Camera::for_view(View::Dj, &framing, Some(booth), &view);
        let target = framing.target();
        assert!(
            framing.fits(dj.position(), target, &view),
            "{:?}",
            dj.position()
        );
        let bearing = (booth - target).truncate().normalize();
        let eye = (dj.position() - target).truncate().normalize();
        assert!(eye.abs_diff_eq(bearing, 1e-3), "{eye:?} vs {bearing:?}");
        assert!((dj.position().z - framing.floor_z() - EYE_HEIGHT_M).abs() < 1e-4);
    }

    #[test]
    fn view_names_round_trip() {
        for view in View::ALL {
            assert_eq!(view.name().parse::<View>(), Ok(view));
            assert_eq!(view.name(), view.to_string());
        }
        assert!("sideways".parse::<View>().is_err());
    }

    #[test]
    fn project_round_trips_through_ray() {
        let c = Camera::default();
        let aspect = 1.777;
        let world = Vec3::new(1.0, -2.0, 0.5);
        let ndc = c.project(world, aspect);
        let ray = c.ray(ndc.truncate(), aspect);
        let t = ray.t_of(world);
        assert!(ray.at(t).abs_diff_eq(world, 1e-3));
    }
}
