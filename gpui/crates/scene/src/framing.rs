//! What a camera has to fit: the extent of a rig, and the closed-form solve
//! for the distance that fits it.
//!
//! One rig has one framing, and every camera that looks at it — the opening
//! pose, the orbit's dolly limits, each of the named [`View`]s — is derived
//! from it. Keeping the rule here rather than at the call sites is what makes
//! "the same venue from the front" mean the same thing in the desktop viewport
//! and in an agent's `luma.venue.render(view="front")`.
//!
//! # Why fitting is a solve, and why it runs on points
//!
//! A sphere is the same size from every direction, so fitting one ignores the
//! shape of the rig and the shape of the window at once: a wide truss in a wide
//! pane gets framed as though it were a ball, and pulls the camera back until
//! the picture is mostly empty floor. [`Framing::fit`] instead asks the only
//! question that matters — *at what distance does every framed point land
//! inside this frame* — and answers it in closed form, per direction and per
//! aspect ratio. Aspect is therefore an input to every view.
//!
//! A box is the same mistake one order of magnitude smaller. Collapsing the rig
//! to an AABB invents content in the corners it does not occupy: a club whose
//! lit volume runs across the middle of a 5.9 × 6.4 m plan gets fitted to the
//! empty near corner of that plan, and the picture comes back a third full. So
//! the extent stays a point cloud all the way into the solve, and the AABB
//! survives only where a *summary* is what is wanted — where to aim, how big
//! the rig is, where its floor is.
//!
//! [`View`]: crate::camera::View

use glam::Vec3;

use crate::aabb::Aabb;
use crate::camera::Camera;

/// The extent a camera has to fit, in render-world space (Z-up): the cloud of
/// points every view has to keep on screen, and the box that summarises them.
///
/// The extent is **the rig and its light**: every fixture head, every point
/// those heads throw light at (see [`Beam`]), and the pieces the rig hangs on.
/// It is deliberately not the room. A venue's guardrails, speakers, decks and
/// booth furniture are drawn but not framed, because they are metres wider
/// than anything that lights up and fitting them puts the show in the middle
/// of the picture at half size — measured at 30% of the frame on a real club
/// rig whose light occupied two thirds of it.
///
/// Framing the beams and not just the hardware is what makes the rule hold in
/// both directions: a bank of movers over a stage lights the floor below its
/// own box, and a row of floor uplighters lights the air above it. Neither is
/// in frame if only the heads are.
///
/// The cloud may repeat points and is in no order; nothing downstream cares,
/// and deduplicating it would cost more than the handful of extra terms the
/// fold takes.
#[derive(Clone, Debug, PartialEq)]
pub struct Framing {
    /// Everything the fit has to keep in frame.
    points: Vec<Vec3>,
    /// The AABB of `points`. Derived once, and only ever read for a summary —
    /// where to aim, how big the rig is, where its floor is. The fit itself
    /// never touches it; see the module note.
    bounds: Aabb,
}

impl Framing {
    /// Keeps the eye off the +Z pole, where `look_at`'s Z up vector degenerates.
    pub const MIN_POLAR: f32 = 0.12;
    /// Keeps the eye above the target's horizon, and so out of the floor.
    pub const MAX_POLAR: f32 = std::f32::consts::FRAC_PI_2 - 0.03;
    /// Fraction of each half-frame left empty around a fitted rig. Applied to
    /// both axes, so a 16:9 frame gets more absolute margin horizontally than vertically —
    /// which is what "a margin" looks like to an eye.
    pub const MARGIN: f32 = 0.08;
    /// How far outside the extent a dolly may come. Inside it is inside the
    /// beams, where every pixel is one saturated colour.
    const NEAR_MARGIN: f32 = 1.25;
    /// Furthest out a dolly may go, as a multiple of the distance it opened at.
    const FAR_MULTIPLE: f32 = 6.0;
    /// Smallest half-diagonal a rig is treated as having. A one-fixture venue
    /// still needs a scale.
    const MIN_RADIUS: f32 = 1.0;

    /// Half the size of a fixture body, in metres — how far outside its own
    /// lens a head is framed.
    ///
    /// A head is a body hanging off a yoke, not the point its beam leaves
    /// from, and fitting the point alone crops the bracket off the top of
    /// every mover in the rig. One constant rather than the definition's real
    /// dimensions: the framing is a picture, the difference between a 300 mm
    /// par and a 500 mm wash is a few pixels of air, and it buys
    /// [`Framing::of`] independence from the fixture catalogue.
    pub const HEAD_RADIUS: f32 = 0.35;

