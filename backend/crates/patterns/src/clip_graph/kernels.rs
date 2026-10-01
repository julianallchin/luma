//! The tensor kernels a clip graph lowers onto. Every call covers the whole
//! batch. Tensors are (heads, time, channels); a wire downstream of a clock
//! carries that clock's live events on the channel axis, and a vector or
//! color travels as three component ports.
use super::clock_table::{self, Clock};
use super::heads;
use crate::runtime::{Batch, EvaluatedValue};
use crate::*;
use ndarray::Array3;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kernel {
    /// A clock's period integrated over the clip, prepared once.
    ClockTable,
    /// Turns of a clock table at each beat.
    Clock,
    /// The live events of a clock: progress, presence and index per event.
    Events,
    /// Progress 0–1 over the clip.
    ClipProgress,
    /// Each head's own clock: progress less its delay over the duration,
    /// then, when a phase is set, plus the phase, wrapped.
    Shift,
    /// Each unit's position: the centroid of its heads.
    Group,
    /// Positions folded across a mirror plane.
    Fold,
    /// Each unit's rank in its span, shuffled per event.
    Rank,
    /// The raw axis coordinate of each head within its span.
    Axis,
    /// A coordinate slid by a shift and read over a scale, wrapped on a
    /// ring: the shader's `(p - shift) / scale`.
    Slide,
    /// Coherent noise over normalized position and time.
    Noise4,
    /// A curve's shape between low and high, or through a gradient.
    Curve,
    /// The live events of one clock as one layer: the largest value per
    /// channel (light), or each newer event over the older by its weight
    /// (aim). No live event is transparent.
    Combine,
    /// Two values combined by a math node's op: `*`, `+`, `-`, `max` or
    /// `min`.
    Math,
    ColorOut,
    StrobeOut,
    AimOut,
}

/// How many stacked mirrors an aim reads.
pub(crate) const AIM_MIRRORS: usize = 4;

impl Kernel {
    pub const ALL: [Kernel; 17] = [
        Kernel::ClockTable,
        Kernel::Clock,
        Kernel::Events,
        Kernel::ClipProgress,
        Kernel::Shift,
        Kernel::Group,
        Kernel::Fold,
        Kernel::Rank,
        Kernel::Axis,
        Kernel::Slide,
        Kernel::Noise4,
        Kernel::Curve,
        Kernel::Combine,
        Kernel::Math,
        Kernel::ColorOut,
        Kernel::StrobeOut,
        Kernel::AimOut,
    ];

