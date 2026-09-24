//! Floor materials: the ground's maps, and the numbers `floor.wgsl` reads.
//!
//! Each [`Floor`] is a CC0 material set in `resources/meshes/floors`, made by
//! that directory's `fetch.py` from Poly Haven and ambientCG. `floors.json`
//! there is the one list of sets: the script reads it for where each set
//! comes from and what albedo it is scaled to, and this module reads it for
//! how the renderer lays the set down — its tile, its parallax depth, how
//! uneven the ground is and how much it varies over tens of metres. Grass and
//! dirt each name a second set that breaks through in patches.
//!
//! On the GPU a set is two images in the ground draw's material slots:
//! albedo with height in alpha (sRGB), and normal X and Y, roughness and
//! occlusion (linear). The transition set takes two more. The maps are 2K;
//! [`Quality::Low`] halves them on load and turns parallax off.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

use crate::assets::{Image, Library};
use crate::scene_desc::{Floor, Quality};

/// One set, as `floors.json` spells it. The source fields are the script's.
#[derive(Debug, Deserialize)]
struct Set {
    /// Mean linear albedo the colour map is scaled to.
    albedo: f32,
    /// The packed colour map's mean linear colour (`fetch.py` writes it).
    mean_rgb: [f32; 3],
    /// World size of one tile, metres.
    tile_m: f32,
    /// The least roughness the ground has at the scale it is seen from.
    /// A scan's roughness is a blade's or a grain's; a field of them
    /// scatters the light over every angle its blades face, so grass is
    /// matte however glossy one blade is.
    #[serde(default)]
    min_roughness: f32,
    /// Depth of the height field, metres; zero draws no parallax.
    #[serde(default)]
    parallax_m: f32,
    /// Slope spread of the ground above the tile's scale, as a GGX alpha.
    #[serde(default)]
    unevenness: f32,
    /// Strength of the brightness, warmth and roughness variation over tens
    /// of metres.
    #[serde(default)]
    variation: f32,
    /// The two ends of the colour the ground varies between over metres to
    /// tens of metres, as multipliers of the albedo: dry and lush grass, damp
    /// and dry earth, stained and worn concrete.
    #[serde(default = "no_tints")]
    tints: [[f32; 3]; 2],
    /// Whether the maps are hex tiled: false for planks, whose joints must
    /// line up. They repeat plainly, as a laid floor does.
    #[serde(default = "yes")]
    hex: bool,
    /// The most a hex cell's copy is turned, degrees, either way. A set with
    /// a direction (wind ripples, form strips, carpet rows) gets 0: its
    /// cells are only shifted, so the direction holds across them.
    #[serde(default = "half_turn")]
    max_rotation_deg: f32,
    /// The set that breaks through in patches, if any.
    #[serde(default)]
    transition: Option<String>,
    /// The share of the ground the transition covers.
    #[serde(default)]
    transition_share: f32,
}

fn no_tints() -> [[f32; 3]; 2] {
    [[1.0; 3]; 2]
}

fn yes() -> bool {
    true
}

fn half_turn() -> f32 {
    180.0
}

fn manifest() -> &'static BTreeMap<String, Set> {
    static MANIFEST: OnceLock<BTreeMap<String, Set>> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../../resources/meshes/floors/floors.json"
        ))
        .expect("floors.json is a map of sets")
    })
}

fn set(name: &str) -> &'static Set {
    manifest()
        .get(name)
        .unwrap_or_else(|| panic!("floors.json has no set {name:?}"))
}

/// The floor's mean albedo, as luminance.
#[must_use]
pub fn mean_albedo(floor: Floor) -> f32 {
    set(floor.set()).albedo
}

/// The floor's mean colour, linear RGB, as `floor.wgsl` draws it on
/// average: the colour map's mean, the transition set's for the share of
/// the ground it covers, and the macro variation's two tints at their mean
/// (the zone noise sits at one half on average; the mottle averages out).
/// What the sky's ground and the ambient light bounce off, so a box on
/// grass is lit green from below and the near floor and the ground at the
/// horizon are one surface.
#[must_use]
pub fn mean_color(floor: Floor) -> glam::Vec3 {
    let set = set(floor.set());
    let mut color = glam::Vec3::from(set.mean_rgb);
    if let Some(transition) = set.transition.as_deref() {
        let other = glam::Vec3::from(self::set(transition).mean_rgb);
        color = color.lerp(other, set.transition_share);
    }
    let [a, b] = set.tints;
    let tint = (glam::Vec3::from(a) + glam::Vec3::from(b)) * 0.5;
    color * glam::Vec3::ONE.lerp(tint, set.variation)
}