    /// How far up an unterminated beam is framed, in metres. See [`Beam`].
    ///
    /// It is a taste number, not a physical one: the beam of an uplighter
    /// aimed at the ceiling has no end, so something has to say how much of it
    /// belongs in the picture. Five metres is a room's worth — enough that the
    /// cone reads as a cone rather than as a stub, short enough that ten of
    /// them do not frame the sky.
    pub const BEAM_REACH: f32 = 5.0;

    /// Frame a rig from its beams and the pieces it hangs on, all in world
    /// space.
    ///
    /// Pieces come as boxes rather than points because a truss twelve metres
    /// wide is not where its origin is, and only *rig-bearing* pieces belong
    /// here — see the type's own note.
    ///
    /// A head contributes the eight corners of its body box rather than its
    /// lens alone, and a piece the eight corners of its own — a box's corners
    /// are the only points of it that can leave a frame, so the cloud loses
    /// nothing by carrying just those.
    ///
    /// An empty rig frames a unit box at the origin: nothing is drawn, so only
    /// the units matter.
    #[must_use]
    pub fn of(
        beams: impl IntoIterator<Item = Beam>,
        pieces: impl IntoIterator<Item = Aabb>,
    ) -> Self {
        let mut points = Vec::new();
        for beam in beams {
            let head = Vec3::splat(Self::HEAD_RADIUS);
            points.extend(Aabb::new(beam.origin - head, beam.origin + head).corners());
            points.push(beam.reach());
        }
        for piece in pieces {
            if !piece.is_empty() {
                points.extend(piece.corners());
            }
        }
        if points.is_empty() {
            points.extend(Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0)).corners());
        }
        let bounds = Aabb::from_points(points.iter().copied());
        Self { points, bounds }
    }

    /// Every point a view has to keep on screen.
    pub fn points(&self) -> impl Iterator<Item = Vec3> + '_ {
        self.points.iter().copied()
    }

    /// The box around everything framed.
    ///
    /// A summary, not the thing that is fitted — see the module note on why
    /// the two are different pictures.
    #[must_use]
    pub fn bounds(&self) -> Aabb {
        self.bounds
    }

    /// Height of the floor the rig stands on. Zero for an ordinary room: a
    /// stage with a pit is still one room, so anything reaching below `z = 0`
    /// lowers it.
    #[must_use]
    pub fn floor_z(&self) -> f32 {
        self.bounds.min.z.min(0.0)
    }

    /// Centre of the extent, and what every view aims at.
    ///
    /// There is no second, biased aim point. There used to be — the extent was
    /// the hardware alone, so a rig of movers had to be aimed *below* its own
    /// box to keep the pools on screen. Now that the extent is the rig and its
    /// light, the pools are in the box and its centre is the picture's centre.
    #[must_use]
    pub fn target(&self) -> Vec3 {
        self.bounds.center()
    }

    /// Half the extent's diagonal, never below one metre.
    #[must_use]
    pub fn radius(&self) -> f32 {
        (self.bounds.size().length() * 0.5).max(Self::MIN_RADIUS)
    }

    /// Smallest distance along `direction` at which every framed point fits the
    /// viewfinder's *usable* rectangle.
    ///
    /// `direction` is the unit vector from the aim point *towards the eye*. The
    /// solve is closed form. Write a point's offset from the framing target as
    /// `q`; its depth in front of the eye is `d − q·direction` whatever the aim
    /// shift is, because the shift is perpendicular to `direction`. With the
    /// aim placed so the target lands at the usable rectangle's centre `c` and
    /// its half-extent `h` (see [`Viewfinder`]), a point is inside the frame
    /// exactly when
    ///
    /// ```text
    /// d ≥ (q·direction)·(c + h)/h + (q·right)/(h·tan(fovx/2))
    /// d ≥ (q·direction)·(h − c)/h − (q·right)/(h·tan(fovx/2))
    /// ```
    ///
    /// and likewise on the up axis. Four bounds per point, and the answer is
    /// their maximum — no search, no iteration. With no insets, `c = 0` and
    /// `h = 1 − margin`, and the pair collapses to the symmetric
    /// `d ≥ q·direction + |q·right|/(h·tan(fovx/2))`.
    ///
    /// The result is never closer than one near plane in front of the furthest
    /// point, so nothing the camera is fitting is ever behind it.
    #[must_use]
    pub fn required_distance(&self, target: Vec3, direction: Vec3, view: &Viewfinder) -> f32 {
        let direction = direction.normalize_or(Vec3::NEG_Y);
        let (right, up) = frame_axes(direction);
        let (cx, hx) = view.horizontal();
        let (cy, hy) = view.vertical();
        let (tx, ty) = view.tangents();
        self.points().fold(Camera::MIN_RADIUS, |d, point| {
            let q = point - target;
            let along = q.dot(direction);
            let (u, v) = (q.dot(right), q.dot(up));
            d.max(along * (cx + hx) / hx + u / (hx * tx))
                .max(along * (hx - cx) / hx - u / (hx * tx))
                .max(along * (cy + hy) / hy + v / (hy * ty))
                .max(along * (hy - cy) / hy - v / (hy * ty))
                .max(along + Camera::MIN_RADIUS)
        })
    }

    /// Where a camera `distance` away has to *aim* for the framing target to
    /// land at the centre of the usable rectangle.
    ///
    /// Zero whenever the insets are symmetric — which includes the headless
    /// case, where there is no chrome at all.
    #[must_use]
    pub fn aim(&self, target: Vec3, direction: Vec3, view: &Viewfinder, distance: f32) -> Vec3 {
        let direction = direction.normalize_or(Vec3::NEG_Y);
        let (right, up) = frame_axes(direction);
        let (tx, ty) = view.tangents();
        target
            - right * (view.horizontal().0 * tx * distance)
            - up * (view.vertical().0 * ty * distance)
    }

    /// The camera at [`Framing::required_distance`] along `direction`, aimed so
    /// the rig sits centred in whatever the chrome leaves visible.
    ///
    /// This is the fit rule. The two standing views ([`View::Audience`],
    /// [`View::Dj`]) deliberately do *not* use it — their eye is pinned to a
    /// height, so their direction is a function of the distance being solved
    /// for and the closed form does not apply. See [`Framing::fits`].
    ///
    /// [`View::Audience`]: crate::camera::View::Audience
    /// [`View::Dj`]: crate::camera::View::Dj
    #[must_use]
    pub fn fit(&self, target: Vec3, direction: Vec3, view: &Viewfinder) -> Camera {
        let direction = direction.normalize_or(Vec3::NEG_Y);
        let d = self.required_distance(target, direction, view);
        let aim = self.aim(target, direction, view, d);
        Camera::looking_from(aim + direction * d, aim, view.fov_y_deg)
    }

    /// Whether every framed point is inside the frame of a camera at `eye`.
    ///
    /// The predicate behind the standing views: it is monotone in how far the
    /// eye is from the target, which is what lets a distance along the floor be
    /// bisected for. It measures from the *unshifted* target, because the shift
    /// is a function of the distance being tested; the caller applies
    /// [`Framing::aim`] once, at the end.
    #[must_use]
    pub fn fits(&self, eye: Vec3, target: Vec3, view: &Viewfinder) -> bool {
        let offset = eye - target;
        let distance = offset.length();
        distance >= self.required_distance(target, offset / distance.max(1e-6), view)
    }

    /// Radii a dolly may reach, given the distance the view opened at: never
    /// inside the rig, never so far out that the scene is a speck with no way
    /// back.
    #[must_use]
    pub fn radius_bounds(&self, fitted: f32) -> (f32, f32) {
        let near = (self.radius() * Self::NEAR_MARGIN).max(0.5);
        (near, (fitted * Self::FAR_MULTIPLE).max(near * 2.0))
    }

    /// The polar range an *orbit* may reach — off the pole, above the horizon.
    ///
    /// Named views are not bound by it (an audience eye looks *up* at a truss),
    /// which is why this is a verb the orbit calls rather than a clamp baked
    /// into [`Camera`].
    #[must_use]
    pub fn clamp_polar(polar: f32) -> f32 {
        polar.clamp(Self::MIN_POLAR, Self::MAX_POLAR)
    }
}

