//! Sky clouds: one layer of cloud over an open-air venue.
//!
//! The layer is a spherical shell some hundreds of metres to a few kilometres
//! up, and it is marched every frame from the real camera, as Unreal's
//! Volumetric Cloud and HDRP's Volumetric Clouds are: the camera may walk
//! hundreds of metres from the stage or rise over it, and the cloud bases
//! turn in perspective as it does. See `experiments/clouds-prior-art.md`.
//!
//! This file is the CPU half: what each preset is, the weather map, the blue
//! noise the march is jittered with, and the per-quality budget. The GPU
//! half is `cloud_gpu.rs` and `atmosphere_cloud_*.wgsl`.
//!
//! # One layer, every consumer
//!
//! The weather map is made here and uploaded as it is. From it the GPU draws
//! the clouds in the picture, the probe the rig's ambient comes from, and a
//! shadow map that the ground, the stage, the haze and the aerial
//! perspective all read. The sun on the stage, the shafts in the air and the
//! grey of an overcast day all follow from one layer.

use std::sync::OnceLock;

use glam::Vec2;

use crate::scene_desc::{CloudCover, Quality};

/// Texels per side of the weather map.
pub(crate) const WEATHER_SIZE: u32 = 512;

/// Farthest a view ray is marched into the layer, kilometres. Past this the
/// air in front hides it anyway, and the horizon is where a march is longest.
pub(crate) const MAX_MARCH_KM: f32 = 80.0;

/// Texels per side of the blue-noise tile the march is jittered with.
pub(crate) const BLUE_NOISE_SIZE: u32 = 64;

/// Rows of the shadow map written per frame: one of every this many. The
/// map changes only as the wind moves the layer, a few metres a second.
pub(crate) const SHADOW_BANDS: u32 = 4;

/// One preset's layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Layer {
    /// Height of the cloud base above the ground, kilometres.
    pub base_km: f32,
    /// Depth of the shell, kilometres.
    pub thickness_km: f32,
    /// Extinction at full density, 1/km. Droplets barely absorb, so this is
    /// also the scattering coefficient.
    pub extinction: f32,
    /// Fraction of the ground under cloud, on average over the weather map.
    pub coverage: f32,
    /// How far coverage swings between regions of the map, plus or minus.
    pub coverage_spread: f32,
    /// Lowest coverage anywhere: zero where there is sky between clouds,
    /// most of one for a deck.
    pub floor: f32,
    /// Size of one cloud field in the weather map, kilometres.
    pub feature_km: f32,
    /// Ground distance the weather map spans before it repeats, kilometres.
    pub tile_km: f32,
    /// Range of cloud kind across the map: 0 stratus, 0.5 stratocumulus,
    /// 1 cumulus.
    pub kind: (f32, f32),
    /// How much the fine noise eats into the edges.
    pub erosion: f32,
    /// Period of the shape noise, kilometres: the size of a cloud's lobes.
    pub shape_km: f32,
    /// Period of the detail noise, kilometres.
    pub detail_km: f32,
    /// Scale on the sky and ground light a cloud is lit by.
    pub ambient: f32,
    /// Darkening of thin edges away from the sun (HDRP's powder).
    pub powder: f32,
    /// Gain on density inside the outline: high for a crisp cumulus, low
    /// for a soft deck.
    pub sharpness: f32,
    /// Fraction of the clear sky's diffuse light still reaching the air
    /// under the layer, for the aerial volume's skylight.
    pub sky_light: f32,
    /// Exposure gain over the clear sky's. A clouded day is darker and a
    /// camera opens up for it — part of the way, so a storm still reads dim.
    pub exposure: f32,
    /// Wind at the layer, metres per second, toward +X.
    pub wind_mps: f32,
    /// The thin high layer above it, if any.
    pub cirrus: Cirrus,
    /// Seed of the weather map.
    seed: u32,
}

