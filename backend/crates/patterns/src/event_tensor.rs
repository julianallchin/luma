//! Stateless event → signal operations. Broadcasting forms T×E ages and N×T×E
//! spatial samples; Max reduces E. There is no replay of a graph for each event.
use crate::*;
use ndarray::{Array1, Array2, Array3, Axis, Zip};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

/// Validated once when analysis is loaded; frames share an immutable tensor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Arc<[f64]>", into = "Arc<[f64]>")]
pub struct EventTimes(Arc<[f64]>);

impl EventTimes {
    pub fn new(times: impl Into<Arc<[f64]>>) -> Result<Self> {
        let times = times.into();
        if times.iter().any(|t| !t.is_finite()) || times.windows(2).any(|p| p[0] > p[1]) {
            return Err(Error("event timestamps must be finite and ordered".into()));
        }
        Ok(Self(times))
    }
    pub fn as_slice(&self) -> &[f64] {
        &self.0
    }
}
impl TryFrom<Arc<[f64]>> for EventTimes {
    type Error = Error;
    fn try_from(times: Arc<[f64]>) -> Result<Self> {
        Self::new(times)
    }
}
impl From<EventTimes> for Arc<[f64]> {
    fn from(times: EventTimes) -> Self {
        times.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum Events {
    Targeted {
        events: Box<Events>,
        targets: EventTargets,
    },
    /// An unwired trigger uses the effect's repeat/grid/delay controls.
    Automatic,
    Periodic {
        repeat: f64,
        grid_aligned: bool,
        delay: f64,
    },
    Beats {
        times: EventTimes,
    },
}

impl Events {
    pub fn targeted(self, targets: EventTargets) -> Result<Self> {
        let result = Self::Targeted {
            events: Box::new(self),
            targets,
        };
        result.validate()?;
        Ok(result)
    }
    pub(crate) fn schedule(&self) -> &Self {
        let mut source = self;
        while let Self::Targeted { events, .. } = source {
            source = events;
        }
        source
    }
    fn fixtures(&self) -> Option<&[String]> {
        if let Self::Targeted { targets, .. } = self {
            Some(targets.fixtures())
        } else {
            None
        }
    }
    pub fn validate(&self) -> Result<()> {
        let mut source = self;
        let mut depth = 0;
        while let Self::Targeted { events, targets } = source {
            depth += 1;
            if depth > 32 {
                return Err(Error("too many nested event selectors".into()));
            }
            targets.validate_source(events)?;
            if let Some(inner) = events.fixtures() {
                let outer: std::collections::BTreeSet<_> = targets.fixtures().iter().collect();
                if outer != inner.iter().collect() {
                    return Err(Error(
                        "nested event selectors have different fixture domains".into(),
                    ));
                }
            }
            source = events;
        }
        if depth > 0 && matches!(source, Self::Automatic) {
            return Err(Error(
                "targeted events require explicit timestamps or a periodic schedule".into(),
            ));
        }
        let valid = match source {
            Self::Targeted { .. } => unreachable!(),
            Self::Automatic => true,
            Self::Periodic { repeat, delay, .. } => {
                repeat.is_finite() && *repeat > 0.0 && delay.is_finite() && *delay >= 0.0
            }
            Self::Beats { .. } => true,
        };
        if valid {
            Ok(())
        } else {
            Err(Error(
                "events need ordered finite timestamps, or positive repeat and nonnegative delay"
                    .into(),
            ))
        }
    }
}

#[derive(Clone, Copy)]
pub struct ChaseShape<'a> {
    pub travel: f64,
    pub start: f64,
    pub end: f64,
    pub path: &'a Envelope,
    pub width: f64,
    pub shape: &'a Envelope,
    pub boundary: Boundary,
}

impl ChaseShape<'_> {
    fn validate(&self) -> Result<()> {
        self.path.validate()?;
        self.shape.validate()?;
        if !self.start.is_finite()
            || !self.end.is_finite()
            || !self.width.is_finite()
            || !(0.0..=1.0).contains(&self.width)
        {
            return Err(Error(
                "chase needs finite endpoints and width in 0..1".into(),
            ));
        }
        Ok(())
    }
}

