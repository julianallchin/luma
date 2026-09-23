//! Curve conversions: authored envelopes of the old graphs into the keyframes
//! that form sources store, and the reshaping a form input needs (a path
//! moved onto another range, a stroke shape seen from its other end, a curve
//! cut where a clip now starts).
use luma_patterns::{Envelope, EnvelopeCurve, Key, Keyframes, Segment};

/// Keyframes hold at most this many points.
const MAX_POINTS: usize = 256;

/// `envelope` with every value and handle times `scale`, as keyframes. Each
/// segment keeps its kind, a Bézier its handles, so the curve is exact.
pub(crate) fn keyframes(envelope: &Envelope, scale: f64) -> (Keyframes, bool) {
    let scaled = |p: [f64; 2]| [p[0], p[1] * scale];
    let curve = Keyframes {
        points: envelope
            .points
            .iter()
            .map(|p| (p[0], Key::Number(p[1] * scale)))
            .collect(),
        segments: (0..envelope.points.len() - 1)
            .map(|i| match envelope.curve(i) {
                EnvelopeCurve::Linear => Segment::Linear,
                EnvelopeCurve::Hold => Segment::Hold,
                EnvelopeCurve::Step => Segment::Step,
                EnvelopeCurve::Bezier { control1, control2 } => Segment::Bezier {
                    control1: scaled(control1),
                    control2: scaled(control2),
                },
            })
            .collect(),
    };
    (curve, true)
}

/// A straight-line curve through `sample` at the `breaks` and evenly between
/// them, within the point budget.
pub(crate) fn resample(sample: impl Fn(f64) -> f64, breaks: &[f64]) -> Keyframes {
    let mut xs: Vec<f64> = breaks
        .iter()
        .copied()
        .filter(|x| (0.0..=1.0).contains(x))
        .collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    if xs.len() > MAX_POINTS - 2 {
        xs.clear();
    }
    let even = MAX_POINTS - 1 - xs.len();
    xs.extend((0..=even).map(|i| i as f64 / even as f64));
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    Keyframes {
        points: xs
            .into_iter()
            .map(|x| (x, Key::Number(sample(x))))
            .collect(),
        segments: Vec::new(),
    }
}

/// Every anchor x of the envelopes.
pub(crate) fn breaks(envelopes: &[&Envelope]) -> Vec<f64> {
    envelopes
        .iter()
        .flat_map(|envelope| envelope.points.iter().map(|p| p[0]))
        .collect()
}

/// `a + b·y` for every value and handle of `envelope`.
pub(crate) fn affine(envelope: &Envelope, a: f64, b: f64) -> Envelope {
    let map = |p: [f64; 2]| [p[0], a + b * p[1]];
    Envelope {
        points: envelope.points.iter().copied().map(map).collect(),
        curves: envelope
            .curves
            .iter()
            .map(|curve| match *curve {
                EnvelopeCurve::Bezier { control1, control2 } => EnvelopeCurve::Bezier {
                    control1: map(control1),
                    control2: map(control2),
                },
                other => other,
            })
            .collect(),
    }
}

/// The envelope read from its other end: `x ↦ 1 − x`. A hold becomes a step,
/// since both keep the value of the side that is now on the other end.
pub(crate) fn mirror(envelope: &Envelope) -> Envelope {
    let flip = |p: [f64; 2]| [1.0 - p[0], p[1]];
    let segments = envelope.points.len() - 1;
    Envelope {
        points: envelope.points.iter().rev().copied().map(flip).collect(),
        curves: (0..segments)
            .rev()
            .filter(|_| !envelope.curves.is_empty())
            .map(|i| match envelope.curve(i) {
                EnvelopeCurve::Bezier { control1, control2 } => EnvelopeCurve::Bezier {
                    control1: flip(control2),
                    control2: flip(control1),
                },
                EnvelopeCurve::Hold => EnvelopeCurve::Step,
                EnvelopeCurve::Step => EnvelopeCurve::Hold,
                EnvelopeCurve::Linear => EnvelopeCurve::Linear,
            })
            .collect(),
    }
}

