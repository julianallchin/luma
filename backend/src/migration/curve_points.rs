//! Convert stored curves from the two old formats to the one curve format.
//!
//! Old envelope: `{"points": [[x, y]...], "curves": [{"kind": ...}...]}`.
//! Old `time`/`hit` keyframes: `{"points": [[x, value]...], "segments":
//! ["linear" | "hold" | "step" | {"bezier": {...}}...]}`. Both kept Bézier
//! handles in absolute curve coordinates. The new format is `{"points":
//! [[x, value] or [x, value, ease]...]}` with eases local to their segment
//! (see `luma_patterns::Curve`).
//!
//! Every converted curve is sampled in its old and its new form. A curve is
//! exact when the two agree everywhere; otherwise the largest difference and
//! the reasons are reported.
use luma_patterns::{Ease, Value};
use serde_json::{json, Value as Json};

/// Below this difference a conversion is exact.
pub const EXACT: f64 = 1e-9;
/// Even samples over 0–1, besides both sides of every point.
const SAMPLES: usize = 2000;

/// One old segment.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Old {
    Linear,
    Hold,
    Step,
    Bezier([f64; 2], [f64; 2]),
}

/// An old curve, read from its JSON.
struct OldCurve {
    points: Vec<(f64, Vec<f64>)>,
    segments: Vec<Old>,
    /// An envelope clamps outside 0–1; keyframes hold their end values.
    envelope: bool,
}

/// The outcome for one curve.
#[derive(Debug)]
pub struct Converted {
    pub value: Json,
    /// Largest difference, over every channel, between the old and the new.
    pub max_diff: f64,
    /// Why the curve is not exact, when it is not.
    pub reasons: Vec<String>,
    /// Holds an ease equal to the old smooth ease (handles at the thirds, at
    /// the end values), which the shipped presets now name "ease-in-out".
    pub smoothstep: bool,
}

/// Whether an input tagged `kind` holds a curve.
pub fn is_curve_type(kind: &str) -> bool {
    matches!(kind, "envelope" | "time" | "hit")
}

/// Convert the `value` of an input tagged `kind` ("envelope", "time" or
/// "hit"). `Err` when the old value cannot be read at all.
pub fn convert(kind: &str, value: &Json) -> Result<Converted, String> {
    let old = read(kind, value)?;
    let mut reasons = Vec::new();
    let mut smoothstep = false;
    // Drop the earlier of two points at one x: the old sample took the later.
    let mut points = old.points.clone();
    let mut segments = old.segments.clone();
    segments.resize(points.len().saturating_sub(1), Old::Linear);
    let mut i = 0;
    while i + 1 < points.len() {
        if points[i + 1].0 <= points[i].0 {
            if i > 0 && segments[i - 1] != Old::Hold && points[i].1 != points[i + 1].1 {
                reasons.push(format!("a jump at x {} after a slope", points[i].0));
            }
            points.remove(i);
            segments.remove(i);
        } else {
            i += 1;
        }
    }
    let mut eases = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        let (a, b) = (&points[i], &points[i + 1]);
        eases.push(match *segment {
            Old::Linear => Ease::Linear,
            Old::Hold => Ease::Hold,
            // Filled in below, once every value is known.
            Old::Step => Ease::Hold,
            Old::Bezier(c1, c2) => {
                let (x0, x1) = (a.0, b.0);
                let (y0, y1) = (a.1[0], b.1[0]);
                if (y1 - y0).abs() <= EXACT {
                    if [c1[1], c2[1]].iter().any(|y| (y - y0).abs() > EXACT) {
                        reasons.push(format!("a bent flat segment at x {x0}"));
                    }
                    Ease::Linear
                } else {
                    let local = |c: [f64; 2]| [(c[0] - x0) / (x1 - x0), (c[1] - y0) / (y1 - y0)];
                    let ([hx1, hy1], [hx2, hy2]) = (local(c1), local(c2));
                    let handles = [hx1, hy1, hx2, hy2];
                    if handles.iter().any(|h| !(0.0..=1.0).contains(h)) {
                        reasons.push(format!("handles out of the segment at x {x0}"));
                    }
                    let handles = handles.map(|h| round(h.clamp(0., 1.)));
                    smoothstep |= handles
                        .iter()
                        .zip([1. / 3., 0., 2. / 3., 1.])
                        .all(|(h, s)| (h - s).abs() < 1e-6);
                    Ease::Bezier(handles)
                }
            }
        });
    }
    // A step takes the next value at once: its point takes that value and
    // holds it. The segment before now ends at the new value, which only
    // matters when it slopes.
    for i in (0..segments.len()).rev() {
        if segments[i] == Old::Step {
            if i > 0
                && !matches!(segments[i - 1], Old::Hold | Old::Step)
                && points[i].1 != points[i + 1].1
            {
                reasons.push(format!("a step at x {} after a slope", points[i].0));
            }
            points[i].1 = points[i + 1].1.clone();
        }
    }
    // Keyframes held their end values past their first and last points.
    if !old.envelope {
        if points[0].0 > 0. {
            points.insert(0, (0., points[0].1.clone()));
            eases.insert(0, Ease::Linear);
        }
        if points.last().unwrap().0 < 1. {
            points.push((1., points.last().unwrap().1.clone()));
            eases.push(Ease::Linear);
        }
    }
    let value = json!({
        "points": points
            .iter()
            .enumerate()
            .map(|(i, (x, v))| {
                let v = if v.len() == 1 { json!(v[0]) } else { json!(v) };
                match eases.get(i) {
                    Some(Ease::Linear) | None => json!([x, v]),
                    Some(ease) => json!([x, v, ease]),
                }
            })
            .collect::<Vec<_>>()
    });
    let new: Value = serde_json::from_value(json!({"type": kind, "value": value}))
        .map_err(|e| format!("the converted curve does not read: {e}"))?;
    new.validate()
        .map_err(|e| format!("the converted curve is not valid: {e}"))?;
    let max_diff = compare(&old, &new);
    if max_diff > EXACT && reasons.is_empty() {
        reasons.push("sampled values differ".into());
    }
    Ok(Converted {
        value,
        max_diff,
        reasons,
        smoothstep,
    })
}

