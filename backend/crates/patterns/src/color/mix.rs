use super::*;
use crate::runtime::EvaluatedValue;
use ndarray::{Array2, Array3, Axis};

fn lab(stop: &ColorStop) -> [f64; 5] {
    let (l, a, b) = crate::oklab::srgb_to_oklab(
        stop.color[0] as f32,
        stop.color[1] as f32,
        stop.color[2] as f32,
    );
    let (l, a, b) = (f64::from(l), f64::from(a), f64::from(b));
    [l, a, b, stop.alpha, a.hypot(b)]
}
fn rescue(color: &mut [f64], target: f64, vibrance: f64) {
    let chroma = color[1].hypot(color[2]);
    if chroma > 1e-6 {
        let scale = ((chroma + (target - chroma) * vibrance) / chroma).clamp(0., 10.);
        color[1] *= scale;
        color[2] *= scale;
    }
}
fn sample(gradient: &Gradient, position: f64, vibrance: f64) -> [f64; 5] {
    if gradient.stops.is_empty() {
        return [0., 0., 0., 1., 0.];
    }
    let right = gradient.stops.partition_point(|s| s.t <= position);
    if right == 0 {
        return lab(&gradient.stops[0]);
    }
    if right == gradient.stops.len() {
        return lab(&gradient.stops[right - 1]);
    }
    let (a, b) = (&gradient.stops[right - 1], &gradient.stops[right]);
    let t = (position - a.t) / (b.t - a.t);
    let (a, b) = (lab(a), lab(b));
    let mut mixed = std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t);
    rescue(&mut mixed, a[4] + (b[4] - a[4]) * t, vibrance);
    mixed[4] = mixed[1].hypot(mixed[2]);
    mixed
}
pub(crate) fn mix_palette(
    inputs: &BTreeMap<String, EvaluatedValue>,
    clock: &Signal,
) -> Result<(Signal, Signal)> {
    let Value::Boolean(perceptual) = inputs["perceptual"].control(0) else {
        unreachable!()
    };
    let vibrance = inputs["vibrance"].numeric();
    if vibrance.values().dim().0 != 1 || vibrance.values().dim().2 != 1 {
        return Err(Error(
            "palette vibrance needs one global value per sample".into(),
        ));
    }
    let weights = inputs["weights"]
        .numeric()
        .zip(clock, Unit::Number, |w, _| w)?
        .zip(vibrance, Unit::Number, |w, _| w)?;
    let (n, t, c) = weights.values().dim();
    let mut colors = Array3::zeros((n, t, 3));
    let mut alpha = Array3::zeros((n, t, 1));
    for time in 0..t {
        let Value::Gradient(gradient) = inputs["gradient"].control(time) else {
            unreachable!()
        };
        let vibrance = vibrance.at(0, time, 0);
        let mut palette = Array2::zeros((c, 5));
        for channel in 0..c {
            let position = channel as f64 / c.saturating_sub(1).max(1) as f64;
            let color = if *perceptual {
                if c == gradient.stops.len() {
                    lab(&gradient.stops[channel])
                } else {
                    sample(gradient, position, vibrance)
                }
            } else {
                let [r, g, b] = gradient.sample(position);
                [r, g, b, gradient.sample_alpha(position), 0.]
            };
            for (i, value) in color.into_iter().enumerate() {
                palette[[channel, i]] = value;
            }
        }
        let mixed = weights.values().index_axis(Axis(1), time).dot(&palette);
        for head in 0..n {
            let mut color = [mixed[[head, 0]], mixed[[head, 1]], mixed[[head, 2]]];
            if *perceptual {
                rescue(&mut color, mixed[[head, 4]], vibrance);
                let (r, g, b) =
                    crate::oklab::oklab_to_srgb(color[0] as f32, color[1] as f32, color[2] as f32);
                color = [r, g, b].map(f64::from);
            }
            for i in 0..3 {
                colors[[head, time, i]] = color[i];
            }
            alpha[[head, time, 0]] = mixed[[head, 3]];
        }
    }
    let domain = weights.fixtures().map(Into::into);
    Ok((
        Signal::new(colors, Unit::Proportion, Channels::Rgb, domain.clone())?,
        Signal::new(alpha, Unit::Proportion, Channels::Value, domain)?,
    ))
}
