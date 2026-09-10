//! Immutable event preparation and indexed event queries. Envelopes are graphs
//! over the returned time/channel tensors, with no playback history.
use crate::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct TrackTiming {
    pub clock: BeatTimeline,
    beats: EventTimes,
    downbeats: EventTimes,
    bpm: f64,
}
impl TrackTiming {
    pub fn new(
        clock: BeatTimeline,
        beats: EventTimes,
        downbeats: EventTimes,
        bpm: f64,
    ) -> Result<Self> {
        if !(bpm as f32).is_finite()
            || beats
                .as_slice()
                .iter()
                .chain(downbeats.as_slice())
                .any(|t| !(*t as f32).is_finite())
        {
            return Err(Error("track timing exceeds seconds precision".into()));
        }
        Ok(Self {
            clock,
            beats,
            downbeats,
            bpm,
        })
    }
    fn grid_events(&self, subdivision: f64, offset: f64, downbeats: bool) -> Result<Events> {
        let subdivision = subdivision as f32;
        let offset = offset as f32;
        if !subdivision.is_finite() || !offset.is_finite() {
            return Err(Error("grid subdivision and offset must be finite".into()));
        }
        let source = if downbeats {
            self.downbeats.as_slice()
        } else {
            self.beats.as_slice()
        };
        if source.is_empty() {
            return Ok(Events::Beats {
                times: EventTimes::new(Vec::new())?,
            });
        }
        let step = if subdivision.abs() < 1e-3 {
            1.
        } else {
            (1. / subdivision).abs()
        }
        .max(1e-4);
        let last = (source.len() - 1) as f32;
        if (last / step).ceil() > 1_000_000. {
            return Err(Error(
                "beat grid exceeds 1,000,000 events; lower the subdivision".into(),
            ));
        }
        let beat_len = if self.bpm > 0. {
            60. / self.bpm as f32
        } else {
            0.5
        };
        let anchor = source.partition_point(|t| (*t as f32) < -1e-4) as f32;
        let mut position = if subdivision.abs() < 1. {
            anchor + step * 0.5
        } else {
            0.
        };
        while position - step >= 0. {
            position -= step;
        }
        let mut times = Vec::new();
        while position <= last + 1e-4 {
            let index = position.floor() as usize;
            let t0 = source[index] as f32;
            let time = if index + 1 < source.len() {
                t0 + (source[index + 1] as f32 - t0) * (position - index as f32)
            } else {
                t0
            };
            times.push(self.clock.beat_at(f64::from(time + offset * beat_len))?);
            let next = position + step;
            if next <= position {
                return Err(Error(
                    "beat grid subdivision exceeds timestamp precision".into(),
                ));
            }
            position = next;
        }
        Ok(Events::Beats {
            times: EventTimes::new(times)?,
        })
    }
    fn seconds(&self, events: &Events) -> Result<Vec<f32>> {
        let Events::Beats { times } = events else {
            return Err(Error(
                "this operation needs recorded event timestamps".into(),
            ));
        };
        times
            .as_slice()
            .iter()
            .map(|beat| self.seconds32(*beat))
            .collect()
    }
    pub(crate) fn seconds32(&self, beat: f64) -> Result<f32> {
        let seconds = self.clock.seconds_at(beat)? as f32;
        if !seconds.is_finite() {
            return Err(Error("event timestamp exceeds seconds precision".into()));
        }
        Ok(seconds)
    }
    pub(crate) fn event_index(&self, times: &EventTimes, seconds: f64) -> Result<usize> {
        let recorded = times.as_slice();
        let (mut left, mut right) = (0, recorded.len());
        while left < right {
            let middle = left + (right - left) / 2;
            let at = self.seconds32(recorded[middle])?;
            if f64::from(at) <= seconds {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        Ok(left)
    }
}

pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    let seconds = ValueType::Signal(SignalType::new(Unit::Seconds, Channels::Value));
    let vector = |unit| {
        ValueType::Signal(SignalType {
            unit: Some(unit),
            channels: None,
        })
    };
    let port = |name, kind, value| crate::signals::port(name, kind, Some(value));
    let fixed = |name, kind, value| Input {
        rate: Rate::Fixed,
        ..port(name, kind, value)
    };
    let empty = || {
        Value::Events(Events::Beats {
            times: EventTimes::new(Vec::new()).unwrap(),
        })
    };
    let (name, inputs, outputs) = match op {
        Primitive::TrackTime => (
            "Track time",
            vec![],
            vec![
                ("seconds", seconds),
                ("clip_start", seconds),
                ("clip_duration", seconds),
                ("bpm", ValueType::Number),
            ],
        ),
        Primitive::GridEvents => (
            "Beat grid events",
            vec![
                (
                    "subdivision",
                    fixed("Subdivision", ValueType::Number, Value::Number(1.)),
                ),
                (
                    "offset",
                    fixed("Beat offset", ValueType::Number, Value::Number(0.)),
                ),
                (
                    "downbeats",
                    fixed("Only downbeats", ValueType::Boolean, Value::Boolean(false)),
                ),
            ],
            vec![("events", ValueType::Events)],
        ),
        Primitive::EventWindow => (
            "Recent events",
            vec![
                ("events", fixed("Events", ValueType::Events, empty())),
                (
                    "time",
                    port("Query time (seconds)", seconds, Value::Seconds(0.)),
                ),
                (
                    "count",
                    fixed("Event count", ValueType::Number, Value::Number(1.)),
                ),
            ],
            vec![
                ("times", vector(Unit::Seconds)),
                ("present", vector(Unit::Proportion)),
                ("weights", vector(Unit::Proportion)),
                ("index", ValueType::Number),
            ],
        ),
        Primitive::EventSpacing => (
            "Minimum event gap",
            vec![
                ("events", fixed("Events", ValueType::Events, empty())),
                (
                    "minimum",
                    fixed("Ignore gaps up to (seconds)", seconds, Value::Seconds(0.)),
                ),
            ],
            vec![("spacing", seconds)],
        ),
        Primitive::ThinEvents => (
            "Separate events",
            vec![
                ("events", fixed("Events", ValueType::Events, empty())),
                (
                    "minimum",
                    fixed("Minimum separation (seconds)", seconds, Value::Seconds(0.)),
                ),
            ],
            vec![("events", ValueType::Events)],
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
                        rate: if op == Primitive::TrackTime && k != "seconds" {
                            Rate::Fixed
                        } else {
                            Rate::Frame
                        },
                    },
                )
            })
            .collect(),
        body: Body::Primitive(op),
    })
}