fn round(v: f64) -> f64 {
    (v * 1e12).round() / 1e12
}

fn read(kind: &str, value: &Json) -> Result<OldCurve, String> {
    let object = value.as_object().ok_or("not an object")?;
    let points = object
        .get("points")
        .and_then(Json::as_array)
        .ok_or("no points")?
        .iter()
        .map(|point| {
            let x = point
                .get(0)
                .and_then(Json::as_f64)
                .ok_or("a point has no x")?;
            let v = match point.get(1) {
                Some(Json::Array(rgb)) => rgb.iter().filter_map(Json::as_f64).collect(),
                Some(v) => vec![v.as_f64().ok_or("a point has no value")?],
                None => return Err("a point has no value"),
            };
            if point.as_array().is_some_and(|p| p.len() != 2) {
                return Err("a point is not [x, value]");
            }
            Ok((x, v))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if points.is_empty() {
        return Err("no points".into());
    }
    let pair = |c: &Json| -> Result<[f64; 2], String> {
        let x = c.get(0).and_then(Json::as_f64).ok_or("bad handle")?;
        let y = c.get(1).and_then(Json::as_f64).ok_or("bad handle")?;
        Ok([x, y])
    };
    let mut segments = Vec::new();
    for key in object.keys() {
        if key != "points" && key != "segments" && key != "curves" {
            return Err(format!("unknown field {key}"));
        }
    }
    if let Some(list) = object.get("segments").and_then(Json::as_array) {
        for segment in list {
            segments.push(match segment {
                Json::String(name) => match name.as_str() {
                    "linear" => Old::Linear,
                    "hold" => Old::Hold,
                    "step" => Old::Step,
                    other => return Err(format!("unknown segment {other}")),
                },
                other => {
                    let bezier = other.get("bezier").ok_or("unknown segment")?;
                    Old::Bezier(pair(&bezier["control1"])?, pair(&bezier["control2"])?)
                }
            });
        }
    }
    if let Some(list) = object.get("curves").and_then(Json::as_array) {
        for curve in list {
            segments.push(match curve["kind"].as_str() {
                Some("linear") => Old::Linear,
                Some("hold") => Old::Hold,
                Some("step") => Old::Step,
                Some("bezier") => Old::Bezier(pair(&curve["control1"])?, pair(&curve["control2"])?),
                _ => return Err("unknown curve kind".into()),
            });
        }
    }
    Ok(OldCurve {
        points,
        segments,
        envelope: kind == "envelope",
    })
}

impl OldCurve {
    /// The old evaluator: the value at `progress`, one entry per channel.
    fn sample(&self, progress: f64) -> Vec<f64> {
        let last = self.points.len() - 1;
        let (first_x, last_x) = if self.envelope {
            (0., 1.)
        } else {
            (self.points[0].0, self.points[last].0)
        };
        if progress < first_x {
            return self.points[0].1.clone();
        }
        if progress >= last_x || last == 0 {
            return self.points[last].1.clone();
        }
        let i = self
            .points
            .partition_point(|p| p.0 <= progress)
            .saturating_sub(1)
            .min(last - 1);
        let (a, b) = (&self.points[i], &self.points[i + 1]);
        let t = match self.segments.get(i).copied().unwrap_or(Old::Linear) {
            Old::Linear => (progress - a.0) / (b.0 - a.0),
            Old::Hold => 0.,
            Old::Step => 1.,
            Old::Bezier(c1, c2) => {
                let controls = [[a.0, a.1[0]], c1, c2, [b.0, b.1[0]]];
                return vec![at(controls, parameter(controls, progress))[1]];
            }
        };
        a.1.iter().zip(&b.1).map(|(a, b)| a + (b - a) * t).collect()
    }
}

/// The largest difference between the old curve and `new`.
fn compare(old: &OldCurve, new: &Value) -> f64 {
    let sample = |x: f64| -> Vec<f64> {
        match new {
            Value::Envelope(e) => vec![e.sample(x)],
            Value::Time(k) | Value::Hit(k) => {
                let v = k.sample(x);
                if k.is_color() {
                    v.to_vec()
                } else {
                    vec![v[0]]
                }
            }
            _ => unreachable!("a curve"),
        }
    };
    let mut xs: Vec<f64> = (0..=SAMPLES).map(|i| i as f64 / SAMPLES as f64).collect();
    for (x, _) in &old.points {
        xs.extend([x - 1e-7, *x, x + 1e-7]);
    }
    xs.into_iter()
        .filter(|x| (0.0..=1.0).contains(x))
        .map(|x| {
            old.sample(x)
                .iter()
                .zip(sample(x))
                .map(|(a, b)| (a - b).abs())
                .fold(0., f64::max)
        })
        .fold(0., f64::max)
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
    for _ in 0..60 {
        let mid = (low + high) * 0.5;
        if at(controls, mid)[0] < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(kind: &str, old: Json, new: Json) {
        let converted = convert(kind, &old).unwrap();
        assert_eq!(converted.value, new);
        assert!(converted.max_diff <= EXACT, "{converted:?}");
        assert!(converted.reasons.is_empty(), "{converted:?}");
    }

    #[test]
    fn absolute_handles_become_local_eases() {
        exact(
            "envelope",
            json!({"points": [[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]], "curves": [
                {"kind": "bezier", "control1": [0.2, 0.0], "control2": [0.3, 1.0]},
                {"kind": "bezier", "control1": [0.7, 1.0], "control2": [0.8, 0.0]}]}),
            json!({"points": [[0.0, 0.0, [0.4, 0.0, 0.6, 1.0]], [0.5, 1.0, [0.4, 0.0, 0.6, 1.0]], [1.0, 0.0]]}),
        );
        exact(
            "hit",
            json!({"points": [[0.0, 1.0], [1.0, 0.0]], "segments": [
                {"bezier": {"control1": [0.25, 0.0], "control2": [0.5, 0.0]}}]}),
            json!({"points": [[0.0, 1.0, [0.25, 1.0, 0.5, 1.0]], [1.0, 0.0]]}),
        );
    }

    #[test]
    fn holds_short_curves_and_steps_convert_exactly() {
        exact(
            "envelope",
            json!({"points": [[0.0, 0.25], [0.5, 0.75], [1.0, 0.75]], "curves": [{"kind": "hold"}, {"kind": "hold"}]}),
            json!({"points": [[0.0, 0.25, "hold"], [0.5, 0.75, "hold"], [1.0, 0.75]]}),
        );
        exact(
            "hit",
            json!({"points": [[0.0, 1.0], [0.1875, 0.5], [0.75, 0.0]], "segments": ["linear", "linear"]}),
            json!({"points": [[0.0, 1.0], [0.1875, 0.5], [0.75, 0.0], [1.0, 0.0]]}),
        );
        exact(
            "time",
            json!({"points": [[0.0, 0.2], [0.5, 0.6], [1.0, 1.0]], "segments": ["hold", "step"]}),
            json!({"points": [[0.0, 0.2, "hold"], [0.5, 1.0, "hold"], [1.0, 1.0]]}),
        );
    }

    #[test]
    fn a_step_after_a_slope_is_reported() {
        let converted = convert(
            "hit",
            &json!({"points": [[0.0, 1.0], [0.75, 0.25], [1.0, 0.0]], "segments": ["linear", "step"]}),
        )
        .unwrap();
        assert_eq!(
            converted.value,
            json!({"points": [[0.0, 1.0], [0.75, 0.0, "hold"], [1.0, 0.0]]})
        );
        assert!((converted.max_diff - 0.25).abs() < 1e-3, "{converted:?}");
        assert_eq!(converted.reasons.len(), 1);
    }
}
