use crate::curve::{lerp, parameter};
use crate::{Curve, CurvePoint, Ease, Error, Result};

/// An authored curve with values 0..1. The consumer supplies time or
/// spatial meaning.
pub type Envelope = Curve<f64>;

impl Curve<f64> {
    pub fn linear(points: Vec<[f64; 2]>) -> Self {
        Self::eased(points, &[])
    }

    /// A curve through `points` with `eases[i]` from point `i`.
    pub fn eased(points: Vec<[f64; 2]>, eases: &[Ease]) -> Self {
        Self::with_eases(points.into_iter().map(|[x, y]| (x, y)), eases)
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
        self.check(|v| {
            (!v.is_finite() || !(0. ..=1.).contains(v))
                .then(|| format!("an envelope value must be in 0..1, not {v}"))
        })
    }

    /// Point `i` as `[x, y]`.
    pub fn point(&self, i: usize) -> [f64; 2] {
        [self.points[i].x, self.points[i].value]
    }

    /// The segment's control polygon in the envelope's own coordinates. A
    /// straight or held segment has its handles on the line, at the thirds.
    pub fn controls(&self, segment: usize) -> [[f64; 2]; 4] {
        let (a, b) = (self.point(segment), self.point(segment + 1));
        let [x1, y1, x2, y2] = self
            .ease(segment)
            .handles()
            .unwrap_or(Ease::Linear.handles().unwrap());
        let place = |x: f64, y: f64| [a[0] + x * (b[0] - a[0]), a[1] + y * (b[1] - a[1])];
        [a, place(x1, y1), place(x2, y2), b]
    }

    /// The value at `progress`. At a jump `progress` reads the value after
    /// it, as a shader's `step`. Between points a Bézier ease may overshoot
    /// the two values.
    pub fn sample(&self, progress: f64) -> f64 {
        let (i, share) = self.locate(progress);
        let (a, b) = (self.points[i].value, self.points[i + 1].value);
        a + (b - a) * share
    }

    pub fn set_ease(&mut self, segment: usize, ease: Ease) -> Result<()> {
        self.validate()?;
        if segment >= self.points.len() - 1 {
            return Err(Error("unknown envelope segment".into()));
        }
        let mut next = self.clone();
        next.points[segment].ease = ease;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Set a segment's handles from two points in the envelope's own
    /// coordinates, kept inside the segment's box. A flat segment keeps
    /// its handles' heights.
    pub fn set_handles(&mut self, segment: usize, c1: [f64; 2], c2: [f64; 2]) -> Result<()> {
        if segment + 1 >= self.points.len() {
            return Err(Error("unknown envelope segment".into()));
        }
        let (a, b) = (self.point(segment), self.point(segment + 1));
        let old = self
            .ease(segment)
            .handles()
            .unwrap_or(Ease::Linear.handles().unwrap());
        let [x1, y1] = local(a, b, c1, [old[0], old[1]]);
        let [x2, y2] = local(a, b, c2, [old[2], old[3]]);
        self.set_ease(segment, Ease::Bezier([x1, y1, x2, y2]))
    }

    /// Move a point. Its eases are local to its segments, so they keep their
    /// shape. Ordering and bounds belong to this value, so native and
    /// programmatic edits obey one rule.
    pub fn move_point(&mut self, index: usize, point: [f64; 2]) -> Result<()> {
        self.validate()?;
        if index >= self.points.len() {
            return Err(Error("unknown envelope anchor".into()));
        }
        let mut next = self.clone();
        next.points[index].x = point[0];
        next.points[index].value = point[1];
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
            || self.points.iter().any(|p| (p.x - x).abs() < 1e-9)
        {
            return Err(Error(
                "new envelope anchor must lie inside a segment".into(),
            ));
        }
        let i = self.points.partition_point(|p| p.x < x) - 1;
        let ease = self.ease(i);
        let (point, left, right) = match ease {
            Ease::Linear => ([x, self.sample(x)], ease, ease),
            // A held segment keeps its value on both sides of the cut.
            Ease::Hold => ([x, self.points[i].value], ease, ease),
            _ => {
                let [a, b, c, d] = self.controls(i);
                let t = parameter([a, b, c, d], x);
                let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
                let (abc, bcd) = (lerp(ab, bc, t), lerp(bc, cd, t));
                let m = lerp(abc, bcd, t);
                let half = |from, to, c1, c2| {
                    let [x1, y1] = local(from, to, c1, [1. / 3., 1. / 3.]);
                    let [x2, y2] = local(from, to, c2, [2. / 3., 2. / 3.]);
                    Ease::Bezier([x1, y1, x2, y2])
                };
                (m, half(a, m, ab, abc), half(m, d, bcd, cd))
            }
        };
        let mut next = self.clone();
        next.points[i].ease = left;
        next.points
            .insert(i + 1, CurvePoint::eased(point[0], point[1], right));
        next.validate()?;
        *self = next;
        Ok(i + 1)
    }

    pub fn remove_point(&mut self, index: usize) -> Result<()> {
        self.validate()?;
        if index == 0 || index >= self.points.len() - 1 {
            return Err(Error("envelope endpoints cannot be removed".into()));
        }
        let merged = match (self.ease(index - 1), self.ease(index)) {
            (left, right) if left == right && matches!(left, Ease::Linear | Ease::Hold) => left,
            (Ease::Hold, _) | (_, Ease::Hold) => Ease::Linear,
            _ => {
                // The outer handles of the two segments shape the one left.
                let (a, b) = (self.point(index - 1), self.point(index + 1));
                let [x1, y1] = local(a, b, self.controls(index - 1)[1], [1. / 3., 1. / 3.]);
                let [x2, y2] = local(a, b, self.controls(index)[2], [2. / 3., 2. / 3.]);
                Ease::Bezier([x1, y1, x2, y2])
            }
        };
        let mut next = self.clone();
        next.points.remove(index);
        next.points[index - 1].ease = merged;
        next.validate()?;
        *self = next;
        Ok(())
    }
}

/// `p` as a share of the box from `a` to `b`: x kept in 0..1, so the
/// segment's x only grows; y any share, so a handle may overshoot (CSS
/// `cubic-bezier` allows it). A flat box has no share of height, so
/// `fallback` keeps its height.
fn local(a: [f64; 2], b: [f64; 2], p: [f64; 2], fallback: [f64; 2]) -> [f64; 2] {
    let share = |from: f64, to: f64, v: f64, fallback: f64| {
        if (to - from).abs() > 1e-12 {
            (v - from) / (to - from)
        } else {
            fallback
        }
    };
    [
        share(a[0], b[0], p[0], fallback[0]).clamp(0., 1.),
        share(a[1], b[1], p[1], fallback[1]),
    ]
}
