//! Clip input sources. A form input takes a plain value of its type or one of
//! these. A form lowers each source into graph nodes when a clip is prepared.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// The kinds of value a form input accepts besides a plain value. `events`
/// is the stamped event list that `every` accepts in place of beats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Time,
    Hit,
    Noise,
    Audio,
    Events,
}

/// How a keyframe curve moves from one point to the next. `hold` keeps the
/// start value for the whole segment; `step` takes the end value at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Segment {
    Hold,
    #[default]
    Linear,
    Ease,
    Step,
}

/// One keyframe value: a number in the input's own unit, or a color.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Key {
    Number(f64),
    Color([f64; 3]),
}

/// Keyframes over progress 0–1: the clip for `time`, one event's life for
/// `hit`. Before the first point and after the last, the end values hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframes {
    pub points: Vec<(f64, Key)>,
    /// One per adjacent pair of points. Empty means all linear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<Segment>,
}

impl Keyframes {
    pub fn numbers(points: &[[f64; 2]], segments: &[Segment]) -> Self {
        Self {
            points: points.iter().map(|[x, y]| (*x, Key::Number(*y))).collect(),
            segments: segments.to_vec(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=256).contains(&self.points.len()) {
            return Err(Error(format!(
                "a curve has {} points; expected 1–256",
                self.points.len()
            )));
        }
        let color = matches!(self.points[0].1, Key::Color(_));
        for (i, (x, key)) in self.points.iter().enumerate() {
            if !x.is_finite() || !(0.0..=1.0).contains(x) {
                return Err(Error(format!("curve point {i}: x must be in 0..1")));
            }
            if i > 0 && self.points[i - 1].0 > *x {
                return Err(Error(format!(
                    "curve point {i}: x must not precede the previous point"
                )));
            }
            let finite = match key {
                Key::Number(v) => !color && v.is_finite(),
                Key::Color(rgb) => color && rgb.iter().all(|v| v.is_finite()),
            };
            if !finite {
                return Err(Error(format!(
                    "curve point {i}: values must be finite and all numbers or all colors"
                )));
            }
        }
        if !self.segments.is_empty() && self.segments.len() != self.points.len() - 1 {
            return Err(Error(format!(
                "a curve has {} segments; expected {} (one per pair of points)",
                self.segments.len(),
                self.points.len() - 1
            )));
        }
        Ok(())
    }
    pub fn is_color(&self) -> bool {
        matches!(self.points.first(), Some((_, Key::Color(_))))
    }
    /// Every channel value, for range checks.
    pub fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.points.iter().flat_map(|(_, key)| match key {
            Key::Number(v) => vec![*v],
            Key::Color(rgb) => rgb.to_vec(),
        })
    }
    /// The value at `progress`, one entry per channel.
    pub fn sample(&self, progress: f64) -> [f64; 3] {
        let key = |i: usize| match self.points[i].1 {
            Key::Number(v) => [v; 3],
            Key::Color(rgb) => rgb,
        };
        let last = self.points.len() - 1;
        if progress < self.points[0].0 {
            return key(0);
        }
        if progress >= self.points[last].0 {
            return key(last);
        }
        let i = self
            .points
            .partition_point(|(x, _)| *x <= progress)
            .saturating_sub(1);
        let (x0, x1) = (self.points[i].0, self.points[i + 1].0);
        let (a, b) = (key(i), key(i + 1));
        let t = (progress - x0) / (x1 - x0);
        let t = match self.segments.get(i).copied().unwrap_or_default() {
            Segment::Hold => 0.0,
            Segment::Step => 1.0,
            Segment::Linear => t,
            Segment::Ease => t * t * (3.0 - 2.0 * t),
        };
        std::array::from_fn(|ch| a[ch] + (b[ch] - a[ch]) * t)
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

/// A band of the track's full mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    Low,
    Mid,
    High,
    Full,
}
impl Band {
    pub fn hz(self) -> (f64, f64) {
        match self {
            Self::Low => (20.0, 250.0),
            Self::Mid => (250.0, 4000.0),
            Self::High => (4000.0, 16000.0),
            Self::Full => (20.0, 16000.0),
        }
    }
}

/// The energy of one band of the full mix, scaled over the clip so its
/// quietest moment maps to `range[0]` and its loudest to `range[1]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioLevel {
    pub band: Band,
    pub range: [f64; 2],
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
    pub fn validate(&self) -> Result<()> {
        range(self.range)
    }
}
fn range(range: [f64; 2]) -> Result<()> {
    if range.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(Error("a source range must be finite".into()))
    }
}
