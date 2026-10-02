//! Coherent value noise over three space axes and time. A pure function of
//! its coordinates and seed.
use crate::{Error, Result};

/// Value noise at `point`, 0–1, smooth across the lattice.
pub(crate) fn coherent_noise(point: [f64; 4], seed: u64) -> Result<f64> {
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

/// Noise node `node`'s value at one head, as playback computes it (spec
/// 2.3): `seed` is the clip's, `place` the head's position over its span's
/// largest extent (0–1 on each axis), `beats` the beats since the clip
/// start. `period` is beats per turn; `scale` `None` gives one value for
/// every head.
pub fn sample_noise(
    node: &str,
    seed: u64,
    place: [f64; 3],
    beats: f64,
    period: f64,
    scale: Option<f64>,
    contrast: f64,
) -> Result<f64> {
    if period.is_nan() || period <= 0. {
        return Err(Error(format!("{node}.period needs beats above 0")));
    }
    let (p, size) = match scale {
        Some(scale) => (place, scale.max(1e-6)),
        None => ([0.; 3], 1.),
    };
    let raw = coherent_noise(
        [p[0] / size, p[1] / size, p[2] / size, beats / period],
        seed ^ fnv(node),
    )?;
    Ok(((raw - 0.5) * (1. + 3. * contrast) + 0.5).clamp(0., 1.))
}

/// A stable 64-bit hash of a name (FNV-1a).
pub(crate) fn fnv(name: &str) -> u64 {
    name.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