    /// The kernel's id in the library.
    pub fn id(self) -> &'static str {
        match self {
            Kernel::ClockTable => "kernel/clock_table",
            Kernel::Clock => "kernel/clock",
            Kernel::Events => "kernel/events",
            Kernel::ClipProgress => "kernel/clip_progress",
            Kernel::Shift => "kernel/shift",
            Kernel::Group => "kernel/group",
            Kernel::Fold => "kernel/fold",
            Kernel::Rank => "kernel/rank",
            Kernel::Axis => "kernel/axis",
            Kernel::Slide => "kernel/slide",
            Kernel::Noise4 => "kernel/noise4",
            Kernel::Curve => "kernel/curve",
            Kernel::Combine => "kernel/combine",
            Kernel::Math => "kernel/math",
            Kernel::ColorOut => "kernel/color_out",
            Kernel::StrobeOut => "kernel/strobe_out",
            Kernel::AimOut => "kernel/aim_out",
        }
    }

    pub(crate) fn reads_time(self) -> bool {
        matches!(
            self,
            Kernel::ClockTable | Kernel::Clock | Kernel::Events | Kernel::ClipProgress
        )
    }

    /// (numeric ports, fixed number ports, outputs).
    fn ports(
        self,
    ) -> (
        Vec<String>,
        &'static [&'static str],
        &'static [&'static str],
    ) {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match self {
            Kernel::ClockTable => (names(&["period"]), &[], &["value"]),
            Kernel::Clock => (names(&["table"]), &[], &["value"]),
            Kernel::Events => (
                names(&["every", "duration"]),
                &[],
                &["progress", "present", "index"],
            ),
            Kernel::ClipProgress => (vec![], &[], &["value"]),
            Kernel::Shift => (
                names(&["progress", "delay", "duration", "phase"]),
                &["has_phase"],
                &["value"],
            ),
            Kernel::Group => (names(&["positions", "unit"]), &[], &["positions"]),
            Kernel::Fold => (
                names(&[
                    "positions",
                    "base",
                    "span",
                    "normal_x",
                    "normal_y",
                    "normal_z",
                    "at",
                ]),
                &["has_normal"],
                &["positions", "folded", "normal"],
            ),
            Kernel::Rank => (names(&["index", "unit", "span", "first"]), &[], &["value"]),
            Kernel::Axis => (
                names(&[
                    "positions",
                    "base",
                    "unit",
                    "span",
                    "rank",
                    "dir_x",
                    "dir_y",
                    "dir_z",
                    "centre_x",
                    "centre_y",
                    "centre_z",
                ]),
                &["kind", "has_direction", "wrap", "shuffled"],
                &["value"],
            ),
            Kernel::Slide => (names(&["a", "at", "shift", "scale"]), &["wrap"], &["value"]),
            Kernel::Noise4 => (
                names(&["positions", "span", "turns", "scale", "contrast"]),
                &["uniform", "salt_lo", "salt_hi"],
                &["value"],
            ),
            Kernel::Curve => (
                names(&["x", "low_x", "low_y", "low_z", "high_x", "high_y", "high_z"]),
                &["kind"],
                &["x", "y", "z"],
            ),
            Kernel::Combine => (
                names(&["value", "weight", "present"]),
                &["over"],
                &["value", "alpha"],
            ),
            Kernel::Math => (names(&["a", "b"]), &["op"], &["value"]),
            Kernel::ColorOut => (names(&["r", "g", "b", "alpha"]), &[], &["color"]),
            Kernel::StrobeOut => (names(&["rate", "alpha"]), &[], &["strobe"]),
            Kernel::AimOut => {
                let mut ports = names(&[
                    "dir_x",
                    "dir_y",
                    "dir_z",
                    "point_x",
                    "point_y",
                    "point_z",
                    "yaw",
                    "pitch",
                    "alpha",
                    "positions",
                ]);
                for m in 0..AIM_MIRRORS {
                    ports.push(format!("fold{m}"));
                    ports.push(format!("normal{m}"));
                }
                (ports, &["base", "mirrors"], &["aim", "turn"])
            }
        }
    }

    pub(crate) fn definition(self) -> Definition {
        let (numbers, fixed, outputs) = self.ports();
        let mut inputs: BTreeMap<String, Input> = numbers
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    crate::graph::port(name, ValueType::Signal(SignalType::ANY), None),
                )
            })
            .collect();
        for name in fixed {
            inputs.insert(
                name.to_string(),
                Input {
                    rate: Rate::Fixed,
                    ..crate::graph::port(name, ValueType::Number, None)
                },
            );
        }
        if self == Kernel::Curve {
            for (name, kind) in [
                ("shape", ValueType::Points),
                ("gradient", ValueType::Gradient),
            ] {
                inputs.insert(
                    name.into(),
                    Input {
                        rate: Rate::Fixed,
                        ..crate::graph::port(name, kind, None)
                    },
                );
            }
        }
        Definition {
            name: format!("{self:?}"),
            inputs,
            outputs: outputs
                .iter()
                .map(|name| {
                    (
                        name.to_string(),
                        Output {
                            value_type: ValueType::Signal(SignalType::ANY),
                            rate: if self == Kernel::ClockTable {
                                Rate::Fixed
                            } else {
                                Rate::Frame
                            },
                        },
                    )
                })
                .collect(),
            body: Body::Primitive(Primitive::Kernel(self)),
        }
    }
}

