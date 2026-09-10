//! One-time conversion of saved stage controls to the ordinary editable curve.
//! No stage-specific evaluator remains in the resulting graph.
use super::{parameter, NodeInstance};
use luma_patterns::{Envelope, EnvelopeCurve};

pub(super) fn convert(node: &NodeInstance) -> Result<Envelope, String> {
    let weights = [
        ("attack", 0.3),
        ("decay", 0.2),
        ("sustain", 0.3),
        ("release", 0.2),
    ]
    .map(|(id, default)| parameter(node, id, default).clamp(0., 1.));
    let total: f64 = weights.iter().sum();
    if total < 1e-6 {
        return Ok(Envelope::linear(vec![[0., 0.], [1., 0.]]));
    }
    let [attack, decay, sustain, _release] = weights.map(|w| w / total);
    let level = parameter(node, "sustain_level", 0.7);
    let low = level.min(0.);
    let range = level.max(1.) - low;
    let normalize = |y: f64| ((y - low) / range).clamp(0., 1.);
    let mut shape = Envelope::linear(vec![[0., normalize(0.)]]);
    for (start, end, from, to, curve) in [
        (0., attack, 0., 1., parameter(node, "attack_curve", 0.)),
        (
            attack,
            attack + decay,
            1.,
            level,
            -parameter(node, "decay_curve", 0.),
        ),
        (attack + decay, attack + decay + sustain, level, level, 0.),
        (attack + decay + sustain, 1., level, 0., 0.),
    ] {
        let start = start.clamp(0., 1.);
        let end = end.clamp(start, 1.);
        if curve.abs() < 0.001 || start == end || from == to {
            shape.points.push([end, normalize(to)]);
            shape.curves.push(EnvelopeCurve::Linear);
            continue;
        }
        let exponent = 1. + 5. * curve.abs();
        let sample = |x: f64| {
            let (y, slope) = if curve > 0. {
                (x.powf(exponent), exponent * x.powf(exponent - 1.))
            } else {
                (
                    1. - (1. - x).powf(exponent),
                    exponent * (1. - x).powf(exponent - 1.),
                )
            };
            (
                (from + (to - from) * y - low) / range,
                (to - from) * slope / range,
            )
        };
        let position = |x: f64| start + (end - start) * x;
        fit(&mut shape, &sample, &position, 0., 1., 0)?;
    }
    if shape.curves.iter().all(|c| *c == EnvelopeCurve::Linear) {
        shape.curves.clear();
    }
    shape.validate().map_err(|e| e.to_string())?;
    Ok(shape)
}

fn fit(
    shape: &mut Envelope,
    sample: &impl Fn(f64) -> (f64, f64),
    position: &impl Fn(f64) -> f64,
    a: f64,
    b: f64,
    depth: usize,
) -> Result<(), String> {
    let (ya, da) = sample(a);
    let (yb, db) = sample(b);
    let h = b - a;
    let c = (ya + da * h / 3.).clamp(ya.min(yb), ya.max(yb));
    let d = (yb - db * h / 3.).clamp(ya.min(yb), ya.max(yb));
    let accurate = (1..8).all(|i| {
        let t = i as f64 / 8.;
        let u = 1. - t;
        let value = u * u * u * ya + 3. * u * u * t * c + 3. * u * t * t * d + t * t * t * yb;
        (value - sample(a + h * t).0).abs() <= 1e-6
    });
    if !accurate {
        if depth >= 24 {
            return Err("saved envelope curvature exceeds editable curve precision".into());
        }
        let mid = (a + b) * 0.5;
        fit(shape, sample, position, a, mid, depth + 1)?;
        return fit(shape, sample, position, mid, b, depth + 1);
    }
    if shape.points.len() >= 253 {
        return Err("saved envelope needs too many anchors to preserve its curve".into());
    }
    shape.points.push([position(b), yb.clamp(0., 1.)]);
    shape.curves.push(EnvelopeCurve::Bezier {
        control1: [position(a + h / 3.), c.clamp(0., 1.)],
        control2: [position(b - h / 3.), d.clamp(0., 1.)],
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_curves_remain_editable_and_match_the_authored_function() {
        for bias in [-100., -3., -0.7, -0.0005, 0., 0.6, 3., 100.] {
            for level in [-0.3, 0.7, 1.3] {
                let node = NodeInstance {
                    id: "curve".into(),
                    type_id: "adsr".into(),
                    params: serde_json::from_value(serde_json::json!({
                        "attack": 0.25, "decay": 0.25, "sustain": 0.25, "release": 0.25,
                        "attack_curve": bias, "decay_curve": bias, "sustain_level": level,
                    }))
                    .unwrap(),
                    position_x: None,
                    position_y: None,
                };
                let shape = convert(&node).unwrap();
                let curve = parameter(&node, "attack_curve", 0.);
                let level = parameter(&node, "sustain_level", 0.7);
                let low = level.min(0.);
                let range = level.max(1.) - low;
                let biased = |t: f64| {
                    if curve.abs() < 0.001 {
                        t
                    } else if curve > 0. {
                        t.powf(1. + 5. * curve)
                    } else {
                        1. - (1. - t).powf(1. - 5. * curve)
                    }
                };
                for i in 0..=4000 {
                    let t = i as f64 / 4000.;
                    let expected = if t < 0.25 {
                        biased(t * 4.)
                    } else if t < 0.5 {
                        level + (1. - level) * biased(2. - t * 4.)
                    } else if t < 0.75 {
                        level
                    } else {
                        level * (4. - t * 4.)
                    };
                    let actual = low + range * shape.sample(t);
                    assert!(
                        (actual - expected).abs() < 3e-6 * range,
                        "bias={bias}, level={level}, time={t}: {actual} != {expected}"
                    );
                }
                if bias == 0. {
                    assert_eq!(shape.points.len(), 5);
                    assert!(shape.curves.is_empty());
                }
                let roundtrip: Envelope =
                    serde_json::from_str(&serde_json::to_string(&shape).unwrap()).unwrap();
                roundtrip.validate().unwrap();
                assert_eq!(roundtrip.points.len(), shape.points.len());
                for i in 0..=100 {
                    let t = i as f64 / 100.;
                    assert!((roundtrip.sample(t) - shape.sample(t)).abs() < 1e-12);
                }
                let mut edited = shape;
                edited.move_point(1, [edited.points[1][0], 0.8]).unwrap();
                edited.validate().unwrap();
            }
        }
    }
}
