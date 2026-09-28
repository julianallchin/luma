//! Clip input sources. A form input takes a plain value of its type or one of
//! these. A form lowers each source into graph nodes when a clip is prepared.
use crate::{Boundary, Curve, Ease, Envelope, Error, Gradient, MappingSpec, Result, Value};
use serde::{Deserialize, Serialize};

/// The kinds of value a form input accepts besides a plain value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Time,
    Hit,
    Noise,
    Audio,
    Space,
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

/// What a `time` or `hit` source gives over progress 0–1: keyframes, or a
/// gradient read at positions that follow a curve. The gradient blends in
/// OKLab like every gradient; color keyframes blend in RGB.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SourceCurve {
    /// `{"points": [...]}`: numbers in the input's unit, or colors.
    Keys(Keyframes),
    /// `{"gradient": {...}, "curve": {...}}`: the curve gives the gradient
    /// position, 0–1, at each progress. Color inputs only.
    Gradient(GradientCurve),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradientCurve {
    pub gradient: Gradient,
    pub curve: Envelope,
}

impl<'de> Deserialize<'de> for SourceCurve {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        // Pick the variant by its keys, so a malformed curve keeps the
        // curve's own error.
        let raw = serde_json::Value::deserialize(deserializer)?;
        if raw.get("gradient").is_some() {
            GradientCurve::deserialize(raw)
                .map(Self::Gradient)
                .map_err(D::Error::custom)
        } else {
            Keyframes::deserialize(raw)
                .map(Self::Keys)
                .map_err(D::Error::custom)
        }
    }
}

impl From<Keyframes> for SourceCurve {
    fn from(keys: Keyframes) -> Self {
        Self::Keys(keys)
    }
}

impl SourceCurve {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Keys(keys) => keys.validate(),
            Self::Gradient(GradientCurve { gradient, curve }) => {
                gradient.validate()?;
                curve.validate()
            }
        }
    }
    pub fn is_color(&self) -> bool {
        match self {
            Self::Keys(keys) => keys.is_color(),
            Self::Gradient(_) => true,
        }
    }
    /// The keyframes, when the source has them.
    pub fn keys(&self) -> Option<&Keyframes> {
        match self {
            Self::Keys(keys) => Some(keys),
            Self::Gradient(_) => None,
        }
    }
    /// Every channel value the source can give, for range checks.
    pub fn values(&self) -> Box<dyn Iterator<Item = f64> + '_> {
        match self {
            Self::Keys(keys) => Box::new(keys.values()),
            Self::Gradient(GradientCurve { gradient, .. }) => {
                Box::new(gradient.stops.iter().flat_map(|stop| stop.color))
            }
        }
    }
}

/// Values laid out along an axis of the heads. The axis gives each head a
/// position 0–1; `gradient` (a color input) or `curve` (a number input)
/// gives the value at that position. With `move`, the values are a stroke
/// that travels along the axis once per hit: they run across the stroke from
/// its tail (0) to its head (1), and the heads outside it get 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceSource {
    pub axis: MappingSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<Gradient>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Envelope>,
    #[serde(rename = "move", default, skip_serializing_if = "Option::is_none")]
    pub movement: Option<Box<Movement>>,
}

/// How a stroke travels along the axis. Each hit of the form's `every`
/// starts one stroke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Movement {
    /// Where the stroke is over its life: 0 is the axis start, 1 its end.
    pub path: Envelope,
    /// Beats for one stroke to cross the whole axis; 0 is the whole clip. A
    /// `beats` value or a `time` curve of beats.
    pub travel: Value,
    /// Stroke size: a share of the axis, or of the gap between strokes. A
    /// `number` value, or a `time` or `hit` curve of numbers.
    pub width: Value,
    /// Width is a share of the gap between strokes; otherwise of the axis.
    pub width_relative: bool,
    /// What happens at the ends of the axis: `clip` or `wrap`.
    pub boundary: Boundary,
}

impl SpaceSource {
    pub fn validate(&self) -> Result<()> {
        self.axis.validate()?;
        if self.axis.reverse {
            return Err(Error(
                "an axis has no reverse; choose a backward path".into(),
            ));
        }
        if self.axis.per_group {
            return Err(Error(
                "an axis has no per_group; choose the group span".into(),
            ));
        }
        match (&self.gradient, &self.curve) {
            (Some(gradient), None) => gradient.validate()?,
            (None, Some(curve)) => curve.validate()?,
            _ => {
                return Err(Error(
                    "a space source needs a gradient (color) or a curve (number), not both".into(),
                ))
            }
        }
        if let Some(movement) = &self.movement {
            movement.validate()?;
        }
        Ok(())
    }
}

impl Movement {
    pub fn validate(&self) -> Result<()> {
        self.path
            .validate()
            .map_err(|e| Error(format!("move.path: {e}")))?;
        if !matches!(self.boundary, Boundary::Clip | Boundary::Wrap) {
            return Err(Error("move.boundary must be clip or wrap".into()));
        }
        // The form checks the ranges, as it checked the chase's inputs.
        self.travel
            .validate()
            .map_err(|e| Error(format!("move.travel: {e}")))?;
        self.width
            .validate()
            .map_err(|e| Error(format!("move.width: {e}")))?;
        Ok(())
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