/// Where one fixture head is and where its light goes, in world space.
///
/// A beam is the framing primitive rather than the head position, because a
/// head's position says nothing about what it lights: a mover over a stage and
/// an uplighter on the floor in front of it are a metre apart and their
/// pictures are three metres apart in opposite directions. Two points per head
/// — the lens and what it reaches — bracket that with no knowledge of cone
/// angle, throw or intensity, none of which a camera needs.
///
/// The direction is whatever the renderer would draw the cone along at the
/// state the scene pins, so the framed beam and the drawn beam cannot disagree.
/// A pool is a disc and this is its centre; the radius is deliberately not
/// modelled, and [`Framing::MARGIN`] absorbs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beam {
    /// The head, at the lens.
    pub origin: Vec3,
    /// Unit vector the light leaves along. Need not be normalized.
    pub direction: Vec3,
}

impl Beam {
    /// The far point of the beam: where it crosses the floor plane, or
    /// [`Framing::BEAM_REACH`] along it when it never does.
    ///
    /// The floor plane is `z = 0` — the one the renderer draws, not
    /// [`Framing::floor_z`], which is derived from the extent this is helping
    /// to build. A beam that lands is followed all the way down however high
    /// its head is: the pool is where the picture is, and a reach that clipped
    /// it would frame a stub of cone hanging in the air. Only a beam that
    /// *never* lands — level, or aimed up — is truncated, because an
    /// uplighter's picture is the column of air over it and framing nothing
    /// there loses the shot. A zero direction reaches nowhere, which is how a
    /// head with no beam is framed.
    #[must_use]
    pub fn reach(self) -> Vec3 {
        let direction = self.direction.normalize_or_zero();
        let travel = if direction.z < -FLOOR_GRAZE {
            (-self.origin.z / direction.z).max(0.0)
        } else {
            Framing::BEAM_REACH
        };
        self.origin + direction * travel
    }
}