pub(crate) fn run(
    op: Primitive,
    inputs: &BTreeMap<String, EvaluatedValue>,
    outputs: &BTreeMap<String, Output>,
    batch: crate::runtime::Batch<'_>,
) -> Result<BTreeMap<String, EvaluatedValue>> {
    let source = batch
        .frame
        .features
        .ok_or_else(|| Error("this graph requires analyzed track timing".into()))?;
    let FeatureSample::Timing(timing) =
        source.sample(&FeatureRequest::Timing, batch.frame.clip_start)?
    else {
        return Err(Error("track source returned the wrong timing data".into()));
    };
    let fixed = |key: &str| inputs[key].fixed_scalar();
    let signal = |key: &str, value: Signal| {
        Ok(BTreeMap::from([(
            key.into(),
            EvaluatedValue::from_signal(outputs[key].value_type, value)?,
        )]))
    };
    let structured = |value: Events| {
        Ok(BTreeMap::from([(
            "events".into(),
            EvaluatedValue::literal(&Value::Events(value))?,
        )]))
    };
    if op == Primitive::TrackTime {
        let times = batch
            .times
            .iter()
            .map(|beat| timing.clock.seconds_at(*beat))
            .collect::<Result<Vec<_>>>()?;
        let start = timing.clock.seconds_at(batch.frame.clip_start)?;
        let duration = timing
            .clock
            .seconds_at(batch.frame.clip_start + batch.frame.clip_duration)?
            - start;
        let mut values = signal("seconds", Signal::series(&times, Unit::Seconds)?)?;
        for (key, value, unit) in [
            ("clip_start", start, Unit::Seconds),
            ("clip_duration", duration, Unit::Seconds),
            ("bpm", timing.bpm, Unit::Number),
        ] {
            values.extend(signal(key, Signal::scalar(value, unit)?)?);
        }
        return Ok(values);
    }
    if op == Primitive::GridEvents {
        let Value::Boolean(downbeats) = inputs["downbeats"].control(0) else {
            unreachable!()
        };
        return structured(timing.grid_events(
            fixed("subdivision")?,
            fixed("offset")?,
            *downbeats,
        )?);
    }
    let Value::Events(events) = inputs["events"].control(0) else {
        unreachable!()
    };
    match op {
        Primitive::EventWindow => {
            let count = fixed("count")?;
            if count.fract() != 0. || !(1. ..=65535.).contains(&count) {
                return Err(Error("event count must be an integer in 1..65,535".into()));
            }
            let count = count as usize;
            let query = inputs["time"].numeric();
            let (n, t, c) = query.values().dim();
            if n != 1 || c != 1 {
                return Err(Error(
                    "event query time must have one fixture and one channel".into(),
                ));
            }
            if t.saturating_mul(count) > 16_777_216 {
                return Err(Error(
                    "event query tensor exceeds 16,777,216 elements".into(),
                ));
            }
            let mut times = ndarray::Array3::zeros((1, t, count));
            let mut present = ndarray::Array3::zeros((1, t, count));
            let mut indices = Vec::with_capacity(t);
            let recorded = match events.schedule() {
                Events::Beats { times } => Some(times),
                _ => None,
            };
            events.validate()?;
            for row in 0..t {
                let seconds = query.values()[[0, row, 0]];
                if let Some(recorded) = recorded {
                    // Compare seconds directly: round trips across a tempo
                    // segment must not move an event to the following sample.
                    let index = timing.event_index(recorded, seconds)?;
                    indices.push(index as f64 - 1.);
                    let recorded = recorded.as_slice();
                    for channel in 0..count.min(index) {
                        times[[0, row, channel]] =
                            f64::from(timing.seconds32(recorded[index - channel - 1])?);
                        present[[0, row, channel]] = 1.;
                    }
                } else if let Events::Periodic {
                    repeat,
                    grid_aligned,
                    delay,
                } = events.schedule()
                {
                    let origin = if *grid_aligned {
                        0.
                    } else {
                        batch.frame.clip_start
                    } + delay;
                    let index = ((timing.clock.beat_at(seconds)? - origin) / repeat).floor();
                    if !index.is_finite() || index.abs() + count as f64 > 9_007_199_254_740_991. {
                        return Err(Error(
                            "periodic event index exceeds timestamp precision".into(),
                        ));
                    }
                    indices.push(index);
                    for channel in 0..count {
                        times[[0, row, channel]] = timing
                            .clock
                            .seconds_at(origin + (index - channel as f64) * repeat)?;
                        present[[0, row, channel]] = 1.;
                    }
                } else {
                    return Err(Error(
                        "unwired events must resolve to repeat controls".into(),
                    ));
                }
            }
            let channels = Channels::components(count)?;
            let weights = recent_weights(events, batch.fixtures, &indices, &present, channels)?;
            Ok(BTreeMap::from([
                (
                    "weights".into(),
                    EvaluatedValue::from_signal(outputs["weights"].value_type, weights)?,
                ),
                (
                    "index".into(),
                    EvaluatedValue::from_signal(
                        outputs["index"].value_type,
                        Signal::series(&indices, Unit::Number)?,
                    )?,
                ),
                (
                    "times".into(),
                    EvaluatedValue::from_signal(
                        outputs["times"].value_type,
                        Signal::new(times, Unit::Seconds, channels, None)?,
                    )?,
                ),
                (
                    "present".into(),
                    EvaluatedValue::from_signal(
                        outputs["present"].value_type,
                        Signal::new(present, Unit::Proportion, channels, None)?,
                    )?,
                ),
            ]))
        }
        Primitive::EventSpacing | Primitive::ThinEvents => {
            let minimum = fixed("minimum")? as f32;
            if !minimum.is_finite() || minimum < 0. {
                return Err(Error(
                    "minimum event gap must be finite and nonnegative".into(),
                ));
            }
            let times = timing.seconds(events)?;
            if op == Primitive::EventSpacing {
                let gap = times
                    .windows(2)
                    .map(|w| w[1] - w[0])
                    .filter(|gap| *gap > minimum)
                    .reduce(f32::min)
                    .unwrap_or(0.);
                signal("spacing", Signal::scalar(f64::from(gap), Unit::Seconds)?)
            } else {
                let mut retained = Vec::<f32>::new();
                for time in times {
                    if retained
                        .last()
                        .is_none_or(|previous| time - previous >= minimum)
                    {
                        retained.push(time);
                    }
                }
                let times = retained
                    .into_iter()
                    .map(|time| timing.clock.beat_at(f64::from(time)))
                    .collect::<Result<Vec<_>>>()?;
                structured(Events::Beats {
                    times: EventTimes::new(times)?,
                })
            }
        }
        _ => unreachable!(),
    }
}