/// Cirrus: a sheet of ice cloud kilometres above the cumulus, too thin to
/// march. It is a 2D layer of streaks stretched along the wind (as the
/// engines draw cirrus), lit by the sun through it and the sky over it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Cirrus {
    /// Height above the ground, kilometres.
    pub altitude_km: f32,
    /// Optical depth straight through the densest streak.
    pub optical_depth: f32,
    /// Fraction of the sky with cirrus in it; zero for none.
    pub coverage: f32,
    /// Length of a streak along the wind, kilometres.
    pub streak_km: f32,
    /// Width of a streak across the wind, kilometres.
    pub width_km: f32,
}

impl Cirrus {
    const NONE: Self = Self {
        altitude_km: 8.0,
        optical_depth: 0.0,
        coverage: 0.0,
        streak_km: 20.0,
        width_km: 2.0,
    };
}

impl Layer {
    /// The layer a preset stands for, or `None` for a clear sky.
    #[must_use]
    pub(crate) fn of(cover: CloudCover) -> Option<Self> {
        match cover {
            CloudCover::Clear => None,
            CloudCover::FairWeather => Some(Self {
                base_km: 1.1,
                thickness_km: 0.9,
                extinction: 80.0,
                coverage: 0.3,
                coverage_spread: 0.12,
                floor: 0.0,
                feature_km: 2.0,
                tile_km: 40.0,
                kind: (0.6, 0.95),
                erosion: 0.3,
                shape_km: 1.2,
                detail_km: 0.18,
                ambient: 0.55,
                powder: 0.5,
                sharpness: 2.0,
                sky_light: 1.0,
                exposure: 1.0,
                wind_mps: 6.0,
                cirrus: Cirrus {
                    coverage: 0.2,
                    optical_depth: 0.12,
                    ..Cirrus::NONE
                },
                seed: 11,
            }),
            CloudCover::Wispy => Some(Self {
                // No cumulus: zero extinction skips the march. The layer is
                // kept so the passes, the probe and the shadow map run.
                base_km: 1.5,
                thickness_km: 1.0,
                extinction: 0.0,
                coverage: 0.0,
                coverage_spread: 0.0,
                floor: 0.0,
                feature_km: 5.0,
                tile_km: 64.0,
                kind: (0.5, 0.5),
                erosion: 0.2,
                shape_km: 2.0,
                detail_km: 0.25,
                ambient: 0.55,
                powder: 0.0,
                sharpness: 1.0,
                sky_light: 1.0,
                exposure: 1.0,
                wind_mps: 20.0,
                cirrus: Cirrus {
                    altitude_km: 8.5,
                    optical_depth: 1.0,
                    coverage: 0.35,
                    streak_km: 14.0,
                    width_km: 1.5,
                },
                seed: 61,
            }),
            CloudCover::Scattered => Some(Self {
                base_km: 1.2,
                thickness_km: 1.8,
                extinction: 70.0,
                coverage: 0.42,
                coverage_spread: 0.2,
                floor: 0.0,
                feature_km: 5.0,
                tile_km: 64.0,
                kind: (0.35, 0.8),
                erosion: 0.25,
                shape_km: 2.0,
                detail_km: 0.25,
                ambient: 0.55,
                powder: 0.5,
                sharpness: 1.5,
                sky_light: 0.85,
                exposure: 1.1,
                wind_mps: 8.0,
                cirrus: Cirrus {
                    coverage: 0.25,
                    optical_depth: 0.2,
                    altitude_km: 9.0,
                    ..Cirrus::NONE
                },
                seed: 23,
            }),
            CloudCover::Overcast => Some(Self {
                base_km: 0.8,
                thickness_km: 1.6,
                extinction: 45.0,
                coverage: 0.95,
                coverage_spread: 0.05,
                floor: 0.45,
                feature_km: 5.0,
                tile_km: 64.0,
                kind: (0.1, 0.45),
                erosion: 0.2,
                shape_km: 3.0,
                detail_km: 0.5,
                ambient: 0.7,
                powder: 0.3,
                sharpness: 2.0,
                sky_light: 0.35,
                exposure: 2.0,
                wind_mps: 5.0,
                cirrus: Cirrus::NONE,
                seed: 37,
            }),
            CloudCover::Storm => Some(Self {
                base_km: 0.6,
                thickness_km: 3.5,
                extinction: 80.0,
                coverage: 1.0,
                coverage_spread: 0.0,
                floor: 0.55,
                feature_km: 6.0,
                tile_km: 64.0,
                kind: (0.4, 0.9),
                erosion: 0.2,
                shape_km: 2.0,
                detail_km: 0.5,
                ambient: 0.3,
                powder: 0.4,
                sharpness: 3.0,
                sky_light: 0.15,
                exposure: 2.0,
                wind_mps: 14.0,
                cirrus: Cirrus::NONE,
                seed: 53,
            }),
        }
    }