/// How steeply down a beam has to point to be treated as hitting the floor at
/// all. A beam a hair off level meets `z = 0` hundreds of metres away, which is
/// the same answer as never for every purpose here.
const FLOOR_GRAZE: f32 = 0.05;

/// Edges of the frame that something else is drawn over, as fractions of the
/// frame's own width or height.
///
/// A floating toolbar across the bottom of a viewport is not a smaller
/// viewport: the renderer still fills the pane, so a rig fitted to the whole
/// frame is fitted partly under the chrome. Declaring the covered bands lets
/// the fit spend its distance on what stays visible. A headless render — the
/// agent's `luma.venue.render(...)` — has no chrome and passes [`Insets::NONE`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    /// Fraction of the frame's height covered at the top.
    pub top: f32,
    /// Fraction of the frame's height covered at the bottom.
    pub bottom: f32,
    /// Fraction of the frame's width covered on the left.
    pub left: f32,
    /// Fraction of the frame's width covered on the right.
    pub right: f32,
}

impl Insets {
    /// Nothing is drawn over the frame.
    pub const NONE: Insets = Insets {
        top: 0.0,
        bottom: 0.0,
        left: 0.0,
        right: 0.0,
    };

    /// Chrome along the top and bottom edges only, which is what a viewport
    /// with a stats overlay and a floating toolbar has.
    #[must_use]
    pub fn vertical(top: f32, bottom: f32) -> Self {
        Self {
            top,
            bottom,
            ..Self::NONE
        }
    }
}

/// The frame a rig is being fitted into: how wide the lens is, how wide the
/// frame is, what covers its edges, and how much air to leave around the rig.
///
/// It travels as one value because every one of the four is needed to answer a
/// single question — *where does the camera go* — and a fit missing any of them
/// is wrong rather than approximate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewfinder {
    /// Vertical field of view, in degrees.
    pub fov_y_deg: f32,
    /// Frame width over frame height.
    pub aspect: f32,
    /// What is drawn over the frame's edges.
    pub insets: Insets,
    /// Fraction of the *usable* half-frame left empty on each side, as visual
    /// breathing room. Applies on top of the insets, not instead of them.
    pub margin: f32,
}

impl Viewfinder {
    /// A bare frame: no chrome, the standard margin.
    #[must_use]
    pub fn new(fov_y_deg: f32, aspect: f32) -> Self {
        Self {
            fov_y_deg,
            aspect,
            insets: Insets::NONE,
            margin: Framing::MARGIN,
        }
    }

    /// The same frame with chrome over its edges.
    #[must_use]
    pub fn inset(mut self, insets: Insets) -> Self {
        self.insets = insets;
        self
    }

    /// `(centre, half-extent)` of the usable rectangle on the horizontal axis,
    /// in NDC. The centre is *not* reduced by the margin — the margin shrinks
    /// the rectangle, it does not move it.
    #[must_use]
    fn horizontal(&self) -> (f32, f32) {
        let (l, r) = (self.insets.left, self.insets.right);
        (
            l - r,
            ((1.0 - l - r) * (1.0 - self.margin)).max(MIN_HALF_FRAME),
        )
    }

