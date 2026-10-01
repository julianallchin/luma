use super::*;

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
    match op {
        Primitive::Kernel(kernel) => crate::clip_graph::kernels::run(kernel, inputs, batch),
        Primitive::Output => Ok(BTreeMap::from([(
            crate::clip_graph::OUTPUT.into(),
            EvaluatedValue::output(LightingSignal::terminal(inputs, batch.fixtures)?),
        )])),
        Primitive::BandEnergy => {
            let literals = inputs
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.sample(0)?)))
                .collect::<Result<_>>()?;
            let request = crate::features::request(op, &literals)?.expect("feature operation");
            let source = batch
                .frame
                .features
                .ok_or_else(|| Error("this graph requires analyzed track data".into()))?;
            // The band's level over the whole track, 0–1: a clip reads the
            // same level as any other clip at the same moment.
            let (low, high) = source.range(&request)?;
            if !(low.is_finite() && high.is_finite() && low <= high) {
                return Err(Error(
                    "track source returned the wrong feature range".into(),
                ));
            }
            let values = batch
                .times
                .iter()
                .map(|beat| match source.sample(&request, *beat)? {
                    value if value.is_finite() && value >= 0.0 => Ok(if high - low > 1e-12 {
                        ((value - low) / (high - low)).clamp(0., 1.)
                    } else {
                        0.
                    }),
                    _ => Err(Error(
                        "track source returned the wrong feature type or range".into(),
                    )),
                })
                .collect::<Result<Vec<_>>>()?;
            let kind = outputs["value"].value_type;
            Ok(BTreeMap::from([(
                "value".into(),
                EvaluatedValue::from_signal(
                    kind,
                    Signal::series(&values, kind.signal_type().unwrap().unit.unwrap())?,
                )?,
            )]))
        }
    }
}
