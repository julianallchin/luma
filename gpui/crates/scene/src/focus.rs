//! A camera on a few things in a room: framed from the direction asked for,
//! and turned until nothing stands between the eye and them.
//!
//! The room is boxes. A truss span, a speaker stack or a deck is its bounds,
//! which is coarse for a mesh and exact enough for "is the truss in front of
//! the light I asked to see". The turn is a search over nearby directions,
//! nearest first, so a clear view from where the caller asked is always the
//! answer, and a blocked one moves only as far as it has to.

use glam::Vec3;

use crate::camera::{Camera, MIN_EYE_Z};
use crate::framing::{Framing, Viewfinder};
use crate::Aabb;

/// A camera on the things asked for, and how much of them it sees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    pub camera: Camera,
    /// The share of the targets' sample points the eye sees past the
    /// occluders, 0 to 1.
    pub visible: f32,
}

/// Azimuth turns tried, in degrees, nearest first.
const TURNS: [f32; 11] = [
    0.0, 20.0, -20.0, 40.0, -40.0, 60.0, -60.0, 90.0, -90.0, 135.0, -135.0,
];
/// Elevation changes tried, in degrees off the asked direction's polar angle;
/// negative looks down from higher.
const TILTS: [f32; 4] = [0.0, -15.0, 15.0, -30.0];
/// How near a sample an occluder may touch without hiding it: a light hangs
/// on its truss, and the truss it hangs on does not hide it.
const CONTACT_M: f32 = 0.3;
/// The least a focus frames, in metres across.
const CONTEXT_M: f32 = 2.5;

/// Frame `targets` from `toward` (from the targets to the eye), and turn as
/// little as it takes to see them past `occluders`.
///
/// Among the directions tried, the one that sees the most of the targets
/// wins, and among those the nearest to `toward`. Every eye stays above the
/// floor.
#[must_use]
pub fn focus(targets: &[Aabb], occluders: &[Aabb], toward: Vec3, view: &Viewfinder) -> Focus {
    // Never closer than a few metres of room: a picture of one light alone
    // says nothing about where it is.
    let bounds = Aabb::from_points(
        targets
            .iter()
            .filter(|b| !b.is_empty())
            .flat_map(Aabb::corners),
    );
    let context = (!bounds.is_empty()).then(|| {
        let c = bounds.center();
        Aabb::new(
            c - Vec3::splat(CONTEXT_M / 2.0),
            c + Vec3::splat(CONTEXT_M / 2.0),
        )
    });
    let framing = Framing::of([], targets.iter().copied().chain(context));
    let target = framing.target();
    let toward = toward.normalize_or(Vec3::Y);
    let azimuth = toward.y.atan2(toward.x);
    let polar = toward.z.clamp(-1.0, 1.0).acos();
    let samples: Vec<Vec3> = targets
        .iter()
        .filter(|b| !b.is_empty())
        .flat_map(|b| {
            let c = b.center();
            std::iter::once(c).chain(b.corners().map(|p| c + (p - c) * 0.7))
        })
        .collect();
    let mut candidates: Vec<(f32, f32, f32)> = TURNS
        .iter()
        .flat_map(|&a| TILTS.iter().map(move |&p| (a.abs() + p.abs(), a, p)))
        .collect();
    candidates.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut best: Option<Focus> = None;
    for (_, turn, tilt) in candidates {
        let p = (polar + tilt.to_radians()).clamp(Framing::MIN_POLAR, Framing::MAX_POLAR);
        let a = azimuth + turn.to_radians();
        let (sp, cp) = p.sin_cos();
        let (sa, ca) = a.sin_cos();
        let camera = framing
            .fit(target, Vec3::new(sp * ca, sp * sa, cp), view)
            .above_floor(framing.floor_z());
        let eye = camera.position();
        if eye.z < framing.floor_z() + MIN_EYE_Z - 1e-3 {
            continue;
        }
        let visible = seen(eye, &samples, occluders);
        if best.is_none_or(|b| visible > b.visible + 1e-6) {
            best = Some(Focus { camera, visible });
        }
        if visible >= 1.0 {
            break;
        }
    }
    best.unwrap_or_else(|| Focus {
        camera: framing.fit(target, toward, view),
        visible: 0.0,
    })
}