struct EventAges {
    phase: Array2<f64>,
    origins: Array1<i64>,
    stride: i64,
}

struct TargetWeights {
    values: Array2<f64>,
    columns: Array2<usize>,
}
impl TargetWeights {
    fn at(&self, n: usize, t: usize, e: usize) -> f64 {
        self.values[[n, self.columns[[t, e]]]]
    }
}
impl EventAges {
    fn targets(&self, events: &Events, fixtures: &[String]) -> Result<Option<TargetWeights>> {
        if events.fixtures().is_none() {
            return Ok(None);
        }
        let ids = Array2::from_shape_fn(self.phase.dim(), |(t, e)| {
            self.origins[t] + self.stride * e as i64
        });
        let unique: std::collections::BTreeSet<_> = ids.iter().copied().collect();
        let indices: Vec<_> = unique.into_iter().collect();
        let lookup: BTreeMap<_, _> = indices.iter().enumerate().map(|(i, e)| (*e, i)).collect();
        Ok(
            crate::event_targets::weights(events, fixtures, &indices)?.map(|values| {
                TargetWeights {
                    values,
                    columns: ids.mapv(|e| lookup[&e]),
                }
            }),
        )
    }
}

fn ages(times: &[f64], events: &Events, travel: f64, clip_start: f64) -> Result<EventAges> {
    events.validate()?;
    if !travel.is_finite()
        || travel <= 0.0
        || !clip_start.is_finite()
        || times.iter().any(|v| !v.is_finite())
    {
        return Err(Error(
            "travel must be finite and positive; sample times and clip start must be finite".into(),
        ));
    }
    if times.is_empty() {
        return Ok(EventAges {
            phase: Array2::zeros((0, 0)),
            origins: Array1::zeros(0),
            stride: 1,
        });
    }
    // Gather only potentially active timestamps into each row. The event axis
    // can be padded because Max ignores inactive entries; a wide time batch
    // never allocates fixture × time × every event in the track.
    let samples = Array1::from(times.to_vec());
    let check_shape = |events: usize| {
        if times
            .len()
            .checked_mul(events.max(1))
            .is_none_or(|count| count > 16_777_216)
        {
            Err(Error(
                "event time tensor exceeds 16,777,216 elements; request fewer time samples".into(),
            ))
        } else {
            Ok(())
        }
    };
    let (phase, origins, stride) = match events.schedule() {
        Events::Targeted { .. } => unreachable!(),
        Events::Automatic => {
            return Err(Error(
                "unwired events must resolve to the effect's repeat controls".into(),
            ))
        }
        Events::Beats { times: events } => {
            let events = events.as_slice();
            let starts = samples.mapv(|time| events.partition_point(|at| *at <= time - travel));
            let ends = samples.mapv(|time| events.partition_point(|at| *at <= time));
            let width = (&ends - &starts).iter().copied().max().unwrap_or(0);
            check_shape(width)?;
            let timestamps = Array2::from_shape_fn((times.len(), width), |(t, e)| {
                if starts[t] + e < ends[t] {
                    events[starts[t] + e]
                } else {
                    samples[t]
                }
            });
            let phase = (&samples.insert_axis(Axis(1)) - &timestamps) / travel;
            (
                Zip::indexed(&phase)
                    .map_collect(|(t, e), age| if starts[t] + e < ends[t] { *age } else { 1.0 }),
                starts.mapv(|index| index as i64),
                1,
            )
        }
        Events::Periodic {
            repeat,
            grid_aligned,
            delay,
        } => {
            let origin = if *grid_aligned { 0.0 } else { clip_start } + delay;
            let last = samples.mapv(|time| ((time - origin) / repeat).floor());
            let width = (travel / repeat).ceil().max(1.0);
            if !width.is_finite()
                || width > 1_000_000.0
                || last.iter().any(|index| {
                    !index.is_finite() || index.abs() + width > 9_007_199_254_740_991.0
                })
            {
                return Err(Error(
                    "periodic events exceed supported density or timestamp precision".into(),
                ));
            }
            let width = width as usize;
            check_shape(width)?;
            let origins = last.mapv(|index| index as i64);
            let indices = last.insert_axis(Axis(1))
                - Array1::range(0.0, width as f64, 1.0).insert_axis(Axis(0));
            let timestamps = indices.mapv(|index| origin + index * repeat);
            (
                (&samples.insert_axis(Axis(1)) - &timestamps) / travel,
                origins,
                -1,
            )
        }
    };
    Ok(EventAges {
        phase,
        origins,
        stride,
    })
}

