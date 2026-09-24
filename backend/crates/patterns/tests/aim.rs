//! `aim@1`: presets on a straight truss and a ring, the offset frame, spread
//! and the alpha blend between aim clips.
use luma_patterns::aim::{frame, lean, offset, slerp};
use luma_patterns::*;
use std::collections::BTreeMap;
use std::f64::consts::TAU;

const START: f64 = 8.0;
const LENGTH: f64 = 16.0;
/// The resting aim of the presets: 40° down toward downstage.
const REST: [f64; 3] = [0.0, 0.766, -0.643];

fn cell(id: String, uvz: [f64; 3]) -> Cell {
    Cell {
        id,
        group: "rig".into(),
        world: Cell::stage_coordinates(uvz),
        uvz,
    }
}
/// Five heads on a truss along stage right, 6 m up, 1 m apart.
fn truss() -> Vec<Cell> {
    (0..5)
        .map(|i| cell(format!("truss:{i}"), [f64::from(i) - 2.0, 0.0, 6.0]))
        .collect()
}
/// Eight heads on a flat ring of radius 2 m, 6 m up.
fn ring() -> Vec<Cell> {
    (0..8)
        .map(|i| {
            let a = TAU * f64::from(i) / 8.0;
            cell(format!("ring:{i}"), [2.0 * a.cos(), 2.0 * a.sin(), 6.0])
        })
        .collect()
}

fn preset(name: &str) -> BTreeMap<String, Value> {
    let preset = presets().preset(name).unwrap_or_else(|| panic!("{name}"));
    assert_eq!(preset.form, "aim@1", "{name}");
    preset.inputs.clone()
}

fn program(cells: &[Cell], inputs: &BTreeMap<String, Value>) -> PreparedGraph {
    PreparedGraph::new(
        &standard_library(),
        "aim@1",
        inputs,
        Frame {
            cells,
            features: None,
            beat: START,
            clip_start: START,
            clip_duration: LENGTH,
            seed: 11,
        },
    )
    .unwrap()
}

/// Each head's aim, in cell order, `beat` beats into the clip.
fn aims(cells: &[Cell], inputs: &BTreeMap<String, Value>, beat: f64) -> Vec<Aim> {
    let result = program(cells, inputs).evaluate(START + beat).unwrap();
    let Value::Lighting(heads) = &result["lighting"] else {
        panic!("expected lighting")
    };
    cells
        .iter()
        .map(|cell| heads[&cell.id].aim.expect("an aim"))
        .collect()
}
fn directions(cells: &[Cell], inputs: &BTreeMap<String, Value>, beat: f64) -> Vec<[f64; 3]> {
    aims(cells, inputs, beat)
        .into_iter()
        .map(|aim| {
            assert!((aim.weight - 1.0).abs() < 1e-12);
            aim.direction
        })
        .collect()
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|x| x / length)
}
fn degrees(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (a, b) = (unit(a), unit(b));
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2])
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}
fn close(a: [f64; 3], b: [f64; 3]) {
    assert!(
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-6),
        "{a:?} != {b:?}"
    );
}
fn set(inputs: &mut BTreeMap<String, Value>, key: &str, value: Value) {
    assert!(inputs.insert(key.into(), value).is_some(), "{key}");
}