    /// The same on the vertical axis. NDC +Y is up, so `top` raises the low
    /// edge of the usable rectangle and `bottom` lowers its high edge.
    #[must_use]
    fn vertical(&self) -> (f32, f32) {
        let (t, b) = (self.insets.top, self.insets.bottom);
        (
            b - t,
            ((1.0 - t - b) * (1.0 - self.margin)).max(MIN_HALF_FRAME),
        )
    }

    /// `(tan(fovx/2), tan(fovy/2))` for the whole frame, chrome included.
    #[must_use]
    fn tangents(&self) -> (f32, f32) {
        let ty = (self.fov_y_deg.to_radians() * 0.5).tan().max(1e-3);
        (ty * self.aspect.max(1e-3), ty)
    }
}

/// Smallest usable half-frame the fit will accept. Chrome covering the whole
/// pane is a layout bug, not a camera that has to retreat to infinity.
const MIN_HALF_FRAME: f32 = 0.05;

impl Default for Framing {
    /// The scale to use before a rig has loaded — nothing is drawn at that
    /// point, so only the units matter.
    fn default() -> Self {
        Self::of(
            [],
            [Aabb::new(
                Vec3::new(-4.0, -4.0, 0.0),
                Vec3::new(4.0, 4.0, 4.0),
            )],
        )
    }
}