/// The same brightness seen from either end.
pub(crate) fn symmetric(envelope: &Envelope) -> bool {
    (0..=64).all(|i| {
        let x = i as f64 / 64.0;
        (envelope.sample(x) - envelope.sample(1.0 - x)).abs() < 1e-9
    })
}

/// +1 when the envelope never falls, −1 when it never rises, 0 when it does
/// both, measured on a fine grid.
pub(crate) fn direction(envelope: &Envelope) -> i8 {
    let values: Vec<f64> = (0..=512)
        .map(|i| envelope.sample(i as f64 / 512.0))
        .collect();
    let rises = values.windows(2).any(|w| w[1] > w[0] + 1e-12);
    let falls = values.windows(2).any(|w| w[1] < w[0] - 1e-12);
    match (rises, falls) {
        (_, false) => 1,
        (false, true) => -1,
        (true, true) => 0,
    }
}

/// True when no segment holds or steps: the chase form then lets the stroke
/// run past both ends of the axis.
pub(crate) fn glides(envelope: &Envelope) -> bool {
    envelope
        .curves
        .iter()
        .all(|curve| !matches!(curve, EnvelopeCurve::Hold | EnvelopeCurve::Step))
}

/// Constant at `value` everywhere.
pub(crate) fn constant(envelope: &Envelope) -> Option<f64> {
    let first = envelope.points[0][1];
    let flat = envelope.points.iter().all(|p| (p[1] - first).abs() < 1e-12)
        && envelope.curves.iter().all(|curve| match curve {
            EnvelopeCurve::Bezier { control1, control2 } => {
                (control1[1] - first).abs() < 1e-12 && (control2[1] - first).abs() < 1e-12
            }
            _ => true,
        });
    flat.then_some(first)
}