#[test]
fn the_nine_presets_are_valid_clips() {
    let library = standard_library();
    let names: Vec<_> = presets()
        .presets
        .iter()
        .filter(|preset| preset.form == "aim@1")
        .map(|preset| preset.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "Position", "Fan", "Converge", "Bloom", "Sweep", "Nod wave", "Circle", "Figure-8",
            "Ballyhoo"
        ]
    );
    for name in names {
        let preset = presets().preset(name).unwrap();
        preset.validate(&library).unwrap();
        let mut score = Score::default();
        score
            .clips
            .insert("clip".into(), preset.clip(START, LENGTH));
        score
            .validate(&library)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn position_aims_every_head_the_same_way() {
    for aim in directions(&truss(), &preset("Position"), 3.0) {
        close(aim, unit(REST));
    }
}

#[test]
fn fan_leans_the_heads_along_the_truss() {
    let cells = truss();
    let aims = directions(&cells, &preset("Fan"), 0.0);
    // The two end heads are the fan apart; the middle one keeps the rest.
    assert!((degrees(aims[0], aims[4]) - 40.0).abs() < 1e-6);
    close(aims[2], unit(REST));
    for (i, aim) in aims.iter().enumerate() {
        let c = i as f64 / 4.0;
        close(*aim, lean(REST, [1.0, 0.0, 0.0], 40.0 * (c - 0.5)));
    }
    // Stage right heads lean stage right.
    assert!(aims[4][0] > 0.0 && aims[0][0] < 0.0);
}

#[test]
fn converge_points_every_head_at_center_stage() {
    let cells = truss();
    for (cell, aim) in cells
        .iter()
        .zip(directions(&cells, &preset("Converge"), 0.0))
    {
        close(aim, unit(cell.uvz.map(|v| -v)));
    }
}

#[test]
fn bloom_opens_a_ring_out_from_its_middle_over_the_clip() {
    let cells = ring();
    let inputs = preset("Bloom");
    for (beat, fan) in [(0.0, 0.0), (LENGTH / 2.0, 20.0), (LENGTH - 1e-9, 40.0)] {
        for (cell, aim) in cells.iter().zip(directions(&cells, &inputs, beat)) {
            // Every head on a ring is outermost: it leans the whole fan.
            assert!(
                (degrees(aim, [0.0, 0.0, -1.0]) - fan).abs() < 1e-5,
                "{beat}"
            );
            if fan > 0.0 {
                // It leans away from the middle.
                let out = unit([cell.uvz[0], cell.uvz[1], 0.0]);
                let flat = unit([aim[0], aim[1], 0.0]);
                close(flat, out);
            }
        }
    }
}

#[test]
fn a_radial_fan_on_a_truss_keeps_the_middle_head_still() {
    let cells = truss();
    let inputs = preset("Bloom");
    let aims = directions(&cells, &inputs, LENGTH / 2.0);
    close(aims[2], [0.0, 0.0, -1.0]);
    // The ends lean the fan; the next heads half of it, away from the middle.
    for (i, share) in [(0, 1.0), (1, 0.5), (3, 0.5), (4, 1.0)] {
        assert!((degrees(aims[i], [0.0, 0.0, -1.0]) - 20.0 * share).abs() < 1e-6);
        assert_eq!(aims[i][0] > 0.0, i > 2);
    }
}

#[test]
fn an_angle_fan_turns_every_ring_head_the_same_way_around() {
    let cells = ring();
    let mut inputs = preset("Bloom");
    set(&mut inputs, "fan", Value::Number(30.0));
    set(
        &mut inputs,
        "axis",
        axis_presets()
            .into_iter()
            .find(|(name, _)| *name == "Angle")
            .unwrap()
            .1,
    );
    for (cell, aim) in cells.iter().zip(directions(&cells, &inputs, 0.0)) {
        assert!((degrees(aim, [0.0, 0.0, -1.0]) - 30.0).abs() < 1e-6);
        // Along the tangent, from stage right toward downstage.
        let tangent = unit([-cell.uvz[1], cell.uvz[0], 0.0]);
        close(unit([aim[0], aim[1], 0.0]), tangent);
    }
}

#[test]
fn sweep_swings_all_heads_left_and_right_together() {
    let cells = truss();
    let inputs = preset("Sweep");
    // Every 8 beats: a quarter cycle at beat 2 is the full swing right.
    for (beat, yaw) in [(0.0, 0.0), (2.0, 45.0), (6.0, -45.0)] {
        for aim in directions(&cells, &inputs, beat) {
            close(aim, offset(REST, yaw, 0.0));
        }
    }
}

#[test]
fn spread_zero_moves_every_head_identically() {
    let cells = ring();
    for name in ["Sweep", "Circle", "Figure-8"] {
        let inputs = preset(name);
        for step in 0..16 {
            let aims = directions(&cells, &inputs, f64::from(step) * 0.37);
            for aim in &aims {
                close(*aim, aims[0]);
            }
        }
    }
}

#[test]
fn nod_wave_travels_along_the_truss() {
    let cells = truss();
    let inputs = preset("Nod wave");
    let beat = 1.0;
    let aims = directions(&cells, &inputs, beat);
    for (i, aim) in aims.iter().enumerate() {
        let phase = beat / 4.0 - 0.6 * i as f64 / 4.0;
        close(*aim, offset(REST, 0.0, 25.0 * (TAU * phase).sin()));
    }
    assert!(degrees(aims[0], aims[4]) > 1.0);
}

#[test]
fn circle_and_figure_8_draw_their_shapes() {
    let cells = truss();
    let circle = preset("Circle");
    let Value::Vector(down) = circle["direction"] else {
        panic!()
    };
    for (beat, yaw, pitch) in [(0.0, 18.0, 0.0), (1.0, 0.0, 18.0), (2.0, -18.0, 0.0)] {
        for aim in directions(&cells, &circle, beat) {
            close(aim, offset(down, yaw, pitch));
            assert!((degrees(aim, down) - 18.0).abs() < 1e-6);
        }
    }
    let eight = preset("Figure-8");
    for (beat, yaw, pitch) in [(0.5, 25.0 * 0.5_f64.sqrt(), 12.5), (1.0, 25.0, 0.0)] {
        for aim in directions(&cells, &eight, beat) {
            close(aim, offset(REST, yaw, pitch));
        }
    }
}

#[test]
fn ballyhoo_wanders_each_head_on_its_own_and_seeks_exactly() {
    let cells = truss();
    let inputs = preset("Ballyhoo");
    let Value::Vector(rest) = inputs["direction"] else {
        panic!()
    };
    let prepared = program(&cells, &inputs);
    let mut moved = false;
    for step in 0..32 {
        let beat = START + f64::from(step) * 0.5;
        let a = prepared.evaluate(beat).unwrap();
        assert_eq!(a, prepared.evaluate(beat).unwrap());
        let Value::Lighting(heads) = &a["lighting"] else {
            panic!()
        };
        let aims: Vec<_> = cells
            .iter()
            .map(|cell| heads[&cell.id].aim.unwrap().direction)
            .collect();
        for aim in &aims {
            // Yaw and pitch each stay within ±40°.
            assert!(degrees(*aim, rest) <= 40.0 * 2.0_f64.sqrt() + 1e-6);
        }
        moved |= degrees(aims[0], aims[1]) > 1.0;
    }
    assert!(moved, "no two heads should move alike");
}

#[test]
fn the_offset_frame_holds_at_the_poles() {
    // Straight down: right is stage right, up is downstage.
    let (right, up) = frame([0.0, 0.0, -1.0]);
    close(right, [1.0, 0.0, 0.0]);
    close(up, [0.0, 1.0, 0.0]);
    close(offset([0.0, 0.0, -1.0], 90.0, 0.0), [1.0, 0.0, 0.0]);
    close(offset([0.0, 0.0, -1.0], 0.0, 90.0), [0.0, 1.0, 0.0]);
    // Straight up: right is stage right, up is upstage.
    let (right, up) = frame([0.0, 0.0, 1.0]);
    close(right, [1.0, 0.0, 0.0]);
    close(up, [0.0, -1.0, 0.0]);
    // Level toward downstage: right is stage right, up is up.
    let (right, up) = frame([0.0, 1.0, 0.0]);
    close(right, [1.0, 0.0, 0.0]);
    close(up, [0.0, 0.0, 1.0]);
    // A small wobble at a pole stays a small wobble.
    assert!((degrees(offset([0.0, 0.0, -1.0], 3.0, 4.0), [0.0, 0.0, -1.0]) - 5.0).abs() < 0.01);
}

#[test]
fn alpha_blends_toward_the_aim_under_along_the_shortest_arc() {
    let cells = truss();
    let under = aims(&cells, &preset("Position"), 0.0);
    let mut fan = preset("Fan");
    set(&mut fan, "alpha", Value::Proportion(0.25));
    let top = aims(&cells, &fan, 0.0);
    for (below, above) in under.iter().zip(&top) {
        assert!((above.weight - 0.25).abs() < 1e-12);
        let mut head = FixtureOutput {
            aim: Some(*below),
            ..FixtureOutput::default()
        };
        head.composite(
            &FixtureOutput {
                aim: Some(*above),
                ..FixtureOutput::default()
            },
            BlendMode::Replace,
        );
        let blended = head.aim.unwrap();
        assert_eq!(blended.weight, 1.0);
        close(
            blended.direction,
            slerp(below.direction, above.direction, 0.25),
        );
        // A quarter of the way along the arc.
        let whole = degrees(below.direction, above.direction);
        assert!((degrees(below.direction, blended.direction) - whole / 4.0).abs() < 1e-6);
    }
    // Over no aim, the clip blends from home: its alpha is its weight.
    let mut empty = FixtureOutput::default();
    empty.composite(
        &FixtureOutput {
            aim: Some(top[0]),
            ..FixtureOutput::default()
        },
        BlendMode::Replace,
    );
    assert_eq!(empty.aim.unwrap().weight, 0.25);
    // Alpha 0 is no clip.
    set(&mut fan, "alpha", Value::Proportion(0.0));
    assert!(aims(&cells, &fan, 0.0).iter().all(|aim| aim.weight == 0.0));
}

#[test]
fn a_direction_path_and_a_wandering_direction() {
    let cells = truss();
    let mut inputs = preset("Position");
    set(
        &mut inputs,
        "direction",
        Value::Time(Keyframes {
            points: vec![
                (0.0, Key::Color([1.0, 0.0, 0.0])),
                (1.0, Key::Color([0.0, 1.0, 0.0])),
            ],
            segments: vec![],
        }),
    );
    let s = 0.5_f64.sqrt();
    for aim in directions(&cells, &inputs, LENGTH / 2.0) {
        close(aim, [s, s, 0.0]);
    }
    set(
        &mut inputs,
        "direction",
        Value::Noise(NoiseSource {
            speed: 2.0,
            range: [-1.0, 1.0],
        }),
    );
    let first = directions(&cells, &inputs, 0.0);
    let later = directions(&cells, &inputs, 5.0);
    assert!(degrees(first[0], later[0]) > 1.0);
    for aim in first {
        close(aim, directions(&cells, &inputs, 0.0)[0]);
    }
}

#[test]
fn aim_inputs_are_checked() {
    let library = standard_library();
    let check = |inputs: &BTreeMap<String, Value>| {
        let mut score = Score::default();
        let mut clip = presets().preset("Position").unwrap().clip(0.0, 4.0);
        clip.inputs = inputs.clone();
        score.clips.insert("clip".into(), clip);
        score.validate(&library)
    };
    let mut inputs = preset("Sweep");
    set(&mut inputs, "every", Value::Beats(0.0));
    assert!(check(&inputs).unwrap_err().0.contains("every"));
    let mut inputs = preset("Position");
    set(&mut inputs, "shape", Value::Choice("zigzag".into()));
    assert!(check(&inputs).unwrap_err().0.contains("zigzag"));
    let mut inputs = preset("Position");
    set(&mut inputs, "direction", Value::Vector([0.0; 3]));
    assert!(check(&inputs).is_err());
    let mut inputs = preset("Position");
    set(&mut inputs, "fan", Value::Number(120.0));
    assert!(check(&inputs).is_err());
    let mut inputs = preset("Position");
    set(&mut inputs, "direction", Value::Color([1.0, 0.0, 0.0]));
    assert!(check(&inputs).is_err());

    let mut score = Score::default();
    let mut clip = presets().preset("Position").unwrap().clip(0.0, 4.0);
    clip.blend_mode = BlendMode::Add;
    score.clips.insert("clip".into(), clip);
    assert!(score.validate(&library).unwrap_err().0.contains("replace"));
}