    /// Fraction of the sky with no cloud in it, for the horizon, where the
    /// march gives way to the air in front of it.
    #[must_use]
    pub(crate) fn clear_fraction(&self) -> f32 {
        if self.floor > 0.0 {
            0.0
        } else {
            1.0 - self.coverage
        }
    }

    /// The share of the sun's flux that reaches the air under the layer as
    /// diffuse light through its cloud: the two-stream estimate through a
    /// typical column (`cloud_diffusion` in `atmosphere_cloud_common.wgsl`),
    /// over the part of the sky the cloud covers. The air under a deck is lit
    /// by it and nothing else; the far ground and the deck at the horizon
    /// both fade to that air, so they meet without a seam.
    #[must_use]
    pub(crate) fn deck_diffuse(&self) -> f32 {
        let tau = self.extinction * self.thickness_km * 0.3;
        (1.0 - self.clear_fraction()) / (1.0 + 0.75 * (1.0 - 0.85) * tau)
    }

    /// How far toward the sun a sample looks for the cloud shading it,
    /// kilometres: through the layer at the sun's slant, so a storm's base
    /// is lit through all of it, and at least across one cloud.
    #[must_use]
    pub(crate) fn light_km(&self, sun_z: f32) -> f32 {
        (self.thickness_km / sun_z.max(0.25)).clamp(0.8, 6.0) * 0.5
    }
}

/// Per-quality budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Budget {
    /// The view buffer is the output divided by this, per axis.
    pub divisor: u32,
    /// View steps for a ray straight up through the layer.
    pub steps: u32,
    /// Light steps per sample.
    pub light_steps: u32,
    /// Size of the probe's panorama.
    pub panorama: (u32, u32),
    /// Side of the shadow map, texels.
    pub shadow: u32,
}

impl Budget {
    #[must_use]
    pub(crate) fn of(quality: Quality) -> Self {
        match quality {
            Quality::High => Self {
                divisor: 2,
                steps: 64,
                light_steps: 6,
                panorama: (512, 256),
                shadow: 1024,
            },
            Quality::Low => Self {
                divisor: 4,
                steps: 32,
                light_steps: 4,
                panorama: (256, 128),
                shadow: 512,
            },
        }
    }
}

/// Side of the shadow map, kilometres: the ground around the camera whose
/// sun it answers for, and the reach of the shafts in the air.
pub(crate) const SHADOW_SPAN_KM: f32 = 40.0;

/// The weather map: RGBA8, row-major, [`WEATHER_SIZE`] a side, repeating
/// every [`Layer::tile_km`]. `r` is coverage, `g` cloud kind, `b` density.
pub(crate) struct Weather {
    pub texels: Vec<u8>,
}

/// The weather map for `cover`, made once per process. `None` when clear.
pub(crate) fn weather(cover: CloudCover) -> Option<&'static Weather> {
    static MAPS: [OnceLock<Weather>; 6] = [const { OnceLock::new() }; 6];
    let layer = Layer::of(cover)?;
    let slot = CloudCover::ALL.iter().position(|c| *c == cover)?;
    Some(MAPS[slot].get_or_init(|| build_weather(&layer)))
}