/// One frame's floor: which set, and the numbers `floor.wgsl` reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    /// The floor.
    pub floor: Floor,
    /// Tile size, metres.
    pub tile_m: f32,
    /// The least roughness the ground is drawn with.
    pub min_roughness: f32,
    /// Parallax depth, metres; zero draws none.
    pub parallax_m: f32,
    /// Slope spread above the tile's scale, as a GGX alpha.
    pub unevenness: f32,
    /// Strength of the variation over tens of metres.
    pub variation: f32,
    /// The transition set's tile, metres, or zero when there is none.
    pub transition_tile_m: f32,
    /// The share of the ground the transition covers.
    pub transition_share: f32,
    /// The two ends of the ground's colour variation, as albedo multipliers.
    pub tints: [[f32; 3]; 2],
    /// Whether the maps are hex tiled; planks are not (`hex` in
    /// `floors.json`), and keep only the variation.
    pub hex: bool,
    /// The most a hex copy of the set is turned, as a share of a half turn.
    pub max_rotation: f32,
    /// The same for the transition set.
    pub transition_max_rotation: f32,
    /// Whether the maps are laid down through hex tiling and macro
    /// variation. Off, every set repeats plainly every tile: the picture the
    /// anti-tiling is measured against.
    pub anti_tiling: bool,
}

impl Surface {
    /// `floor` as drawn at `quality`. [`Quality::Low`] draws no parallax.
    #[must_use]
    pub fn of(floor: Floor, quality: Quality) -> Self {
        let set = set(floor.set());
        let transition = set.transition.as_deref().map(self::set);
        Self {
            floor,
            tile_m: set.tile_m,
            min_roughness: set.min_roughness,
            parallax_m: match quality {
                Quality::High => set.parallax_m,
                Quality::Low => 0.0,
            },
            unevenness: set.unevenness,
            variation: set.variation,
            transition_tile_m: transition.map_or(0.0, |t| t.tile_m),
            transition_share: set.transition_share,
            tints: set.tints,
            hex: set.hex,
            max_rotation: set.max_rotation_deg / 180.0,
            transition_max_rotation: transition.map_or(0.0, |t| t.max_rotation_deg / 180.0),
            anti_tiling: true,
        }
    }

    /// `globals.floor_a` to `floor_d`.
    pub(crate) fn uniform(&self) -> [[f32; 4]; 4] {
        // 0: hex tiling and variation; 1: plain tiling and no variation, the
        // picture the anti-tiling is measured against; 2: plain tiling with
        // the variation, for planks.
        let tiling = if !self.anti_tiling {
            1.0
        } else if self.hex {
            0.0
        } else {
            2.0
        };
        let [a, b] = self.tints;
        [
            [
                self.tile_m,
                self.parallax_m,
                self.unevenness,
                self.variation,
            ],
            [
                self.transition_tile_m,
                self.transition_share,
                tiling,
                self.min_roughness,
            ],
            [a[0], a[1], a[2], self.max_rotation],
            [b[0], b[1], b[2], self.transition_max_rotation],
        ]
    }

    /// The transition set's name, if the floor has one.
    pub(crate) fn transition(&self) -> Option<&'static str> {
        set(self.floor.set()).transition.as_deref()
    }
}

/// One set's two GPU images: albedo and height, then normal, roughness and
/// occlusion.
#[derive(Clone)]
pub struct Maps {
    /// sRGB albedo, linear height in alpha.
    pub albedo: Image,
    /// Normal X and Y, roughness, occlusion; all linear.
    pub detail: Image,
}

