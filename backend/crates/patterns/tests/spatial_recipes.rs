mod support;
use luma_patterns::*;
use std::collections::{BTreeMap, BTreeSet};
#[allow(unused_imports)]
use support::EvaluateEffect;

fn cells() -> Vec<Cell> {
    (0..16)
        .map(|i| {
            let world = [i as f64, (i % 3) as f64, (i % 2) as f64];
            Cell {
                id: format!("bar:{i:02}"),
                group: "bars".into(),
                world,
                uvz: Cell::stage_coordinates(world),
            }
        })
        .collect()
}
fn frame(cells: &[Cell]) -> Frame<'_> {
    Frame {
        cells,
        features: None,
        beat: 0.,
        clip_start: 0.,
        clip_duration: 16.,
        seed: 291,
    }
}
fn selected(program: &PreparedGraph, beat: f64) -> BTreeSet<String> {
    let value = program.evaluate(beat).unwrap();
    let mask = &support::field(&value["mask"]);
    assert!(mask.values().all(|v| *v == 0. || *v == 1.));
    mask.iter()
        .filter(|(_, v)| **v == 1.)
        .map(|(id, _)| id.clone())
        .collect()
}

#[test]
fn shuffled_walk_lights_exact_head_counts_without_repeating_until_needed() {
    let library = standard_library();
    let mut cells = cells();
    let args = BTreeMap::from([("count".into(), Value::Number(2.))]);
    let program =
        support::prepare_effect(&library, "random_heads_mask", &args, frame(&cells)).unwrap();
    let mut seen = BTreeSet::new();
    for beat in 0..8 {
        let current = selected(&program, beat as f64);
        assert_eq!(current.len(), 2);
        assert!(seen.is_disjoint(&current));
        seen.extend(current);
    }
    assert_eq!(seen.len(), 16);
    assert_eq!(selected(&program, 0.), selected(&program, 8.));
    assert_eq!(selected(&program, 1.), selected(&program, 1.99));
    cells.reverse();
    let reordered =
        support::prepare_effect(&library, "random_heads_mask", &args, frame(&cells)).unwrap();
    for beat in [20., 1., 7., 0.1, 3.] {
        assert_eq!(selected(&program, beat), selected(&reordered, beat));
    }
    for (count, expected) in [(-1., 0), (0., 0), (1., 1), (2.9, 2), (16., 16), (100., 16)] {
        let args = BTreeMap::from([("count".into(), Value::Number(count))]);
        let p =
            support::prepare_effect(&library, "random_heads_mask", &args, frame(&cells)).unwrap();
        for beat in [0., 1., 17.] {
            assert_eq!(selected(&p, beat).len(), expected);
        }
    }
    let empty = support::prepare_effect(&library, "random_heads_mask", &args, frame(&[])).unwrap();
    assert!(selected(&empty, 4.).is_empty());
}

#[test]
fn fresh_random_sets_and_phase_delay_replay_after_seeking() {
    let library = standard_library();
    let cells = cells();
    let args = BTreeMap::from([
        ("count".into(), Value::Number(3.)),
        ("shuffle".into(), Value::Boolean(true)),
        ("delay".into(), Value::Beats(0.5)),
        ("grid_aligned".into(), Value::Boolean(true)),
    ]);
    let program =
        support::prepare_effect(&library, "random_heads_mask", &args, frame(&cells)).unwrap();
    assert_eq!(selected(&program, 0.5), selected(&program, 1.499));
    assert_ne!(selected(&program, 0.5), selected(&program, 1.5));
    let before_seek = selected(&program, 1.5);
    program.evaluate(15.).unwrap();
    assert_eq!(before_seek, selected(&program, 1.5));
    assert_eq!(selected(&program, 0.5).len(), 3);
    let mut moved = frame(&cells);
    moved.clip_start = 0.25;
    let moved = support::prepare_effect(&library, "random_heads_mask", &args, moved).unwrap();
    assert_eq!(selected(&program, 1.5), selected(&moved, 1.5));
}

#[test]
fn ranks_break_coordinate_ties_by_head_identity_and_radius_keeps_stage_aspect() {
    let library = standard_library();
    let cells = cells();
    let rank = library
        .evaluate_effect(
            "core/rank",
            &BTreeMap::from([(
                "value".into(),
                Value::Field(BTreeMap::from([
                    ("a".into(), -0.),
                    ("b".into(), 0.),
                    ("c".into(), -2.),
                ])),
            )]),
            frame(&cells),
        )
        .unwrap();
    assert_eq!(
        support::field(&rank["value"]),
        BTreeMap::from([("a".into(), 1.), ("b".into(), 2.), ("c".into(), 0.)])
    );
    let positions = [[-4., 0., 7.], [4., 0., 9.], [0., -1., 2.], [0., 1., 0.]];
    let cells: Vec<_> = positions
        .into_iter()
        .enumerate()
        .map(|(i, world)| Cell {
            id: i.to_string(),
            group: "all".into(),
            world,
            uvz: Cell::stage_coordinates(world),
        })
        .collect();
    let program =
        support::prepare_effect(&library, "radial_distance", &BTreeMap::new(), frame(&cells))
            .unwrap();
    assert_eq!(program.dynamic_step_count(), 0);
    let batch = program.evaluate_batch(&[0.0]).unwrap();
    let signal = batch["value"].signal().unwrap();
    assert_eq!(
        signal.values().iter().copied().collect::<Vec<_>>(),
        [4.0, 4.0, 1.0, 1.0]
    );
    assert_eq!(signal.fixtures().unwrap(), ["0", "1", "2", "3"]);
    assert_eq!(signal.unit(), Unit::Number);
    assert!(library
        .evaluate_effect(
            "core/square_root",
            &BTreeMap::from([(
                "value".into(),
                Value::Field(BTreeMap::from([("a".into(), -1.)]))
            )]),
            frame(&cells)
        )
        .is_err());
}

