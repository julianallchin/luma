//! What a build promises Luma: housing space (hung, origin at the pivot),
//! joints that turn the way the spec means, cells in canonical order, and the
//! same answer every time.

mod common;

use glam::{Vec2, Vec3, Vec4Swizzles};
use luma_fixture::{build, Built, Fixture, JointKind};
use serde_json::Value;

fn built(id: &str) -> Built {
    build(&common::fixture(id)).unwrap()
}

fn joint(b: &Built, kind: JointKind) -> usize {
    b.joints
        .iter()
        .position(|j| j.kind == kind && j.element.is_none())
        .unwrap()
}

/// Where head 0's beam points, in housing space, at `angles`.
fn beam(b: &Built, angles: &[f32]) -> Vec3 {
    let w = b.world_matrices(angles);
    let h = &b.heads[0];
    w[h.node].transform_vector3(h.beam).normalize()
}

#[test]
fn a_moving_head_hangs_from_its_pivot() {
    let b = built("claypaky/sharpy");
    let w = b.world_matrices(&[]);
    let tilt = w[b.joints[joint(&b, JointKind::Tilt)].node].w_axis.xyz();
    let pan = w[b.joints[joint(&b, JointKind::Pan)].node].w_axis.xyz();
    assert!(
        tilt.length() < 1e-5,
        "the origin is the tilt pivot, got {tilt}"
    );
    assert!(
        pan.x.abs() < 1e-5 && pan.z.abs() < 1e-5 && pan.y > 0.0,
        "the pan axis runs up from the pivot to the base, got {pan}"
    );
    let (_, hi) = b.bounds().unwrap();
    assert!(
        hi.y > pan.y,
        "the clamp side is above the base's pan bearing"
    );
    assert!(
        beam(&b, &[]).dot(Vec3::NEG_Y) > 0.9999,
        "at rest the beam points down"
    );
    let lens = w[b.heads[0].node].transform_point3(b.heads[0].lens_centre);
    assert!(lens.y < 0.0, "the lens hangs below the pivot");
}

#[test]
fn positive_tilt_turns_the_beam_to_the_front() {
    let b = built("claypaky/sharpy");
    let mut angles = vec![0.0; b.joints.len()];
    angles[joint(&b, JointKind::Tilt)] = 30.0;
    assert!(
        beam(&b, &angles).z > 0.4,
        "the spec's front is +Z, and a positive tilt aims at it"
    );
}

#[test]
fn a_bar_set_facing_front_rests_facing_front() {
    // tetra-bar's manual tilt rests at 90: facing front, horizontal.
    let b = built("venue/tetra-bar");
    let tilt = &b.joints[joint(&b, JointKind::Tilt)];
    assert_eq!(tilt.rest, 90.0);
    assert!(
        beam(&b, &[]).dot(Vec3::NEG_Y) > 0.9999,
        "the meshes are built at tilt 0"
    );
    assert!(
        beam(&b, &b.rest_angles()).dot(Vec3::Z) > 0.9999,
        "rest faces the front"
    );
}

#[test]
fn a_hanging_spec_is_not_turned_over_again() {
    let f = common::fixture("oppsk/hyt-006-384sl");
    assert_eq!(f.rig.mount.hang, Some(true));
    let b = build(&f).unwrap();
    // The spec builds it hung already, so housing space is a translation of it.
    let m = b.housing_from_spec;
    assert!((m.x_axis.x - 1.0).abs() < 1e-6 && (m.y_axis.y - 1.0).abs() < 1e-6);
    assert!(beam(&b, &[]).dot(Vec3::NEG_Y) > 0.9999);
}

#[test]
fn cells_run_left_to_right_as_seen_from_the_front() {
    // A bar's cells, back in the spec's standing frame at rest: canonical
    // order is reading order from the audience, which looks along -Z.
    let b = built("venue/tetra-bar");
    let w = b.world_matrices(&b.rest_angles());
    let spec_from_housing = b.housing_from_spec.inverse();
    let xs: Vec<f32> = b
        .cells
        .iter()
        .map(|c| {
            spec_from_housing
                .transform_point3(w[c.node.unwrap()].transform_point3(c.centre))
                .x
        })
        .collect();
    assert!(xs.len() > 1);
    assert!(xs.windows(2).all(|p| p[0] < p[1]), "{xs:?}");
}

#[test]
fn cell_counts_agree_with_the_specs_own_summary() {
    for (id, text) in common::sample() {
        let summary: Value = serde_json::from_str(&text).unwrap();
        let Some(count) = summary.pointer("/cells/count").and_then(Value::as_u64) else {
            continue;
        };
        let b = build(&Fixture::from_json(&text).unwrap()).unwrap();
        assert_eq!(b.cells.len() as u64, count, "{id}");
        assert!(
            b.cells
                .iter()
                .enumerate()
                .all(|(i, c)| c.index as usize == i),
            "{id}"
        );
    }
}