fn recent_weights(
    events: &Events,
    fixtures: &[String],
    indices: &[f64],
    present: &ndarray::Array3<f64>,
    channels: Channels,
) -> Result<Signal> {
    if !matches!(events, Events::Targeted { .. }) {
        return Signal::new(present.clone(), Unit::Proportion, channels, None);
    }
    let (_, t, c) = present.dim();
    if fixtures.len().saturating_mul(t).saturating_mul(c) > 16_777_216 {
        return Err(Error(
            "event target tensor exceeds 16,777,216 elements".into(),
        ));
    }
    let queried: std::collections::BTreeSet<_> = present
        .indexed_iter()
        .filter(|(_, value)| **value > 0.)
        .map(|((_, t, c), _)| indices[t] as i64 - c as i64)
        .collect();
    let queried: Vec<_> = queried.into_iter().collect();
    let columns: BTreeMap<_, _> = queried.iter().enumerate().map(|(i, e)| (*e, i)).collect();
    let weights = crate::event_targets::weights(events, fixtures, &queried)?.unwrap();
    Signal::new(
        ndarray::Array3::from_shape_fn((fixtures.len(), t, c), |(n, t, c)| {
            if present[[0, t, c]] == 0. {
                0.
            } else {
                weights[[n, columns[&(indices[t] as i64 - c as i64)]]]
            }
        }),
        Unit::Proportion,
        channels,
        Some(fixtures.to_vec().into()),
    )
}
