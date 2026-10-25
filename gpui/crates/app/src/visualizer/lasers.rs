//! The stage's lasers: each projector's point stream, as a DAC would play
//! it, handed to the renderer in world space (`docs/design/lasers.md`).
//!
//! The solve is the backend's own, so the picture is the output: nothing
//! here decides where light goes.
use luma_lib::laser::LaserRig;
use luma_lib::models::universe::UniverseState;
use luma_render::coords;
use luma_render::laser::{LaserPoint, LaserScan};

/// Every projector's scan for `state`, at `clock` seconds on the stage's
/// free-running clock: how far through its stream each projector has got.
pub(super) fn scans(rig: &LaserRig, state: &UniverseState, clock: f64) -> Vec<LaserScan> {
    if rig.is_empty() {
        return Vec::new();
    }
    let world =
        |v: glam::DVec3| coords::three_to_world_basis() * coords::three_from_data(v.as_vec3());
    rig.solve(state)
        .into_iter()
        .filter_map(|frame| {
            let projector = rig.projector(&frame.projector)?;
            let basis = projector.basis;
            Some(LaserScan {
                origin: world(basis.origin),
                forward: world(basis.forward),
                right: world(basis.right),
                up: world(basis.up),
                half_field: projector.field.map(|f| (f / 2.) as f32),
                rate: frame.rate as f32,
                phase: (clock * f64::from(frame.rate)).rem_euclid(frame.points.len().max(1) as f64)
                    as f32,
                points: frame
                    .points
                    .iter()
                    .map(|p| LaserPoint {
                        x: p.x,
                        y: p.y,
                        rgb: p.rgb,
                    })
                    .collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{DVec3, Vec3};
    use luma_lib::laser::{Basis, Projector};
    use luma_patterns::laser::Beam;

    #[test]
    fn a_venue_with_no_projectors_draws_no_lasers() {
        assert!(scans(&LaserRig::default(), &UniverseState::default(), 0.).is_empty());
    }

    /// A projector upstage at data (2, 3, 1) facing the house: its scan
    /// starts where it hangs, and the beam in the middle of the default zone
    /// leaves toward the house, a little above level, in the renderer's
    /// world (data with Y mirrored).
    #[test]
    fn a_scan_is_posed_in_world_space() {
        let rig = LaserRig {
            projectors: vec![Projector {
                id: "p".into(),
                basis: Basis {
                    origin: DVec3::new(2., 3., 1.),
                    forward: DVec3::NEG_Y,
                    right: DVec3::X,
                    up: DVec3::Z,
                },
                field: [60., 60.],
                rate: 30_000,
                limit: None,
            }],
            zones: Vec::new(),
        };
        let state = UniverseState {
            beams: vec![Beam {
                projector: "p".into(),
                zone: String::new(),
                path: 0,
                s: 0.,
                x: 0.,
                y: 0.,
                join: 0.,
                intensity: 1.,
            }],
            ..Default::default()
        };
        let scans = scans(&rig, &state, 0.);
        let [scan] = scans.as_slice() else {
            panic!("one projector, one scan: {scans:?}");
        };
        assert!(
            (scan.origin - Vec3::new(2., -3., 1.)).length() < 1e-5,
            "{scan:?}"
        );
        let lit = scan
            .points
            .iter()
            .find(|p| p.rgb != [0.; 3])
            .expect("a lit point");
        let elevation = 15_f32.to_radians();
        let toward = Vec3::new(0., elevation.cos(), elevation.sin());
        let direction = scan.direction(lit.x, lit.y);
        assert!(
            (direction - toward).length() < 1e-3,
            "{direction} vs {toward}"
        );
    }
}
