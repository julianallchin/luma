//! Clip input sources. A form input takes a plain value of its type or one of
//! these. A form prepares the source tree once for seekable evaluation.
use crate::{Boundary, Curve, Ease, Envelope, Error, Gradient, MappingSpec, Result, Value};
use serde::{Deserialize, Serialize};

/// The kinds of value a form input accepts besides a plain value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Time,
    Random,
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

/// Keyframes over event progress 0–1.
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

/// What a Time source gives over progress 0–1: keyframes, or a
/// gradient read at positions that follow a curve. The gradient blends in
/// OKLab like every gradient; color keyframes blend per channel, in linear
/// Rec. 2020.
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

/// A clock belongs to a source. Omitting it inherits the enclosing clock;
/// an explicit zero period always means the whole clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Events {
    SameAs { same_as: String },
    Own(EventClock),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventClock {
    pub every: Box<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life: Option<Box<Value>>,
}
impl Events {
    pub fn clip() -> Self {
        Self::Own(EventClock {
            every: Box::new(Value::Beats(0.)),
            life: None,
        })
    }
    pub fn repeating(every: Value, life: Option<Value>) -> Self {
        Self::Own(EventClock {
            every: Box::new(every),
            life: life.map(Box::new),
        })
    }
}
fn zero() -> Box<Value> {
    Box::new(Value::Number(0.))
}
fn one() -> Box<Value> {
    Box::new(Value::Number(1.))
}
fn beats() -> Box<Value> {
    Box::new(Value::Beats(4.))
}
fn width() -> Box<Value> {
    Box::new(Value::Number(0.2))
}
fn is_zero(v: &Value) -> bool {
    v.scalar_value() == Some(0.)
}
fn is_one(v: &Value) -> bool {
    v.scalar_value() == Some(1.)
}
fn boundary() -> Boundary {
    Boundary::Clip
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeSource {
    #[serde(flatten)]
    pub curve: SourceCurve,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Events>,
    /// A phase shift in turns. Sources allow a spatial wave or a second axis
    /// following the same clock with a quarter-turn shift.
    #[serde(default = "zero", skip_serializing_if = "is_zero")]
    pub phase: Box<Value>,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub gain: Box<Value>,
}
impl From<SourceCurve> for TimeSource {
    fn from(curve: SourceCurve) -> Self {
        Self {
            curve,
            events: None,
            phase: zero(),
            gain: one(),
        }
    }
}
impl From<Keyframes> for TimeSource {
    fn from(curve: Keyframes) -> Self {
        SourceCurve::Keys(curve).into()
    }
}
impl std::ops::Deref for TimeSource {
    type Target = SourceCurve;
    fn deref(&self) -> &SourceCurve {
        &self.curve
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grain {
    #[default]
    Head,
    Fixture,
    Clump2,
    Clump4,
    Clump8,
}
impl Grain {
    pub fn size(self) -> usize {
        match self {
            Self::Head => 1,
            Self::Fixture => 0,
            Self::Clump2 => 2,
            Self::Clump4 => 4,
            Self::Clump8 => 8,
        }
    }
}

/// A spatial curve, optionally a stroke positioned by another source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceSource {
    pub axis: MappingSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<Gradient>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Envelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<Box<Value>>,
    #[serde(default = "width")]
    pub width: Box<Value>,
    #[serde(default)]
    pub width_relative: bool,
    #[serde(default = "boundary")]
    pub boundary: Boundary,
    #[serde(default)]
    pub grain: Grain,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub gain: Box<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RandomSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Events>,
    #[serde(default)]
    pub grain: Grain,
    pub coverage: Box<Value>,
    #[serde(default = "one")]
    pub level: Box<Value>,
}

/// Smooth noise: no scale means uniform across the selection. A positive
/// scale gives spatial noise. Identity grain gives independent head wandering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoiseSource {
    #[serde(default = "beats")]
    pub speed: Box<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Box<Value>>,
    #[serde(default = "zero")]
    pub contrast: Box<Value>,
    pub range: [Box<Value>; 2],
    #[serde(default)]
    pub grain: Grain,
    /// Independent wandering rather than a continuous spatial field.
    #[serde(default)]
    pub independent: bool,
    /// Stable noise coordinates survive moving a source inside another one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
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
    #[serde(default, skip_serializing_if = "audio_zero")]
    pub threshold: f64,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub gain: Box<Value>,
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
fn audio_zero(v: &f64) -> bool {
    *v == 0.
}

impl TimeSource {
    pub fn validate(&self) -> Result<()> {
        self.curve.validate()
    }
}
impl SpaceSource {
    pub fn validate(&self) -> Result<()> {
        self.axis.validate()?;
        match (&self.gradient, &self.curve) {
            (Some(g), None) => g.validate()?,
            (None, Some(c)) => c.validate()?,
            _ => return Err(Error("Space needs a gradient or curve, not both".into())),
        }
        if !matches!(self.boundary, Boundary::Clip | Boundary::Wrap) {
            return Err(Error("Space boundary must be clip or wrap".into()));
        }
        Ok(())
    }
}
impl NoiseSource {
    pub fn validate(&self) -> Result<()> {
        Ok(())
    }
}
impl RandomSource {
    pub fn validate(&self) -> Result<()> {
        Ok(())
    }
}