/// The right and up axes of a camera whose eye lies along `direction` from its
/// target, with world +Z up — the same basis `Mat4::look_at_rh` builds, spelled
/// out here because the fit solve needs the axes rather than the matrix.
fn frame_axes(direction: Vec3) -> (Vec3, Vec3) {
    let forward = -direction;
    // Straight down (or up) has no distinct right vector under a Z up; +X is as
    // good as any other and matches the polar clamp the orbit uses.
    let right = forward.cross(Vec3::Z).normalize_or(Vec3::X);
    (right, right.cross(forward).normalize_or(Vec3::Y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(origin: Vec3) -> Beam {
        Beam {
            origin,
            direction: Vec3::NEG_Z,
        }
    }

    fn rig() -> Framing {
        Framing::of(
            [
                down(Vec3::new(-3.0, 6.0, 5.5)),
                down(Vec3::new(1.0, 6.0, 5.5)),
                down(Vec3::new(4.0, 7.5, 5.5)),
            ],
            [Aabb::new(
                Vec3::new(-1.0, 8.0, 0.0),
                Vec3::new(1.0, 9.0, 1.2),
            )],
        )
    }

    /// The whole point of framing beams rather than heads: a rig of bare
    /// movers has nothing under it, and its box still reaches what it lights.
    #[test]
    fn a_beam_pulls_the_box_down_to_its_pool() {
        let f = Framing::of([down(Vec3::new(0.0, 0.0, 6.0))], []);
        // The pool sets the bottom; the head plus its body sets the top.
        assert!(f.bounds().min.z.abs() < 1e-5, "{:?}", f.bounds());
        assert!((f.bounds().max.z - (6.0 + Framing::HEAD_RADIUS)).abs() < 1e-5);
    }

    /// ...and the mirror case, which no bias could ever have covered: floor
    /// uplighters light the air above themselves.
    #[test]
    fn an_up_beam_pulls_the_box_up_by_its_reach() {
        let f = Framing::of(
            [Beam {
                origin: Vec3::new(0.0, 0.0, 0.2),
                direction: Vec3::Z,
            }],
            [],
        );
        assert!((f.bounds().max.z - (0.2 + Framing::BEAM_REACH)).abs() < 1e-5);
        assert!(f.floor_z() >= -Framing::HEAD_RADIUS);
    }

    /// A beam that grazes level would meet the floor plane hundreds of metres
    /// away; the reach truncates it instead of framing the horizon.
    #[test]
    fn a_grazing_beam_is_truncated_rather_than_chased() {
        let f = Framing::of(
            [Beam {
                origin: Vec3::new(0.0, 0.0, 4.0),
                direction: Vec3::new(0.0, -1.0, -0.01),
            }],
            [],
        );
        assert!(f.bounds().size().length() < 3.0 * Framing::BEAM_REACH);
        assert!((f.bounds().min.y + Framing::BEAM_REACH).abs() < 0.1);
    }

    #[test]
    fn a_pit_lowers_the_floor_but_a_riser_does_not() {
        let pit = Framing::of(
            [down(Vec3::new(0.0, 0.0, 4.0))],
            [Aabb::new(Vec3::splat(-1.0), Vec3::ZERO)],
        );
        assert!((pit.floor_z() + 1.0).abs() < 1e-5);
        let riser = Framing::of(
            [down(Vec3::new(0.0, 0.0, 4.0))],
            [Aabb::new(Vec3::new(-1.0, -1.0, 1.0), Vec3::splat(2.0))],
        );
        assert!(riser.floor_z().abs() < 1e-5);
    }

    #[test]
    fn a_narrower_frame_needs_more_distance() {
        let f = rig();
        let dir = Vec3::new(0.0, -3.0, 1.0).normalize();
        let wide = f.required_distance(f.target(), dir, &Viewfinder::new(50.0, 16.0 / 9.0));
        let tall = f.required_distance(f.target(), dir, &Viewfinder::new(50.0, 9.0 / 16.0));
        assert!(tall > wide, "{tall} should exceed {wide}");
    }

    /// The inset solve has to *be* the old symmetric one when there is nothing
    /// over the frame — that is what makes the headless path free of chrome
    /// arithmetic rather than merely close to it.
    #[test]
    fn no_insets_reproduces_the_symmetric_fit() {
        let f = rig();
        let view = Viewfinder::new(50.0, 16.0 / 9.0);
        let dir = Vec3::new(0.0, -3.0, 1.0).normalize();
        let (_, right) = (Vec3::ZERO, frame_axes(dir).0);
        let up = frame_axes(dir).1;
        let (tx, ty) = view.tangents();
        let keep = 1.0 - view.margin;
        let target = f.target();
        let symmetric = f.points().fold(Camera::MIN_RADIUS, |d, c| {
            let q = c - target;
            let along = q.dot(dir);
            d.max(along + q.dot(right).abs() / (keep * tx))
                .max(along + q.dot(up).abs() / (keep * ty))
                .max(along + Camera::MIN_RADIUS)
        });
        let solved = f.required_distance(target, dir, &view);
        assert!((solved - symmetric).abs() < 1e-3, "{solved} vs {symmetric}");
        // ...and with nothing to shift for, the aim is the target itself.
        assert!(f.aim(target, dir, &view, solved).abs_diff_eq(target, 1e-5));
    }

    #[test]
    fn an_empty_rig_still_has_a_scale() {
        let f = Framing::of([], []);
        assert!(f.radius() >= 1.0);
        assert!(f
            .required_distance(Vec3::ZERO, Vec3::NEG_Y, &Viewfinder::new(50.0, 1.0))
            .is_finite());
    }

    /// Why the fit runs on points and not on the box. An L-shaped rig leaves
    /// one quadrant of its own plan empty, and a fit that believed in the box
    /// would spend distance framing that air — which is exactly what a real
    /// club did: a 5.9 x 6.4 m plan whose light ran across the middle of it,
    /// framed at a third of the frame.
    #[test]
    fn an_l_shaped_rig_fits_closer_than_its_box() {
        let arm =
            |along: Vec3| (0..5).map(move |i| down(along * i as f32 + Vec3::new(0.0, 0.0, 5.0)));
        let l = Framing::of(arm(Vec3::X).chain(arm(Vec3::Y)), []);
        // The same rig collapsed to the intermediate the fit used to run on.
        let boxed = Framing::of([], [l.bounds()]);
        assert_eq!(boxed.bounds(), l.bounds());

        let view = Viewfinder::new(50.0, 16.0 / 9.0);
        // Down the diagonal whose near corner is the empty one.
        let dir = Vec3::splat(1.0).normalize();
        let target = l.target();
        let points = l.required_distance(target, dir, &view);
        let whole_box = boxed.required_distance(target, dir, &view);
        assert!(points < whole_box * 0.9, "{points} vs {whole_box}");
    }

    /// Chrome over the whole pane is a layout bug, not a camera at infinity.
    #[test]
    fn chrome_covering_everything_does_not_send_the_camera_to_infinity() {
        let f = rig();
        let view = Viewfinder::new(50.0, 1.0).inset(Insets::vertical(0.6, 0.6));
        let d = f.required_distance(f.target(), Vec3::NEG_Y, &view);
        assert!(d.is_finite() && d < 1.0e4, "{d}");
    }
}
