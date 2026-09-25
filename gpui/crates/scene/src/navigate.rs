//! Stage-camera navigation: orbit and zoom about the point under the pointer,
//! and a fly mode for the held right button.
//!
//! The camera stays spherical (see [`crate::camera`]). Every verb here moves
//! the eye and the target together and hands the pose back through
//! [`Camera::looking_from`], or slides the target along the view with the
//! angles untouched — so no verb stores a position beside the angles.
//!
//! # Why the pivot is picked, and why from an area
//!
//! An orbit about the rig's centre turns the camera about a point the operator
//! is usually not looking at, and a zoom that scales the distance to it slows
//! to nothing as it arrives. So a drag or a wheel step first asks what is under
//! the pointer ([`area_pick`]) and turns or zooms about that. A truss is mostly
//! air, and a single ray through a gap finds the floor metres behind it; a
//! small disc of rays finds the truss the pointer is visibly on.

use glam::{Quat, Vec2, Vec3};

use crate::bvh::Ray;
use crate::camera::{Camera, MIN_EYE_Z};
use crate::framing::Framing;

/// Radius of the pointer's pick area, in logical pixels.
pub const PICK_RADIUS_PX: f32 = 12.0;

/// A pick further than this multiple of the current pivot distance does not
/// size the zoom step. See [`Camera::zoom_toward`].
pub const JUMP_LIMIT: f32 = 2.0;

/// What a pick ray met.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// Truss, fixtures and the other pieces of the rig: what the operator
    /// inspects.
    Rig,
    /// The floor, a deck, the ground: what the rig stands on.
    Floor,
}

/// One ray's nearest hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceHit {
    pub point: Vec3,
    /// From the ray's origin, in metres.
    pub distance: f32,
    pub surface: Surface,
}

/// Pixel offsets of the pick rays around the pointer: the pointer itself, a
/// ring at half the radius and a ring at the full [`PICK_RADIUS_PX`].
///
/// The outer ring's spacing is about six pixels, finer than a truss chord at
/// any distance the operator zooms from.
pub fn pick_offsets() -> impl Iterator<Item = Vec2> {
    let ring = |count: u32, radius: f32| {
        (0..count).map(move |i| {
            let angle = std::f32::consts::TAU * i as f32 / count as f32;
            Vec2::new(angle.cos(), angle.sin()) * radius
        })
    };
    std::iter::once(Vec2::ZERO)
        .chain(ring(8, PICK_RADIUS_PX * 0.5))
        .chain(ring(12, PICK_RADIUS_PX))
}

/// The pivot among the nearest hits of `rays`, which `cast` answers one ray at
/// a time.
///
/// The rig wins: the nearest rig hit anywhere in the area is the pivot, even
/// where the pointer's own ray went through a gap to the floor. The floor is
/// the pivot only when no ray met the rig. `None` when nothing was hit.
pub fn area_pick(
    rays: impl IntoIterator<Item = Ray>,
    mut cast: impl FnMut(Ray) -> Option<SurfaceHit>,
) -> Option<SurfaceHit> {
    let (mut rig, mut floor) = (None::<SurfaceHit>, None::<SurfaceHit>);
    for hit in rays.into_iter().filter_map(&mut cast) {
        let best = match hit.surface {
            Surface::Rig => &mut rig,
            Surface::Floor => &mut floor,
        };
        if best.is_none_or(|best| hit.distance < best.distance) {
            *best = Some(hit);
        }
    }
    rig.or(floor)
}

/// Where `ray` meets the ground plane `z = ground_z` from above.
#[must_use]
pub fn ground_hit(ray: Ray, ground_z: f32) -> Option<SurfaceHit> {
    if ray.dir.z >= -1e-6 || ray.origin.z < ground_z {
        return None;
    }
    let distance = (ground_z - ray.origin.z) / ray.dir.z;
    Some(SurfaceHit {
        point: ray.at(distance),
        distance,
        surface: Surface::Floor,
    })
}