/// `curve` read from progress `from` to 1, stretched over 0–1. A negative
/// `from` starts earlier: the curve holds its first value until then. It is
/// exact for linear, hold, step and Bézier segments; `false` when the cut
/// falls inside a Bézier segment, which is then approximated.
pub(crate) fn crop(curve: &Keyframes, from: f64) -> (Keyframes, bool) {
    let span = 1.0 - from;
    let at = |x: f64| (x - from) / span;
    // A Bézier's handles move along x with its points.
    let moved = |segment: Segment| match segment {
        Segment::Bezier { control1, control2 } => Segment::Bezier {
            control1: [at(control1[0]), control1[1]],
            control2: [at(control2[0]), control2[1]],
        },
        other => other,
    };
    if from.abs() < 1e-12 {
        return (curve.clone(), true);
    }
    if from < 0.0 {
        let mut points = vec![(0.0, curve.points[0].1)];
        points.extend(curve.points.iter().map(|(x, key)| (at(*x), *key)));
        let mut segments = vec![Segment::Hold];
        segments.extend(
            (0..curve.points.len() - 1)
                .map(|i| moved(curve.segments.get(i).copied().unwrap_or_default())),
        );
        return (Keyframes { points, segments }, true);
    }
    let segment = |i: usize| curve.segments.get(i).copied().unwrap_or_default();
    let cut = curve.points.partition_point(|(x, _)| *x <= from);
    if cut == curve.points.len() {
        let value = curve.sample(from)[0];
        return (Keyframes::numbers(&[[0.0, value], [1.0, value]], &[]), true);
    }
    // The segment the cut falls in, if any, keeps its kind from the cut on.
    let inside = cut.checked_sub(1).map(segment);
    if matches!(inside, Some(Segment::Bezier { .. })) {
        return (resample(|x| curve.sample(from + x * span)[0], &[]), false);
    }
    let start = match (inside, cut) {
        (Some(Segment::Step), i) => curve.points[i].1,
        (Some(Segment::Hold), i) => curve.points[i - 1].1,
        _ => Key::Number(curve.sample(from)[0]),
    };
    let mut points = vec![(0.0, start)];
    let mut segments = vec![inside.unwrap_or(Segment::Hold)];
    for (i, (x, key)) in curve.points.iter().enumerate().skip(cut) {
        points.push((at(*x), *key));
        if i + 1 < curve.points.len() {
            segments.push(moved(segment(i)));
        }
    }
    (Keyframes { points, segments }, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(points: &[[f64; 2]], curves: &[EnvelopeCurve]) -> Envelope {
        Envelope {
            points: points.to_vec(),
            curves: curves.to_vec(),
        }
    }

    #[test]
    fn a_mirrored_hold_is_a_step_and_reads_the_same_values() {
        let stepped = envelope(
            &[[0.0, 0.2], [0.5, 0.8], [1.0, 0.8]],
            &[EnvelopeCurve::Hold, EnvelopeCurve::Linear],
        );
        let mirrored = mirror(&stepped);
        for x in [0.1, 0.3, 0.45, 0.55, 0.9] {
            assert!(
                (mirrored.sample(1.0 - x) - stepped.sample(x)).abs() < 1e-12,
                "{x}"
            );
        }
    }

    #[test]
    fn a_bezier_keeps_its_handles_and_samples_the_same() {
        let drawn = envelope(
            &[[0.0, 0.0], [0.6, 1.0], [1.0, 1.0]],
            &[
                EnvelopeCurve::Bezier {
                    control1: [0.1, 0.7],
                    control2: [0.5, 0.2],
                },
                EnvelopeCurve::Linear,
            ],
        );
        let (curve, exact) = keyframes(&drawn, 0.5);
        assert!(exact);
        assert_eq!(
            curve.segments[0],
            Segment::Bezier {
                control1: [0.1, 0.35],
                control2: [0.5, 0.1]
            }
        );
        for i in 0..=50 {
            let x = f64::from(i) / 50.0;
            assert!(
                (curve.sample(x)[0] - drawn.sample(x) * 0.5).abs() < 1e-12,
                "{x}"
            );
        }
        // Cropped where the Bézier has ended, it keeps its values.
        let (late, exact) = crop(&curve, -0.25);
        assert!(exact);
        late.validate().unwrap();
        for x in [0.3, 0.5, 0.9] {
            let old = curve.sample(-0.25 + x * 1.25)[0];
            assert!((late.sample(x)[0] - old).abs() < 1e-9, "{x}");
        }
    }

    #[test]
    fn a_curve_started_earlier_holds_its_first_value() {
        let curve = Keyframes::numbers(&[[0.0, 0.2], [1.0, 1.0]], &[Segment::Linear]);
        let (early, exact) = crop(&curve, -0.25);
        assert!(exact);
        assert!((early.sample(0.1)[0] - 0.2).abs() < 1e-12);
        for x in [0.3, 0.6, 1.0] {
            let old = curve.sample(-0.25 + x * 1.25)[0];
            assert!((early.sample(x)[0] - old).abs() < 1e-12, "{x}");
        }
    }

    #[test]
    fn a_cropped_curve_keeps_its_values_after_the_cut() {
        let curve = Keyframes::numbers(
            &[[0.0, 0.0], [0.5, 1.0], [0.75, 0.25], [1.0, 0.25]],
            &[Segment::Linear, Segment::Hold, Segment::Linear],
        );
        for from in [0.1, 0.5, 0.6] {
            let (cropped, exact) = crop(&curve, from);
            assert!(exact);
            for i in 0..=20 {
                let x = i as f64 / 20.0;
                let old = curve.sample(from + x * (1.0 - from))[0];
                assert!((cropped.sample(x)[0] - old).abs() < 1e-12, "{from} {x}");
            }
        }
    }
}
