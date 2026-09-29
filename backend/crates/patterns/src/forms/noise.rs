//! One reading of a Noise source. The source kernel and the clip sheet's
//! preview both call [`level`], so a preview shows what playback gives.
use super::geometry::{identity, salt};
use crate::{value_noise::hash, Error, NoiseSource, Result};

/// Where one head reads the noise.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NoisePoint {
    /// Its own wandering, from a seed per unit.
    Own(u64),
    /// A place in the spatial field: U and V over the scale.
    Field { x: f64, y: f64, scale: f64 },
    /// The one value of the whole selection, per channel.
    Shared { salt: f64, channel: usize },
}

/// The value at `turns` of the clock, `low` to `high`. `seed` is the clip's.
pub(crate) fn level(
    point: NoisePoint,
    turns: f64,
    contrast: f64,
    low: f64,
    high: f64,
    seed: u64,
) -> Result<f64> {
    let raw = match point {
        NoisePoint::Own(unit) => (crate::value_noise::noise1(turns, 1., unit) + 1.) / 2.,
        NoisePoint::Field { x, y, scale } => {
            let scale = scale.max(0.01);
            crate::signals::coherent_noise([x / scale, y / scale, turns], seed)?
        }
        NoisePoint::Shared { salt, channel } => {
            crate::signals::coherent_noise([turns, salt + 101. * channel as f64, 0.], seed)?
        }
    };
    let level = ((raw - 0.5) * (1. + contrast * 3.) + 0.5).clamp(0., 1.);
    Ok(low + (high - low) * level)
}

/// The seed of unit `unit`'s own wandering on channel `channel`. `key` is
/// the source's key, or its input path.
pub(crate) fn unit_seed(seed: u64, unit: &str, key: &str, channel: usize) -> u64 {
    let component = key
        .parse::<u64>()
        .unwrap_or_else(|_| hash(identity(key), channel as u64));
    hash(hash(seed, identity(unit)), component)
}

/// A Noise source's numeric settings as fixed numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseSettings {
    /// Beats per turn of the noise clock.
    pub speed: f64,
    /// The spatial blob size; read only when the source has a scale.
    pub scale: f64,
    pub contrast: f64,
    pub range: [f64; 2],
}

/// The value `source` gives one head, `beats` after the clip start, with
/// `settings` in place of its numeric settings. `path` is the input's path
/// as the form names it ("brightness", "lean/gain"), `seed` the clip's,
/// `unit` the id of the head's grain unit (its first cell) and `position`
/// the unit's U and V, 0–1 over the selection. The first channel only.
pub fn noise_value(
    source: &NoiseSource,
    settings: NoiseSettings,
    path: &str,
    seed: u64,
    unit: &str,
    position: [f64; 2],
    beats: f64,
) -> Result<f64> {
    if !(settings.speed.is_finite() && settings.speed > 0.) {
        return Err(Error("noise speed must stay positive".into()));
    }
    let key = source.key.as_deref().unwrap_or(path);
    let point = if source.independent {
        NoisePoint::Own(unit_seed(seed, unit, key, 0))
    } else if source.scale.is_some() {
        NoisePoint::Field {
            x: position[0],
            y: position[1],
            scale: settings.scale,
        }
    } else {
        NoisePoint::Shared {
            salt: salt(key),
            channel: 0,
        }
    };
    let [low, high] = settings.range;
    level(
        point,
        beats / settings.speed,
        settings.contrast,
        low,
        high,
        seed,
    )
}