/// How far a zoom may go, for one rig. See [`Framing::zoom_limits`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomLimits {
    /// Closest the eye comes to a pivot before it dollies through instead.
    pub near: f32,
    /// Furthest the eye may back away from a pivot.
    pub far: f32,
    /// Smallest distance a step is sized from, so a step never shrinks to
    /// nothing at a surface.
    pub reach: f32,
    /// Floor the eye stays above, by [`MIN_EYE_Z`].
    pub floor_z: f32,
}

/// The keys a fly holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlyKeys {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub down: bool,
    pub up: bool,
    pub fast: bool,
}

impl FlyKeys {
    /// Speed multiple while Shift is held.
    pub const FAST: f32 = 3.0;

    /// Take a key name (`w`, `a`, `s`, `d`, `q`, `e`) as pressed or released.
    /// Whether it named a fly key.
    pub fn set(&mut self, key: &str, down: bool) -> bool {
        let slot = match key {
            "w" => &mut self.forward,
            "s" => &mut self.back,
            "a" => &mut self.left,
            "d" => &mut self.right,
            "q" => &mut self.down,
            "e" => &mut self.up,
            _ => return false,
        };
        *slot = down;
        true
    }

    /// Whether any movement key is held.
    #[must_use]
    pub fn moving(self) -> bool {
        self.forward || self.back || self.left || self.right || self.down || self.up
    }
}

/// Fly speed at the default multiple, in metres per second: half the rig's
/// size, so a fly crosses any rig in a few seconds.
#[must_use]
pub fn fly_speed(framing: &Framing) -> f32 {
    framing.radius() * 0.5
}

impl Camera {
    /// Unit vector the eye looks along.
    #[must_use]
    pub fn forward(&self) -> Vec3 {
        (self.target - self.position()).normalize_or(Vec3::NEG_Z)
    }

    /// The ray through a pixel of a `viewport`-sized pane, `at` measured from
    /// its top-left corner.
    #[must_use]
    pub fn pixel_ray(&self, at: Vec2, viewport: Vec2) -> Ray {
        let ndc = Vec2::new(
            at.x / viewport.x.max(1.0) * 2.0 - 1.0,
            1.0 - at.y / viewport.y.max(1.0) * 2.0,
        );
        self.ray(ndc, viewport.x / viewport.y.max(1.0))
    }

    /// Turn the eye and the target together about `pivot`: `azimuth` about
    /// world +Z and `polar` down from it, the orbit's own angle deltas.
    ///
    /// A rigid turn, so the pivot stays where it is on screen. About the
    /// target it is the plain spherical orbit; about the eye it turns the
    /// camera in place. The view's polar keeps
    /// [`Framing::clamp_orbit_polar`], measured about the pivot, so the eye
    /// stays off the pole and above the floor.
    pub fn orbit_about(&mut self, pivot: Vec3, azimuth: f32, polar: f32, framing: &Framing) {
        let (eye, target) = (self.position(), self.target);
        let polar_now = (eye - target)
            .normalize_or(Vec3::Z)
            .z
            .clamp(-1.0, 1.0)
            .acos();
        let wanted = framing.clamp_orbit_polar(polar_now + polar, pivot.z, eye.distance(pivot));
        let spin = Quat::from_rotation_z(azimuth);
        // Tilting about the horizontal axis square to the view's plane moves
        // the view's polar by exactly the angle, and keeps the horizon level.
        let axis = Vec3::Z.cross(spin * (eye - target)).normalize_or(Vec3::X);
        let turn = Quat::from_axis_angle(axis, wanted - polar_now) * spin;
        let (fov_y_deg, znear) = (self.fov_y_deg, self.znear);
        *self = Camera {
            fov_y_deg,
            znear,
            ..Camera::looking_from(
                pivot + turn * (eye - pivot),
                pivot + turn * (target - pivot),
                fov_y_deg,
            )
        };
    }