/// All requested samples, including out-of-order seeks, in one array operation.
pub fn chase_signal(
    mapping: &Mapping,
    times: &[f64],
    events: &Events,
    clip_start: f64,
    shape: ChaseShape<'_>,
) -> Result<Signal> {
    shape.validate()?;
    chase_sampled(mapping, times, events, clip_start, shape.travel, |_, _| {
        shape
    })
}

pub(crate) fn chase_sampled<'a>(
    mapping: &Mapping,
    times: &[f64],
    events: &Events,
    clip_start: f64,
    travel: f64,
    sample: impl Fn(usize, usize) -> ChaseShape<'a>,
) -> Result<Signal> {
    mapping.validate()?;
    let ages = ages(times, events, travel, clip_start)?;
    let ids: Vec<_> = mapping.coordinates.iter().map(|c| c.cell.clone()).collect();
    let targets = ages.targets(events, &ids)?;
    let phase = ages.phase;
    let (n, t, e) = (mapping.coordinates.len(), times.len(), phase.ncols());
    if n.checked_mul(t)
        .and_then(|v| v.checked_mul(e.max(1)))
        .is_none_or(|size| size > 16_777_216)
    {
        return Err(Error(
            "chase sample tensor exceeds 16,777,216 elements; request fewer time samples".into(),
        ));
    }
    let samples = Array2::from_shape_fn((n, t), |(n, t)| sample(n, t));
    for shape in &samples {
        shape.validate()?;
    }
    let fixtures = Some(
        mapping
            .coordinates
            .iter()
            .map(|c| c.cell.clone())
            .collect::<Vec<_>>()
            .into(),
    );
    if e == 0 {
        return Signal::new(
            Array3::zeros((n, t, 1)),
            Unit::Proportion,
            Channels::Value,
            fixtures,
        );
    }
    let active = phase.mapv(|p| if (0.0..1.0).contains(&p) { 1.0 } else { 0.0 });
    let centers = Array3::from_shape_fn((n, t, e), |(n, t, e)| {
        let shape = samples[[n, t]];
        let p = phase[[t, e]];
        shape.start + (shape.end - shape.start) * shape.path.sample(p.clamp(0.0, 1.0))
    });
    let position = Array1::from_iter(mapping.coordinates.iter().map(|c| c.position))
        .insert_axis(Axis(1))
        .insert_axis(Axis(2));
    let wrap = Array2::from_shape_fn((n, t), |(n, t)| {
        let boundary = samples[[n, t]].boundary;
        boundary == Boundary::Wrap
            || (boundary == Boundary::Natural && mapping.coordinates[n].closed)
    })
    .insert_axis(Axis(2));
    let delta = &position - &centers;
    let coverage = Zip::indexed(&delta)
        .and_broadcast(&wrap)
        .and_broadcast(active.view().insert_axis(Axis(0)))
        .map_collect(|(n, t, e), delta, wrap, active| {
            let shape = samples[[n, t]];
            if shape.width == 0.0 {
                return 0.0;
            }
            let delta = if *wrap {
                (delta + 0.5).rem_euclid(1.0) - 0.5
            } else {
                *delta
            };
            let p = delta / shape.width + 0.5;
            let inside = (p > 1e-12 && p < 1.0 - 1e-12) || (*wrap && shape.width == 1.0);
            if inside {
                shape.shape.sample(p) * active * targets.as_ref().map_or(1., |w| w.at(n, t, e))
            } else {
                0.0
            }
        });
    let values = coverage
        .fold_axis(Axis(2), 0.0_f64, |a, b| a.max(*b))
        .insert_axis(Axis(2));
    Signal::new(values, Unit::Proportion, Channels::Value, fixtures)
}

