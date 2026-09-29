//! Tensor kernels for source graphs. Every invocation covers the entire batch;
//! event channels are separate from the three component output ports.
use super::clock_table::{self, Clock};
use super::noise::NoisePoint;
use crate::{
    runtime::{Batch, EvaluatedValue},
    *,
};
use ndarray::Array3;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOp {
    ClockTable,
    Clock,
    Events,
    ClipProgress,
    Latest,
    Time,
    Space,
    Random,
    Noise,
    Audio,
    Vector,
    Color,
    Aim,
    Multiply,
}
impl SourceOp {
    pub(crate) fn definition(self) -> Definition {
        let (numbers, control, output_names): (&[&str], Option<(&str, ValueType)>, &[&str]) =
            match self {
                Self::ClockTable => (&["period"], None, &["value"]),
                Self::Clock => (&["table"], None, &["value"]),
                Self::Events => (
                    &["every", "life", "once", "hold"],
                    None,
                    &["progress", "present", "index", "spacing"],
                ),
                Self::ClipProgress => (&[], None, &["value"]),
                Self::Latest => (&["value", "present", "index"], None, &["value"]),
                Self::Time => (
                    &["progress", "phase", "gain"],
                    Some(("curve", ValueType::Time)),
                    &["r", "g", "b"],
                ),
                Self::Space => (
                    &[
                        "position", "offset", "gain", "width", "spacing", "progress", "vector",
                        "x", "y", "z",
                    ],
                    Some(("settings", ValueType::Space)),
                    &["r", "g", "b"],
                ),
                Self::Random => (
                    &["coverage", "level", "index", "unit", "keys"],
                    None,
                    &["value"],
                ),
                Self::Noise => (
                    &[
                        "turns", "contrast", "low", "high", "scale", "salt", "vector", "x", "y",
                        "seed0_lo", "seed0_hi", "seed1_lo", "seed1_hi", "seed2_lo", "seed2_hi",
                    ],
                    Some(("settings", ValueType::Noise)),
                    &["r", "g", "b"],
                ),
                Self::Audio => (
                    &["energy", "minimum", "maximum", "gain", "floor", "threshold"],
                    None,
                    &["value"],
                ),
                Self::Vector => (&["r", "g", "b"], None, &["value"]),
                Self::Color => (
                    &[
                        "r",
                        "g",
                        "b",
                        "level",
                        "fade",
                        "present",
                        "index",
                        "color_present",
                        "color_index",
                        "same_events",
                    ],
                    None,
                    &["value"],
                ),
                Self::Aim => (
                    &[
                        "direction",
                        "point",
                        "lean",
                        "yaw",
                        "pitch",
                        "fade",
                        "point_mode",
                        "mirror",
                        "px",
                        "py",
                        "pz",
                        "mx",
                        "my",
                        "mz",
                    ],
                    None,
                    &["value", "turn"],
                ),
                Self::Multiply => (&["a", "b"], None, &["value"]),
            };
        let mut inputs: BTreeMap<_, _> = numbers
            .iter()
            .map(|k| {
                (
                    (*k).into(),
                    crate::signals::port(k, ValueType::Signal(SignalType::ANY), None),
                )
            })
            .collect();
        if let Some((key, value_type)) = control {
            inputs.insert(
                key.into(),
                Input {
                    name: key.into(),
                    description: String::new(),
                    optional: false,
                    value_type,
                    rate: Rate::Fixed,
                    default: None,
                    author: None,
                    promotable: vec![],
                },
            );
        }
        Definition {
            name: format!("Source {self:?}"),
            inputs,
            outputs: output_names
                .iter()
                .map(|k| {
                    (
                        (*k).into(),
                        Output {
                            value_type: ValueType::Signal(SignalType::ANY),
                            rate: if self == Self::ClockTable {
                                Rate::Fixed
                            } else {
                                Rate::Frame
                            },
                        },
                    )
                })
                .collect(),
            body: Body::Primitive(Primitive::Source(self)),
        }
    }
}
fn shape(signals: &[&Signal]) -> Result<(usize, usize, usize)> {
    let mut out = (1, 1, 1);
    for s in signals {
        let d = s.values().dim();
        for (a, b) in [(&mut out.0, d.0), (&mut out.1, d.1), (&mut out.2, d.2)] {
            if *a != 1 && b != 1 && *a != b {
                return Err(Error("source tensor axes do not broadcast".into()));
            }
            if *a == 1 {
                *a = b;
            }
        }
    }
    if out.0.saturating_mul(out.1).saturating_mul(out.2) > 16_777_216 {
        return Err(Error("source tensor exceeds 16,777,216 elements".into()));
    }
    Ok(out)
}
fn signal(values: Array3<f64>, channels: Channels, batch: &Batch<'_>) -> Result<EvaluatedValue> {
    let fixtures = (values.dim().0 != 1).then(|| batch.fixtures.to_vec().into());
    EvaluatedValue::from_signal(
        ValueType::Signal(SignalType::ANY),
        Signal::new(values, Unit::Number, channels, fixtures)?,
    )
}
fn single(value: EvaluatedValue) -> Result<BTreeMap<String, EvaluatedValue>> {
    Ok(BTreeMap::from([("value".into(), value)]))
}
fn components(
    values: [Array3<f64>; 3],
    batch: &Batch<'_>,
) -> Result<BTreeMap<String, EvaluatedValue>> {
    ["r", "g", "b"]
        .into_iter()
        .zip(values)
        .map(|(k, v)| {
            Ok((k.into(), {
                let channels = Channels::components(v.dim().2)?;
                signal(v, channels, batch)?
            }))
        })
        .collect()
}
fn latest(present: &Signal, index: &Signal, n: usize, t: usize) -> Option<usize> {
    (0..present.values().dim().2.max(index.values().dim().2))
        .filter(|e| present.at(n, t, *e) > 0.)
        .max_by(|a, b| index.at(n, t, *a).total_cmp(&index.at(n, t, *b)))
}
pub(crate) fn run(
    op: SourceOp,
    inputs: &BTreeMap<String, EvaluatedValue>,
    batch: Batch<'_>,
) -> Result<BTreeMap<String, EvaluatedValue>> {
    let s = |k: &str| inputs[k].numeric();
    let f = |k: &str| inputs[k].fixed_scalar();
    let clock = |table, head| Clock {
        table,
        head,
        start: batch.frame.clip_start,
        span: batch.frame.clip_duration,
    };
    match op {
        SourceOp::ClockTable => single(EvaluatedValue::from_signal(
            ValueType::Signal(SignalType::ANY),
            clock_table::integrate(s("period"), batch.frame)?,
        )?),
        SourceOp::Clock => {
            let table = s("table");
            let values = Array3::from_shape_fn(
                (table.values().dim().0, batch.times.len(), 1),
                |(n, t, _)| clock(table, n).turns(batch.times[t]),
            );
            single(signal(values, Channels::Value, &batch)?)
        }
        SourceOp::ClipProgress => single(signal(
            Array3::from_shape_fn((1, batch.times.len(), 1), |(_, t, _)| {
                ((batch.times[t] - batch.frame.clip_start) / batch.frame.clip_duration)
                    .clamp(0., 1.)
            }),
            Channels::Value,
            &batch,
        )?),
        SourceOp::Events => {
            let every = s("every");
            let life = s("life");
            let once = f("once")? != 0.;
            let hold = f("hold")? != 0.;
            let heads = every.values().dim().0.max(life.values().dim().0);
            let times = batch.times.len();
            let mut rows = Vec::with_capacity(heads * times);
            let mut width = 1;
            for n in 0..heads {
                let schedule = clock(every, n);
                let life = clock(life, n);
                for beat in batch.times {
                    let now = life.turns(*beat);
                    let last = if once {
                        1
                    } else {
                        (schedule.turns(*beat).floor() as i64 + 1).max(0)
                    };
                    let first = if once {
                        0
                    } else {
                        (schedule.turns(life.beat_at(now - 1.)).floor() as i64).max(0)
                    };
                    if last - first > 65535 {
                        return Err(Error(
                            "events are too dense; raise every or shorten life".into(),
                        ));
                    }
                    width = width.max((last - first).max(0) as usize);
                    rows.push((now, first, last));
                }
            }
            if heads.saturating_mul(times).saturating_mul(width) > 16_777_216 {
                return Err(Error("event tensor exceeds 16,777,216 elements".into()));
            }
            let dims = (heads, times, width);
            let mut progress = Array3::zeros(dims);
            let mut present = Array3::zeros(dims);
            let mut index = Array3::zeros(dims);
            let mut spacing = Array3::ones(dims);
            for n in 0..heads {
                let schedule = clock(every, n);
                let life = clock(life, n);
                for t in 0..times {
                    let (now, first, last) = rows[n * times + t];
                    for (e, k) in (first..last).enumerate() {
                        let born = life.turns(schedule.beat_at(k as f64));
                        let age = now - born;
                        progress[[n, t, e]] = age.clamp(0., 1.);
                        present[[n, t, e]] = if (0. ..1.).contains(&age) || hold {
                            1.
                        } else {
                            0.
                        };
                        index[[n, t, e]] = k as f64;
                        spacing[[n, t, e]] = if once {
                            1.
                        } else {
                            life.turns(schedule.beat_at(k as f64 + 1.)) - born
                        };
                    }
                }
            }
            ["progress", "present", "index", "spacing"]
                .into_iter()
                .zip([progress, present, index, spacing])
                .map(|(k, v)| Ok((k.into(), signal(v, Channels::components(width)?, &batch)?)))
                .collect()
        }
        SourceOp::Latest => {
            let value = s("value");
            let present = s("present");
            let index = s("index");
            let (n, t, _) = shape(&[value, present, index])?;
            single(signal(
                Array3::from_shape_fn((n, t, 1), |(n, t, _)| {
                    latest(present, index, n, t).map_or(0., |e| value.at(n, t, e))
                }),
                Channels::Value,
                &batch,
            )?)
        }
        SourceOp::Time => {
            let Value::Time(settings) = inputs["curve"].control(0) else {
                unreachable!()
            };
            let progress = s("progress");
            let phase = s("phase");
            let gain = s("gain");
            let dims = shape(&[progress, phase, gain])?;
            let mut out = std::array::from_fn(|_| Array3::zeros(dims));
            for ((n, t, e), _) in progress.values().broadcast(dims).unwrap().indexed_iter() {
                let p = progress.at(n, t, e);
                let ph = phase.at(n, t, e);
                let p = if ph == 0. { p } else { (p + ph).rem_euclid(1.) };
                let v = match &settings.curve {
                    SourceCurve::Keys(k) => k.sample(p),
                    SourceCurve::Gradient(g) => g.gradient.sample(g.curve.sample(p)),
                };
                for ch in 0..3 {
                    out[ch][[n, t, e]] = v[ch] * gain.at(n, t, e);
                }
            }
            components(out, &batch)
        }
        SourceOp::Space => {
            let Value::Space(settings) = inputs["settings"].control(0) else {
                unreachable!()
            };
            let position = s("position");
            let offset = s("offset");
            let gain = s("gain");
            let width = s("width");
            let spacing = s("spacing");
            let progress = s("progress");
            let xyz = [s("x"), s("y"), s("z")];
            let dims = shape(&[
                position, offset, gain, width, spacing, progress, xyz[0], xyz[1], xyz[2],
            ])?;
            let vector = f("vector")? != 0.;
            let mut out = std::array::from_fn(|_| Array3::zeros(dims));
            for n in 0..dims.0 {
                for t in 0..dims.1 {
                    for e in 0..dims.2 {
                        let g = gain.at(n, t, e);
                        if vector && settings.offset.is_none() {
                            for ch in 0..3 {
                                out[ch][[n, t, e]] = xyz[ch].at(n, t, e) * g;
                            }
                            continue;
                        }
                        let mut across = position.at(n, t, e);
                        let mut visible = true;
                        let mut backward = false;
                        if let Some(offset_source) = &settings.offset {
                            let (glides, reverse) =
                                if let Value::Time(time) = offset_source.as_ref() {
                                    if let SourceCurve::Keys(keys) = &time.curve {
                                        let p = progress.at(n, t, e);
                                        let a = keys.sample(p + 1e-3)[0];
                                        let b = keys.sample(p - 1e-3)[0];
                                        (
                                            keys.points.iter().all(|p| p.ease != Ease::Hold),
                                            if (a - b).abs() > 1e-9 {
                                                a < b
                                            } else {
                                                keys.sample(1.)[0] < keys.sample(0.)[0]
                                            },
                                        )
                                    } else {
                                        (false, false)
                                    }
                                } else {
                                    (false, false)
                                };
                            backward = reverse;
                            let overrun = settings.boundary == Boundary::Clip && glides;
                            let mut w = width.at(n, t, e);
                            if settings.width_relative {
                                let gap =
                                    (w * spacing.at(n, t, e)).min(if overrun { 0.8 } else { 4. });
                                w = if overrun { gap / (1. - gap) } else { gap };
                            }
                            let off = offset.at(n, t, e);
                            let center = off + if overrun { (off - 0.5) * w } else { 0. };
                            let distance = if settings.boundary == Boundary::Wrap {
                                (across - center + 0.5).rem_euclid(1.) - 0.5
                            } else {
                                across - center
                            };
                            across = distance / w.max(1e-9) + 0.5;
                            let edge = if overrun { 0. } else { 2e-6 };
                            visible = w > 1e-9 && across + edge > 1e-9 && 1. + edge - across > 1e-9;
                        }
                        let directed = if backward { 1. - across } else { across };
                        let v = if let Some(gradient) = &settings.gradient {
                            gradient.sample(directed)
                        } else {
                            [settings.curve.as_ref().unwrap().sample(directed); 3]
                        };
                        for ch in 0..3 {
                            out[ch][[n, t, e]] = if visible { v[ch] * g } else { 0. };
                        }
                    }
                }
            }
            components(out, &batch)
        }
        SourceOp::Random => {
            let coverage = s("coverage");
            let level = s("level");
            let index = s("index");
            let unit = s("unit");
            let keys = s("keys");
            let dims = shape(&[coverage, level, index, unit])?;
            let count = keys.values().dim().1;
            let mut ranks: HashMap<i64, Vec<usize>> = HashMap::new();
            for event in index.values() {
                let event = *event as i64;
                ranks.entry(event).or_insert_with(|| {
                    let seed = crate::spatial::epoch_seed(batch.frame.seed, event);
                    let mut order: Vec<_> = (0..count)
                        .map(|g| {
                            let mut h = 0xcbf29ce484222325_u64 ^ seed;
                            for b in 1..=keys.at(0, g, 0) as usize {
                                h = (h ^ (keys.at(0, g, b) as u64)).wrapping_mul(0x100000001b3);
                            }
                            h = (h ^ (h >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                            h = (h ^ (h >> 27)).wrapping_mul(0x94d049bb133111eb);
                            h ^= h >> 31;
                            (((h >> 11) as f64 + 0.5) / 9007199254740992., g)
                        })
                        .collect();
                    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                    let mut rank = vec![0; count];
                    for (r, (_, g)) in order.into_iter().enumerate() {
                        rank[g] = r;
                    }
                    rank
                });
            }
            let out = Array3::from_shape_fn(dims, |(n, t, e)| {
                let lit = (coverage.at(n, t, e).clamp(0., 1.) * count as f64).round() as usize;
                if ranks[&(index.at(n, t, e) as i64)][unit.at(n, t, e) as usize] < lit {
                    level.at(n, t, e)
                } else {
                    0.
                }
            });
            single(signal(out, Channels::components(dims.2)?, &batch)?)
        }
        SourceOp::Noise => {
            let Value::Noise(settings) = inputs["settings"].control(0) else {
                unreachable!()
            };
            let turns = s("turns");
            let contrast = s("contrast");
            let low = s("low");
            let high = s("high");
            let scale = s("scale");
            let x = s("x");
            let y = s("y");
            let salt = f("salt")?;
            let seeds = [
                (s("seed0_lo"), s("seed0_hi")),
                (s("seed1_lo"), s("seed1_hi")),
                (s("seed2_lo"), s("seed2_hi")),
            ];
            let dims = shape(&[turns, contrast, low, high, scale, x, y, seeds[0].0])?;
            let mut out = std::array::from_fn(|_| Array3::zeros(dims));
            let vector = f("vector")? != 0.;
            for n in 0..dims.0 {
                for t in 0..dims.1 {
                    for e in 0..dims.2 {
                        for ch in 0..if vector { 3 } else { 1 } {
                            let point = if settings.independent {
                                NoisePoint::Own(
                                    (seeds[ch].0.at(n, t, e) as u64)
                                        | ((seeds[ch].1.at(n, t, e) as u64) << 32),
                                )
                            } else if settings.scale.is_some() {
                                NoisePoint::Field {
                                    x: x.at(n, t, e),
                                    y: y.at(n, t, e),
                                    scale: scale.at(n, t, e),
                                }
                            } else {
                                NoisePoint::Shared { salt, channel: ch }
                            };
                            out[ch][[n, t, e]] = super::noise::level(
                                point,
                                turns.at(n, t, e),
                                contrast.at(n, t, e),
                                low.at(n, t, e),
                                high.at(n, t, e),
                                batch.frame.seed,
                            )?;
                        }
                    }
                }
            }
            if !vector {
                out[1] = out[0].clone();
                out[2] = out[0].clone();
            }
            components(out, &batch)
        }
        SourceOp::Audio => {
            let raw = s("energy");
            let low = f("minimum")?;
            let high = f("maximum")?;
            let floor = f("floor")?;
            let threshold = f("threshold")?;
            let gain = s("gain");
            let dims = shape(&[raw, gain])?;
            single(signal(
                Array3::from_shape_fn(dims, |(n, t, e)| {
                    let energy = if high - low > 1e-9 {
                        ((raw.at(n, t, e) - low) / (high - low)).clamp(0., 1.)
                    } else {
                        0.
                    };
                    if energy < threshold {
                        0.
                    } else {
                        (floor + (1. - floor) * energy) * gain.at(n, t, e)
                    }
                }),
                Channels::components(dims.2)?,
                &batch,
            )?)
        }
        SourceOp::Multiply => {
            let a = s("a");
            let b = s("b");
            let dims = shape(&[a, b])?;
            single(signal(
                Array3::from_shape_fn(dims, |(n, t, e)| a.at(n, t, e) * b.at(n, t, e)),
                Channels::components(dims.2)?,
                &batch,
            )?)
        }
        SourceOp::Vector => {
            let rgb = [s("r"), s("g"), s("b")];
            let (n, t, _) = shape(&rgb)?;
            single(signal(
                Array3::from_shape_fn((n, t, 3), |(n, t, ch)| rgb[ch].at(n, t, 0)),
                crate::tensor::VECTOR,
                &batch,
            )?)
        }
        SourceOp::Color => {
            let rgb = [s("r"), s("g"), s("b")];
            let level = s("level");
            let fade = s("fade");
            let present = s("present");
            let index = s("index");
            let color_index = s("color_index");
            let color_present = s("color_present");
            let same = f("same_events")? != 0.;
            let signals = [
                rgb[0],
                rgb[1],
                rgb[2],
                level,
                fade,
                present,
                index,
                color_index,
                color_present,
            ];
            let n = signals.iter().map(|s| s.values().dim().0).max().unwrap();
            let t = if signals.iter().any(|s| s.values().dim().1 == 0) {
                0
            } else {
                signals.iter().map(|s| s.values().dim().1).max().unwrap()
            };
            let events = level
                .values()
                .dim()
                .2
                .max(present.values().dim().2)
                .max(index.values().dim().2);
            let mut out = Array3::zeros((n, t, 3));
            for n in 0..n {
                for t in 0..t {
                    let winner =
                        (0..events)
                            .filter(|e| present.at(n, t, *e) > 0.)
                            .max_by(|a, b| {
                                level
                                    .at(n, t, *a)
                                    .total_cmp(&level.at(n, t, *b))
                                    .then(index.at(n, t, *a).total_cmp(&index.at(n, t, *b)))
                            });
                    if let Some(e) = winner {
                        let selected = if same {
                            (0..color_index
                                .values()
                                .dim()
                                .2
                                .max(color_present.values().dim().2))
                                .find(|c| {
                                    color_present.at(n, t, *c) > 0.
                                        && color_index.at(n, t, *c) == index.at(n, t, e)
                                })
                                .or_else(|| latest(color_present, color_index, n, t))
                        } else {
                            Some(0)
                        };
                        if let Some(c) = selected {
                            let brightness = level.at(n, t, e) * fade.at(n, t, 0).clamp(0., 1.);
                            for ch in 0..3 {
                                out[[n, t, ch]] = rgb[ch].at(n, t, c) * brightness;
                            }
                        }
                    }
                }
            }
            single(signal(out, Channels::Rgb, &batch)?)
        }
        SourceOp::Aim => {
            let direction = s("direction");
            let point = s("point");
            let lean = s("lean");
            let yaw = s("yaw");
            let pitch = s("pitch");
            let fade = s("fade");
            let mirror = s("mirror");
            let xyz = [s("px"), s("py"), s("pz")];
            let m = [s("mx"), s("my"), s("mz")];
            let point_mode = f("point_mode")? != 0.;
            let heads = batch.fixtures.len();
            let times = if [direction, point, lean, yaw, pitch, fade]
                .iter()
                .any(|s| s.values().dim().1 == 0)
            {
                0
            } else {
                [direction, point, lean, yaw, pitch, fade]
                    .iter()
                    .map(|s| s.values().dim().1)
                    .max()
                    .unwrap_or(1)
            };
            let mut aim = Array3::zeros((heads, times, 3));
            let mut turns = Array3::zeros((heads, times, crate::aim::TURN_CHANNELS));
            for n in 0..heads {
                for t in 0..times {
                    let direction = std::array::from_fn(|ch| {
                        if point_mode {
                            point.at(n, t, ch) - xyz[ch].at(n, t, 0)
                        } else {
                            direction.at(n, t, ch)
                        }
                    });
                    let lean = std::array::from_fn(|ch| lean.at(n, t, ch));
                    let yaw = yaw.at(n, t, 0);
                    let pitch = pitch.at(n, t, 0);
                    let fade = fade.at(n, t, 0).clamp(0., 1.);
                    let mirror = (mirror.at(n, t, 0) != 0.)
                        .then(|| std::array::from_fn(|ch| m[ch].at(n, t, 0)));
                    let turn = crate::aim::Turn {
                        lean,
                        yaw,
                        pitch,
                        mirror,
                        alpha: fade,
                    };
                    let leaned =
                        crate::aim::lean(direction, lean, crate::aim::dot(lean, lean).sqrt());
                    let moved = crate::aim::mirrored_offset(leaned, yaw, pitch, mirror);
                    for ch in 0..3 {
                        aim[[n, t, ch]] = moved[ch] * fade;
                    }
                    for (ch, v) in turn.channels().into_iter().enumerate() {
                        turns[[n, t, ch]] = v;
                    }
                }
            }
            Ok(BTreeMap::from([
                ("value".into(), signal(aim, crate::tensor::VECTOR, &batch)?),
                (
                    "turn".into(),
                    signal(
                        turns,
                        Channels::components(crate::aim::TURN_CHANNELS)?,
                        &batch,
                    )?,
                ),
            ]))
        }
    }
}