    /// One zoom step along `ray`, the pointer's ray, towards `hit`, the
    /// point [`area_pick`] found there. `factor` below one zooms in and above
    /// one out, as the wheel's curve gives it. Returns the pivot.
    ///
    /// The step is a share of the distance to the pivot, measured from no
    /// closer than [`ZoomLimits::reach`], so it never shrinks to nothing. A
    /// hit more than [`JUMP_LIMIT`] times the current distance away — a gap
    /// in the rig, the far wall — does not size the step: the zoom keeps the
    /// current distance and only aims along the ray. So does a miss.
    ///
    /// Zooming in, the eye stops no closer than [`ZoomLimits::near`] to the
    /// pivot. A step that would pass it moves the eye and the target forward
    /// together instead, through the surface, and the next pick finds the
    /// surface behind. Zooming out stops at [`ZoomLimits::far`]. The view
    /// never turns: afterwards the target sits at the pivot's depth.
    pub fn zoom_toward(
        &mut self,
        factor: f32,
        ray: Ray,
        hit: Option<Vec3>,
        limits: &ZoomLimits,
    ) -> Vec3 {
        let eye = self.position();
        let forward = self.forward();
        let (distance, aim) = match hit {
            Some(point) if point.distance(eye) <= JUMP_LIMIT * self.radius => {
                (point.distance(eye), (point - eye).normalize_or(ray.dir))
            }
            _ => (self.radius, ray.dir),
        };
        let pivot = eye + aim * distance;
        let step = distance.max(limits.reach) * (factor - 1.0).abs();
        let travel = if factor < 1.0 {
            if distance - step < limits.near {
                // Through: the pair moves on, and the depth stays.
                let travel = above_floor(eye, forward * step, limits.floor_z);
                self.target += travel;
                return pivot;
            }
            aim * step
        } else {
            -aim * step.min((limits.far - distance).max(0.0))
        };
        let eye = eye + above_floor(eye, travel, limits.floor_z);
        self.radius = (pivot - eye).dot(forward).max(limits.near);
        self.target = eye + forward * self.radius;
        pivot
    }

    /// One frame of a fly: move the eye and the target together, `speed`
    /// metres per second for `dt` seconds along the held keys. Forward is the
    /// view, right is level, and up is world +Z.
    pub fn fly(&mut self, keys: FlyKeys, speed: f32, dt: f32, floor_z: f32) {
        if !keys.moving() {
            return;
        }
        let axis = |plus: bool, minus: bool| f32::from(u8::from(plus)) - f32::from(u8::from(minus));
        let forward = self.forward();
        let right = forward.cross(Vec3::Z).normalize_or(Vec3::X);
        let direction = (forward * axis(keys.forward, keys.back)
            + right * axis(keys.right, keys.left)
            + Vec3::Z * axis(keys.up, keys.down))
        .normalize_or_zero();
        let fast = if keys.fast { FlyKeys::FAST } else { 1.0 };
        self.target += above_floor(self.position(), direction * speed * fast * dt, floor_z);
    }

    /// Keep the eye and the view, and put the target `distance` in front.
    pub fn refocus(&mut self, distance: f32) {
        let eye = self.position();
        self.radius = distance;
        self.target = eye + self.forward() * distance;
    }
}

