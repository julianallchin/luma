//! The Random node's field: coherent value noise over three space axes and
//! time, a pure function of its coordinates and seed, read on a clock
//! that holds each turn's draw for all but its last `glide`.
use crate::{Error, Result};

/// Value noise at `point`, 0–1, smooth across the lattice.
fn coherent_noise(point: [f64; 4], seed: u64) -> Result<f64> {
    if point.iter().any(|v| !v.is_finite() || v.abs() > 1e12) {
        return Err(Error(
            "noise coordinates must be finite and within ±1e12".into(),
        ));
    }
    let base = point.map(|v| v.floor() as i64);
    let fraction = point.map(|v| {
        let t = v - v.floor();
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    });
    let mut sum = 0.0;
    for corner in 0..16 {
        let mut hash = seed;
        let mut weight = 1.0;
        for axis in 0..4 {
            let upper = ((corner >> axis) & 1) as i64;
            hash = crate::spatial::epoch_seed(hash, base[axis] + upper);
            weight *= if upper == 0 {
                1.0 - fraction[axis]
            } else {
                fraction[axis]
            };
        }
        sum += weight * ((hash >> 11) as f64 / (1u64 << 53) as f64);
    }
    Ok(sum.clamp(0.0, 1.0))
}

/// The noise field's time coordinate at `turns` of a Random's clock: the
/// turn's draw held for the first 1 − `glide` of the turn, then eased to
/// the next over the rest. `glide` 1 drifts the whole turn.
pub(crate) fn glided(turns: f64, glide: f64) -> f64 {
    let glide = glide.clamp(0., 1.);
    let turn = turns.floor();
    let hold = 1. - glide;
    let into = turns - turn;
    if into <= hold {
        turn
    } else {
        turn + (into - hold) / glide
    }
}

/// The field's value at `place` (0–1 per axis) and time coordinate
/// `time`, with `contrast` spreading it from the middle.
pub(crate) fn field(
    place: [f64; 3],
    size: f64,
    time: f64,
    contrast: f64,
    seed: u64,
) -> Result<f64> {
    let size = size.max(1e-6);
    let raw = coherent_noise(
        [place[0] / size, place[1] / size, place[2] / size, time],
        seed,
    )?;
    Ok(((raw - 0.5) * (1. + 3. * contrast) + 0.5).clamp(0., 1.))
}

/// Random node `node`'s value at one head, as playback computes it (spec
/// 2.3): `seed` is the clip's, `place` the head's position over its span's
/// largest extent (0–1 on each axis), `beats` the beats since the clip
/// start. `every` is beats per turn; `scale` `None` gives one value for
/// every head.
#[allow(clippy::too_many_arguments)]
pub fn sample_random(
    node: &str,
    seed: u64,
    place: [f64; 3],
    beats: f64,
    every: f64,
    glide: f64,
    scale: Option<f64>,
    contrast: f64,
) -> Result<f64> {
    if every.is_nan() || every <= 0. {
        return Err(Error(format!("{node}.every needs beats above 0")));
    }
    let (place, size) = match scale {
        Some(scale) => (place, scale),
        None => ([0.; 3], 1.),
    };
    field(
        place,
        size,
        glided(beats / every, glide),
        contrast,
        seed ^ fnv(node),
    )
}

/// A stable 64-bit hash of a name (FNV-1a).
pub(crate) fn fnv(name: &str) -> u64 {
    name.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