fn build_weather(layer: &Layer) -> Weather {
    let n = WEATHER_SIZE as usize;
    let cells = ((layer.tile_km / layer.feature_km).round() as u32).max(2);
    let seed = layer.seed;
    let mut field = Vec::with_capacity(n * n);
    let mut regions = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let uv = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / n as f32;
            // Two scales of region: where the sky is busy or open, and what
            // kind of cloud it holds. A few across the tile, so the sky is
            // not one pattern repeated to the horizon.
            let region = fbm(uv, 3, 2, seed + 10);
            let kind = fbm(uv, 3, 3, seed + 20);
            let density = fbm(uv, cells / 2 + 1, 3, seed + 30);
            // Clustered fields: fBm, with Worley folded in where the kind is
            // cumulus so the fields break into heaps. Some regions hold
            // clouds half the size of others.
            let size = fbm(uv, 3, 2, seed + 40);
            let smooth = fbm(uv, cells, 5, seed) * (1.0 - size)
                + fbm(uv, (cells / 2).max(2), 5, seed + 2) * size;
            let heaps = worley(uv, cells * 2, seed + 1);
            let k = layer.kind.0 + (layer.kind.1 - layer.kind.0) * kind;
            field.push(smooth * (1.0 - 0.45 * k) + heaps * 0.45 * k);
            regions.push((region, k, density));
        }
    }
    // Rank the field, so a local coverage of 0.3 is 30% of the ground
    // whatever the noise's distribution.
    let mut order: Vec<u32> = (0..field.len() as u32).collect();
    order.sort_by(|&a, &b| field[a as usize].total_cmp(&field[b as usize]));
    let mut rank = vec![0.0_f32; field.len()];
    for (i, &index) in order.iter().enumerate() {
        rank[index as usize] = i as f32 / (field.len() - 1) as f32;
    }
    let mut texels = Vec::with_capacity(n * n * 4);
    for (i, &(region, kind, density)) in regions.iter().enumerate() {
        // Broken cloud gathers: wide stretches of open sky, and clusters
        // with most of the region's cloud in them. A deck only varies.
        let local = if layer.floor > 0.0 {
            layer.coverage + (region - 0.5) * 2.0 * layer.coverage_spread
        } else {
            let t = ((region - 0.35) / 0.3).clamp(0.0, 1.0);
            layer.coverage * (0.05 + 1.9 * t * t * (3.0 - 2.0 * t))
        }
        .clamp(0.0, 1.0);
        // A wide soft band, so coverage rises slowly into a field. A
        // column's height follows its coverage, so the band is what rounds
        // a cloud into a dome and keeps its sides from standing as walls.
        let c = ((rank[i] - (1.0 - local)) / 0.6).clamp(0.0, 1.0);
        let c = layer.floor + (1.0 - layer.floor) * c;
        texels.extend([
            (c * 255.0).round() as u8,
            (kind * 255.0).round() as u8,
            (density * 255.0).round() as u8,
            255,
        ]);
    }
    Weather { texels }
}

/// How much of the sun reaches the eye through the layer, 0 to 1: a CPU
/// estimate for what cannot wait for the GPU's shadow map, such as the
/// lens's veil from a sun off the frame. It walks the sun's ray through
/// the layer over the weather map, at the wind's drift, and takes each
/// step's density as the mean the shape noise leaves at that coverage.
#[must_use]
pub(crate) fn sun_visibility(
    cover: CloudCover,
    eye_m: glam::Vec3,
    sun: glam::Vec3,
    time: f32,
) -> f32 {
    let (Some(layer), Some(weather)) = (Layer::of(cover), weather(cover)) else {
        return 1.0;
    };
    if sun.z <= 0.0 || layer.extinction <= 0.0 {
        return 1.0;
    }
    let drift = layer.wind_mps * 0.001 * time.max(0.0);
    let n = WEATHER_SIZE as usize;
    let eye = eye_m * 0.001;
    const STEPS: u32 = 12;
    let dh = layer.thickness_km / STEPS as f32;
    let mut depth = 0.0;
    for i in 0..STEPS {
        let h = layer.base_km + (i as f32 + 0.5) * dh;
        let xy = eye.truncate() + sun.truncate() / sun.z * (h - eye.z) + Vec2::new(-drift, 0.0);
        let uv = (xy / layer.tile_km).fract_gl();
        let (x, y) = (
            ((uv.x * n as f32) as usize).min(n - 1),
            ((uv.y * n as f32) as usize).min(n - 1),
        );
        let c = f32::from(weather.texels[(y * n + x) * 4]) / 255.0;
        // Mean body at this coverage: `remap(noise, 1 - c, 1) * c` over a
        // noise spread about a half.
        depth += layer.extinction * 0.5 * c * c * dh / sun.z;
    }
    (-depth).exp()
}

