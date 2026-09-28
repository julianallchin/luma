//! Primitives that forms are built from: an odometer clock, event life, a
//! keyframe curve, a random share of heads, a test for a gliding path and
//! the aim steps (base, fan, motion, offset, and the turn an Offset clip
//! gives the aim under it). None of them keeps state.
use super::pace::Pace;
use crate::runtime::{Batch, EvaluatedValue};
use crate::*;
use ndarray::Array3;
use std::collections::BTreeMap;

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    let port = |name: &str, description: &str, value_type, rate, default| Input {
        optional: false,
        name: name.into(),
        description: description.into(),
        value_type,
        rate,
        default,
        author: None,
        promotable: Vec::new(),
    };
    let curve = |name: &str, description: &str| Input {
        optional: true,
        ..port(name, description, ValueType::Time, Rate::Fixed, None)
    };
    let beats = |name: &str, description: &str, value: f64| {
        port(
            name,
            description,
            ValueType::Beats,
            Rate::Fixed,
            Some(Value::Beats(value)),
        )
    };
    let signal = |unit| {
        ValueType::Signal(SignalType {
            unit: Some(unit),
            channels: None,
        })
    };
    let vector = || ValueType::Signal(SignalType::new(Unit::Number, crate::tensor::VECTOR));
    let choice_port = |name: &str, description: &str, value: &str| {
        port(
            name,
            description,
            ValueType::Choice,
            Rate::Fixed,
            Some(Value::Choice(value.into())),
        )
    };
    let axis_port = || {
        port(
            "Axis",
            "How the heads are laid out",
            ValueType::Mapping,
            Rate::Fixed,
            Some(Value::Mapping(MappingSpec {
                span: Default::default(),
                plane: None,
                mirror: None,
                source: MappingSource::Order,
                per_group: false,
                reverse: false,
            })),
        )
    };
    let (name, inputs, outputs) = match op {
        Primitive::Odometer => (
            "Odometer",
            vec![
                (
                    "period",
                    beats(
                        "Period",
                        "Beats for one turn; zero means the whole clip",
                        4.0,
                    ),
                ),
                (
                    "period_curve",
                    curve("Period curve", "Period over the clip, in beats"),
                ),
            ],
            vec![(
                "turns",
                ValueType::Signal(SignalType::new(Unit::Number, Channels::Value)),
            )],
        ),
        Primitive::EventLife => (
            "Event life",
            vec![
                (
                    "every",
                    beats(
                        "Every",
                        "Beats between events; zero means one event over the clip",
                        4.0,
                    ),
                ),
                (
                    "every_curve",
                    curve("Every curve", "Beats between events, over the clip"),
                ),
                (
                    "life",
                    beats(
                        "Life",
                        "Beats one event lasts; zero means the whole clip",
                        2.0,
                    ),
                ),
                (
                    "life_curve",
                    curve("Life curve", "Beats one event lasts, over the clip"),
                ),
            ],
            vec![
                ("progress", signal(Unit::Proportion)),
                ("present", signal(Unit::Proportion)),
                ("index", signal(Unit::Number)),
                ("spacing", signal(Unit::Number)),
            ],
        ),
        Primitive::SampleCurve => (
            "Curve",
            vec![
                (
                    "curve",
                    port(
                        "Curve",
                        "Keyframes over progress",
                        ValueType::Time,
                        Rate::Fixed,
                        None,
                    ),
                ),
                (
                    "progress",
                    port(
                        "Progress",
                        "Where to read the curve",
                        signal(Unit::Proportion),
                        Rate::Frame,
                        Some(Value::Proportion(0.0)),
                    ),
                ),
            ],
            vec![("value", ValueType::Signal(SignalType::ANY))],
        ),
        Primitive::PathGlides => (
            "Path glides",
            vec![(
                "path",
                port(
                    "Path",
                    "A curve of position over life",
                    ValueType::Envelope,
                    Rate::Fixed,
                    Some(Value::Envelope(Envelope::linear(vec![[0., 0.], [1., 1.]]))),
                ),
            )],
            vec![(
                "value",
                ValueType::Signal(SignalType::new(Unit::Number, Channels::Value)),
            )],
        ),
        Primitive::RandomShare => (
            "Random share",
            vec![
                (
                    "index",
                    port(
                        "Event index",
                        "Each index draws a new random set",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "coverage",
                    port(
                        "Coverage",
                        "Share of heads lit",
                        signal(Unit::Proportion),
                        Rate::Frame,
                        Some(Value::Proportion(0.5)),
                    ),
                ),
                (
                    "grain",
                    port(
                        "Grain",
                        "1 is one head, 0 is one fixture, N is a clump of N heads",
                        ValueType::Number,
                        Rate::Fixed,
                        Some(Value::Number(1.0)),
                    ),
                ),
            ],
            vec![("selected", signal(Unit::Proportion))],
        ),
        Primitive::AimBase => (
            "Aim base",
            vec![
                (
                    "base",
                    choice_port("Base", "direction or point", "direction"),
                ),
                (
                    "direction",
                    port(
                        "Direction",
                        "The aim when base is direction",
                        vector(),
                        Rate::Frame,
                        Some(Value::Vector(crate::aim::DOWN)),
                    ),
                ),
                (
                    "point",
                    port(
                        "Point",
                        "U, V, Z in metres that every head points at when base is point",
                        vector(),
                        Rate::Frame,
                        Some(Value::Vector([0.0; 3])),
                    ),
                ),
            ],
            vec![("direction", vector())],
        ),
        Primitive::AimFan => (
            "Aim fan",
            vec![
                (
                    "direction",
                    port(
                        "Direction",
                        "The aim of each head",
                        vector(),
                        Rate::Frame,
                        Some(Value::Vector(crate::aim::DOWN)),
                    ),
                ),
                (
                    "fan",
                    port(
                        "Fan",
                        "Degrees",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                ("axis", axis_port()),
            ],
            vec![("direction", vector())],
        ),
        Primitive::AimMotion => (
            "Aim motion",
            vec![
                (
                    "motion",
                    choice_port("Motion", "none, shape or noise", "none"),
                ),
                (
                    "shape",
                    choice_port(
                        "Shape",
                        "swing_left_right, swing_up_down, circle or figure_8",
                        "swing_left_right",
                    ),
                ),
                (
                    "cycles",
                    port(
                        "Cycles",
                        "Shape cycles counted since the clip start",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "spread",
                    port(
                        "Spread",
                        "Degrees of phase across the axis; 360 is one cycle",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "size",
                    port(
                        "Size",
                        "Degrees",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "wander",
                    port(
                        "Wander",
                        "Noise steps counted since the clip start",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                ("axis", axis_port()),
            ],
            vec![
                ("yaw", signal(Unit::Number)),
                ("pitch", signal(Unit::Number)),
            ],
        ),
        Primitive::AimOffset => (
            "Aim offset",
            vec![
                (
                    "direction",
                    port(
                        "Direction",
                        "The aim of each head",
                        vector(),
                        Rate::Frame,
                        Some(Value::Vector(crate::aim::DOWN)),
                    ),
                ),
                (
                    "yaw",
                    port(
                        "Left/right",
                        "Degrees toward the right of the aim",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "pitch",
                    port(
                        "Up/down",
                        "Degrees toward the up of the aim",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                ("axis", axis_port()),
            ],
            vec![("direction", vector())],
        ),
        Primitive::AimTurn => (
            "Aim turn",
            vec![
                (
                    "fan",
                    port(
                        "Fan",
                        "Degrees",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "yaw",
                    port(
                        "Left/right",
                        "Degrees toward the right of the aim",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "pitch",
                    port(
                        "Up/down",
                        "Degrees toward the up of the aim",
                        signal(Unit::Number),
                        Rate::Frame,
                        Some(Value::Number(0.0)),
                    ),
                ),
                (
                    "alpha",
                    port(
                        "Alpha",
                        "The share of every angle that applies",
                        signal(Unit::Proportion),
                        Rate::Frame,
                        Some(Value::Proportion(1.0)),
                    ),
                ),
                ("axis", axis_port()),
            ],
            vec![(
                "turn",
                ValueType::Signal(SignalType::new(
                    Unit::Number,
                    Channels::components(crate::aim::TURN_CHANNELS).expect("nine channels"),
                )),
            )],
        ),
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: outputs
            .into_iter()
            .map(|(k, value_type)| {
                (
                    k.into(),
                    Output {
                        value_type,
                        rate: Rate::Frame,
                    },
                )
            })
            .collect(),
        body: Body::Primitive(op),
    })
}

fn keyframes<'a>(inputs: &'a BTreeMap<String, EvaluatedValue>, key: &str) -> Option<&'a Keyframes> {
    inputs.get(key).map(|value| match value.control(0) {
        Value::Time(curve) => curve,
        _ => unreachable!("validated curve input"),
    })
}

pub(crate) fn run(
    op: Primitive,
    inputs: &BTreeMap<String, EvaluatedValue>,
    outputs: &BTreeMap<String, Output>,
    batch: Batch<'_>,
) -> Result<BTreeMap<String, EvaluatedValue>> {
    let frame = batch.frame;
    let numeric = |key: &str, value: Signal| {
        Ok((
            key.to_string(),
            EvaluatedValue::from_signal(outputs[key].value_type, value)?,
        ))
    };
    match op {
        Primitive::Odometer => {
            let pace = Pace::new(
                frame.clip_start,
                frame.clip_duration,
                whole_clip(inputs["period"].fixed_scalar()?, frame.clip_duration),
                keyframes(inputs, "period_curve"),
            )?;
            let turns: Vec<_> = batch.times.iter().map(|b| pace.turns(*b)).collect();
            Ok(BTreeMap::from([numeric(
                "turns",
                Signal::series(&turns, Unit::Number)?,
            )?]))
        }
        Primitive::EventLife => {
            let origin = frame.clip_start;
            let span = frame.clip_duration;
            let schedule = Schedule(Pace::new(
                origin,
                span,
                whole_clip(inputs["every"].fixed_scalar()?, span),
                keyframes(inputs, "every_curve"),
            )?);
            let life = Pace::new(
                origin,
                span,
                whole_clip(inputs["life"].fixed_scalar()?, span),
                keyframes(inputs, "life_curve"),
            )?;
            event_life(batch.times, &schedule, &life)?
                .into_iter()
                .map(|(key, value)| numeric(key, value))
                .collect()
        }
        Primitive::SampleCurve => {
            let curve = keyframes(inputs, "curve").expect("required curve");
            let progress = inputs["progress"].numeric();
            let (n, t, c) = progress.values().dim();
            let signal = if curve.is_color() {
                if c != 1 {
                    return Err(Error("a color curve needs one progress channel".into()));
                }
                Signal::new(
                    Array3::from_shape_fn((n, t, 3), |(n, t, ch)| {
                        curve.sample(progress.at(n, t, 0))[ch]
                    }),
                    Unit::Proportion,
                    Channels::Rgb,
                    progress.fixtures().map(Into::into),
                )?
            } else {
                progress.map(Unit::Number, |p| curve.sample(p)[0])?
            };
            Ok(BTreeMap::from([numeric("value", signal)?]))
        }
        Primitive::PathGlides => {
            let Value::Envelope(path) = inputs["path"].control(0) else {
                unreachable!("validated path")
            };
            let glides = path.points.iter().all(|point| point.ease != Ease::Hold);
            Ok(BTreeMap::from([numeric(
                "value",
                Signal::scalar(if glides { 1.0 } else { 0.0 }, Unit::Number)?,
            )?]))
        }
        Primitive::RandomShare => {
            let grain = inputs["grain"].fixed_scalar()?;
            if grain.fract() != 0.0 || !(0.0..=65535.0).contains(&grain) {
                return Err(Error(
                    "grain must be 0 (fixture) or a whole number of heads".into(),
                ));
            }
            let index = inputs["index"].numeric();
            let coverage = inputs["coverage"].numeric();
            let layout = index.zip(coverage, Unit::Number, |a, _| a)?;
            let (_, t, c) = layout.values().dim();
            let grains = grains(batch.fixtures, grain as usize);
            let count = grains.iter().max().map_or(0, |g| g + 1);
            let keys = grain_keys(batch.fixtures, &grains, count);
            let mut values = Array3::zeros((batch.fixtures.len(), t, c));
            let mut order: Vec<(f64, usize)> = Vec::with_capacity(count);
            for time in 0..t {
                for channel in 0..c {
                    let event = index.at(0, time, channel);
                    if event.fract() != 0.0 || event.abs() > 9_007_199_254_740_991.0 {
                        return Err(Error("event index must be a whole number".into()));
                    }
                    let seed = crate::spatial::epoch_seed(frame.seed, event as i64);
                    order.clear();
                    order.extend(
                        keys.iter()
                            .enumerate()
                            .map(|(g, key)| (crate::spatial::threshold(key, seed), g)),
                    );
                    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                    let share = coverage.at(0, time, channel).clamp(0.0, 1.0);
                    let lit = (share * count as f64).round() as usize;
                    let mut on = vec![false; count];
                    for (_, g) in order.iter().take(lit) {
                        on[*g] = true;
                    }
                    for (n, g) in grains.iter().enumerate() {
                        if on[*g] {
                            values[[n, time, channel]] = 1.0;
                        }
                    }
                }
            }
            Ok(BTreeMap::from([numeric(
                "selected",
                Signal::new(
                    values,
                    Unit::Proportion,
                    *layout.channels(),
                    Some(batch.fixtures.to_vec().into()),
                )?,
            )?]))
        }
        Primitive::AimBase
        | Primitive::AimFan
        | Primitive::AimMotion
        | Primitive::AimOffset
        | Primitive::AimTurn => aim_step(op, inputs, batch)?
            .into_iter()
            .map(|(key, value)| numeric(key, value))
            .collect(),
        _ => unreachable!("form primitive"),
    }
}

fn choice<'a>(inputs: &'a BTreeMap<String, EvaluatedValue>, key: &str) -> &'a str {
    match inputs[key].control(0) {
        Value::Choice(name) => name,
        _ => unreachable!("validated choice input"),
    }
}
fn axis<'a>(inputs: &'a BTreeMap<String, EvaluatedValue>) -> &'a MappingSpec {
    match inputs["axis"].control(0) {
        Value::Mapping(spec) => spec,
        _ => unreachable!("validated axis input"),
    }
}

/// One aim step over every head and sample. Directions are unit vectors
/// in U, V, Z; angles are degrees.
fn aim_step(
    op: Primitive,
    inputs: &BTreeMap<String, EvaluatedValue>,
    batch: Batch<'_>,
) -> Result<Vec<(&'static str, Signal)>> {
    use crate::aim;
    let fixtures = batch.fixtures;
    let cells: BTreeMap<&str, &Cell> = batch
        .frame
        .cells
        .iter()
        .map(|cell| (cell.id.as_str(), cell))
        .collect();
    let signals: BTreeMap<&str, &Signal> = inputs
        .iter()
        .filter_map(|(key, value)| Some((key.as_str(), value.signal()?)))
        .collect();
    let times = signals
        .values()
        .map(|signal| signal.values().dim().1)
        .max()
        .unwrap_or(1)
        .max(1);
    let get = |key: &str, n: usize, t: usize| signals[key].at(n, t, 0);
    let vector = |key: &str, n: usize, t: usize| -> [f64; 3] {
        std::array::from_fn(|ch| signals[key].at(n, t, ch))
    };
    let position = |n: usize| -> Result<[f64; 3]> {
        cells
            .get(fixtures[n].as_str())
            .map(|cell| cell.uvz)
            .ok_or_else(|| Error(format!("fixture {} is not in the selection", fixtures[n])))
    };
    // A signal row per head in `fixtures`; a broadcast input reads row 0.
    let row = |key: &str, n: usize| {
        if signals[key].fixtures().is_some() {
            n
        } else {
            0
        }
    };
    let directions = |at: &dyn Fn(usize, usize) -> Result<[f64; 3]>| -> Result<Signal> {
        let mut values = Array3::zeros((fixtures.len(), times, 3));
        for n in 0..fixtures.len() {
            for t in 0..times {
                let d = at(n, t)?;
                for ch in 0..3 {
                    values[[n, t, ch]] = d[ch];
                }
            }
        }
        Signal::new(
            values,
            Unit::Number,
            crate::tensor::VECTOR,
            Some(fixtures.to_vec().into()),
        )
    };
    for key in ["direction", "point"] {
        if let Some(signal) = signals.get(key) {
            if signal.values().dim().2 != 3 {
                return Err(Error(format!("{key} needs three channels: U, V and Z")));
            }
        }
    }
    let direction = |n: usize, t: usize| vector("direction", row("direction", n), t);
    Ok(match op {
        Primitive::AimBase => {
            let converge = match choice(inputs, "base") {
                "direction" => false,
                "point" => true,
                other => return Err(Error(format!("unknown base {other}"))),
            };
            vec![(
                "direction",
                directions(&|n, t| {
                    Ok(if converge {
                        let head = position(n)?;
                        let point = vector("point", row("point", n), t);
                        aim::unit(std::array::from_fn(|a| point[a] - head[a]))
                    } else {
                        aim::unit(direction(n, t))
                    })
                })?,
            )]
        }
        Primitive::AimFan => {
            let leans = fan_leans(axis(inputs), batch.frame.cells, batch.frame.seed)?;
            vec![(
                "direction",
                directions(&|n, t| {
                    let (share, toward) = leans(&fixtures[n]);
                    let fan = get("fan", row("fan", n), t);
                    Ok(aim::lean(direction(n, t), toward, fan * share))
                })?,
            )]
        }
        Primitive::AimOffset => {
            let mirrored = axis(inputs).mirrored(batch.frame.cells)?;
            vec![(
                "direction",
                directions(&|n, t| {
                    Ok(aim::mirrored_offset(
                        direction(n, t),
                        get("yaw", row("yaw", n), t),
                        get("pitch", row("pitch", n), t),
                        mirrored.get(fixtures[n].as_str()).copied(),
                    ))
                })?,
            )]
        }
        Primitive::AimTurn => {
            let leans = fan_leans(axis(inputs), batch.frame.cells, batch.frame.seed)?;
            let mirrored = axis(inputs).mirrored(batch.frame.cells)?;
            let mut values = Array3::zeros((fixtures.len(), times, aim::TURN_CHANNELS));
            for (n, id) in fixtures.iter().enumerate() {
                let (share, toward) = leans(id);
                for t in 0..times {
                    let fan = get("fan", row("fan", n), t) * share;
                    let turn = aim::Turn {
                        lean: toward.map(|v| v * fan),
                        yaw: get("yaw", row("yaw", n), t),
                        pitch: get("pitch", row("pitch", n), t),
                        mirror: mirrored.get(id.as_str()).copied(),
                        alpha: get("alpha", row("alpha", n), t),
                    };
                    for (ch, v) in turn.channels().into_iter().enumerate() {
                        values[[n, t, ch]] = v;
                    }
                }
            }
            vec![(
                "turn",
                Signal::new(
                    values,
                    Unit::Number,
                    Channels::components(aim::TURN_CHANNELS)?,
                    Some(fixtures.to_vec().into()),
                )?,
            )]
        }
        Primitive::AimMotion => {
            let motion = choice(inputs, "motion");
            let shape = choice(inputs, "shape");
            let coordinates: BTreeMap<String, f64> = axis(inputs)
                .resolve(batch.frame.cells, batch.frame.seed)?
                .coordinates
                .into_iter()
                .map(|c| (c.cell, c.position))
                .collect();
            let tau = std::f64::consts::TAU;
            let mut yaw = Array3::zeros((fixtures.len(), times, 1));
            let mut pitch = Array3::zeros((fixtures.len(), times, 1));
            for (n, id) in fixtures.iter().enumerate() {
                // Each head wanders on its own, from the clip seed and its identity.
                let head = crate::value_noise::hash(batch.frame.seed, identity(id));
                for t in 0..times {
                    let size = get("size", row("size", n), t);
                    let (y, p) = match motion {
                        "none" => (0.0, 0.0),
                        "shape" => {
                            let c = coordinates.get(id).copied().unwrap_or(0.0);
                            // Spread is degrees of phase: 360 is one cycle.
                            let phase = get("cycles", row("cycles", n), t)
                                - get("spread", row("spread", n), t) / 360.0 * c;
                            let (s, c1) = (tau * phase).sin_cos();
                            match shape {
                                "swing_left_right" => (size * s, 0.0),
                                "swing_up_down" => (0.0, size * s),
                                "circle" => (size * c1, size * s),
                                "figure_8" => (size * s, size / 2.0 * (2.0 * tau * phase).sin()),
                                other => return Err(Error(format!("unknown shape {other}"))),
                            }
                        }
                        "noise" => {
                            let x = get("wander", row("wander", n), t);
                            (
                                size * crate::value_noise::noise1(
                                    x,
                                    1.0,
                                    crate::value_noise::hash(head, 1),
                                ),
                                size * crate::value_noise::noise1(
                                    x,
                                    1.0,
                                    crate::value_noise::hash(head, 2),
                                ),
                            )
                        }
                        other => return Err(Error(format!("unknown motion {other}"))),
                    };
                    yaw[[n, t, 0]] = y;
                    pitch[[n, t, 0]] = p;
                }
            }
            let degrees = |values| {
                Signal::new(
                    values,
                    Unit::Number,
                    Channels::Value,
                    Some(fixtures.to_vec().into()),
                )
            };
            vec![("yaw", degrees(yaw)?), ("pitch", degrees(pitch)?)]
        }
        _ => unreachable!("aim step"),
    })
}

/// Each head's share of the fan and the way it leans. A head on the low
/// side of the mirror leans the mirror image of the way it would.
fn fan_leans(
    axis: &MappingSpec,
    cells: &[Cell],
    seed: u64,
) -> Result<impl Fn(&str) -> (f64, [f64; 3])> {
    let leans = axis.leans(cells, seed)?;
    let mirrored = axis.mirrored(cells)?;
    Ok(move |id: &str| {
        let (share, toward) = leans.get(id).copied().unwrap_or((0.0, [0.0; 3]));
        match mirrored.get(id) {
            Some(normal) => (share, crate::aim::reflect(toward, *normal)),
            None => (share, toward),
        }
    })
}

/// A stable number per head identity.
fn identity(id: &str) -> u64 {
    id.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}

/// A period of zero beats lasts the whole clip.
fn whole_clip(period: f64, clip_duration: f64) -> f64 {
    if period == 0.0 {
        clip_duration
    } else {
        period
    }
}

/// Event k starts when the pace has counted k periods.
struct Schedule(Pace);
impl Schedule {
    /// Events that start at or before `beat`.
    fn count(&self, beat: f64) -> i64 {
        let turns = self.0.turns(beat);
        if turns < 0.0 {
            0
        } else {
            turns.floor() as i64 + 1
        }
    }
    fn time(&self, k: i64) -> Option<f64> {
        (k >= 0).then(|| self.0.beat_at(k as f64))
    }
}

/// Live events per sample as channels. `progress` is the share of each
/// event's life that has passed; `spacing` is the life share between an
/// event and the next one. Padding channels have zero presence.
fn event_life(
    times: &[f64],
    schedule: &Schedule,
    life: &Pace,
) -> Result<Vec<(&'static str, Signal)>> {
    let mut rows = Vec::with_capacity(times.len());
    let mut width = 1;
    for beat in times {
        if !beat.is_finite() {
            return Err(Error("musical time must be finite".into()));
        }
        let now = life.turns(*beat);
        let first = (schedule.count(life.beat_at(now - 1.0)) - 1).max(0);
        let last = schedule.count(*beat);
        if last - first > 1_000_000 {
            return Err(Error(
                "events are too dense; raise every or shorten the life".into(),
            ));
        }
        width = width.max((last - first).max(0) as usize);
        rows.push((now, first, last));
    }
    if times.len().saturating_mul(width) > 16_777_216 {
        return Err(Error(
            "event tensor exceeds 16,777,216 elements; request fewer time samples".into(),
        ));
    }
    let shape = (1, times.len(), width);
    let mut progress = Array3::from_elem(shape, 1.0);
    let mut present = Array3::zeros(shape);
    let mut index = Array3::zeros(shape);
    let mut spacing = Array3::from_elem(shape, 1.0);
    for (t, (now, first, last)) in rows.into_iter().enumerate() {
        for (e, k) in (first..last).enumerate() {
            let Some(start) = schedule.time(k) else {
                continue;
            };
            let born = life.turns(start);
            let age = now - born;
            progress[[0, t, e]] = age.clamp(0.0, 1.0);
            present[[0, t, e]] = if (0.0..1.0).contains(&age) { 1.0 } else { 0.0 };
            index[[0, t, e]] = k as f64;
            let gap = match (schedule.time(k + 1), schedule.time(k - 1)) {
                (Some(next), _) => life.turns(next) - born,
                (None, Some(previous)) => born - life.turns(previous),
                (None, None) => 1.0,
            };
            spacing[[0, t, e]] = gap;
        }
    }
    let channels = Channels::components(width)?;
    Ok(vec![
        (
            "progress",
            Signal::new(progress, Unit::Proportion, channels, None)?,
        ),
        (
            "present",
            Signal::new(present, Unit::Proportion, channels, None)?,
        ),
        ("index", Signal::new(index, Unit::Number, channels, None)?),
        (
            "spacing",
            Signal::new(spacing, Unit::Number, channels, None)?,
        ),
    ])
}

use crate::mapping::fixture_of;

/// The grain of each head. Grain 0 is the whole fixture. Grain N groups N
/// heads of one fixture in head order; 1 is each head alone.
fn grains(fixtures: &[String], grain: usize) -> Vec<usize> {
    let mut heads: BTreeMap<&str, Vec<(u64, &str, usize)>> = BTreeMap::new();
    for (n, id) in fixtures.iter().enumerate() {
        let head = id
            .rsplit_once(':')
            .and_then(|(_, head)| head.parse().ok())
            .unwrap_or(u64::MAX);
        heads.entry(fixture_of(id)).or_default().push((head, id, n));
    }
    let mut result = vec![0; fixtures.len()];
    let mut next = 0;
    for members in heads.values_mut() {
        members.sort();
        let size = if grain == 0 { members.len() } else { grain };
        for chunk in members.chunks(size.max(1)) {
            for (_, _, n) in chunk {
                result[*n] = next;
            }
            next += 1;
        }
    }
    result
}

/// A stable identity per grain: its first head's identity.
fn grain_keys(fixtures: &[String], grains: &[usize], count: usize) -> Vec<String> {
    let mut keys = vec![String::new(); count];
    for (id, g) in fixtures.iter().zip(grains) {
        if keys[*g].is_empty() {
            keys[*g] = id.clone();
        }
    }
    keys
}
