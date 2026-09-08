use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// One segment between anchors. Bézier handles use the envelope's normalized
/// coordinates, just like anchors; x is ordered and y stays within 0..1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnvelopeCurve {
    #[default]
    Linear,
    Bezier {
        control1: [f64; 2],
        control2: [f64; 2],
    },
}

/// Authored anchors and curves. The consumer supplies time or spatial meaning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub points: Vec<[f64; 2]>,
    /// Empty means all straight segments, preserving existing authored history.
    /// Otherwise there is exactly one curve per adjacent pair of anchors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curves: Vec<EnvelopeCurve>,
}

impl Envelope {
    pub fn linear(points: Vec<[f64; 2]>) -> Self {
        Self {
            points,
            curves: Vec::new(),
        }
    }

    pub fn soft_edges(softness: f64) -> Self {
        let edge = softness.clamp(0., 1.) * 0.5;
        Self::linear(if edge <= 0. {
            vec![[0., 1.], [1., 1.]]
        } else if edge >= 0.5 {
            vec![[0., 0.], [0.5, 1.], [1., 0.]]
        } else {
            vec![[0., 0.], [edge, 1.], [1. - edge, 1.], [1., 0.]]
        })
    }

    pub fn validate(&self) -> Result<()> {
        if !(2..=256).contains(&self.points.len()) {
            return Err(Error(format!(
                "envelope.points has {} anchors; expected 2–256",
                self.points.len()
            )));
        }
        for (i, point) in self.points.iter().enumerate() {
            if !normalized(*point) {
                return Err(Error(format!(
                    "envelope.points[{i}] is {point:?}; both coordinates must be finite and in 0..1"
                )));
            }
            if i > 0 && self.points[i - 1][0] >= point[0] {
                return Err(Error(format!(
                    "envelope.points[{i}].x is {}; must be greater than the preceding x ({})",
                    point[0],
                    self.points[i - 1][0]
                )));
            }
        }
        if self.points[0][0] != 0. || self.points.last().unwrap()[0] != 1. {
            return Err(Error(format!(
                "envelope.points must start at x=0 and end at x=1; got {} and {}",
                self.points[0][0],
                self.points.last().unwrap()[0]
            )));
        }
        if !self.curves.is_empty() && self.curves.len() != self.points.len() - 1 {
            return Err(Error(format!(
                "envelope.curves has {} entries; expected {} (one per segment), or omit curves for straight segments",
                self.curves.len(), self.points.len() - 1
            )));
        }
        for (i, curve) in self.curves.iter().enumerate() {
            if let EnvelopeCurve::Bezier { control1, control2 } = curve {
                if !normalized(*control1)
                    || !normalized(*control2)
                    || control1[0] < self.points[i][0]
                    || control1[0] > control2[0]
                    || control2[0] > self.points[i + 1][0]
                {
                    return Err(Error(format!(
                        "envelope.curves[{i}]: Bézier handles {control1:?}, {control2:?} must be normalized and ordered between anchors {:?} and {:?}",
                        self.points[i], self.points[i + 1]
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn curve(&self, segment: usize) -> EnvelopeCurve {
        self.curves.get(segment).copied().unwrap_or_default()
    }

    /// Actual control polygon; straight segments have collinear third handles.
    pub fn controls(&self, segment: usize) -> [[f64; 2]; 4] {
        let a = self.points[segment];
        let b = self.points[segment + 1];
        let (c, d) = match self.curve(segment) {
            EnvelopeCurve::Linear => (lerp(a, b, 1. / 3.), lerp(a, b, 2. / 3.)),
            EnvelopeCurve::Bezier { control1, control2 } => (control1, control2),
        };
        [a, c, d, b]
    }

    pub fn sample(&self, progress: f64) -> f64 {
        if progress <= 0. {
            return self.points[0][1];
        }
        if progress >= 1. {
            return self.points.last().unwrap()[1];
        }
        let i = self
            .points
            .partition_point(|p| p[0] < progress)
            .saturating_sub(1);
        match self.curve(i) {
            EnvelopeCurve::Linear => {
                let a = self.points[i];
                let b = self.points[i + 1];
                lerp(a, b, (progress - a[0]) / (b[0] - a[0]))[1]
            }
            EnvelopeCurve::Bezier { .. } => {
                let controls = self.controls(i);
                at(controls, parameter(controls, progress))[1]
            }
        }
    }

    pub fn set_curve(&mut self, segment: usize, curve: EnvelopeCurve) -> Result<()> {
        self.validate()?;
        if segment >= self.points.len() - 1 {
            return Err(Error("unknown envelope segment".into()));
        }
        let mut next = self.clone();
        next.curves
            .resize(self.points.len() - 1, EnvelopeCurve::Linear);
        next.curves[segment] = curve;
        next.validate()?;
        next.compact();
        *self = next;
        Ok(())
    }

    /// Move an anchor and its attached handles together. Ordering and bounds
    /// belong to this value, so native and programmatic edits obey one rule.
    pub fn move_point(&mut self, index: usize, point: [f64; 2]) -> Result<()> {
        self.validate()?;
        let old = *self
            .points
            .get(index)
            .ok_or_else(|| Error("unknown envelope anchor".into()))?;
        let mut next = self.clone();
        next.points[index] = point;
        Self::linear(next.points.clone()).validate()?;
        for i in index.saturating_sub(1)..=(index.min(self.points.len() - 2)) {
            if let Some(EnvelopeCurve::Bezier { control1, control2 }) = next.curves.get_mut(i) {
                let attached = if i == index {
                    &mut *control1
                } else {
                    &mut *control2
                };
                for axis in 0..2 {
                    attached[axis] = (attached[axis] + point[axis] - old[axis]).clamp(0., 1.);
                }
                let (a, b) = (next.points[i][0], next.points[i + 1][0]);
                if a >= b {
                    return Err(Error("envelope anchors must remain ordered".into()));
                }
                control1[0] = control1[0].clamp(a, b);
                control2[0] = control2[0].clamp(control1[0], b);
            }
        }
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Insert an anchor on the curve using de Casteljau subdivision. Adding a
    /// handle must not change the light or motion before the user moves it.
    pub fn insert_point(&mut self, x: f64) -> Result<usize> {
        self.validate()?;
        if !x.is_finite()
            || x <= 0.
            || x >= 1.
            || self.points.iter().any(|p| (p[0] - x).abs() < 1e-9)
        {
            return Err(Error(
                "new envelope anchor must lie inside a segment".into(),
            ));
        }
        let i = self.points.partition_point(|p| p[0] < x) - 1;
        let mut next = self.clone();
        let [a, b, c, d] = self.controls(i);
        let t = match self.curve(i) {
            EnvelopeCurve::Linear => (x - a[0]) / (d[0] - a[0]),
            _ => parameter([a, b, c, d], x),
        };
        let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
        let (abc, bcd) = (lerp(ab, bc, t), lerp(bc, cd, t));
        next.points.insert(i + 1, lerp(abc, bcd, t));
        next.curves
            .resize(self.points.len() - 1, EnvelopeCurve::Linear);
        let (left, right) = match self.curve(i) {
            EnvelopeCurve::Linear => (EnvelopeCurve::Linear, EnvelopeCurve::Linear),
            _ => (
                EnvelopeCurve::Bezier {
                    control1: ab,
                    control2: abc,
                },
                EnvelopeCurve::Bezier {
                    control1: bcd,
                    control2: cd,
                },
            ),
        };
        next.curves[i] = left;
        next.curves.insert(i + 1, right);
        next.validate()?;
        next.compact();
        *self = next;
        Ok(i + 1)
    }

    pub fn remove_point(&mut self, index: usize) -> Result<()> {
        self.validate()?;
        if index == 0 || index >= self.points.len() - 1 {
            return Err(Error("envelope endpoints cannot be removed".into()));
        }
        let mut next = self.clone();
        let merged = if self.curve(index - 1) == EnvelopeCurve::Linear
            && self.curve(index) == EnvelopeCurve::Linear
        {
            EnvelopeCurve::Linear
        } else {
            EnvelopeCurve::Bezier {
                control1: self.controls(index - 1)[1],
                control2: self.controls(index)[2],
            }
        };
        next.points.remove(index);
        next.curves
            .resize(self.points.len() - 1, EnvelopeCurve::Linear);
        next.curves[index - 1] = merged;
        next.curves.remove(index);
        next.validate()?;
        next.compact();
        *self = next;
        Ok(())
    }

    fn compact(&mut self) {
        if self.curves.iter().all(|c| *c == EnvelopeCurve::Linear) {
            self.curves.clear();
        }
    }
}

fn normalized(p: [f64; 2]) -> bool {
    p.iter().all(|v| v.is_finite() && (0. ..=1.).contains(v))
}
fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]
}
fn at([a, b, c, d]: [[f64; 2]; 4], t: f64) -> [f64; 2] {
    lerp(
        lerp(lerp(a, b, t), lerp(b, c, t), t),
        lerp(lerp(b, c, t), lerp(c, d, t), t),
        t,
    )
}
fn parameter(controls: [[f64; 2]; 4], x: f64) -> f64 {
    if x <= controls[0][0] {
        return 0.;
    }
    if x >= controls[3][0] {
        return 1.;
    }
    let (mut low, mut high) = (0., 1.);
    for _ in 0..40 {
        let mid = (low + high) * 0.5;
        if at(controls, mid)[0] < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) * 0.5
}