/// A tile of blue noise, [`BLUE_NOISE_SIZE`] a side, one byte a texel: the
/// rank of each texel in a void-and-cluster ordering (Ulichney 1993), so
/// every threshold of it is evenly spread and neighbours differ. Made once.
pub(crate) fn blue_noise() -> &'static [u8] {
    static TILE: OnceLock<Vec<u8>> = OnceLock::new();
    TILE.get_or_init(|| {
        let n = BLUE_NOISE_SIZE as i32;
        let count = (n * n) as usize;
        // Gaussian energy, sigma 1.5, on the torus.
        let radius = 6;
        let mut kernel = Vec::new();
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let d2 = (dx * dx + dy * dy) as f32;
                kernel.push((dx, dy, (-d2 / (2.0 * 1.5 * 1.5)).exp()));
            }
        }
        let mut energy = vec![0.0_f32; count];
        let mut placed = vec![false; count];
        let mut rank = vec![0_u8; count];
        // A fixed first point; after it, each next point is the emptiest
        // spot left — the middle of the largest void.
        let mut next = 0_usize;
        for order in 0..count {
            placed[next] = true;
            rank[next] = (order * 256 / count) as u8;
            let (x, y) = ((next as i32) % n, (next as i32) / n);
            for &(dx, dy, w) in &kernel {
                let i = ((y + dy).rem_euclid(n) * n + (x + dx).rem_euclid(n)) as usize;
                energy[i] += w;
            }
            let mut best = f32::MAX;
            for (i, &e) in energy.iter().enumerate() {
                if !placed[i] && e < best {
                    best = e;
                    next = i;
                }
            }
        }
        rank
    })
}

// --- tiling noise -----------------------------------------------------------

fn hash(x: u32, y: u32, seed: u32) -> u32 {
    let mut h = x
        .wrapping_mul(0x8da6_b343)
        .wrapping_add(y.wrapping_mul(0xd816_3841))
        .wrapping_add(seed.wrapping_mul(0xcb1a_b31f));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^ (h >> 15)
}

fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Inverted distance to the nearest of one jittered point per cell, over
/// `cells` cells that wrap.
fn worley(uv: Vec2, cells: u32, seed: u32) -> f32 {
    let p = uv * cells as f32;
    let cell = p.floor();
    let mut nearest = f32::MAX;
    for j in -1..=1 {
        for i in -1..=1 {
            let c = cell + Vec2::new(i as f32, j as f32);
            let wrapped = (
                (c.x as i32).rem_euclid(cells as i32) as u32,
                (c.y as i32).rem_euclid(cells as i32) as u32,
            );
            let h = hash(wrapped.0, wrapped.1, seed);
            let point = c + Vec2::new(unit(h), unit(hash(h, 0x51ed, seed)));
            nearest = nearest.min(point.distance_squared(p));
        }
    }
    (1.0 - nearest.sqrt()).clamp(0.0, 1.0)
}