/// The share of `samples` an eye at `eye` sees past `occluders`.
fn seen(eye: Vec3, samples: &[Vec3], occluders: &[Aabb]) -> f32 {
    if samples.is_empty() {
        return 1.0;
    }
    let clear = samples
        .iter()
        .filter(|&&s| {
            let to = s - eye;
            let distance = to.length();
            let reach = distance - CONTACT_M;
            reach <= 0.0
                || !occluders
                    .iter()
                    .any(|b| !inside(&grown(b, CONTACT_M), s) && hits(b, eye, to / distance, reach))
        })
        .count();
    clear as f32 / samples.len() as f32
}

fn inside(b: &Aabb, p: Vec3) -> bool {
    p.cmpge(b.min).all() && p.cmple(b.max).all()
}

fn grown(b: &Aabb, by: f32) -> Aabb {
    Aabb::new(b.min - Vec3::splat(by), b.max + Vec3::splat(by))
}

/// Whether the ray from `origin` along unit `dir` enters `b` within `reach`.
fn hits(b: &Aabb, origin: Vec3, dir: Vec3, reach: f32) -> bool {
    let inv = dir.recip();
    let t1 = (b.min - origin) * inv;
    let t2 = (b.max - origin) * inv;
    let near = t1.min(t2).max_element().max(0.0);
    let far = t1.max(t2).min_element().min(reach);
    near <= far
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> Viewfinder {
        Viewfinder::new(50.0, 16.0 / 9.0)
    }

    fn light(at: Vec3) -> Aabb {
        Aabb::new(at - Vec3::splat(0.2), at + Vec3::splat(0.2))
    }

    /// The front, as the camera module has it: toward the audience at +y,
    /// a little up.
    const FRONT: Vec3 = Vec3::new(0.0, 3.0, 1.0);

    #[test]
    fn a_clear_view_is_the_view_asked_for() {
        let at = Vec3::new(0.0, 0.0, 5.0);
        let got = focus(&[light(at)], &[], FRONT, &view());
        assert_eq!(got.visible, 1.0);
        let looking = (got.camera.position() - got.camera.target).normalize();
        assert!(looking.dot(FRONT.normalize()) > 0.999, "{looking}");
    }

    #[test]
    fn the_framed_light_fills_the_picture_and_is_in_it() {
        let at = Vec3::new(3.0, -2.0, 6.0);
        let got = focus(&[light(at)], &[], FRONT, &view());
        let ndc = got.camera.project(at, view().aspect);
        assert!(ndc.x.abs() < 0.2 && ndc.y.abs() < 0.2, "{ndc}");
        // A single light is framed close, but with room round it.
        let distance = (got.camera.position() - at).length();
        assert!((1.5..8.0).contains(&distance), "{distance}");
    }

    /// A wall of truss between the asked view and the light: the camera turns
    /// to see round it, where the asked view would have seen nothing.
    #[test]
    fn a_blocked_view_turns_until_the_light_is_seen() {
        let at = Vec3::new(0.0, 0.0, 5.0);
        let wall = Aabb::new(Vec3::new(-3.0, 1.0, 0.0), Vec3::new(3.0, 1.3, 12.0));
        let open = focus(&[light(at)], &[], FRONT, &view());
        assert_eq!(seen(open.camera.position(), &[at], &[wall]), 0.0);
        let got = focus(&[light(at)], &[wall], FRONT, &view());
        assert_eq!(got.visible, 1.0);
        assert_eq!(seen(got.camera.position(), &[at], &[wall]), 1.0);
    }

    /// The truss a light hangs on does not hide it.
    #[test]
    fn the_truss_a_light_hangs_on_does_not_hide_it() {
        let at = Vec3::new(0.0, 0.0, 5.0);
        let truss = Aabb::new(Vec3::new(-6.0, -0.15, 5.25), Vec3::new(6.0, 0.15, 5.55));
        let got = focus(&[light(at)], &[truss], FRONT, &view());
        assert_eq!(got.visible, 1.0);
    }

    #[test]
    fn every_eye_is_above_the_floor() {
        let at = Vec3::new(0.0, 0.0, 0.3);
        let below = Vec3::new(0.0, 1.0, -1.0);
        let got = focus(&[light(at)], &[], below, &view());
        assert!(got.camera.position().z >= MIN_EYE_Z - 1e-3);
    }
}
