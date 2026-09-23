//! Primitives that forms are built from: an odometer clock, event life, a
//! keyframe curve and a random share of heads. None of them keeps state.
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
                ("every", beats("Every", "Beats between events", 4.0)),
                (
                    "every_curve",
                    curve("Every curve", "Beats between events, over the clip"),
                ),
                (
                    "stamps",
                    Input {
                        optional: true,
                        ..port(
                            "Stamped events",
                            "Event times in beats from the clip start",
                            ValueType::Events,
                            Rate::Fixed,
                            None,
                        )
                    },
                ),
                ("life", beats("Life", "Beats one event lasts", 2.0)),
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
            let period = inputs["period"].fixed_scalar()?;
            let period = if period == 0.0 {
                frame.clip_duration
            } else {
                period
            };
            let pace = Pace::new(
                frame.clip_start,
                frame.clip_duration,
                period,
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
            let schedule = match inputs.get("stamps").map(|value| value.control(0)) {
                Some(Value::Events(Events::Beats { times })) => {
                    Schedule::Stamps(times.as_slice().iter().map(|t| origin + t).collect())
                }
                Some(_) => {
                    return Err(Error(
                        "stamped events must be a list of beats from the clip start".into(),
                    ))
                }
                None => Schedule::Paced(Pace::new(
                    origin,
                    span,
                    inputs["every"].fixed_scalar()?,
                    keyframes(inputs, "every_curve"),
                )?),
            };
            let life = Pace::new(
                origin,
                span,
                inputs["life"].fixed_scalar()?,
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
        _ => unreachable!("form primitive"),
    }
}

enum Schedule {
    /// Absolute event beats, in order.
    Stamps(Vec<f64>),
    /// Event k starts when the pace has counted k periods.
    Paced(Pace),
}
impl Schedule {
    /// Events that start at or before `beat`.
    fn count(&self, beat: f64) -> i64 {
        match self {
            Self::Stamps(times) => times.partition_point(|t| *t <= beat) as i64,
            Self::Paced(pace) => {
                let turns = pace.turns(beat);
                if turns < 0.0 {
                    0
                } else {
                    turns.floor() as i64 + 1
                }
            }
        }
    }
    fn time(&self, k: i64) -> Option<f64> {
        match self {
            Self::Stamps(times) => usize::try_from(k).ok().and_then(|k| times.get(k)).copied(),
            Self::Paced(pace) => (k >= 0).then(|| pace.beat_at(k as f64)),
        }
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

/// The fixture part of a head identity (`fixture:head`).
fn fixture_of(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(fixture, _)| fixture)
}

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