/// Decode and pack set `name` from `root/floors/name`, halved when `half`.
pub(crate) fn load(root: &std::path::Path, name: &str, half: bool) -> anyhow::Result<Maps> {
    let dir = root.join("floors").join(name);
    let read = |file: &str| -> anyhow::Result<image::RgbImage> {
        let path = dir.join(file);
        Ok(image::load(
            std::io::BufReader::new(std::fs::File::open(&path)?),
            image::ImageFormat::Jpeg,
        )
        .map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?
        .into_rgb8())
    };
    let albedo = read("albedo.jpg")?;
    let normal = read("normal.jpg")?;
    let (width, height) = albedo.dimensions();
    anyhow::ensure!(
        normal.dimensions() == (width, height),
        "{name}: albedo and normal maps differ in size"
    );
    // Roughness and occlusion ship at half size; bring them up to the rest.
    let surface = image::imageops::resize(
        &read("surface.jpg")?,
        width,
        height,
        image::imageops::FilterType::Triangle,
    );
    let mut a = Vec::with_capacity((width * height * 4) as usize);
    let mut d = Vec::with_capacity((width * height * 4) as usize);
    for ((c, n), s) in albedo.pixels().zip(normal.pixels()).zip(surface.pixels()) {
        a.extend([c[0], c[1], c[2], n[2]]);
        d.extend([n[0], n[1], s[0], s[1]]);
    }
    let pack = |rgba: Vec<u8>| {
        let image = image::RgbaImage::from_raw(width, height, rgba).expect("sized above");
        let image = if half {
            image::imageops::resize(
                &image,
                width / 2,
                height / 2,
                image::imageops::FilterType::Triangle,
            )
        } else {
            image
        };
        Image {
            width: image.width(),
            height: image.height(),
            rgba: Arc::from(image.into_raw()),
        }
    };
    Ok(Maps {
        albedo: pack(a),
        detail: pack(d),
    })
}

/// A floor's sets, loaded through `library`'s cache: its own and its
/// transition's.
pub(crate) fn maps(
    library: &mut Library,
    surface: &Surface,
    quality: Quality,
) -> anyhow::Result<(Maps, Option<Maps>)> {
    let half = quality == Quality::Low;
    let own = library.floor(surface.floor.set(), half)?;
    let transition = surface
        .transition()
        .map(|name| library.floor(name, half))
        .transpose()?;
    Ok((own, transition))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each set's tile as its source states it: Poly Haven's `dimensions`
    /// and ambientCG's `dimensionX` (API v2), in metres. ambientCG gives no
    /// size for Carpet012; one cut-pile tuft is about 12 of the map's 2048
    /// pixels, and a tuft is 5 to 7 mm across, so the map is about a metre.
    #[test]
    fn every_set_is_its_source_size() {
        let stated = [
            ("grass", 1.40),
            ("dirt", 1.75),
            ("fine_sand", 2.07),
            ("beach", 3.00),
            ("gravelly_sand", 2.48),
            ("asphalt", 4.60),
            ("concrete", 3.00),
            ("gravel", 2.00),
            ("stage_deck", 1.90),
            ("hall_floor", 1.70),
            ("black_stage", 0.75),
            ("carpet", 1.00),
            ("moss", 2.10),
            ("mud", 1.30),
        ];
        for (name, metres) in stated {
            assert!((set(name).tile_m - metres).abs() < 0.005, "{name}");
        }
        assert_eq!(manifest().len(), stated.len(), "a set with no stated size");
    }

    #[test]
    fn every_set_records_its_albedo_mean_colour() {
        // `mean_rgb` is the packed map's own mean, so the sky's ground and
        // the ambient bounce take the colour the floor is drawn with.
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
        let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        for (name, set) in manifest() {
            let image = image::open(root.join("floors").join(name).join("albedo.jpg"))
                .unwrap()
                .into_rgb8();
            let mut sum = [0.0f64; 3];
            for pixel in image.pixels() {
                for c in 0..3 {
                    sum[c] += f64::from(crate::coords::srgb_to_linear(f32::from(pixel[c]) / 255.0));
                }
            }
            let n = f64::from(image.width() * image.height());
            let measured = sum.map(|v| (v / n) as f32);
            for c in 0..3 {
                assert!(
                    (measured[c] - set.mean_rgb[c]).abs() < 0.002,
                    "{name}: mean_rgb {:?}, the map's mean {measured:?}; run fetch.py --means",
                    set.mean_rgb
                );
            }
            assert!(
                (luma(set.mean_rgb) - set.albedo).abs() < 0.01,
                "{name}: mean_rgb {:?} is not the set's albedo {}",
                set.mean_rgb,
                set.albedo
            );
        }
    }

    #[test]
    fn every_offered_floor_has_a_set_and_every_transition_exists() {
        for floor in Floor::OUTDOOR.iter().chain(&Floor::INDOOR) {
            let surface = Surface::of(*floor, Quality::High);
            assert!(surface.tile_m > 0.0, "{floor:?}");
            assert!(
                surface.min_roughness > 0.0,
                "{floor:?} has no least roughness"
            );
            assert!((0.02..0.5).contains(&mean_albedo(*floor)), "{floor:?}");
            if let Some(name) = surface.transition() {
                assert!(set(name).tile_m > 0.0, "{floor:?} -> {name}");
            }
        }
    }
}