#[test]
fn every_emitter_mesh_lights_a_cell_that_lists_it() {
    for (id, text) in common::sample() {
        let b = build(&Fixture::from_json(&text).unwrap()).unwrap();
        for n in &b.nodes {
            for m in n.meshes.iter().filter(|m| m.cell.is_some()) {
                let cell = &b.cells[m.cell.unwrap() as usize];
                assert!(
                    !cell.emitters.is_empty(),
                    "{id}: cell {} has a mesh but no emitters",
                    cell.index
                );
            }
        }
        for m in b.nodes.iter().flat_map(|n| &n.meshes) {
            assert_eq!(m.positions.len(), m.normals.len(), "{id}");
            assert!(
                m.indices.iter().all(|&i| (i as usize) < m.positions.len()),
                "{id}"
            );
        }
    }
}

#[test]
fn the_same_spec_builds_the_same_fixture() {
    for (id, text) in common::sample() {
        let f = Fixture::from_json(&text).unwrap();
        assert_eq!(build(&f).unwrap(), build(&f).unwrap(), "{id}");
    }
}

#[test]
fn a_spec_round_trips() {
    for (id, text) in common::sample() {
        let f = Fixture::from_json(&text).unwrap();
        let again = Fixture::from_json(&f.to_json()).unwrap();
        assert_eq!(f, again, "{id}");
        let original: Value = serde_json::from_str(&text).unwrap();
        let written: Value = serde_json::from_str(&f.to_json()).unwrap();
        for key in ["control", "optics"] {
            assert_eq!(original.get(key), written.get(key), "{id}: {key}");
        }
        assert_eq!(
            original.pointer("/meta/provenance"),
            written.pointer("/meta/provenance"),
            "{id}"
        );
    }
}

#[test]
fn a_spec_that_is_not_luma_fixture_0_is_refused() {
    let text = common::sample()
        .remove(0)
        .1
        .replace("luma-fixture/0", "luma-fixture/9");
    assert!(matches!(
        Fixture::from_json(&text),
        Err(luma_fixture::Error::Format(_))
    ));
}

/// A dense flat LED panel is one face with its LEDs painted on: every LED
/// its cell lists sits on a lit spot of the face's pattern with gap just past
/// its edge, and the face holds exactly its rows times its columns of them.
#[test]
fn a_flat_led_panel_paints_the_leds_its_cell_lists() {
    let mut panels = 0;
    for (id, text) in common::sample() {
        let b = build(&Fixture::from_json(&text).unwrap()).unwrap();
        let worlds = b.world_matrices(&[]);
        for (n, node) in b.nodes.iter().enumerate() {
            for m in &node.meshes {
                let Some(pattern) = m.pattern else { continue };
                assert_eq!(m.uvs.len(), m.positions.len(), "{id}");
                let cell = &b.cells[m.cell.expect("a panel lights a cell") as usize];
                let into = worlds[n].inverse() * worlds[cell.node.unwrap()];
                // Each face is a quad: top left, top right, bottom right,
                // bottom left.
                for q in 0..m.positions.len() / 4 {
                    panels += 1;
                    let p = |i: usize| Vec3::from(m.positions[4 * q + i]);
                    let t = |i: usize| Vec2::from(m.uvs[4 * q + i]);
                    let (eu, ev) = (p(1) - p(0), p(3) - p(0));
                    let span = t(2) - t(0);
                    let pitch_u = eu.length() / span.x;
                    let mut on = 0;
                    for e in &cell.emitters {
                        let at = into.transform_point3(e.centre) - p(0);
                        let (a, c) = (
                            at.dot(eu) / eu.length_squared(),
                            at.dot(ev) / ev.length_squared(),
                        );
                        if !(-0.01..=1.01).contains(&a) || !(-0.01..=1.01).contains(&c) {
                            continue;
                        }
                        on += 1;
                        let uv = t(0) + span * Vec2::new(a, c);
                        assert!(pattern.lit(uv.into()), "{id}: an LED at {uv} is dark");
                        if pattern.fill[0] < 0.95 {
                            let past = uv + Vec2::new(0.525 * e.size[0] / pitch_u, 0.0);
                            assert!(!pattern.lit(past.into()), "{id}: no gap at {past}");
                        }
                    }
                    // Rows whose LED centres (half a pitch into each row) it holds.
                    let rows = ((t(2).y - 0.5).floor() - (t(0).y - 0.5).ceil() + 1.0) as u32;
                    assert_eq!(on, rows * pattern.cols, "{id}: LEDs on face {q}");
                }
            }
        }
    }
    assert!(panels > 0, "no sample fixture has a flat LED panel");
}
