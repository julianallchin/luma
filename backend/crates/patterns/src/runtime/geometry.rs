//! Geometry algorithms over the same fixture × time × channel signal axes.
//! Fits use the scene geometry's f32 precision, including explicit sample order.
use super::*;

fn number_order(a: f64, b: f64) -> std::cmp::Ordering {
    if a == b {
        std::cmp::Ordering::Equal
    } else {
        a.total_cmp(&b)
    }
}
fn ordered_signal(position: &Signal, order: &Signal, fixtures: &[String]) -> Result<Signal> {
    let fixtures = position.fixtures().or(order.fixtures()).unwrap_or(fixtures);
    position
        .on_fixtures(fixtures)?
        .join_channels(&order.on_fixtures(fixtures)?)
}
/// The last channel supplies sample ordering; it is not a geometric coordinate
/// and does not inherit the fitter's f32 range limit.
fn points<const D: usize>(source: &Signal, time: usize) -> Result<Vec<(usize, [f32; D])>> {
    let mut order: Vec<_> = (0..source.values().dim().0).collect();
    order.sort_by(|a, b| number_order(source.at(*a, time, D), source.at(*b, time, D)));
    order
        .into_iter()
        .map(|i| {
            let point = std::array::from_fn(|c| source.at(i, time, c) as f32);
            if point.iter().any(|v| !v.is_finite()) {
                Err(Error("coordinates exceed the geometry solver range".into()))
            } else {
                Ok((i, point))
            }
        })
        .collect()
}
fn centroid<const D: usize>(points: &[(usize, [f32; D])]) -> [f32; 2] {
    std::array::from_fn(|c| {
        points.iter().map(|(_, p)| p[c]).sum::<f32>() / points.len().max(1) as f32
    })
}
fn phase(dx: f32, dy: f32) -> f32 {
    ((dy.atan2(dx) + std::f32::consts::PI) / std::f32::consts::TAU + 0.25) % 1.
}
fn field(values: Array3<f64>, source: &Signal) -> Result<Signal> {
    Signal::new(
        values,
        Unit::Number,
        Channels::Value,
        source.fixtures().map(Into::into),
    )
}

pub(super) fn circle_phase(
    position: &Signal,
    order: &Signal,
    fixtures: &[String],
) -> Result<Signal> {
    let source = ordered_signal(position, order, fixtures)?;
    let (n, times, _) = source.values().dim();
    let mut values = Array3::zeros((n, times, 1));
    for t in 0..times {
        let points = points::<3>(&source, t)?;
        let coordinates = points
            .iter()
            .map(|(_, p)| (p[0], p[1], p[2]))
            .collect::<Vec<_>>();
        let fit = crate::circle_fit::fit_circle_3d(&coordinates);
        let [cx, cy] = centroid(&points);
        for (at, (i, p)) in points.iter().enumerate() {
            values[[*i, t, 0]] = f64::from(
                fit.as_ref()
                    .map(|f| f.angular_positions[at])
                    .unwrap_or_else(|| phase(p[0] - cx, p[1] - cy)),
            );
        }
    }
    field(values, &source)
}

pub(super) fn rank_nearby(
    value: &Signal,
    position: &Signal,
    order: &Signal,
    tolerance: f64,
    fixtures: &[String],
) -> Result<Signal> {
    let fixtures = value
        .fixtures()
        .or(position.fixtures())
        .or(order.fixtures())
        .unwrap_or(fixtures);
    if tolerance < 0. {
        return Err(Error("merge distance must be nonnegative".into()));
    }
    let joined = value
        .on_fixtures(fixtures)?
        .join_channels(&position.on_fixtures(fixtures)?)?
        .join_channels(&order.on_fixtures(fixtures)?)?;
    let (n, times, _) = joined.values().dim();
    let mut values = Array3::zeros((n, times, 1));
    for t in 0..times {
        let mut order: Vec<_> = (0..n).collect();
        order.sort_by(|a, b| {
            number_order(joined.at(*a, t, 0), joined.at(*b, t, 0))
                .then_with(|| number_order(joined.at(*a, t, 4), joined.at(*b, t, 4)))
        });
        let mut rank = 0.;
        for (at, &i) in order.iter().enumerate() {
            if at > 0 {
                let previous = order[at - 1];
                let distance = (1..4)
                    .map(|c| (joined.at(i, t, c) - joined.at(previous, t, c)).powi(2))
                    .sum::<f64>()
                    .sqrt();
                if distance > tolerance {
                    rank += 1.;
                }
            }
            values[[i, t, 0]] = rank;
        }
    }
    field(values, &joined)
}

