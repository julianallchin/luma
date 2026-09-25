//! Clip input sources. A form input takes a plain value of its type or one of
//! these. A form lowers each source into graph nodes when a clip is prepared.
use crate::{Curve, Ease, Error, Result};
use serde::{Deserialize, Serialize};

/// The kinds of value a form input accepts besides a plain value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Time,
    Hit,
    Noise,
    Audio,
}

/// One keyframe value: a number in the input's own unit, or a color.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Key {
    Number(f64),
    Color([f64; 3]),
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> serde::de::Visitor<'de> for KeyVisitor {
            type Value = Key;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a number, or a color [r, g, b]")
            }
            fn visit_f64<E>(self, v: f64) -> std::result::Result<Key, E> {
                Ok(Key::Number(v))
            }
            fn visit_i64<E>(self, v: i64) -> std::result::Result<Key, E> {
                Ok(Key::Number(v as f64))
            }
            fn visit_u64<E>(self, v: u64) -> std::result::Result<Key, E> {
                Ok(Key::Number(v as f64))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> std::result::Result<Key, A::Error> {
                <[f64; 3]>::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))
                    .map(Key::Color)
            }
        }
        deserializer.deserialize_any(KeyVisitor)
    }
}

/// Keyframes over progress 0–1: the clip for `time`, one event's life for
/// `hit`.
pub type Keyframes = Curve<Key>;

impl Curve<Key> {
    /// A number curve through `points` with `eases[i]` from point `i`.
    pub fn numbers(points: &[[f64; 2]], eases: &[Ease]) -> Self {
        Self::with_eases(points.iter().map(|[x, y]| (*x, Key::Number(*y))), eases)
    }
    pub fn validate(&self) -> Result<()> {
        let color = self.is_color();
        self.check(|key| {
            let finite = match key {
                Key::Number(v) => !color && v.is_finite(),
                Key::Color(rgb) => color && rgb.iter().all(|v| v.is_finite()),
            };
            (!finite).then(|| "values must be finite and all numbers or all colors".into())
        })
    }
    pub fn is_color(&self) -> bool {
        matches!(self.points.first(), Some(point) if matches!(point.value, Key::Color(_)))
    }
    /// Every channel value, for range checks. Every ease stays between the
    /// values of its two points, so these bound the curve.
    pub fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.points.iter().flat_map(|point| match point.value {
            Key::Number(v) => vec![v],
            Key::Color(rgb) => rgb.to_vec(),
        })
    }
    /// The value at `progress`, one entry per channel.
    pub fn sample(&self, progress: f64) -> [f64; 3] {
        let key = |i: usize| match self.points[i].value {
            Key::Number(v) => [v; 3],
            Key::Color(rgb) => rgb,
        };
        let (i, share) = self.locate(progress);
        let (a, b) = (key(i), key(i + 1));
        std::array::from_fn(|ch| a[ch] + (b[ch] - a[ch]) * share)
    }
}

/// Smooth random wandering between `range[0]` and `range[1]`. `speed` is the
/// time for one wander, in beats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoiseSource {
    pub speed: f64,
    pub range: [f64; 2],
}

/// The energy of a frequency range of the track's full mix. The energy is
/// scaled over the clip to 0–1. Below `threshold` the level is 0; at or
/// above it the level is `floor + (1 − floor) × energy`. Threshold 0 is no
/// gate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioLevel {
    pub from_hz: f64,
    pub to_hz: f64,
    pub floor: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub threshold: f64,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

impl NoiseSource {
    pub fn validate(&self) -> Result<()> {
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return Err(Error("noise speed must be positive beats".into()));
        }
        range(self.range)
    }
}
impl AudioLevel {
    pub const MIN_HZ: f64 = 20.0;
    pub const MAX_HZ: f64 = 20000.0;
    pub fn validate(&self) -> Result<()> {
        if !(Self::MIN_HZ..=Self::MAX_HZ).contains(&self.from_hz)
            || !(Self::MIN_HZ..=Self::MAX_HZ).contains(&self.to_hz)
            || self.from_hz >= self.to_hz
        {
            return Err(Error(
                "an audio range needs 20 ≤ from < to ≤ 20,000 Hz".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.floor) {
            return Err(Error("an audio floor must be in 0..1".into()));
        }
        if !(0.0..=1.0).contains(&self.threshold) {
            return Err(Error("an audio threshold must be in 0..1".into()));
        }
        Ok(())
    }
}
fn range(range: [f64; 2]) -> Result<()> {
    if range.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(Error("a source range must be finite".into()))
    }
}