/// The broadcast shape of `signals`.
fn shape(signals: &[&Signal]) -> Result<(usize, usize, usize)> {
    let mut out = (1, 1, 1);
    for s in signals {
        let d = s.values().dim();
        for (a, b) in [(&mut out.0, d.0), (&mut out.1, d.1), (&mut out.2, d.2)] {
            if *a != 1 && b != 1 && *a != b {
                return Err(Error("clip graph tensor axes do not broadcast".into()));
            }
            if *a == 1 {
                *a = b;
            }
        }
    }
    if out.0.saturating_mul(out.1).saturating_mul(out.2) > 16_777_216 {
        return Err(Error(
            "clip graph tensor exceeds 16,777,216 elements".into(),
        ));
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

fn events(values: Array3<f64>, batch: &Batch<'_>) -> Result<EvaluatedValue> {
    let channels = Channels::components(values.dim().2)?;
    signal(values, channels, batch)
}

fn vector_at(s: &Signal, n: usize, t: usize) -> [f64; 3] {
    std::array::from_fn(|ch| s.at(n, t, ch))
}

/// A three-channel tensor of per-head, per-time vectors.
fn vectors(heads: usize, times: usize, at: impl Fn(usize, usize) -> [f64; 3]) -> Array3<f64> {
    let mut values = Array3::zeros((heads, times, 3));
    for n in 0..heads {
        for t in 0..times {
            let v = at(n, t);
            for ch in 0..3 {
                values[[n, t, ch]] = v[ch];
            }
        }
    }
    values
}

/// The cells of each span, in cell order.
fn spans(span: &Signal, heads: usize) -> BTreeMap<i64, Vec<usize>> {
    let mut spans: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for n in 0..heads {
        spans.entry(span.at(n, 0, 0) as i64).or_default().push(n);
    }
    spans
}

/// The first cell of each unit among `members`.
fn unit_heads(unit: &Signal, members: &[usize]) -> Vec<usize> {
    let mut seen = Vec::new();
    let mut firsts = Vec::new();
    for n in members {
        let u = unit.at(*n, 0, 0) as i64;
        if !seen.contains(&u) {
            seen.push(u);
            firsts.push(*n);
        }
    }
    firsts
}

/// A premultiplied value over its alpha, clamped to 0–1; 0 where the
/// layer is transparent.
fn unpremultiply(value: f64, alpha: f64) -> f64 {
    if alpha > 1e-12 {
        (value / alpha).clamp(0., 1.)
    } else {
        0.
    }
}

pub(crate) fn run(
    kernel: Kernel,
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
    let single = |key: &str, value: EvaluatedValue| Ok(BTreeMap::from([(key.to_string(), value)]));
    let heads = batch.fixtures.len();
    match kernel {
        Kernel::ClockTable => single(
            "value",
            EvaluatedValue::from_signal(
                ValueType::Signal(SignalType::ANY),
                clock_table::integrate(s("period"), batch.frame)?,
            )?,
        ),
        Kernel::Clock => {
            let table = s("table");
            let values = Array3::from_shape_fn(
                (table.values().dim().0, batch.times.len(), 1),
                |(n, t, _)| clock(table, n).turns(batch.times[t]),
            );
            single("value", signal(values, Channels::Value, &batch)?)
        }
        Kernel::ClipProgress => single(
            "value",
            signal(
                Array3::from_shape_fn((1, batch.times.len(), 1), |(_, t, _)| {
                    ((batch.times[t] - batch.frame.clip_start) / batch.frame.clip_duration)
                        .clamp(0., 1.)
                }),
                Channels::Value,
                &batch,
            )?,
        ),
        Kernel::Events => {
            let every = s("every");
            let duration = s("duration");
            let rows_n = every.values().dim().0.max(duration.values().dim().0);
            let times = batch.times.len();
            let mut rows = Vec::with_capacity(rows_n * times);
            let mut width = 1;
            for n in 0..rows_n {
                let schedule = clock(every, n);
                let life = clock(duration, n);
                for beat in batch.times {
                    let now = life.turns(*beat);
                    let last = (schedule.turns(*beat).floor() as i64 + 1).max(0);
                    let first = (schedule.turns(life.beat_at(now - 1.)).floor() as i64).max(0);
                    if last - first > 65535 {
                        return Err(Error(
                            "events are too dense; raise every or shorten duration".into(),
                        ));
                    }
                    width = width.max((last - first).max(0) as usize);
                    rows.push((now, first, last));
                }
            }
            if rows_n.saturating_mul(times).saturating_mul(width) > 16_777_216 {
                return Err(Error("event tensor exceeds 16,777,216 elements".into()));
            }
            let dims = (rows_n, times, width);
            let mut progress = Array3::zeros(dims);
            let mut present = Array3::zeros(dims);
            let mut index = Array3::zeros(dims);
            for n in 0..rows_n {
                let schedule = clock(every, n);
                let life = clock(duration, n);
                for t in 0..times {
                    let (now, first, last) = rows[n * times + t];
                    for (e, k) in (first..last).enumerate() {
                        let born = life.turns(schedule.beat_at(k as f64));
                        let age = now - born;
                        progress[[n, t, e]] = age.clamp(0., 1.);
                        present[[n, t, e]] = if (0. ..1.).contains(&age) { 1. } else { 0. };
                        index[[n, t, e]] = k as f64;
                    }
                }
            }
            Ok(BTreeMap::from([
                ("progress".into(), events(progress, &batch)?),
                ("present".into(), events(present, &batch)?),
                ("index".into(), events(index, &batch)?),
            ]))
        }
        Kernel::Shift => {
            let progress = s("progress");
            let delay = s("delay");
            let duration = s("duration");
            let phase = s("phase");
            let wraps = f("has_phase")? != 0.;
            let dims = shape(&[progress, delay, duration, phase])?;
            let values = Array3::from_shape_fn(dims, |(n, t, e)| {
                // A delay in beats moves the head's clock by its share of
                // the event. Below 0 before the head starts, so a curve
                // holds its first value there; never clamped. A phase, any
                // number and 0 too, wraps the clock: fract(τ + phase).
                let duration = duration.at(n, t, e).max(1e-9);
                let local = progress.at(n, t, e) - delay.at(n, t, e) / duration;
                if wraps {
                    (local + phase.at(n, t, e)).rem_euclid(1.)
                } else {
                    local
                }
            });
            single("value", events(values, &batch)?)
        }
        Kernel::Group => {
            let positions = s("positions");
            let unit = s("unit");
            let times = positions.values().dim().1;
            let mut sums: HashMap<(i64, usize), ([f64; 3], f64)> = HashMap::new();
            for n in 0..heads {
                for t in 0..times {
                    let entry = sums
                        .entry((unit.at(n, 0, 0) as i64, t))
                        .or_insert(([0.; 3], 0.));
                    let p = vector_at(positions, n, t);
                    for a in 0..3 {
                        entry.0[a] += p[a];
                    }
                    entry.1 += 1.;
                }
            }
            let values = vectors(heads, times, |n, t| {
                let (sum, count) = sums[&(unit.at(n, 0, 0) as i64, t)];
                sum.map(|v| v / count)
            });
            single("positions", signal(values, crate::tensor::VECTOR, &batch)?)
        }
        Kernel::Fold => {
            let positions = s("positions");
            let base = s("base");
            let span = s("span");
            let normal = [s("normal_x"), s("normal_y"), s("normal_z")];
            let at = s("at");
            let given = f("has_normal")? != 0.;
            let times = shape(&[positions, base, normal[0], normal[1], normal[2], at])?.1;
            let mut moved = Array3::zeros((heads, times, 3));
            let mut folded = Array3::zeros((heads, times, 1));
            let mut normals = Array3::zeros((heads, times, 3));
            for t in 0..times {
                for members in spans(span, heads).values() {
                    let points: Vec<[f64; 3]> = members
                        .iter()
                        .map(|n| vector_at(positions, *n, t))
                        .collect();
                    // The plane sits within the span's positions before
                    // any fold, so 0.5 is its centre for every mirror.
                    let originals: Vec<[f64; 3]> =
                        members.iter().map(|n| vector_at(base, *n, t)).collect();
                    let unit = if given {
                        heads::unit_direction(std::array::from_fn(|a| normal[a].at(0, t, 0)))
                    } else {
                        heads::best_fit_axis(&points)
                    };
                    let place =
                        unit.map_or(0., |unit| heads::plane(&originals, unit, at.at(0, t, 0)));
                    let result = match unit {
                        Some(unit) => heads::fold(&points, unit, place),
                        None => points.iter().map(|p| (*p, false)).collect(),
                    };
                    for (n, (p, low)) in members.iter().zip(result) {
                        for a in 0..3 {
                            moved[[*n, t, a]] = p[a];
                            normals[[*n, t, a]] = unit.map_or(0., |unit| unit[a]);
                        }
                        folded[[*n, t, 0]] = if low { 1. } else { 0. };
                    }
                }
            }
            Ok(BTreeMap::from([
                (
                    "positions".into(),
                    signal(moved, crate::tensor::VECTOR, &batch)?,
                ),
                ("folded".into(), signal(folded, Channels::Value, &batch)?),
                (
                    "normal".into(),
                    signal(normals, crate::tensor::VECTOR, &batch)?,
                ),
            ]))
        }
        Kernel::Rank => {
            let index = s("index");
            let unit = s("unit");
            let span = s("span");
            let first = s("first");
            let groups = spans(span, heads);
            // A random key per unit and event; ties (never in practice)
            // fall back to cell order.
            let ranking = |event: i64| {
                let mut rank = vec![0.; heads];
                for members in groups.values() {
                    let mut units: Vec<(f64, usize)> = unit_heads(unit, members)
                        .into_iter()
                        .map(|n| {
                            let id = &batch.fixtures[first.at(n, 0, 0) as usize];
                            let seed = crate::spatial::epoch_seed(batch.frame.seed, event);
                            (crate::spatial::threshold(id, seed), n)
                        })
                        .collect();
                    units.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                    let place: HashMap<i64, f64> = units
                        .iter()
                        .enumerate()
                        .map(|(r, (_, n))| (unit.at(*n, 0, 0) as i64, r as f64))
                        .collect();
                    for n in members {
                        rank[*n] = place[&(unit.at(*n, 0, 0) as i64)];
                    }
                }
                rank
            };
            let (_, times, width) = index.values().dim();
            let mut cache: HashMap<i64, Vec<f64>> = HashMap::new();
            let mut values = Array3::zeros((heads, times, width));
            for t in 0..times {
                for e in 0..width {
                    let k = index.at(0, t, e) as i64;
                    let rank = cache.entry(k).or_insert_with(|| ranking(k));
                    for n in 0..heads {
                        values[[n, t, e]] = rank[n];
                    }
                }
            }
            single("value", events(values, &batch)?)
        }
        Kernel::Axis => {
            let positions = s("positions");
            let base = s("base");
            let unit = s("unit");
            let span = s("span");
            let rank = s("rank");
            let direction = [s("dir_x"), s("dir_y"), s("dir_z")];
            let centre = [s("centre_x"), s("centre_y"), s("centre_z")];
            let kind = f("kind")? as u8;
            let given = f("has_direction")? != 0.;
            let wrap = f("wrap")? != 0.;
            let shuffled = f("shuffled")? != 0.;
            let groups = spans(span, heads);
            if kind == 1 && shuffled {
                // Shuffled, each unit is its own slot in a random order.
                let (_, times, width) = rank.values().dim();
                let mut values = Array3::zeros((heads, times, width));
                for members in groups.values() {
                    let count = unit_heads(unit, members).len().max(1) as f64;
                    for n in members {
                        for t in 0..times {
                            for e in 0..width {
                                values[[*n, t, e]] = (rank.at(*n, t, e) + 0.5) / count;
                            }
                        }
                    }
                }
                return single("value", events(values, &batch)?);
            }
            let every = [
                positions,
                base,
                direction[0],
                direction[1],
                direction[2],
                centre[0],
                centre[1],
                centre[2],
            ];
            let times = shape(&every)?.1;
            let mut values = Array3::from_elem((heads, times, 1), 0.5);
            for t in 0..times {
                let dir = given
                    .then(|| {
                        heads::unit_direction(std::array::from_fn(|a| direction[a].at(0, t, 0)))
                    })
                    .flatten();
                for members in groups.values() {
                    let firsts = unit_heads(unit, members);
                    // The ruler is measured on the selection before any
                    // fold (`base`): its ends, centre, plane and largest
                    // distance. A mirror moves the heads (`positions`)
                    // along it, never the ruler.
                    let points: Vec<[f64; 3]> =
                        firsts.iter().map(|n| vector_at(positions, *n, t)).collect();
                    let ruler: Vec<[f64; 3]> =
                        firsts.iter().map(|n| vector_at(base, *n, t)).collect();
                    let count = points.len() as f64;
                    // Wrapped, the ruler is a ring of `count` places: the
                    // ends sit one mean spacing apart, as `order` cells
                    // do, and never on one place.
                    let ring = |a: f64| {
                        if wrap {
                            (a * (count - 1.) + 0.5) / count
                        } else {
                            a
                        }
                    };
                    let coordinate: Vec<f64> = if kind == 1 {
                        // Order: the units sorted along the direction (as
                        // line, empty = best fit). Heads at one projection
                        // share a slot; `(slot + 0.5) / slots`. The slots
                        // come from the selection before any fold; a
                        // folded head takes the slot it folded onto.
                        let Some(axis) = dir.or_else(|| heads::best_fit_axis(&ruler)) else {
                            continue;
                        };
                        let slots = heads::order_slots(
                            &ruler
                                .iter()
                                .map(|p| heads::dot(*p, axis))
                                .collect::<Vec<_>>(),
                        );
                        let count = slots.len().max(1) as f64;
                        points
                            .iter()
                            .map(|p| {
                                let slot = heads::order_slot(&slots, heads::dot(*p, axis));
                                (slot as f64 + 0.5) / count
                            })
                            .collect()
                    } else if kind == 0 {
                        // Line: 0 at the selection's lowest head along the
                        // direction, 1 at its highest; all at one value
                        // read 0.5.
                        let Some(axis) = dir.or_else(|| heads::best_fit_axis(&ruler)) else {
                            continue;
                        };
                        let (min, max) = ruler
                            .iter()
                            .map(|p| heads::dot(*p, axis))
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                                (lo.min(v), hi.max(v))
                            });
                        points
                            .iter()
                            .map(|p| {
                                if max - min <= 1e-12 {
                                    0.5
                                } else {
                                    ring((heads::dot(*p, axis) - min) / (max - min))
                                }
                            })
                            .collect()
                    } else {
                        // Radial and angle are measured around `centre`
                        // (0–1 per axis of the selection's box). Radial is
                        // the distance over the selection's largest: 0 at
                        // the centre, 1 at the farthest head.
                        let center = heads::at_in_box(
                            &ruler,
                            std::array::from_fn(|a| centre[a].at(0, t, 0)),
                        );
                        let [first, second] =
                            heads::plane_basis(dir, &ruler, heads::centroid(&ruler));
                        let flat = |p: &[f64; 3]| {
                            let d: [f64; 3] = std::array::from_fn(|a| p[a] - center[a]);
                            (heads::dot(d, first), heads::dot(d, second))
                        };
                        if kind == 2 {
                            let max = ruler
                                .iter()
                                .map(|p| {
                                    let (x, y) = flat(p);
                                    x.hypot(y)
                                })
                                .fold(0., f64::max);
                            points
                                .iter()
                                .map(|p| {
                                    let (x, y) = flat(p);
                                    if max <= 1e-12 {
                                        0.
                                    } else {
                                        ring(x.hypot(y) / max)
                                    }
                                })
                                .collect()
                        } else {
                            points
                                .iter()
                                .map(|p| {
                                    let (x, y) = flat(p);
                                    (y.atan2(x) / std::f64::consts::TAU).rem_euclid(1.)
                                })
                                .collect()
                        }
                    };
                    let place: HashMap<i64, f64> = firsts
                        .iter()
                        .zip(coordinate)
                        .map(|(n, a)| (unit.at(*n, 0, 0) as i64, a))
                        .collect();
                    for n in members {
                        values[[*n, t, 0]] = place[&(unit.at(*n, 0, 0) as i64)];
                    }
                }
            }
            single("value", signal(values, Channels::Value, &batch)?)
        }
        Kernel::Slide => {
            let (a, at, shift, scale) = (s("a"), s("at"), s("shift"), s("scale"));
            let wrap = f("wrap")? != 0.;
            let dims = shape(&[a, at, shift, scale])?;
            let values = Array3::from_shape_fn(dims, |(n, t, e)| {
                // As CSS: `at` is the transform origin, `shift` the
                // translate, `scale` the scale: x = at + (a − at − shift)
                // / scale. Wrapped, the space tiles as a shader's
                // fract(x): scale first, then repeat, so a copy every
                // `scale`. A scale of 0 is a jump at at + shift, wrapped
                // or not.
                let at = at.at(n, t, e);
                let d = a.at(n, t, e) - at - shift.at(n, t, e);
                let scale = scale.at(n, t, e);
                if scale <= 1e-9 {
                    at + (if wrap { d.rem_euclid(1.) } else { d }) / 1e-9
                } else if wrap {
                    (at + d / scale).rem_euclid(1.)
                } else {
                    at + d / scale
                }
            });
            single("value", events(values, &batch)?)
        }
        Kernel::Noise4 => {
            let positions = s("positions");
            let span = s("span");
            let turns = s("turns");
            let scale = s("scale");
            let contrast = s("contrast");
            let uniform = f("uniform")? != 0.;
            let salt = (f("salt_lo")? as u64) | ((f("salt_hi")? as u64) << 32);
            let seed = batch.frame.seed ^ salt;
            let mut dims = shape(&[turns, scale, contrast])?;
            let place_times = positions.values().dim().1;
            if !uniform {
                dims.0 = heads;
                dims.1 = shape(&[turns, scale, contrast, positions])?.1;
            }
            // Positions over the span's largest extent, per span and time.
            let mut unit_positions = vec![[0.; 3]; heads * place_times];
            if !uniform {
                for t in 0..place_times {
                    for members in spans(span, heads).values() {
                        let points: Vec<[f64; 3]> = members
                            .iter()
                            .map(|n| vector_at(positions, *n, t))
                            .collect();
                        let min: [f64; 3] = std::array::from_fn(|a| {
                            points.iter().map(|p| p[a]).fold(f64::INFINITY, f64::min)
                        });
                        let extent = (0..3)
                            .map(|a| points.iter().map(|p| p[a] - min[a]).fold(0., f64::max))
                            .fold(0., f64::max);
                        for (n, p) in members.iter().zip(points) {
                            unit_positions[n * place_times + t] = if extent > 1e-12 {
                                std::array::from_fn(|a| (p[a] - min[a]) / extent)
                            } else {
                                [0.; 3]
                            };
                        }
                    }
                }
            }
            let mut values = Array3::zeros(dims);
            for n in 0..dims.0 {
                for t in 0..dims.1 {
                    for e in 0..dims.2 {
                        let p = if uniform {
                            [0.; 3]
                        } else {
                            unit_positions[n * place_times + if place_times == 1 { 0 } else { t }]
                        };
                        let size = scale.at(n, t, e).max(1e-6);
                        let raw = super::noise::coherent_noise(
                            [p[0] / size, p[1] / size, p[2] / size, turns.at(n, t, e)],
                            seed,
                        )?;
                        let c = contrast.at(n, t, e);
                        values[[n, t, e]] = ((raw - 0.5) * (1. + 3. * c) + 0.5).clamp(0., 1.);
                    }
                }
            }
            single("value", events(values, &batch)?)
        }
        Kernel::Curve => {
            let Value::Points(shape_points) = inputs["shape"].control(0) else {
                unreachable!("a curve shape")
            };
            let Value::Gradient(gradient) = inputs["gradient"].control(0) else {
                unreachable!("a curve gradient")
            };
            let kind = f("kind")? as u8;
            let names = ["x", "low_x", "low_y", "low_z", "high_x", "high_y", "high_z"];
            let all: Vec<&Signal> = names.iter().map(|k| s(k)).collect();
            let dims = shape(&all)?;
            let x = all[0];
            let mut out: [Array3<f64>; 3] = std::array::from_fn(|_| Array3::zeros(dims));
            for n in 0..dims.0 {
                for t in 0..dims.1 {
                    for e in 0..dims.2 {
                        let v = shape_points.sample(x.at(n, t, e));
                        let value: [f64; 3] = if kind == 2 {
                            gradient.sample(v)
                        } else {
                            std::array::from_fn(|c| {
                                let low = all[1 + c].at(n, t, e);
                                let high = all[4 + c].at(n, t, e);
                                low + v * (high - low)
                            })
                        };
                        for c in 0..3 {
                            out[c][[n, t, e]] = value[c];
                        }
                    }
                }
            }
            let [a, b, c] = out;
            Ok(BTreeMap::from([
                ("x".into(), events(a, &batch)?),
                ("y".into(), events(b, &batch)?),
                ("z".into(), events(c, &batch)?),
            ]))
        }
        Kernel::Combine => {
            let (value, weight, present) = (s("value"), s("weight"), s("present"));
            let over = f("over")? != 0.;
            let dims = shape(&[value, weight, present])?;
            let mut values = Array3::zeros((dims.0, dims.1, 1));
            let mut alphas = Array3::zeros((dims.0, dims.1, 1));
            for n in 0..dims.0 {
                for t in 0..dims.1 {
                    // Events lie in birth order on the channel axis.
                    let live = (0..dims.2).filter(|e| present.at(n, t, *e) > 0.);
                    let (v, a) = if over {
                        // Each newer event over the older by its weight:
                        // premultiplied, then divided back.
                        let (lit, alpha) = live.fold((0., 0.), |(lit, alpha), e| {
                            let w = weight.at(n, t, e).clamp(0., 1.);
                            (value.at(n, t, e) * w + lit * (1. - w), w + alpha * (1. - w))
                        });
                        (if alpha > 1e-12 { lit / alpha } else { 0. }, alpha)
                    } else {
                        live.fold((0., 0.), |(lit, alpha): (f64, f64), e| {
                            (
                                lit.max(value.at(n, t, e)),
                                alpha.max(weight.at(n, t, e).clamp(0., 1.)),
                            )
                        })
                    };
                    values[[n, t, 0]] = v;
                    alphas[[n, t, 0]] = a;
                }
            }
            Ok(BTreeMap::from([
                ("value".into(), signal(values, Channels::Value, &batch)?),
                ("alpha".into(), signal(alphas, Channels::Value, &batch)?),
            ]))
        }
        Kernel::Math => {
            let (a, b) = (s("a"), s("b"));
            let op = f("op")? as u8;
            let dims = shape(&[a, b])?;
            let values = Array3::from_shape_fn(dims, |(n, t, e)| {
                let (a, b) = (a.at(n, t, e), b.at(n, t, e));
                match op {
                    0 => a * b,
                    1 => a + b,
                    2 => a - b,
                    3 => a.max(b),
                    _ => a.min(b),
                }
            });
            single("value", events(values, &batch)?)
        }
        // The light arrives premultiplied by alpha (so the events of a
        // clock combine as one layer); the output divides it back. Values
        // clamp here and only here.
        Kernel::ColorOut => {
            let parts = [s("r"), s("g"), s("b"), s("alpha")];
            let (n, t, _) = shape(&parts)?;
            let values = Array3::from_shape_fn((n, t, 3), |(n, t, ch)| {
                unpremultiply(parts[ch].at(n, t, 0), parts[3].at(n, t, 0))
            });
            single("color", signal(values, Channels::Rgb, &batch)?)
        }
        Kernel::StrobeOut => {
            let (rate, alpha) = (s("rate"), s("alpha"));
            let (n, t, _) = shape(&[rate, alpha])?;
            let values = Array3::from_shape_fn((n, t, 1), |(n, t, _)| {
                unpremultiply(rate.at(n, t, 0), alpha.at(n, t, 0))
            });
            single("strobe", signal(values, Channels::Value, &batch)?)
        }
        Kernel::AimOut => {
            let base = f("base")? as u8;
            let mirrors = f("mirrors")? as usize;
            let names = [
                "dir_x",
                "dir_y",
                "dir_z",
                "point_x",
                "point_y",
                "point_z",
                "yaw",
                "pitch",
                "alpha",
                "positions",
            ];
            let all: Vec<&Signal> = names.iter().map(|k| s(k)).collect();
            let folds: Vec<(&Signal, &Signal)> = (0..mirrors)
                .map(|m| (s(&format!("fold{m}")), s(&format!("normal{m}"))))
                .collect();
            let mut every = all.clone();
            for (fold, normal) in &folds {
                every.push(fold);
                every.push(normal);
            }
            let times = shape(&every)?.1;
            let mut aim = Array3::zeros((heads, times, 3));
            let mut turns = Array3::zeros((heads, times, crate::aim::TURN_CHANNELS));
            for n in 0..heads {
                for t in 0..times {
                    let v = |i: usize| all[i].at(n, t, 0);
                    let head = vector_at(all[9], n, t);
                    let direction = [v(0), v(1), v(2)];
                    let point = [v(3), v(4), v(5)];
                    let d = crate::aim::unit(match base {
                        0 => direction,
                        1 => std::array::from_fn(|a| point[a] - head[a]),
                        _ => std::array::from_fn(|a| head[a] - point[a]),
                    });
                    let (yaw, pitch, alpha) = (v(6), v(7), v(8).clamp(0., 1.));
                    let normals: Vec<[f64; 3]> = folds
                        .iter()
                        .filter(|(fold, _)| fold.at(n, t, 0) > 0.5)
                        .map(|(_, normal)| vector_at(normal, n, t))
                        .collect();
                    let reflected = normals
                        .iter()
                        .fold(d, |d, normal| crate::aim::reflect(d, *normal));
                    let turned = crate::aim::offset(reflected, yaw, pitch);
                    let moved = normals
                        .iter()
                        .rev()
                        .fold(turned, |d, normal| crate::aim::reflect(d, *normal));
                    for ch in 0..3 {
                        aim[[n, t, ch]] = moved[ch] * alpha;
                    }
                    let turn = crate::aim::Turn {
                        lean: [0.; 3],
                        yaw,
                        pitch,
                        mirror: (normals.len() % 2 == 1).then(|| normals[normals.len() - 1]),
                        alpha,
                    };
                    for (ch, value) in turn.channels().into_iter().enumerate() {
                        turns[[n, t, ch]] = value;
                    }
                }
            }
            Ok(BTreeMap::from([
                ("aim".into(), signal(aim, crate::tensor::VECTOR, &batch)?),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Live events of a clock whose every and duration fall over the clip
    /// as in Speed-up chase: every 2 → 0.25, duration 2 → 0.5 beats.
    #[test]
    fn speed_up_chase_event_count_grows() {
        let frame = Frame {
            cells: &[],
            features: None,
            beat: 0.,
            clip_start: 0.,
            clip_duration: 16.,
            seed: 0,
        };
        let table = |from: f64, to: f64| {
            let samples: Vec<f64> = (0..clock_table::SAMPLES)
                .map(|i| from + (to - from) * i as f64 / (clock_table::SAMPLES - 1) as f64)
                .collect();
            let period = Signal::series(&samples, Unit::Number).unwrap();
            EvaluatedValue::from_signal(
                ValueType::Signal(SignalType::ANY),
                clock_table::integrate(&period, frame).unwrap(),
            )
            .unwrap()
        };
        let inputs = BTreeMap::from([
            ("every".to_string(), table(2., 0.25)),
            ("duration".to_string(), table(2., 0.5)),
        ]);
        let times = [1.5, 7.5, 15.5];
        let out = run(
            Kernel::Events,
            &inputs,
            Batch {
                frame,
                times: &times,
                fixtures: &[],
            },
        )
        .unwrap();
        let present = out["present"].numeric();
        let count = |t: usize| {
            (0..present.values().dim().2)
                .filter(|e| present.at(0, t, *e) > 0.)
                .count()
        };
        assert!(count(0) < count(2), "{} then {}", count(0), count(2));
        assert!(count(0) <= count(1) && count(1) <= count(2));
    }
}
