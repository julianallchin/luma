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
/// `bezier` is a cubic through the two points with two handles, as in an
/// `Envelope`: each handle is `[progress, value]` in the curve's own units,
/// with progress ordered between the segment's points. It stores as
/// `{"bezier": {"control1": [x, y], "control2": [x, y]}}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Segment {
    Hold,
    #[default]
    Linear,
    Step,
    Bezier {
        control1: [f64; 2],
        control2: [f64; 2],
    },
}

impl Segment {
    /// The smooth ease between `a` and `b`: handles a third of the way along
    /// at the start and end values. It draws `3t² − 2t³`.
    pub fn ease(a: [f64; 2], b: [f64; 2]) -> Self {
        let third = (b[0] - a[0]) / 3.0;
        Self::Bezier {
            control1: [a[0] + third, a[1]],
            control2: [a[0] + 2.0 * third, b[1]],
        }
    }
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
        for (i, segment) in self.segments.iter().enumerate() {
            let Segment::Bezier { control1, control2 } = segment else {
                continue;
            };
            let (x0, x1) = (self.points[i].0, self.points[i + 1].0);
            if color
                || control1.iter().chain(control2).any(|v| !v.is_finite())
                || control1[0] < x0
                || control1[0] > control2[0]
                || control2[0] > x1
            {
                return Err(Error(format!(
                    "curve segment {i}: Bézier handles need finite values, x ordered between the points, and a number curve"
                )));
            }
        }
        Ok(())
    }
    pub fn is_color(&self) -> bool {
        matches!(self.points.first(), Some((_, Key::Color(_))))
    }
    /// Every channel value and Bézier handle value, for range checks. A
    /// Bézier stays within its points and handles, so these bound the curve.
    pub fn values(&self) -> impl Iterator<Item = f64> + '_ {
        let handles = self.segments.iter().flat_map(|segment| match segment {
            Segment::Bezier { control1, control2 } => vec![control1[1], control2[1]],
            _ => Vec::new(),
        });
        self.points
            .iter()
            .flat_map(|(_, key)| match key {
                Key::Number(v) => vec![*v],
                Key::Color(rgb) => rgb.to_vec(),
            })
            .chain(handles)
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
            // The same evaluation as an Envelope's Bézier segment.
            Segment::Bezier { control1, control2 } => {
                let controls = [[x0, a[0]], control1, control2, [x1, b[0]]];
                let at =
                    crate::envelope::at(controls, crate::envelope::parameter(controls, progress));
                return [at[1]; 3];
            }
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