/// The 2D covariance eigensolve. Sample order is explicit because accumulation
/// can choose a different direction for an almost isotropic point cloud.
pub(super) fn principal_direction(
    position: &Signal,
    order: &Signal,
    fixtures: &[String],
) -> Result<Signal> {
    let source = ordered_signal(position, order, fixtures)?;
    let times = source.values().dim().1;
    let mut values = Array3::zeros((1, times, 2));
    for t in 0..times {
        let points = points::<2>(&source, t)?;
        let [cx, cy] = centroid(&points);
        let (mut xx, mut xy, mut yy) = (0_f32, 0_f32, 0_f32);
        for (_, p) in points {
            let x = p[0] - cx;
            let y = p[1] - cy;
            xx += x * x;
            xy += x * y;
            yy += y * y;
        }
        let [x, y] = if xy.abs() < f32::EPSILON {
            if xx >= yy {
                [1., 0.]
            } else {
                [0., 1.]
            }
        } else {
            let trace = xx + yy;
            let determinant = xx * yy - xy * xy;
            let lambda = 0.5 * (trace + (trace * trace - 4. * determinant).max(0.).sqrt());
            let (x, y) = (lambda - yy, xy);
            let length = (x * x + y * y).sqrt();
            [x / length, y / length]
        };
        let flip = if x.abs() >= y.abs() { x < 0. } else { y < 0. };
        values[[0, t, 0]] = f64::from(if flip { -x } else { x });
        values[[0, t, 1]] = f64::from(if flip { -y } else { y });
    }
    Signal::new(values, Unit::Number, Channels::components(2)?, None)
}

pub(super) fn radial_coordinates(
    position: &Signal,
    order: &Signal,
    fixtures: &[String],
) -> Result<(Signal, Signal)> {
    let source = ordered_signal(position, order, fixtures)?;
    let (n, times, _) = source.values().dim();
    let mut phases = Array3::zeros((n, times, 1));
    let mut radii = phases.clone();
    for t in 0..times {
        let points = points::<3>(&source, t)?;
        let [cx, cy] = centroid(&points);
        for (i, p) in points {
            let dx = p[0] - cx;
            let dy = p[1] - cy;
            phases[[i, t, 0]] = f64::from(phase(dx, dy));
            radii[[i, t, 0]] = f64::from((dx * dx + dy * dy).sqrt());
        }
    }
    Ok((field(phases, &source)?, field(radii, &source)?))
}
/// Collapse the fixture axis using an explicit order. Broadcast values already
/// have no fixture axis; preserve them even in an empty venue.
pub(super) fn first(value: &Signal, order: &Signal) -> Result<Signal> {
    if value.fixtures().is_none() {
        return Ok(value.clone());
    }
    let width = value.channels().count();
    let (n, t, _) = value.values().dim();
    let domain = Signal::new(
        Array3::zeros((n, t, 1)),
        Unit::Number,
        Channels::Value,
        value.fixtures().map(Into::into),
    )?;
    let order = order.zip(&domain, Unit::Number, |order, _| order)?;
    let (n, t, _) = order.values().dim();
    let mut result = Array3::zeros((1, t, width));
    for time in 0..t {
        if let Some(row) =
            (0..n).min_by(|a, b| number_order(order.at(*a, time, 0), order.at(*b, time, 0)))
        {
            for channel in 0..width {
                result[[0, time, channel]] = value.at(row, time, channel);
            }
        }
    }
    Signal::new(result, value.unit(), *value.channels(), None)
}