pub fn pulse_signal(
    times: &[f64],
    events: &Events,
    clip_start: f64,
    travel: f64,
    shape: &Envelope,
) -> Result<Signal> {
    shape.validate()?;
    pulse_sampled(times, events, clip_start, travel, |_| shape)
}

pub(crate) fn pulse_sampled<'a>(
    times: &[f64],
    events: &Events,
    clip_start: f64,
    travel: f64,
    shape: impl Fn(usize) -> &'a Envelope,
) -> Result<Signal> {
    for t in 0..times.len() {
        shape(t).validate()?;
    }
    let ages = ages(times, events, travel, clip_start)?;
    let fixtures = events.fixtures().map(|ids| {
        let mut ids = ids.to_vec();
        ids.sort();
        ids
    });
    let targets = ages.targets(events, fixtures.as_deref().unwrap_or(&[]))?;
    let phase = ages.phase;
    let n = fixtures.as_ref().map_or(1, Vec::len);
    if n.checked_mul(phase.len())
        .is_none_or(|size| size > 16_777_216)
    {
        return Err(Error("pulse tensor exceeds 16,777,216 elements".into()));
    }
    let coverage = Array3::from_shape_fn((n, phase.nrows(), phase.ncols()), |(n, t, e)| {
        let p = phase[[t, e]];
        if (0.0..1.0).contains(&p) {
            shape(t).sample(p) * targets.as_ref().map_or(1., |w| w.at(n, t, e))
        } else {
            0.0
        }
    });
    let values = coverage.fold_axis(Axis(2), 0.0_f64, |a, b| a.max(*b));
    Signal::new(
        values.insert_axis(Axis(2)),
        Unit::Proportion,
        Channels::Value,
        fixtures.map(Into::into),
    )
}

pub(crate) struct DissolveSettings {
    pub travel: f64,
    pub reseed: bool,
    pub refresh_every: Option<f64>,
}

