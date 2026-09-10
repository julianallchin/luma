//! Point geometry is numerical data. XYZ triples occupy consecutive channels;
//! proximity projects those sites onto any supplied position signal. Neither
//! operation reads fixture layout, playback state or color.
use crate::runtime::EvaluatedValue;
use crate::*;
use ndarray::Array3;
use std::collections::BTreeMap;

fn vector(values: [f64; 3], unit: Unit) -> Value {
    Value::Signal(
        Signal::new(
            Array3::from_shape_vec((1, 1, 3), values.to_vec()).unwrap(),
            unit,
            Channels::components(3).unwrap(),
            None,
        )
        .unwrap(),
    )
}
pub(crate) fn definition(op: Primitive) -> Option<Definition> {
    let numeric = ValueType::Signal(SignalType {
        unit: Some(Unit::Number),
        channels: None,
    });
    let port = |name, value: Value, rate| Input {
        rate,
        ..crate::signals::port(name, value.value_type(), Some(value))
    };
    let (name, inputs, output, kind) = match op {
        Primitive::WanderPoints => (
            "Wander points",
            vec![
                ("time", port("Time", Value::Seconds(0.), Rate::Frame)),
                ("count", port("Point count", Value::Number(6.), Rate::Fixed)),
                ("seed", port("Seed", Value::Seed(0), Rate::Fixed)),
                (
                    "minimum",
                    port("Minimum XYZ", vector([0.; 3], Unit::Number), Rate::Fixed),
                ),
                (
                    "maximum",
                    port("Maximum XYZ", vector([1.; 3], Unit::Number), Rate::Fixed),
                ),
                (
                    "drift",
                    port("Drift per second", Value::Number(0.05), Rate::Fixed),
                ),
                (
                    "amplitude",
                    port("Wander amplitude", Value::Number(0.16), Rate::Fixed),
                ),
                (
                    "periods",
                    port(
                        "Axis periods",
                        vector([17., 13., 11.], Unit::Seconds),
                        Rate::Fixed,
                    ),
                ),
            ],
            "points",
            numeric,
        ),
        Primitive::ProximityWeights => (
            "Proximity weights",
            vec![
                (
                    "position",
                    crate::signals::port(
                        "Position XYZ",
                        vector([0.; 3], Unit::Number).value_type(),
                        None,
                    ),
                ),
                (
                    "points",
                    crate::signals::port("Point XYZ triples", numeric, None),
                ),
                (
                    "temperature",
                    crate::signals::port(
                        "Blend distance",
                        ValueType::Number,
                        Some(Value::Number(0.3)),
                    ),
                ),
            ],
            "weights",
            ValueType::Signal(SignalType {
                unit: Some(Unit::Proportion),
                channels: None,
            }),
        ),
        _ => return None,
    };
    Some(Definition {
        name: name.into(),
        inputs: inputs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        outputs: BTreeMap::from([(
            output.into(),
            Output {
                value_type: kind,
                rate: Rate::Frame,
            },
        )]),
        body: Body::Primitive(op),
    })
}

