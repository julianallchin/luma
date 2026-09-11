use super::*;
use ndarray::{Axis, Zip};

pub(crate) fn run(
    op: Primitive,
    inputs: &BTreeMap<String, EvaluatedValue>,
    input_types: &BTreeMap<String, Input>,
    outputs: &BTreeMap<String, Output>,
    batch: Batch,
) -> Result<BTreeMap<String, EvaluatedValue>> {
    for (key, value) in inputs {
        let expected = input_types[key].value_type;
        if !expected.accepts(value.kind()) {
            return Err(Error(format!(
                "{key}: expected {expected:?}, got {:?}",
                value.kind()
            )));
        }
    }
    let signal = |key: &str| inputs[key].numeric();
    let fixed = |key: &str| inputs[key].fixed_scalar();
    let control = |key: &str, t: usize| inputs[key].control(t);
    let shape = |key: &str, t| match control(key, t) {
        Value::Envelope(value) => value,
        _ => unreachable!("envelope input"),
    };
    let mapping = |key: &str| match control(key, 0) {
        Value::Coordinates(value) => value,
        _ => unreachable!("coordinates input"),
    };
    let numeric = |key: &str, value: Signal| {
        Ok(BTreeMap::from([(
            key.into(),
            EvaluatedValue::from_signal(outputs[key].value_type, value)?,
        )]))
    };
    let structured = |key: &str, values: Vec<Value>| {
        Ok(BTreeMap::from([(
            key.into(),
            EvaluatedValue::controls(outputs[key].value_type, values)?,
        )]))
    };
    let lighting = |value: LightingSignal| {
        Ok(BTreeMap::from([(
            "lighting".into(),
            EvaluatedValue::output(value),
        )]))
    };
    let fixtures = batch.fixtures;
    let frame = batch.frame;
    let series = |values: Vec<f64>, unit| Signal::series(&values, unit);
    let time = batch.clock;
    match op {
        Primitive::PaletteFallback => structured(
            "gradient",
            (0..batch.times.len().max(1))
                .map(|t| {
                    let value = control("gradient", t);
                    let Value::Gradient(gradient) = value else {
                        unreachable!()
                    };
                    if gradient.stops.is_empty() {
                        control("fallback", t)
                    } else {
                        value
                    }
                    .clone()
                })
                .collect(),
        ),
        Primitive::FilterAudio { highpass } => {
            let Value::AudioSource(source) = control("source", 0) else {
                unreachable!()
            };
            let cutoff_hz = fixed("cutoff_hz")?;
            let filter = if highpass {
                AudioFilter::Highpass { cutoff_hz }
            } else {
                AudioFilter::Lowpass { cutoff_hz }
            };
            structured("source", vec![Value::AudioSource(source.filtered(filter)?)])
        }
        Primitive::WanderPoints => numeric("points", crate::point_fields::wander(inputs)?),
        Primitive::ProximityWeights => numeric(
            "weights",
            crate::point_fields::proximity(
                signal("position"),
                signal("points"),
                signal("temperature"),
            )?,
        ),
        Primitive::ClipRange => {
            crate::clip_range::sample_count(fixed("samples")?)?;
            let value = signal("value");
            let (minimum, maximum) = crate::clip_range::bounds(value);
            crate::clip_range::result(value.unit(), minimum, maximum)
        }
        Primitive::RandomEventTargets => {
            let Value::Events(events) = control("events", 0) else {
                unreachable!()
            };
            let Value::Seed(seed) = control("seed", 0) else {
                unreachable!()
            };
            let Value::Boolean(cycle) = control("cycle", 0) else {
                unreachable!()
            };
            let targets = EventTargets::random(
                fixtures.to_vec(),
                fixed("proportion")?,
                crate::value_noise::hash(frame.seed, *seed),
                *cycle,
            )?;
            structured(
                "events",
                vec![Value::Events(events.clone().targeted(targets)?)],
            )
        }
        Primitive::TrackTime
        | Primitive::GridEvents
        | Primitive::EventWindow
        | Primitive::EventSpacing
        | Primitive::ThinEvents => crate::event_timing::run(op, inputs, outputs, batch),
        Primitive::Output => lighting(LightingSignal::terminal(inputs, fixtures)?),
        Primitive::ChannelCount => numeric(
            "value",
            Signal::scalar(signal("value").channels().count() as f64, Unit::Number)?,
        ),
        Primitive::MixPalette => {
            let (color, opacity) = crate::color::mix_palette(inputs, time)?;
            Ok(BTreeMap::from([
                (
                    "color".into(),
                    EvaluatedValue::from_signal(outputs["color"].value_type, color)?,
                ),
                (
                    "opacity".into(),
                    EvaluatedValue::from_signal(outputs["opacity"].value_type, opacity)?,
                ),
            ]))
        }
        Primitive::WorldGeometry => [("position", 3), ("index", 1)]
            .into_iter()
            .map(|(key, channels)| {
                Ok((
                    key.into(),
                    EvaluatedValue::from_signal(
                        outputs[key].value_type,
                        Signal::new(
                            Array3::from_shape_fn((frame.cells.len(), 1, channels), |(n, _, c)| {
                                if key == "index" {
                                    n as f64
                                } else {
                                    frame.cells[n].world[c]
                                }
                            }),
                            Unit::Number,
                            Channels::components(channels)?,
                            Some(
                                frame
                                    .cells
                                    .iter()
                                    .map(|c| c.id.clone())
                                    .collect::<Vec<_>>()
                                    .into(),
                            ),
                        )?,
                    )?,
                ))
            })
            .collect(),
        Primitive::RadialCoordinates => {
            let (phase, radius) =
                super::geometry::radial_coordinates(signal("position"), signal("order"), fixtures)?;
            Ok(BTreeMap::from([
                (
                    "phase".into(),
                    EvaluatedValue::from_signal(outputs["phase"].value_type, phase)?,
                ),
                (
                    "radius".into(),
                    EvaluatedValue::from_signal(outputs["radius"].value_type, radius)?,
                ),
            ]))
        }
        Primitive::CirclePhase => numeric(
            "phase",
            super::geometry::circle_phase(signal("position"), signal("order"), fixtures)?,
        ),
        Primitive::PrincipalDirection => numeric(
            "direction",
            super::geometry::principal_direction(signal("position"), signal("order"), fixtures)?,
        ),
        Primitive::RankNearby => numeric(
            "value",
            super::geometry::rank_nearby(
                signal("value"),
                signal("position"),
                signal("order"),
                fixed("tolerance")?,
                fixtures,
            )?,
        ),
        Primitive::JoinChannels => numeric("value", signal("a").join_channels(signal("b"))?),
        Primitive::Channel => {
            let source = signal("value");
            let index = fixed("index")?;
            if index < 0. || index.fract() != 0. || index >= source.values().dim().2 as f64 {
                return Err(Error("channel index is outside the signal".into()));
            }
            numeric(
                "value",
                Signal::new(
                    source.values().select(Axis(2), &[index as usize]),
                    source.unit(),
                    Channels::Value,
                    source.fixtures().map(Into::into),
                )?,
            )
        }
        Primitive::ChannelIndex => {
            let source = signal("value");
            numeric(
                "value",
                Signal::new(
                    Array3::from_shape_fn(source.values().dim(), |(_, _, c)| c as f64),
                    Unit::Number,
                    Channels::components(source.values().dim().2)?,
                    source.fixtures().map(Into::into),
                )?,
            )
        }
        Primitive::ChannelArgmax => {
            let source = signal("value");
            let indices = source.values().map_axis(Axis(2), |channels| {
                channels
                    .iter()
                    .enumerate()
                    .fold((0, f64::NEG_INFINITY), |best, (index, &value)| {
                        if value > best.1 {
                            (index, value)
                        } else {
                            best
                        }
                    })
                    .0 as f64
            });
            numeric(
                "value",
                Signal::new(
                    indices.insert_axis(Axis(2)),
                    Unit::Number,
                    Channels::Value,
                    source.fixtures().map(Into::into),
                )?,
            )
        }
        Primitive::ChannelMaximum | Primitive::ChannelSum => {
            let source = signal("value");
            numeric(
                "value",
                Signal::new(
                    if op == Primitive::ChannelSum {
                        source.values().sum_axis(Axis(2)).insert_axis(Axis(2))
                    } else {
                        source
                            .values()
                            .fold_axis(Axis(2), f64::NEG_INFINITY, |a, b| a.max(*b))
                            .insert_axis(Axis(2))
                    },
                    source.unit(),
                    Channels::Value,
                    source.fixtures().map(Into::into),
                )?,
            )
        }
        Primitive::FieldBinary(math) => {
            let a = signal("a");
            let b = signal("b");
            let metadata = SignalType::new(a.unit(), *a.channels())
                .binary(SignalType::new(b.unit(), *b.channels()), math)?;
            numeric(
                "value",
                a.zip(b, metadata.unit.unwrap(), |a, b| math.evaluate(a, b))?,
            )
        }
        Primitive::FieldClamp => numeric(
            "mask",
            signal("value").map(Unit::Proportion, |v| v.clamp(0.0, 1.0))?,
        ),
        Primitive::FieldGreater => {
            let a = signal("a");
            let b = signal("b");
            SignalType::new(a.unit(), *a.channels()).binary(
                SignalType::new(b.unit(), *b.channels()),
                FieldMath::Subtract,
            )?;
            numeric(
                "mask",
                a.zip3(b, signal("tolerance"), Unit::Proportion, |a, b, t| {
                    if a - b > t.max(0.0) {
                        1.0
                    } else {
                        0.0
                    }
                })?,
            )
        }
        Primitive::FieldSelect => {
            let yes = signal("yes");
            let no = signal("no");
            let metadata = SignalType::new(yes.unit(), *yes.channels()).binary(
                SignalType::new(no.unit(), *no.channels()),
                FieldMath::Maximum,
            )?;
            numeric(
                "value",
                signal("condition").zip3(yes, no, metadata.unit.unwrap(), |c, y, n| {
                    if c > 0.0 {
                        y
                    } else {
                        n
                    }
                })?,
            )
        }
        Primitive::ChooseNumber => Ok(BTreeMap::from([(
            "value".into(),
            inputs[if control("condition", 0) == &Value::Boolean(true) {
                "yes"
            } else {
                "no"
            }]
            .clone(),
        )])),
        Primitive::FieldUnary(math) => {
            let source = signal("value");
            if math == UnaryMath::SquareRoot && source.values().iter().any(|value| *value < 0.0) {
                return Err(Error("needs nonnegative values".into()));
            }
            let metadata = SignalType::new(source.unit(), *source.channels()).unary(math)?;
            numeric(
                "value",
                source.map(metadata.unit.unwrap(), |v| match math {
                    UnaryMath::Absolute => v.abs(),
                    UnaryMath::Floor => v.floor(),
                    UnaryMath::Float32 => f64::from(v as f32),
                    UnaryMath::Fraction => v.rem_euclid(1.0),
                    UnaryMath::Sine => (v * std::f64::consts::TAU).sin(),
                    UnaryMath::SquareRoot => v.sqrt(),
                })?,
            )
        }
        Primitive::DomainIndex => {
            let value = signal("value");
            if let Some(ids) = value.fixtures() {
                let order: BTreeMap<_, _> = frame
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.id.as_str(), i))
                    .collect();
                let indices = ids
                    .iter()
                    .map(|id| {
                        order.get(id.as_str()).map(|i| *i as f64).ok_or_else(|| {
                            Error(format!("fixture {id} is not in the prepared selection"))
                        })
                    })
                    .collect::<Result<_>>()?;
                numeric(
                    "value",
                    Signal::new(
                        Array3::from_shape_vec((ids.len(), 1, 1), indices).unwrap(),
                        Unit::Number,
                        Channels::Value,
                        Some(ids.into()),
                    )?,
                )
            } else {
                numeric("value", Signal::scalar(0., Unit::Number)?)
            }
        }
        Primitive::AlignDomain => {
            let value = signal("value");
            let reference = signal("reference");
            numeric(
                "value",
                if let Some(ids) = reference.fixtures() {
                    value.on_fixtures(ids)?
                } else {
                    super::geometry::first(value, signal("order"))?
                },
            )
        }
        Primitive::SeedStream => {
            let Value::Seed(seed) = control("seed", 0) else {
                unreachable!("seed input")
            };
            let Value::Seed(stream) = control("stream", 0) else {
                unreachable!("seed input")
            };
            structured(
                "seed",
                vec![Value::Seed(crate::value_noise::hash(*seed, *stream))],
            )
        }
        Primitive::ValueNoise1d | Primitive::ValueNoise3d => {
            let Value::Seed(seed) = control("seed", 0) else {
                unreachable!("seed input")
            };
            let seed = *seed;
            let keys: &[&str] = if matches!(op, Primitive::ValueNoise1d) {
                &["position"]
            } else {
                &["x", "y", "z"]
            };
            if keys
                .iter()
                .any(|key| signal(key).values().iter().any(|v| v.abs() > 1e12))
            {
                return Err(Error("noise coordinates must be within ±1e12".into()));
            }
            let values = if matches!(op, Primitive::ValueNoise1d) {
                signal("position").zip(signal("octaves"), Unit::Number, |x, o| {
                    crate::value_noise::noise1(x, o, seed)
                })?
            } else {
                signal("x").zip4(
                    signal("y"),
                    signal("z"),
                    signal("octaves"),
                    Unit::Number,
                    |x, y, z, o| crate::value_noise::noise3(x, y, z, o, seed),
                )?
            };
            numeric("value", values)
        }
        Primitive::FieldFirst => numeric(
            "value",
            super::geometry::first(signal("value"), signal("order"))?,
        ),
        Primitive::Power => {
            let base = signal("base");
            let exponent = signal("exponent");
            SignalType::new(base.unit(), *base.channels())
                .power(SignalType::new(exponent.unit(), *exponent.channels()))?;
            // Signal construction rejects non-real results and overflow; do not
            // silently turn an invalid root or reciprocal into a different value.
            numeric("value", base.zip(exponent, Unit::Number, f64::powf)?)
        }
        Primitive::Noise => {
            for key in ["x", "y", "z"] {
                if signal(key).values().iter().any(|v| v.abs() > 1e12) {
                    return Err(Error(
                        "noise coordinates must be finite and within ±1e12".into(),
                    ));
                }
            }
            numeric(
                "value",
                signal("x").zip3(signal("y"), signal("z"), Unit::Number, |x, y, z| {
                    crate::signals::coherent_noise([x, y, z], frame.seed)
                        .expect("validated noise coordinates")
                })?,
            )
        }
        Primitive::RandomField => {
            let epoch = signal("epoch").on_fixtures(fixtures)?;
            if epoch
                .values()
                .iter()
                .any(|v| v.fract() != 0.0 || v.abs() > 9_007_199_254_740_991.0)
            {
                return Err(Error(
                    "random epoch must be an exactly representable integer".into(),
                ));
            }
            let values = Zip::indexed(epoch.values()).map_collect(|(n, _, _), v| {
                crate::spatial::threshold(
                    &fixtures[n],
                    crate::spatial::epoch_seed(frame.seed, *v as i64),
                )
            });
            numeric(
                "value",
                Signal::new(
                    values,
                    Unit::Number,
                    *epoch.channels(),
                    Some(fixtures.to_vec().into()),
                )?,
            )
        }
        Primitive::ClipTime => Ok(BTreeMap::from([
            (
                "elapsed".into(),
                EvaluatedValue::from_signal(
                    ValueType::Beats,
                    time.map(Unit::Beats, |v| (v - frame.clip_start).max(0.0))?,
                )?,
            ),
            (
                "progress".into(),
                EvaluatedValue::from_signal(
                    ValueType::Proportion,
                    time.map(Unit::Proportion, |v| {
                        ((v - frame.clip_start) / frame.clip_duration).clamp(0.0, 1.0)
                    })?,
                )?,
            ),
            (
                "duration".into(),
                EvaluatedValue::literal(&Value::Beats(frame.clip_duration))?,
            ),
            (
                "beat".into(),
                EvaluatedValue::from_signal(ValueType::Number, time.clone())?,
            ),
        ])),
        Primitive::Rhythm => {
            let repeat = fixed("repeat")?;
            if repeat <= 0.0 {
                return Err(Error("repeat interval must be greater than zero".into()));
            }
            let origin = if control("grid_aligned", 0) == &Value::Boolean(true) {
                0.0
            } else {
                frame.clip_start
            };
            let delay = fixed("delay")?;
            let elapsed = time.map(Unit::Number, |v| v - origin - delay)?;
            Ok(BTreeMap::from([
                (
                    "elapsed".into(),
                    EvaluatedValue::from_signal(
                        ValueType::Beats,
                        elapsed.map(Unit::Beats, |v| v.rem_euclid(repeat))?,
                    )?,
                ),
                (
                    "cycle".into(),
                    EvaluatedValue::from_signal(
                        ValueType::Number,
                        elapsed.map(Unit::Number, |v| (v / repeat).floor())?,
                    )?,
                ),
            ]))
        }
        Primitive::ResolveMapping => {
            let Value::Mapping(spec) = control("mapping", 0) else {
                unreachable!()
            };
            structured(
                "coordinates",
                vec![Value::Coordinates(spec.resolve(frame.cells)?)],
            )
        }
        Primitive::CoordinateOffset => {
            // Mapping order encodes selection order; numerical wires are
            // canonical. Align rows by identity before combining them.
            let mut coordinates = mapping("mapping").coordinates.clone();
            coordinates.sort_by(|a, b| a.cell.cmp(&b.cell));
            let Value::Boundary(boundary) = control("boundary", 0) else {
                unreachable!()
            };
            let ids: std::sync::Arc<[String]> = coordinates
                .iter()
                .map(|c| c.cell.clone())
                .collect::<Vec<_>>()
                .into();
            let wraps = |n: usize| {
                *boundary == Boundary::Wrap
                    || (*boundary == Boundary::Natural && coordinates[n].closed)
            };
            let positions = Signal::new(
                Array3::from_shape_vec(
                    (coordinates.len(), 1, 1),
                    coordinates.iter().map(|c| c.position).collect(),
                )
                .unwrap(),
                Unit::Position,
                Channels::Value,
                Some(ids.clone()),
            )?;
            let delta = positions.zip(signal("position"), Unit::Number, |a, b| a - b)?;
            let values = Zip::indexed(delta.values()).map_collect(|(n, _, _), delta| {
                if wraps(n) {
                    (delta + 0.5).rem_euclid(1.0) - 0.5
                } else {
                    *delta
                }
            });
            let wrap =
                Signal::new(
                    Array3::from_shape_fn((coordinates.len(), 1, 1), |(n, _, _)| {
                        if wraps(n) {
                            1.0
                        } else {
                            0.0
                        }
                    }),
                    Unit::Proportion,
                    Channels::Value,
                    Some(ids.clone()),
                )?;
            Ok(BTreeMap::from([
                (
                    "value".into(),
                    EvaluatedValue::from_signal(
                        outputs["value"].value_type,
                        Signal::new(values, Unit::Number, *delta.channels(), Some(ids))?,
                    )?,
                ),
                (
                    "wrapped".into(),
                    EvaluatedValue::from_signal(ValueType::Mask, wrap)?,
                ),
            ]))
        }
        Primitive::FieldEnvelope | Primitive::Envelope => {
            let (key, out) = if op == Primitive::Envelope {
                ("progress", "value")
            } else {
                ("phase", "mask")
            };
            let phase = signal(key).zip(time, Unit::Proportion, |v, _| v)?;
            let values = Zip::indexed(phase.values())
                .map_collect(|(_, t, _), v| shape("shape", t).sample(*v));
            numeric(
                out,
                Signal::new(
                    values,
                    Unit::Proportion,
                    *phase.channels(),
                    phase.fixtures().map(|v| v.to_vec().into()),
                )?,
            )
        }
        Primitive::SoftEdges => structured(
            "shape",
            (0..signal("softness").values().dim().1)
                .map(|t| Value::Envelope(Envelope::soft_edges(signal("softness").at(0, t, 0))))
                .collect(),
        ),
        Primitive::StageCoordinates => ["u", "v", "z"]
            .into_iter()
            .enumerate()
            .map(|(axis, key)| {
                Ok((
                    key.into(),
                    EvaluatedValue::from_signal(
                        ValueType::Field,
                        Signal::new(
                            Array3::from_shape_vec(
                                (fixtures.len(), 1, 1),
                                frame.cells.iter().map(|c| c.uvz[axis]).collect(),
                            )
                            .unwrap(),
                            Unit::Number,
                            Channels::Value,
                            Some(
                                frame
                                    .cells
                                    .iter()
                                    .map(|c| c.id.clone())
                                    .collect::<Vec<_>>()
                                    .into(),
                            ),
                        )?,
                    )?,
                ))
            })
            .collect(),
        Primitive::FieldRank => {
            let source = signal("value");
            let input = if source.fixtures().is_some() {
                source.clone()
            } else {
                source.on_fixtures(fixtures)?
            };
            let ids = input.fixtures().expect("rank has a fixture domain");
            let (n, t, c) = input.values().dim();
            let mut values = Array3::zeros((n, t, c));
            for time in 0..t {
                for channel in 0..c {
                    let mut order: Vec<_> = (0..n).collect();
                    order.sort_by(|a, b| {
                        let av = input.at(*a, time, channel);
                        let bv = input.at(*b, time, channel);
                        if av == bv {
                            ids[*a].cmp(&ids[*b])
                        } else {
                            av.total_cmp(&bv)
                        }
                    });
                    for (rank, n) in order.into_iter().enumerate() {
                        values[[n, time, channel]] = rank as f64;
                    }
                }
            }
            numeric(
                "value",
                Signal::new(
                    values,
                    Unit::Number,
                    *input.channels(),
                    Some(ids.to_vec().into()),
                )?,
            )
        }
        Primitive::FieldReduce(reduction) => {
            let input = signal("value");
            let n = input.values().dim().0;
            let values = input
                .values()
                .map_axis(Axis(0), |column| {
                    if n == 0 {
                        0.0
                    } else {
                        match reduction {
                            FieldReduction::Minimum => {
                                column.iter().copied().fold(f64::INFINITY, f64::min)
                            }
                            FieldReduction::Maximum => {
                                column.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                            }
                            FieldReduction::Count => n as f64,
                            FieldReduction::DistinctCount => {
                                let mut values = column.to_vec();
                                values.sort_by(f64::total_cmp);
                                values.dedup();
                                values.len() as f64
                            }
                            FieldReduction::Mean => column.iter().map(|v| v / n as f64).sum(),
                        }
                    }
                })
                .insert_axis(Axis(0));
            numeric(
                "value",
                Signal::new(
                    values,
                    if matches!(
                        reduction,
                        FieldReduction::Count | FieldReduction::DistinctCount
                    ) {
                        Unit::Number
                    } else {
                        input.unit()
                    },
                    *input.channels(),
                    None,
                )?,
            )
        }
        Primitive::SampleGradient | Primitive::SampleGradientField => {
            let position = signal("position").zip(time, Unit::Proportion, |v, _| v)?;
            let (n, t, _) = position.values().dim();
            let values = Array3::from_shape_fn((n, t, 3), |(n, t, ch)| {
                let Value::Gradient(gradient) = control("gradient", t) else {
                    unreachable!()
                };
                gradient.sample(position.at(n, t, 0))[ch]
            });
            let domain = position
                .fixtures()
                .map(|v| std::sync::Arc::<[String]>::from(v.to_vec()));
            let alpha = Array3::from_shape_fn((n, t, 1), |(n, t, _)| {
                let Value::Gradient(gradient) = control("gradient", t) else {
                    unreachable!()
                };
                gradient.sample_alpha(position.at(n, t, 0))
            });
            let mut result = numeric(
                "color",
                Signal::new(values, Unit::Proportion, Channels::Rgb, domain.clone())?,
            )?;
            result.extend(numeric(
                "opacity",
                Signal::new(alpha, Unit::Proportion, Channels::Value, domain)?,
            )?);
            Ok(result)
        }
        Primitive::MaskColor => numeric(
            "color",
            signal("color").zip(signal("mask"), Unit::Proportion, |a, b| a * b)?,
        ),
        Primitive::RotateHue => {
            let turns = signal("turns");
            let domain = signal("color").zip(turns, Unit::Proportion, |color, _| color)?;
            let mut values = domain.values().clone();
            for (n, mut head) in values.outer_iter_mut().enumerate() {
                for (t, mut rgb) in head.outer_iter_mut().enumerate() {
                    let rotated =
                        crate::color::rotate_hue([rgb[0], rgb[1], rgb[2]], turns.at(n, t, 0));
                    for (channel, value) in rgb.iter_mut().zip(rotated) {
                        *channel = value;
                    }
                }
            }
            numeric(
                "color",
                Signal::new(
                    values,
                    Unit::Proportion,
                    Channels::Rgb,
                    domain.fixtures().map(Into::into),
                )?,
            )
        }
        Primitive::Hsv => {
            let domain = signal("hue").zip3(
                signal("saturation"),
                signal("value"),
                Unit::Proportion,
                |_, _, _| 0.0,
            )?;
            let (n, t, _) = domain.values().dim();
            let values = Array3::from_shape_fn((n, t, 3), |(n, t, ch)| {
                crate::color::hsv(
                    signal("hue").at(n, t, 0),
                    signal("saturation").at(n, t, 0),
                    signal("value").at(n, t, 0),
                )[ch]
            });
            numeric(
                "color",
                Signal::new(
                    values,
                    Unit::Proportion,
                    Channels::Rgb,
                    domain.fixtures().map(|v| v.to_vec().into()),
                )?,
            )
        }
        Primitive::ScalarBinary(_)
        | Primitive::ScalarConvert { .. }
        | Primitive::Broadcast(_)
        | Primitive::ColorField
        | Primitive::MaskToField
        | Primitive::TravelClock
        | Primitive::WriteMask
        | Primitive::WriteStrobeMask
        | Primitive::WriteColor
        | Primitive::WriteSpeed
        | Primitive::WritePosition
        | Primitive::AddLighting
        | Primitive::ChaseEvents
        | Primitive::PulseEvents
        | Primitive::DissolveEvents => Err(Error(
            "historical primitives must be migrated before execution".into(),
        )),
        Primitive::BeatEvents => structured(
            "trigger",
            vec![Value::Events(Events::Periodic {
                repeat: fixed("repeat")?,
                grid_aligned: control("grid_aligned", 0) == &Value::Boolean(true),
                delay: fixed("delay")?,
            })],
        ),
        Primitive::EventAges => {
            let Value::Events(events) = control("events", 0) else {
                unreachable!()
            };
            crate::event_tensor::event_ages(
                batch.times,
                events,
                fixed("duration")?,
                frame.clip_start,
                fixtures,
            )?
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    key.into(),
                    EvaluatedValue::from_signal(outputs[key].value_type, value)?,
                ))
            })
            .collect()
        }
        Primitive::AudioSpectrum => {
            let literals = inputs
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.sample(0)?)))
                .collect::<Result<_>>()?;
            let request = crate::features::request(op, &literals)?.expect("spectrum request");
            let source = frame
                .features
                .ok_or_else(|| Error("this graph requires analyzed track data".into()))?;
            let (spectrum, bin_hz) =
                super::spectrum::sample(source, &request, batch.times, frame.clip_start)?;
            Ok(BTreeMap::from([
                (
                    "spectrum".into(),
                    EvaluatedValue::from_signal(outputs["spectrum"].value_type, spectrum)?,
                ),
                (
                    "bin_hz".into(),
                    EvaluatedValue::from_signal(outputs["bin_hz"].value_type, bin_hz)?,
                ),
            ]))
        }
        Primitive::BandEnergy
        | Primitive::DrumClock
        | Primitive::DrumEvents
        | Primitive::Harmony => {
            let literals = inputs
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.sample(0)?)))
                .collect::<Result<_>>()?;
            let request = crate::features::request(op, &literals)?.expect("feature operation");
            let source = frame
                .features
                .ok_or_else(|| Error("this graph requires analyzed track data".into()))?;
            if op == Primitive::DrumEvents {
                let FeatureRequest::Onsets(drum) = request else {
                    unreachable!()
                };
                return structured(
                    "trigger",
                    vec![Value::Events(Events::Beats {
                        times: source.onsets(drum)?,
                    })],
                );
            }
            let mut columns: BTreeMap<&str, Vec<f64>> = outputs
                .keys()
                .map(|key| (key.as_str(), Vec::with_capacity(batch.times.len())))
                .collect();
            for beat in batch.times {
                let values = match (op, source.sample(&request, *beat)?) {
                    (Primitive::BandEnergy, FeatureSample::Energy(value))
                        if value.is_finite() && value >= 0.0 =>
                    {
                        vec![("value", value)]
                    }
                    (Primitive::DrumClock, FeatureSample::Onset(event)) => {
                        let (elapsed, index, present) = match event {
                            Some((at, index))
                                if at.is_finite()
                                    && at <= *beat
                                    && index <= 9_007_199_254_740_991 =>
                            {
                                (*beat - at, index as f64, 1.0)
                            }
                            None => (0.0, 0.0, 0.0),
                            _ => return Err(Error("invalid analyzed onset".into())),
                        };
                        vec![("elapsed", elapsed), ("index", index), ("present", present)]
                    }
                    (Primitive::Harmony, FeatureSample::PitchClass(pitch))
                        if pitch.is_none_or(|p| p < 12) =>
                    {
                        vec![
                            ("pitch_class", pitch.unwrap_or(0) as f64),
                            ("present", if pitch.is_some() { 1.0 } else { 0.0 }),
                        ]
                    }
                    _ => {
                        return Err(Error(
                            "track source returned the wrong feature type or range".into(),
                        ))
                    }
                };
                for (key, value) in values {
                    columns.get_mut(key).unwrap().push(value);
                }
            }
            columns
                .into_iter()
                .map(|(key, values)| {
                    let kind = outputs[key].value_type;
                    Ok((
                        key.into(),
                        EvaluatedValue::from_signal(kind, series(values, unit(kind).unwrap())?)?,
                    ))
                })
                .collect()
        }
    }
}