/// Smooth value noise over a `cells`-periodic lattice.
fn value(uv: Vec2, cells: u32, seed: u32) -> f32 {
    let p = uv * cells as f32;
    let cell = p.floor();
    let f = p - cell;
    let f = f * f * (Vec2::splat(3.0) - 2.0 * f);
    let at = |i: i32, j: i32| {
        let x = (cell.x as i32 + i).rem_euclid(cells as i32) as u32;
        let y = (cell.y as i32 + j).rem_euclid(cells as i32) as u32;
        unit(hash(x, y, seed))
    };
    let top = at(0, 0) + (at(1, 0) - at(0, 0)) * f.x;
    let bottom = at(0, 1) + (at(1, 1) - at(0, 1)) * f.x;
    top + (bottom - top) * f.y
}

/// Value-noise fBm, 0 to 1, periodic over the tile.
fn fbm(uv: Vec2, cells: u32, octaves: u32, seed: u32) -> f32 {
    let mut sum = 0.0;
    let mut weight = 0.0;
    let mut amplitude = 1.0;
    for octave in 0..octaves {
        sum += value(uv, cells << octave, seed + octave * 7) * amplitude;
        weight += amplitude;
        amplitude *= 0.5;
    }
    sum / weight
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fraction_under_cloud(cover: CloudCover) -> f32 {
        let weather = weather(cover).expect("a clouded sky");
        let texels = weather.texels.chunks_exact(4);
        let count = texels.len();
        // The soft band at a clump's edge holds no visible cloud.
        texels.filter(|t| t[0] > 32).count() as f32 / count as f32
    }

    #[test]
    fn each_preset_covers_the_sky_it_names() {
        assert!(weather(CloudCover::Clear).is_none());
        if let Some(wispy) = weather(CloudCover::Wispy) {
            // Wispy is a cirrus sheet only; the cumulus map stays empty.
            assert!(wispy.texels.chunks_exact(4).all(|t| t[0] == 0));
        }
        let fair = fraction_under_cloud(CloudCover::FairWeather);
        let scattered = fraction_under_cloud(CloudCover::Scattered);
        assert!((0.15..0.4).contains(&fair), "fair weather covers {fair}");
        assert!(
            (0.4..0.7).contains(&scattered),
            "scattered covers {scattered}"
        );
        for deck in [CloudCover::Overcast, CloudCover::Storm] {
            assert_eq!(fraction_under_cloud(deck), 1.0, "{deck:?} has a gap");
        }
    }

    #[test]
    fn the_weather_varies_across_the_sky() {
        // Regions: coverage in one quarter of the tile differs from another's.
        let weather = weather(CloudCover::Scattered).expect("clouded");
        let n = WEATHER_SIZE as usize;
        let quarter = |qx: usize, qy: usize| {
            let mut sum = 0.0;
            for y in qy * n / 2..(qy + 1) * n / 2 {
                for x in qx * n / 2..(qx + 1) * n / 2 {
                    sum += f32::from(weather.texels[(y * n + x) * 4]);
                }
            }
            sum / (n * n / 4) as f32 / 255.0
        };
        let quarters = [quarter(0, 0), quarter(1, 0), quarter(0, 1), quarter(1, 1)];
        let spread = quarters.iter().copied().fold(f32::MIN, f32::max)
            - quarters.iter().copied().fold(f32::MAX, f32::min);
        assert!(spread > 0.05, "one pattern everywhere: {quarters:?}");
    }

    #[test]
    fn blue_noise_ranks_every_level_evenly() {
        let tile = blue_noise();
        assert_eq!(tile.len(), (BLUE_NOISE_SIZE * BLUE_NOISE_SIZE) as usize);
        let mut histogram = [0_u32; 256];
        for &v in tile {
            histogram[v as usize] += 1;
        }
        assert!(histogram.iter().all(|&c| c == 16), "{histogram:?}");
        // Neighbours differ: blue noise has little energy at low frequency.
        let n = BLUE_NOISE_SIZE as usize;
        let mut close = 0;
        for y in 0..n {
            for x in 0..n - 1 {
                let a = i32::from(tile[y * n + x]);
                let b = i32::from(tile[y * n + x + 1]);
                close += i32::from((a - b).abs() < 16);
            }
        }
        assert!(
            close < (n * (n - 1)) as i32 / 10,
            "{close} close neighbours"
        );
    }
}
