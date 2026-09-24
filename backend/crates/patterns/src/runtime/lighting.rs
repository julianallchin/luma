use super::*;
use std::sync::Arc;

/// Channels per head: RGB, dimmer, pan, tilt, strobe, speed, aim U, V, Z
/// and aim weight.
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
    pub(super) fn terminal(
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
        let get = |key: &str, n, t, ch, default| {
            signals.get(key).map(|v| v.at(n, t, ch)).unwrap_or(default)
        };
        let has_color = signals.contains_key("color");
        let has_aim = signals.contains_key("aim");
        // An aim's length is its weight; its direction is the unit vector.
        let aim = |n, t| {
            let v: [f64; 3] = std::array::from_fn(|ch| get("aim", n, t, ch, 0.0));
            let length = crate::aim::dot(v, v).sqrt();
            (crate::aim::unit(v), length.min(1.0))
        };
        let values = Array3::from_shape_fn((fixtures.len(), times, CHANNELS), |(n, t, ch)| {
            // Brightness is the applied color's peak channel. Master/group
            // intensity is applied by the compositor, so headroom above one
            // survives until then and an overdriven pattern is not flattened.
            let brightness = if has_color {
                (0..3)
                    .map(|ch| get("color", n, t, ch, 0.0).max(0.0))
                    .fold(0.0_f64, f64::max)
            } else {
                0.0
            };
            match ch {
                0..=2 if has_color => {
                    if brightness > 1e-5 {
                        get("color", n, t, ch, 0.0).max(0.0) / brightness
                    } else {
                        0.0
                    }
                }
                0..=2 => 1.0,
                3 => brightness,
                4 => get("pan", n, t, 0, 0.0),
                5 => get("tilt", n, t, 0, 0.0),
                6 => get("strobe", n, t, 0, 0.0).clamp(0.0, 1.0),
                7 => get("speed", n, t, 0, 1.0).clamp(0.0, 1.0),
                8..=10 if has_aim => aim(n, t).0[ch - 8],
                11 if has_aim => aim(n, t).1,
                8..=10 => crate::aim::DOWN[ch - 8],
                _ => 0.0,
            }
        });
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
                _ => v.aim.map_or(0.0, |aim| aim.weight),
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
                    },
                )
            })
            .collect())
    }
}