fn count(value: f64) -> Result<usize> {
    if value.fract() != 0. || !(1. ..=4096.).contains(&value) {
        return Err(Error("point count must be an integer in 1..4,096".into()));
    }
    Ok(value as usize)
}
fn fixed_xyz(value: &EvaluatedValue) -> Result<[f64; 3]> {
    let signal = value.numeric();
    if signal.values().dim() != (1, 1, 3) || signal.fixtures().is_some() {
        return Err(Error(
            "point configuration requires one fixed XYZ vector".into(),
        ));
    }
    Ok(std::array::from_fn(|i| signal.values()[[0, 0, i]]))
}
pub(crate) fn wander(inputs: &BTreeMap<String, EvaluatedValue>) -> Result<Signal> {
    let count = count(inputs["count"].fixed_scalar()?)?;
    let minimum = fixed_xyz(&inputs["minimum"])?;
    let maximum = fixed_xyz(&inputs["maximum"])?;
    let periods = fixed_xyz(&inputs["periods"])?;
    if periods.iter().any(|p| *p <= 0.) || (0..3).any(|d| maximum[d] < minimum[d]) {
        return Err(Error(
            "point bounds must be ordered and axis periods positive".into(),
        ));
    }
    let drift = inputs["drift"].fixed_scalar()?;
    let amplitude = inputs["amplitude"].fixed_scalar()?;
    let Value::Seed(seed) = inputs["seed"].control(0) else {
        unreachable!()
    };
    let time = inputs["time"].numeric();
    let (n, t, c) = time.values().dim();
    if n != 1 || c != 1 || time.fixtures().is_some() {
        return Err(Error("point time needs one global value per sample".into()));
    }
    if t.saturating_mul(count).saturating_mul(3) > 16_777_216 {
        return Err(Error("point tensor exceeds 16,777,216 elements".into()));
    }
    // A shifted low-discrepancy lattice supplies evenly spread starting points.
    let g = 1.220_744_084_605_759_5_f64;
    let lattice = [1. / g, 1. / (g * g), 1. / (g * g * g)].map(|v| f64::from(v as f32));
    let random = |key: u64| {
        f64::from(
            (crate::value_noise::hash(*seed, key.wrapping_mul(0x9E37_79B9_7F4A_7C15)) as f64
                / u64::MAX as f64) as f32,
        )
    };
    let mut result = Array3::zeros((1, t, count * 3));
    for k in 0..count {
        for d in 0..3 {
            let base = (random(d as u64) + (k + 1) as f64 * lattice[d]).rem_euclid(1.);
            let direction = 2. * random((k * 6 + d) as u64) - 1.;
            let phase = random((k * 6 + 3 + d) as u64);
            for sample in 0..t {
                let seconds = time.values()[[0, sample, 0]];
                let position = base
                    + drift * seconds * direction
                    + amplitude * (std::f64::consts::TAU * (seconds / periods[d] + phase)).sin();
                let wrapped = position.rem_euclid(2.);
                let reflected = if wrapped > 1. { 2. - wrapped } else { wrapped };
                result[[0, sample, k * 3 + d]] = minimum[d] + (maximum[d] - minimum[d]) * reflected;
            }
        }
    }
    Signal::new(result, Unit::Number, Channels::components(count * 3)?, None)
}

pub(crate) fn proximity(
    position: &Signal,
    points: &Signal,
    temperature: &Signal,
) -> Result<Signal> {
    let (_, pt, pc) = points.values().dim();
    if points.values().dim().0 != 1 || points.fixtures().is_some() || pc % 3 != 0 {
        return Err(Error(
            "points require global XYZ triples in consecutive channels".into(),
        ));
    }
    let count = count((pc / 3) as f64)?;
    if position.values().dim().2 != 3
        || temperature.values().dim().2 != 1
        || temperature.values().iter().any(|t| *t < 0.)
    {
        return Err(Error(
            "proximity needs XYZ positions and nonnegative scalar blend distances".into(),
        ));
    }
    // Only time and fixture axes broadcast here; the site axis becomes channels.
    let clock = Signal::new(
        Array3::zeros((1, pt, 1)),
        Unit::Number,
        Channels::Value,
        None,
    )?;
    let position =
        position
            .zip(&clock, Unit::Number, |v, _| v)?
            .zip(temperature, Unit::Number, |v, _| v)?;
    let (n, t, _) = position.values().dim();
    if n.saturating_mul(t).saturating_mul(count) > 16_777_216 {
        return Err(Error("proximity tensor exceeds 16,777,216 elements".into()));
    }
    let mut weights = Array3::zeros((n, t, count));
    for head in 0..n {
        for time in 0..t {
            let mut minimum = f64::INFINITY;
            for site in 0..count {
                let delta: [f64; 3] = std::array::from_fn(|d| {
                    position.at(head, time, d) - points.at(0, time, site * 3 + d)
                });
                let distance = delta[0].hypot(delta[1]).hypot(delta[2]);
                weights[[head, time, site]] = distance;
                minimum = minimum.min(distance);
            }
            let temperature = temperature.at(head, time, 0);
            let mut sum = 0.;
            for site in 0..count {
                let gap = weights[[head, time, site]] - minimum;
                let weight = if temperature == 0. {
                    if gap == 0. {
                        1.
                    } else {
                        0.
                    }
                } else {
                    (-gap / temperature).exp()
                };
                weights[[head, time, site]] = weight;
                sum += weight;
            }
            for site in 0..count {
                weights[[head, time, site]] /= sum;
            }
        }
    }
    Signal::new(
        weights,
        Unit::Proportion,
        Channels::components(count)?,
        position.fixtures().map(Into::into),
    )
}
