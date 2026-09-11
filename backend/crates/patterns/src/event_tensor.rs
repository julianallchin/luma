//! Stateless event → signal operations. Events within a window become a channel
//! axis: broadcasting forms T×E ages and N×T×E weights, and ordinary graph
//! operations reduce that axis with Max. No graph is replayed for each event.
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
    /// Historical sentinel from version 3 documents, where an unwired trigger
    /// used the effect's repeat controls. Migration replaces it with an
    /// explicit trigger node before execution.
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

struct EventAges {
    /// Time since each event, in beats. Padding columns equal the window.
    elapsed: Array2<f64>,
    /// Elapsed over the window; padding columns are exactly 1.
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
            "duration must be finite and positive; sample times and clip start must be finite"
                .into(),
        ));
    }
    if times.is_empty() {
        return Ok(EventAges {
            elapsed: Array2::zeros((0, 0)),
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
    let (elapsed, phase, origins, stride) = match events.schedule() {
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
            let elapsed = &samples.insert_axis(Axis(1)) - &timestamps;
            let real = |t: usize, e: usize| starts[t] + e < ends[t];
            (
                Zip::indexed(&elapsed)
                    .map_collect(|(t, e), age| if real(t, e) { *age } else { travel }),
                Zip::indexed(&elapsed)
                    .map_collect(|(t, e), age| if real(t, e) { *age / travel } else { 1.0 }),
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
            let elapsed = &samples.insert_axis(Axis(1)) - &timestamps;
            (
                elapsed.clone(),
                elapsed.mapv(|age| age / travel),
                origins,
                -1,
            )
        }
    };
    Ok(EventAges {
        elapsed,
        phase,
        origins,
        stride,
    })
}

/// Events within `duration` beats of each sample, as channels. Padding columns
/// are inactive: elapsed equals the duration and present is zero.
pub(crate) fn event_ages(
    times: &[f64],
    events: &Events,
    duration: f64,
    clip_start: f64,
    fixtures: &[String],
) -> Result<BTreeMap<&'static str, Signal>> {
    let ages = ages(times, events, duration, clip_start)?;
    let targets = ages.targets(events, fixtures)?;
    let (t, e) = ages.phase.dim();
    let width = e.max(1);
    let channels = Channels::components(width)?;
    let phase = Array3::from_shape_fn((1, t, width), |(_, t, e)| {
        ages.phase.get((t, e)).copied().unwrap_or(1.0)
    });
    let present = phase.mapv(|p| if (0.0..1.0).contains(&p) { 1.0 } else { 0.0 });
    let weight = match &targets {
        Some(weights) => {
            let n = fixtures.len();
            if n.checked_mul(t)
                .and_then(|v| v.checked_mul(width))
                .is_none_or(|size| size > 16_777_216)
            {
                return Err(Error(
                    "event weight tensor exceeds 16,777,216 elements; request fewer time samples"
                        .into(),
                ));
            }
            Signal::new(
                Array3::from_shape_fn((n, t, width), |(n, t, e)| {
                    if e < ages.phase.ncols() {
                        present[[0, t, e]] * weights.at(n, t, e)
                    } else {
                        0.0
                    }
                }),
                Unit::Proportion,
                channels,
                Some(fixtures.to_vec().into()),
            )?
        }
        None => Signal::new(present.clone(), Unit::Proportion, channels, None)?,
    };
    let index = Array3::from_shape_fn((1, t, width), |(_, t, e)| {
        (ages.origins[t] + ages.stride * e as i64) as f64
    });
    Ok(BTreeMap::from([
        (
            "elapsed",
            Signal::new(
                Array3::from_shape_fn((1, t, width), |(_, t, e)| {
                    ages.elapsed.get((t, e)).copied().unwrap_or(duration)
                }),
                Unit::Beats,
                channels,
                None,
            )?,
        ),
        (
            "progress",
            Signal::new(
                phase.mapv(|p| p.clamp(0.0, 1.0)),
                Unit::Proportion,
                channels,
                None,
            )?,
        ),
        (
            "present",
            Signal::new(present, Unit::Proportion, channels, None)?,
        ),
        ("weight", weight),
        ("index", Signal::new(index, Unit::Number, channels, None)?),
    ]))
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    let fixed = |name: &str, description: &str, value: Value| Input {
        optional: false,
        name: name.into(),
        description: description.into(),
        value_type: value.value_type(),
        rate: Rate::Fixed,
        default: Some(value),
        author: None,
    };
    let signal = |unit| {
        ValueType::Signal(SignalType {
            unit: Some(unit),
            channels: None,
        })
    };
    let (name, inputs, outputs) = match op {
        Primitive::BeatEvents => (
            "Beat trigger",
            crate::catalog::primitive(Primitive::Rhythm).inputs,
            vec![("trigger", ValueType::Events, Rate::Fixed)],
        ),
        Primitive::EventAges => (
            "Event ages",
            BTreeMap::from([
                (
                    "events".into(),
                    fixed(
                        "Events",
                        "Each event starts an independent response",
                        Value::Events(Events::Beats {
                            times: EventTimes::new(Vec::new()).expect("empty event stream"),
                        }),
                    ),
                ),
                (
                    "duration".into(),
                    fixed(
                        "Duration",
                        "How long each event stays active, in beats",
                        Value::Beats(2.0),
                    ),
                ),
            ]),
            vec![
                ("elapsed", signal(Unit::Beats), Rate::Frame),
                ("progress", signal(Unit::Proportion), Rate::Frame),
                ("present", signal(Unit::Proportion), Rate::Frame),
                ("weight", signal(Unit::Proportion), Rate::Frame),
                ("index", signal(Unit::Number), Rate::Frame),
            ],
        ),
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs,
        outputs: outputs
            .into_iter()
            .map(|(key, value_type, rate)| (key.into(), Output { value_type, rate }))
            .collect(),
        body: Body::Primitive(op),
    })
}

pub(crate) fn validate_parameters(op: Primitive, inputs: &BTreeMap<String, Value>) -> Result<()> {
    let positive = |key: &str| inputs.get(key).is_none_or(|v| v.scalar() > 0.0);
    match op {
        Primitive::EventAges if !positive("duration") => {
            Err(Error("duration must be positive".into()))
        }
        Primitive::BeatEvents if !positive("repeat") => {
            Err(Error("repeat interval must be greater than zero".into()))
        }
        _ => Ok(()),
    }
}