#[test]
fn strobe_and_rainbow_are_graphs_and_preserve_output_capabilities() {
    let library = standard_library();
    let cells = cells();
    for effect in [
        "random_heads",
        "rainbow",
        "strobe",
        "write_strobe",
        "harmony_color",
    ] {
        assert!(matches!(library.definitions[effect].body, Body::Graph(_)));
        let mut score = Score::default();
        score
            .insert_effect(&library, effect, "clip", 0., 8.)
            .unwrap();
        score.validate(&library).unwrap();
    }
    let program = support::prepare_effect(
        &library,
        "strobe",
        &BTreeMap::from([
            ("rate".into(), Value::Proportion(0.4)),
            ("color".into(), Value::Color([0.25, 0., 0.])),
        ]),
        frame(&cells),
    )
    .unwrap();
    let value = program.evaluate(0.).unwrap();
    let Value::Lighting(light) = &value["lighting"] else {
        panic!()
    };
    assert!(
        light
            .values()
            .all(|v| (v.strobe.unwrap() - 0.4).abs() < 1e-7
                && v.dimmer == Some(0.25)
                && v.color == Some([1., 0., 0.])
                && v.position.is_none()
                && v.speed.is_none()),
        "{light:?}"
    );
    let rainbow =
        support::prepare_effect(&library, "rainbow", &BTreeMap::new(), frame(&cells)).unwrap();
    let value = rainbow.evaluate(0.).unwrap();
    let Value::Lighting(light) = &value["lighting"] else {
        panic!()
    };
    assert!(light
        .values()
        .all(|v| v.color == Some([1., 0., 0.]) && v.strobe.is_none()));
    assert_eq!(rainbow.evaluate(0.).unwrap(), rainbow.evaluate(4.).unwrap());
}

#[test]
fn chase_uses_an_editable_travel_envelope_for_bounce_and_offstage_endpoints() {
    let library = standard_library();
    let cells = cells();
    let path = Value::Envelope(Envelope::linear(vec![[0., 0.], [0.5, 1.], [1., 0.]]));
    assert!(matches!(library.definitions["motion"].body, Body::Graph(_)));
    for (elapsed, position, active) in [
        (0., -0.25, 1.),
        (1., 0.5, 1.),
        (2., 1.25, 1.),
        (3., 0.5, 1.),
        (4., -0.25, 0.),
    ] {
        let outputs = library
            .evaluate_effect(
                "motion",
                &BTreeMap::from([
                    ("elapsed".into(), Value::Beats(elapsed)),
                    ("travel".into(), Value::Beats(4.)),
                    ("start".into(), Value::Position(-0.25)),
                    ("end".into(), Value::Position(1.25)),
                    ("path".into(), path.clone()),
                ]),
                frame(&cells),
            )
            .unwrap();
        assert_eq!(outputs["position"].scalar_value(), Some(position));
        assert_eq!(outputs["active"].scalar_value(), Some(active));
    }
    let mut score = Score::default();
    score
        .insert_effect(&library, "beat_chase", "bounce", 0., 8.)
        .unwrap();
    score
        .clips
        .get_mut("bounce")
        .unwrap()
        .inputs
        .insert("path".into(), path);
    score.validate(&library).unwrap();
}

#[test]
fn field_profile_is_dark_outside_its_width_even_with_hard_edges() {
    let library = standard_library();
    let cells: Vec<_> = cells()
        .into_iter()
        .take(5)
        .zip(["a", "b", "c", "d", "e"])
        .map(|(mut cell, id)| {
            cell.id = id.into();
            cell
        })
        .collect();
    let offset = Value::Field(BTreeMap::from([
        ("a".into(), -0.6),
        ("b".into(), -0.49),
        ("c".into(), 0.),
        ("d".into(), 0.49),
        ("e".into(), 0.6),
    ]));
    for width in [1., 0., -1.] {
        let value = library
            .evaluate_effect(
                "profile_mask",
                &BTreeMap::from([
                    ("offset".into(), offset.clone()),
                    ("width".into(), Value::Number(width)),
                    ("shape".into(), Value::Envelope(Envelope::soft_edges(0.))),
                ]),
                frame(&cells),
            )
            .unwrap();
        let mask = &support::field(&value["mask"]);
        assert_eq!(mask["a"], 0.);
        assert_eq!(mask["e"], 0.);
        for head in ["b", "c", "d"] {
            assert_eq!(mask[head], if width > 0. { 1. } else { 0. });
        }
    }
}