/// Event age, deterministic per-head thresholds and Max share the event axis.
/// Random order is keyed by immutable event index; seeking requires no history.
pub(crate) fn dissolve_sampled<'a>(
    fixtures: &[String],
    times: &[f64],
    events: &Events,
    frame: Frame<'_>,
    settings: DissolveSettings,
    softness: &Signal,
    shape: impl Fn(usize) -> &'a Envelope,
) -> Result<Signal> {
    let ages = ages(times, events, settings.travel, frame.clip_start)?;
    let targets = ages.targets(events, fixtures)?;
    let (n, t, e) = (fixtures.len(), times.len(), ages.phase.ncols());
    if n.checked_mul(t)
        .and_then(|v| v.checked_mul(e.max(1)))
        .is_none_or(|size| size > 16_777_216)
    {
        return Err(Error(
            "dissolve sample tensor exceeds 16,777,216 elements; request fewer time samples".into(),
        ));
    }
    let softness = softness.on_fixtures(fixtures)?;
    if (softness.values().dim().1 != 1 && softness.values().dim().1 != t)
        || softness.values().iter().any(|v| !(0.0..=1.0).contains(v))
    {
        return Err(Error(
            "dissolve softness needs compatible samples within 0–1".into(),
        ));
    }
    let refresh = settings
        .refresh_every
        .map(|period| {
            if !period.is_finite() || period <= 0.0 {
                return Err(Error("refresh interval must be positive".into()));
            }
            let indices = Array1::from_iter(
                times
                    .iter()
                    .map(|time| ((time - frame.clip_start) / period).floor()),
            );
            if indices
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 9_007_199_254_740_991.0)
            {
                return Err(Error(
                    "refresh timestamps exceed supported precision".into(),
                ));
            }
            Ok(indices.mapv(|v| v as i64))
        })
        .transpose()?;
    for time in 0..t {
        shape(time).validate()?;
    }
    let progress = Zip::indexed(&ages.phase)
        .map_collect(|(t, _), phase| 1.0 - shape(t).sample(phase.clamp(0.0, 1.0)));
    let coverage = Array3::from_shape_fn((n, t, e), |(n, t, e)| {
        let weight = targets.as_ref().map_or(1., |w| w.at(n, t, e));
        if weight == 0. || !(0.0..1.0).contains(&ages.phase[[t, e]]) {
            return 0.0;
        }
        let epoch = if let Some(refresh) = &refresh {
            refresh[t]
        } else if settings.reseed {
            ages.origins[t] + ages.stride * e as i64
        } else {
            0
        };
        let random =
            crate::spatial::threshold(&fixtures[n], crate::spatial::epoch_seed(frame.seed, epoch));
        let softness = softness.at(n, t, 0);
        if softness > 0.0 {
            (1.0 - (progress[[t, e]] - random * (1.0 - softness)) / softness).clamp(0.0, 1.0)
                * weight
        } else if random > progress[[t, e]] {
            weight
        } else {
            0.0
        }
    });
    let values = coverage
        .fold_axis(Axis(2), 0.0_f64, |a, b| a.max(*b))
        .insert_axis(Axis(2));
    Signal::new(
        values,
        Unit::Proportion,
        Channels::Value,
        Some(fixtures.to_vec().into()),
    )
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    if !matches!(
        op,
        Primitive::BeatEvents
            | Primitive::ChaseEvents
            | Primitive::PulseEvents
            | Primitive::DissolveEvents
    ) {
        return None;
    }
    let mut inputs = crate::catalog::primitive(Primitive::Rhythm).inputs;
    let fixed = |name: &str, value: Value| Input {
        optional: false,
        name: name.into(),
        description: String::new(),
        value_type: value.value_type(),
        rate: Rate::Fixed,
        default: Some(value),
    };
    let (name, key, value_type, rate) = if op == Primitive::BeatEvents {
        ("Beat trigger", "trigger", ValueType::Events, Rate::Fixed)
    } else {
        inputs.insert(
            "trigger".into(),
            Input {
                optional: false,
                rate: Rate::Frame,
                description: "Event timestamps; unwired uses Repeat".into(),
                ..fixed("Trigger", Value::Events(Events::Automatic))
            },
        );
        inputs.insert("travel".into(), fixed("Travel time", Value::Beats(2.0)));
        if op == Primitive::ChaseEvents {
            inputs.extend(
                crate::catalog::pill_inputs()
                    .into_iter()
                    .filter(|(key, _)| key != "position" && key != "active"),
            );
            for (key, name, value) in [
                ("start", "Start position", Value::Position(0.0)),
                ("end", "End position", Value::Position(1.0)),
                (
                    "path",
                    "Travel curve",
                    Value::Envelope(Envelope::linear(vec![[0., 0.], [1., 1.]])),
                ),
            ] {
                inputs.insert(
                    key.into(),
                    Input {
                        optional: false,
                        rate: Rate::Frame,
                        ..fixed(name, value)
                    },
                );
            }
            ("Chase events", "mask", ValueType::Mask, Rate::Frame)
        } else {
            inputs.insert(
                "shape".into(),
                crate::catalog::primitive(Primitive::Envelope).inputs["shape"].clone(),
            );
            if op == Primitive::DissolveEvents {
                inputs.extend(
                    crate::catalog::dissolve_inputs()
                        .into_iter()
                        .filter(|(key, _)| key != "coverage" && key != "cycle"),
                );
                ("Dissolve events", "mask", ValueType::Mask, Rate::Frame)
            } else {
                ("Pulse events", "mask", ValueType::Mask, Rate::Frame)
            }
        }
    };
    Some(Definition {
        name: name.into(),
        inputs,
        outputs: BTreeMap::from([(key.into(), Output { value_type, rate })]),
        body: Body::Primitive(op),
    })
}

pub(crate) fn validate_parameters(op: Primitive, inputs: &BTreeMap<String, Value>) -> Result<()> {
    if matches!(
        op,
        Primitive::ChaseEvents | Primitive::PulseEvents | Primitive::DissolveEvents
    ) && inputs.get("travel").is_some_and(|v| v.scalar() <= 0.0)
    {
        return Err(Error("travel must be positive".into()));
    }
    if op == Primitive::DissolveEvents
        && inputs
            .get("refresh_every")
            .is_some_and(|v| v.scalar() <= 0.0)
    {
        return Err(Error("refresh interval must be positive".into()));
    }
    Ok(())
}