/// `travel` from `eye`, shortened so the eye ends no lower than
/// [`MIN_EYE_Z`] above `floor_z`. Rising is never shortened.
fn above_floor(eye: Vec3, travel: Vec3, floor_z: f32) -> Vec3 {
    let lowest = floor_z + MIN_EYE_Z;
    if travel.z >= 0.0 || eye.z + travel.z >= lowest {
        return travel;
    }
    travel * ((lowest - eye.z) / travel.z).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aabb::Aabb;
    use crate::bvh::TriMesh;

    /// A closed box as a triangle mesh.
    fn cube(bounds: Aabb) -> TriMesh {
        let (lo, hi) = (bounds.min, bounds.max);
        let v = |x: bool, y: bool, z: bool| {
            Vec3::new(
                if x { hi.x } else { lo.x },
                if y { hi.y } else { lo.y },
                if z { hi.z } else { lo.z },
            )
        };
        let positions = (0..8)
            .map(|i| v(i & 1 != 0, i & 2 != 0, i & 4 != 0))
            .collect();
        let quads = [
            [0, 1, 3, 2],
            [4, 6, 7, 5],
            [0, 4, 5, 1],
            [2, 3, 7, 6],
            [0, 2, 6, 4],
            [1, 5, 7, 3],
        ];
        let triangles = quads
            .iter()
            .flat_map(|[a, b, c, d]| [[*a, *b, *c], [*a, *c, *d]])
            .collect();
        TriMesh::new(positions, triangles)
    }

    fn limits() -> ZoomLimits {
        ZoomLimits {
            near: Camera::MIN_RADIUS,
            far: 60.0,
            reach: 1.0,
            floor_z: 0.0,
        }
    }

    /// A camera at `eye` looking at `target`.
    fn at(eye: Vec3, target: Vec3) -> Camera {
        Camera::looking_from(eye, target, 50.0)
    }

    const VIEWPORT: Vec2 = Vec2::new(800.0, 600.0);

    /// Two truss chords 4 m up with a 0.2 m gap between them, and the pointer
    /// on the gap: its own ray reaches the floor, and the pick is the truss.
    #[test]
    fn the_area_pick_prefers_the_rig_over_the_floor_through_a_gap() {
        let chords = [
            cube(Aabb::new(
                Vec3::new(-3.0, -0.15, 3.9),
                Vec3::new(3.0, -0.1, 4.0),
            )),
            cube(Aabb::new(
                Vec3::new(-3.0, 0.1, 3.9),
                Vec3::new(3.0, 0.15, 4.0),
            )),
        ];
        let camera = at(Vec3::new(0.0, 0.0, 12.0), Vec3::new(0.0, 0.0, 4.0));
        let cast = |ray: Ray| {
            let rig = chords
                .iter()
                .filter_map(|chord| chord.raycast(ray))
                .map(|hit| SurfaceHit {
                    point: hit.point,
                    distance: hit.t,
                    surface: Surface::Rig,
                })
                .min_by(|a, b| a.distance.total_cmp(&b.distance));
            let ground = ground_hit(ray, 0.0);
            [rig, ground]
                .into_iter()
                .flatten()
                .min_by(|a, b| a.distance.total_cmp(&b.distance))
        };
        let centre = VIEWPORT / 2.0;
        // The control: the pointer's own ray goes through the gap.
        let alone = cast(camera.pixel_ray(centre, VIEWPORT)).unwrap();
        assert_eq!(alone.surface, Surface::Floor);

        let rays = pick_offsets().map(|offset| camera.pixel_ray(centre + offset, VIEWPORT));
        let picked = area_pick(rays, cast).unwrap();
        assert_eq!(picked.surface, Surface::Rig);
        assert!((picked.point.z - 4.0).abs() < 1e-3, "{:?}", picked.point);

        // With the pointer well away from the truss, the floor is the pivot.
        let rays =
            pick_offsets().map(|offset| camera.pixel_ray(Vec2::new(40.0, 40.0) + offset, VIEWPORT));
        let floor = area_pick(rays, cast).unwrap();
        assert_eq!(floor.surface, Surface::Floor);
        assert!(floor.point.z.abs() < 1e-3);
    }

    #[test]
    fn a_miss_is_no_pivot() {
        let rays = pick_offsets().map(|o| Ray::new(o.extend(0.0), Vec3::Z));
        assert!(area_pick(rays, |_| None).is_none());
    }

    /// A pick through a gap to a far wall moves the eye no further than a
    /// miss does: the far hit aims the zoom but does not size it.
    #[test]
    fn a_far_pick_does_not_inflate_the_zoom_step() {
        let start = at(Vec3::new(0.0, -5.0, 3.0), Vec3::new(0.0, 0.0, 3.0));
        let ray = start.pixel_ray(VIEWPORT / 2.0, VIEWPORT);
        let moved = |hit| {
            let mut camera = start;
            camera.zoom_toward(0.8, ray, hit, &limits());
            camera.position().distance(start.position())
        };
        let far = moved(Some(ray.at(40.0)));
        let miss = moved(None);
        assert!((far - miss).abs() < 1e-4, "{far} vs {miss}");
        // A near hit sizes the step off itself, so it is smaller.
        assert!(moved(Some(ray.at(2.0))) < miss);
    }

    /// Wheeling in at a wall never stalls: every step moves the eye, no step
    /// shrinks below the reach's share, and the eye comes out the far side.
    #[test]
    fn zooming_in_never_stops_and_passes_through_a_surface() {
        let wall_y = 0.0;
        let mut camera = at(Vec3::new(0.0, -6.0, 3.0), Vec3::new(0.0, 0.0, 3.0));
        let limits = limits();
        let smallest = limits.reach * (1.0 - 0.8);
        for step in 0..80 {
            let before = camera.position();
            if before.y > wall_y {
                return;
            }
            let ray = camera.pixel_ray(VIEWPORT / 2.0, VIEWPORT);
            let hit = ray.at((wall_y - ray.origin.y) / ray.dir.y);
            camera.zoom_toward(0.8, ray, Some(hit), &limits);
            let moved = camera.position().distance(before);
            assert!(moved >= smallest - 1e-4, "step {step} moved {moved}");
            assert!(camera.radius >= limits.near - 1e-4);
        }
        panic!("the eye never passed the wall: {:?}", camera.position());
    }

    /// Zooming out stops at the far limit, measured from the pivot.
    #[test]
    fn zooming_out_stops_at_the_far_limit() {
        let mut camera = at(Vec3::new(0.0, -5.0, 3.0), Vec3::new(0.0, 0.0, 3.0));
        for _ in 0..200 {
            let ray = camera.pixel_ray(VIEWPORT / 2.0, VIEWPORT);
            camera.zoom_toward(1.25, ray, None, &limits());
        }
        assert!(camera.radius <= limits().far + 1e-3, "{}", camera.radius);
        assert!(camera.radius > limits().far * 0.9, "{}", camera.radius);
    }

    /// A dolly through never takes the eye through the floor.
    #[test]
    fn zooming_at_the_floor_keeps_the_eye_above_it() {
        let mut camera = at(Vec3::new(0.0, -2.0, 3.0), Vec3::new(0.0, 0.0, 0.0));
        for _ in 0..60 {
            let ray = camera.pixel_ray(VIEWPORT / 2.0, VIEWPORT);
            let hit = ground_hit(ray, 0.0).map(|hit| hit.point);
            camera.zoom_toward(0.8, ray, hit, &limits());
        }
        assert!(
            camera.position().z >= MIN_EYE_Z - 1e-4,
            "{:?}",
            camera.position()
        );
    }

    /// About the target, the rigid turn is the old spherical orbit.
    #[test]
    fn orbiting_about_the_target_is_the_spherical_orbit() {
        let framing = Framing::default();
        let start = at(Vec3::new(3.0, -6.0, 5.0), Vec3::new(0.5, 0.0, 1.0));
        let mut camera = start;
        camera.orbit_about(start.target, 0.3, -0.2, &framing);
        let azimuth = (camera.azimuth - (start.azimuth + 0.3)).rem_euclid(std::f32::consts::TAU);
        assert!(!(1e-4..=std::f32::consts::TAU - 1e-4).contains(&azimuth));
        assert!((camera.polar - (start.polar - 0.2)).abs() < 1e-4);
        assert!((camera.radius - start.radius).abs() < 1e-4);
        assert!(camera.target.abs_diff_eq(start.target, 1e-4));
    }

    /// About any other point, the pivot stays where it was on screen.
    #[test]
    fn orbiting_about_a_picked_point_keeps_it_on_screen() {
        let framing = Framing::default();
        let mut camera = at(Vec3::new(3.0, -8.0, 5.0), Vec3::new(0.0, 0.0, 1.0));
        let pivot = Vec3::new(1.5, 0.5, 4.0);
        let aspect = VIEWPORT.x / VIEWPORT.y;
        let before = camera.project(pivot, aspect);
        camera.orbit_about(pivot, 0.4, 0.15, &framing);
        let after = camera.project(pivot, aspect);
        assert!(
            before.truncate().abs_diff_eq(after.truncate(), 1e-3),
            "{before} vs {after}"
        );
    }

    #[test]
    fn fly_moves_along_the_held_keys() {
        let start = at(Vec3::new(0.0, -5.0, 2.0), Vec3::new(0.0, 0.0, 2.0));
        let flown = |keys: FlyKeys| {
            let mut camera = start;
            camera.fly(keys, 2.0, 0.5, 0.0);
            // The view never turns.
            assert!(camera.forward().abs_diff_eq(start.forward(), 1e-5));
            camera.position() - start.position()
        };
        let keys = |set: &[&str]| {
            let mut keys = FlyKeys::default();
            for key in set {
                assert!(keys.set(key, true));
            }
            keys
        };
        assert!(flown(keys(&["w"])).abs_diff_eq(start.forward(), 1e-4));
        assert!(flown(keys(&["s"])).abs_diff_eq(-start.forward(), 1e-4));
        assert!(flown(keys(&["d"])).abs_diff_eq(Vec3::X, 1e-4));
        assert!(flown(keys(&["a"])).abs_diff_eq(Vec3::NEG_X, 1e-4));
        assert!(flown(keys(&["e"])).abs_diff_eq(Vec3::Z, 1e-4));
        // Diagonals are not faster than one key.
        assert!((flown(keys(&["w", "d"])).length() - 1.0).abs() < 1e-4);
        let fast = FlyKeys {
            fast: true,
            ..keys(&["w"])
        };
        assert!((flown(fast).length() - FlyKeys::FAST).abs() < 1e-4);
        assert_eq!(flown(FlyKeys::default()), Vec3::ZERO);
        // Down stops at the floor.
        let mut camera = start;
        camera.fly(keys(&["q"]), 10.0, 1.0, 0.0);
        assert!((camera.position().z - MIN_EYE_Z).abs() < 1e-4);
        assert!(!FlyKeys::default().set("x", true));
    }

    /// Looking while strafing, frame after frame: a look never moves the
    /// eye, so the eye's path is the strafe alone — every frame the same
    /// step, however the view turns between them.
    #[test]
    fn look_and_strafe_together_keep_the_eye_on_an_even_path() {
        let framing = Framing::default();
        let mut camera = at(Vec3::new(0.0, -5.0, 2.0), Vec3::new(0.0, 0.0, 2.0));
        let mut keys = FlyKeys::default();
        keys.set("d", true);
        let (speed, dt) = (2.0, 1.0 / 60.0);
        for frame in 0..120 {
            let eye = camera.position();
            camera.orbit_about(eye, 0.01, 0.002 * ((frame % 7) as f32 - 3.0), &framing);
            assert!(
                camera.position().abs_diff_eq(eye, 1e-4),
                "frame {frame}: a look moved the eye {:?} -> {:?}",
                eye,
                camera.position()
            );
            camera.fly(keys, speed, dt, 0.0);
            let step = camera.position().distance(eye);
            assert!(
                (step - speed * dt).abs() < 1e-4,
                "frame {frame}: step {step}"
            );
        }
    }

    /// Looking turns in place, and releasing puts the target in front.
    #[test]
    fn looking_turns_the_camera_in_place() {
        let framing = Framing::default();
        let mut camera = at(Vec3::new(0.0, -5.0, 2.0), Vec3::new(0.0, 0.0, 2.0));
        let eye = camera.position();
        camera.orbit_about(eye, 0.5, 0.2, &framing);
        assert!(camera.position().abs_diff_eq(eye, 1e-4));
        camera.refocus(7.0);
        assert!(camera.position().abs_diff_eq(eye, 1e-4));
        assert!((camera.target.distance(eye) - 7.0).abs() < 1e-4);
    }
}
