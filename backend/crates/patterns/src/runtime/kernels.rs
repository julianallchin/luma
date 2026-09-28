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
        Primitive::ClipRange => {
            crate::clip_range::sample_count(fixed("samples")?)?;
            let value = signal("value");
            let (minimum, maximum) = crate::clip_range::bounds(value);
            crate::clip_range::result(value.unit(), minimum, maximum)
        }
        Primitive::Output => lighting(LightingSignal::terminal(inputs, fixtures)?),
        Primitive::Odometer
        | Primitive::EventLife
        | Primitive::SampleCurve
        | Primitive::RandomShare
        | Primitive::PathGlides
        | Primitive::AimBase
        | Primitive::AimFan
        | Primitive::AimMotion
        | Primitive::AimOffset
        | Primitive::AimTurn => crate::forms::ops::run(op, inputs, outputs, batch),
        Primitive::JoinChannels => numeric("value", signal("a").join_channels(signal("b"))?),
        Primitive::ChannelMaximum => {
            let source = signal("value");
            numeric(
                "value",
                Signal::new(
                    source
                        .values()
                        .fold_axis(Axis(2), f64::NEG_INFINITY, |a, b| a.max(*b))
                        .insert_axis(Axis(2)),
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
        Primitive::ChooseNumber => Ok(BTreeMap::from([(
            "value".into(),
            inputs[if control("condition", 0) == &Value::Boolean(true) {
                "yes"
            } else {
                "no"
            }]
            .clone(),
        )])),
        Primitive::Fraction => {
            let source = signal("value");
            numeric("value", source.map(Unit::Number, |v| v.rem_euclid(1.0))?)
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
        Primitive::ResolveMapping => {
            let Value::Mapping(spec) = control("mapping", 0) else {
                unreachable!()
            };
            structured(
                "coordinates",
                vec![Value::Coordinates(spec.resolve(frame.cells, frame.seed)?)],
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
        Primitive::Envelope => {
            let phase = signal("progress").zip(time, Unit::Proportion, |v, _| v)?;
            let values = Zip::indexed(phase.values())
                .map_collect(|(_, t, _), v| shape("shape", t).sample(*v));
            numeric(
                "value",
                Signal::new(
                    values,
                    Unit::Proportion,
                    *phase.channels(),
                    phase.fixtures().map(|v| v.to_vec().into()),
                )?,
            )
        }
        Primitive::SampleGradient => {
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
        Primitive::BandEnergy => {
            let literals = inputs
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.sample(0)?)))
                .collect::<Result<_>>()?;
            let request = crate::features::request(op, &literals)?.expect("feature operation");
            let source = frame
                .features
                .ok_or_else(|| Error("this graph requires analyzed track data".into()))?;
            let values = batch
                .times
                .iter()
                .map(|beat| match source.sample(&request, *beat)? {
                    value if value.is_finite() && value >= 0.0 => Ok(value),
                    _ => Err(Error(
                        "track source returned the wrong feature type or range".into(),
                    )),
                })
                .collect::<Result<Vec<_>>>()?;
            let kind = outputs["value"].value_type;
            numeric("value", series(values, unit(kind).unwrap())?)
        }
    }
}
