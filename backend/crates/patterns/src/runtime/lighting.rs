use super::*;
use std::sync::Arc;

/// Channels per head: RGB, dimmer, pan, tilt, strobe, speed, aim U, V, Z
/// and alpha: the aim's weight for aim, the clip's opacity otherwise.
const CHANNELS: usize = 12;

/// Internal capability bundle at the output boundary. Numerical graph wires
/// remain Signals; missing capabilities stay distinct from explicit zeros.
#[derive(Clone, Debug)]
pub struct LightingSignal {
    values: Arc<Array3<f64>>,
    fixtures: Arc<[String]>,
    writes: [bool; 6],
}
impl LightingSignal {
    pub(crate) fn terminal(
        inputs: &BTreeMap<String, EvaluatedValue>,
        fixtures: &[String],
    ) -> Result<Self> {
        let signals = inputs
            .iter()
            .map(|(key, value)| Ok((key.as_str(), value.numeric().on_fixtures(fixtures)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut times = 1;
        for signal in signals.values() {
            let t = signal.values().dim().1;
            if times == 1 {
                times = t;
            } else if t != 1 && t != times {
                return Err(Error("Output time axes differ".into()));
            }
        }
        // Each input is looked up once here, not once per sample.
        let [color, pan, tilt, strobe, speed, aim, alpha] =
            ["color", "pan", "tilt", "strobe", "speed", "aim", "alpha"].map(|key| signals.get(key));
        let has_color = color.is_some();
        let has_aim = aim.is_some();
        let mut values = Array3::zeros((fixtures.len(), times, CHANNELS));
        for ((n, t), mut row) in values
            .lanes_mut(ndarray::Axis(2))
            .into_iter()
            .enumerate()
            .map(|(i, row)| ((i / times, i % times), row))
        {
            let scalar =
                |signal: Option<&Signal>, default: f64| signal.map_or(default, |v| v.at(n, t, 0));
            // Brightness is the applied color's peak channel. Master/group
            // intensity is applied by the compositor, so headroom above one
            // survives until then and an overdriven pattern is not flattened.
            let rgb = match color {
                Some(color) => {
                    let rgb: [f64; 3] = std::array::from_fn(|ch| color.at(n, t, ch).max(0.0));
                    let brightness = rgb.into_iter().fold(0.0_f64, f64::max);
                    if brightness > 1e-5 {
                        [
                            rgb[0] / brightness,
                            rgb[1] / brightness,
                            rgb[2] / brightness,
                            brightness,
                        ]
                    } else {
                        [0.0, 0.0, 0.0, brightness]
                    }
                }
                None => [1.0, 1.0, 1.0, 0.0],
            };
            // An aim's length is its weight; its direction is the unit
            // vector. Without one, the last channel is the clip's opacity.
            let (direction, last) = match aim {
                Some(aim) => {
                    let v: [f64; 3] = std::array::from_fn(|ch| aim.at(n, t, ch));
                    (crate::aim::unit(v), crate::aim::dot(v, v).sqrt().min(1.0))
                }
                None => (crate::aim::DOWN, scalar(alpha, 1.0).clamp(0.0, 1.0)),
            };
            let sample: [f64; CHANNELS] = [
                rgb[0],
                rgb[1],
                rgb[2],
                rgb[3],
                scalar(pan, 0.0),
                scalar(tilt, 0.0),
                scalar(strobe, 0.0).clamp(0.0, 1.0),
                scalar(speed, 1.0).clamp(0.0, 1.0),
                direction[0],
                direction[1],
                direction[2],
                last,
            ];
            row.iter_mut()
                .zip(sample)
                .for_each(|(slot, value)| *slot = value);
        }
        Ok(Self {
            values: values.into(),
            fixtures: fixtures.to_vec().into(),
            writes: [
                has_color,
                has_color,
                signals.contains_key("pan") || signals.contains_key("tilt"),
                signals.contains_key("strobe"),
                signals.contains_key("speed"),
                has_aim,
            ],
        })
    }
    /// Fixture × time × capability samples in RGB, dimmer, pan, tilt, strobe,
    /// speed, aim U, V, Z, aim weight order. A singleton time axis broadcasts over the requested batch.
    pub fn values(&self) -> &Array3<f64> {
        &self.values
    }
    pub fn fixtures(&self) -> &[String] {
        &self.fixtures
    }
    /// Color, dimmer, position, strobe, speed and aim.
    pub fn writes(&self) -> [bool; 6] {
        self.writes
    }
    fn at(&self, n: usize, t: usize, ch: usize) -> f64 {
        self.values[[n, if self.values.dim().1 == 1 { 0 } else { t }, ch]]
    }
    pub(super) fn literal(values: &BTreeMap<String, FixtureOutput>) -> Result<Self> {
        Value::Lighting(values.clone()).validate()?;
        let fixtures = values.keys().cloned().collect::<Vec<_>>().into();
        let outputs: Vec<_> = values.values().collect();
        let writes = outputs.first().map(|v| v.writes()).unwrap_or([false; 6]);
        let values = Array3::from_shape_fn((outputs.len(), 1, CHANNELS), |(n, _, ch)| {
            let v = outputs[n];
            match ch {
                0..=2 => v.color.unwrap_or([1.0; 3])[ch],
                3 => v.dimmer.unwrap_or(0.0),
                4..=5 => v.position.unwrap_or([0.0; 2])[ch - 4],
                6 => v.strobe.unwrap_or(0.0),
                7 => v.speed.unwrap_or(1.0),
                8..=10 => v.aim.map_or(crate::aim::DOWN, |aim| aim.direction)[ch - 8],
                _ => v.aim.map_or(v.alpha.unwrap_or(1.0), |aim| aim.weight),
            }
        });
        Ok(Self {
            values: values.into(),
            fixtures,
            writes,
        })
    }
    pub fn sample(&self, time: usize) -> Result<BTreeMap<String, FixtureOutput>> {
        if self.values.dim().1 != 1 && time >= self.values.dim().1 {
            return Err(Error("output sample outside batch".into()));
        }
        Ok(self
            .fixtures
            .iter()
            .enumerate()
            .map(|(n, id)| {
                (
                    id.clone(),
                    FixtureOutput {
                        color: self.writes[0].then(|| std::array::from_fn(|c| self.at(n, time, c))),
                        dimmer: self.writes[1].then(|| self.at(n, time, 3)),
                        position: self.writes[2]
                            .then(|| std::array::from_fn(|c| self.at(n, time, c + 4))),
                        strobe: self.writes[3].then(|| self.at(n, time, 6)),
                        speed: self.writes[4].then(|| self.at(n, time, 7)),
                        aim: self.writes[5].then(|| crate::Aim {
                            direction: std::array::from_fn(|c| self.at(n, time, c + 8)),
                            weight: self.at(n, time, 11),
                        }),
                        alpha: (!self.writes[5] && (self.writes[0] || self.writes[3]))
                            .then(|| self.at(n, time, 11)),
                    },
                )
            })
            .collect())
    }
}
