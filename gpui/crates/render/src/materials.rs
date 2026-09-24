//! Named physically based materials, and which one each bundled mesh wears.
//!
//! Every surface the renderer draws takes its constants from this table: the
//! generated truss, the fixture housings, the venue floor, the DJ gear and the
//! bundled stage-lab meshes. The table is the single source of those numbers.
//! A mesh's own glTF constants are used only where the table names nothing:
//! the speaker art, and the mixer's red accents.
//!
//! The DJ gear is in the table because its `SketchUp` export carries one flat
//! finish for everything (roughness 0.85, near-black factors multiplied onto
//! already-dark photo prints, for an albedo under 0.01) and no emission, so a
//! player read as unlit matte plastic with a dead screen.
//!
//! Values are linear base colour (albedo for a dielectric, F0 for a metal),
//! metalness and perceptual roughness, in the ranges measured for the real
//! finish:
//!
//! | Preset | Base colour | Metallic | Roughness |
//! |---|---|---|---|
//! | [`ALUMINIUM`] | 0.91, 0.92, 0.92 | 1 | 0.6 |
//! | [`STEEL`] | 0.56, 0.57, 0.58 | 1 | 0.5 |
//! | [`POWDER_COAT`] | 0.03 | 0 | 0.5 |
//! | [`CARPET`] | 0.05 | 0 | 0.95 |
//! | [`PLASTIC`] | 0.035 | 0 | 0.45 |
//! | [`RUBBER`] | 0.02 | 0 | 0.9 |
//! | [`SCREEN`] | 0.01 | 0 | 0.1 |
//! | [`LED_FACE`] | 0.02 | 0 | 0.25 |
//! | [`VENUE_FLOOR`] | 0.035 | 0 | 0.55 |
//! | [`GROUND`] | the sky's ground albedo | 0 | 0.95 |
//!
//! A textured primitive keeps its texture as detail. The preset then sets the
//! texture's *mean*: the factor becomes the preset colour divided by the
//! texture's mean linear colour, so a carpet or brushed-metal map varies the
//! surface around the preset instead of darkening or tinting it. A preset with
//! an emissive colour glows with the primitive's own print, so a screen shows
//! its picture.

use glam::Vec3;

use crate::assets::{Glb, Image, Material};

/// Mill-finish aluminium: truss and deck frames. The base colour is
/// aluminium's measured reflectance; the roughness is hire-stock truss,
/// oxidised, scuffed and handled, which reads dull grey with a soft sheen.
/// At 0.38 every tube caught the sun as a bright line.
pub const ALUMINIUM: Material = Material {
    base_color: Vec3::new(0.91, 0.92, 0.92),
    metallic: 1.0,
    roughness: 0.6,
    ..PLAIN
};

/// Galvanised steel: crowd barriers and scaffold.
pub const STEEL: Material = Material {
    base_color: Vec3::new(0.56, 0.57, 0.58),
    metallic: 1.0,
    roughness: 0.5,
    ..PLAIN
};

/// Black powder coat: fixture housings, stand legs and painted steel frames.
pub const POWDER_COAT: Material = Material {
    base_color: Vec3::splat(0.03),
    roughness: 0.5,
    ..PLAIN
};

/// Charcoal plush carpet: the deck tops. Fibre is a diffuser, so
/// nearly no sheen.
pub const CARPET: Material = Material {
    base_color: Vec3::splat(0.05),
    roughness: 0.95,
    ..PLAIN
};

/// Black moulded ABS: DJ player and mixer housings, printed panels and knobs.
/// Satin, so the sky and a wash draw soft highlights along its edges.
pub const PLASTIC: Material = Material {
    base_color: Vec3::splat(0.035),
    roughness: 0.45,
    ..PLAIN
};

/// An LCD behind glass: a dark, glossy face that emits its own picture.
///
/// The emissive factor multiplies the print (see [`apply`]); it is in the
/// same display units as [`LED_FACE`]'s pixels, which glow at 5, and is set
/// so a screen reads in a dark room without blooming and stays legible,
/// though washed, in daylight.
pub const SCREEN: Material = Material {
    base_color: Vec3::splat(0.01),
    roughness: 0.1,
    emissive: Vec3::splat(1.5),
    ..PLAIN
};

/// Black rubber: cable cover bodies and equipment feet.
pub const RUBBER: Material = Material {
    base_color: Vec3::splat(0.02),
    roughness: 0.9,
    ..PLAIN
};

/// The dark, glossy face of an LED pixel. Its light is the emissive term the
/// frame builder adds; this is what the face reflects when the pixel is off.
pub const LED_FACE: Material = Material {
    base_color: Vec3::splat(0.02),
    roughness: 0.25,
    ..PLAIN
};

/// An indoor venue floor: black painted concrete. Smooth enough to show a
/// soft sheen under a beam.
pub const VENUE_FLOOR: Material = Material {
    base_color: Vec3::splat(0.035),
    roughness: 0.55,
    ..PLAIN
};

/// Open ground outdoors. The base colour is set by the caller from the sky's
/// ground albedo, so the near floor and the ground the sky bounces light off
/// are one surface.
pub const GROUND: Material = Material {
    base_color: Vec3::splat(0.1),
    roughness: 0.95,
    ..PLAIN
};

/// The neutral fields every preset shares.
const PLAIN: Material = Material {
    base_color: Vec3::ONE,
    metallic: 0.0,
    roughness: 1.0,
    emissive: Vec3::ZERO,
    normal_scale: 1.0,
    occlusion_strength: 1.0,
};

/// What each bundled mesh's glTF materials become, by material name.
///
/// A name that is not listed keeps its authored constants. The test
/// `every_listed_material_exists` fails when a re-exported mesh renames one,
/// so an entry here cannot silently stop applying.
const MESHES: &[(&str, &[(&str, Material)])] = &[
    ("stage_lab/truss_q30_1.22m.glb", &[("_default_", ALUMINIUM)]),
    ("stage_lab/truss_q30_box.glb", &[("_default_", ALUMINIUM)]),
    (
        "stage_lab/truss_q30x45_rect.glb",
        &[("_default_", ALUMINIUM)],
    ),
    ("stage_lab/truss_q40_1.83m.glb", &[("_default_", ALUMINIUM)]),
    ("stage_lab/stage_praticavel_1x1.glb", DECK_PARTS),
    ("stage_lab/stage_praticavel_2x1x1.glb", DECK_PARTS),
    ("stage_lab/guardrail.glb", &[("<auto>29", STEEL)]),
    ("stage_lab/cable_cover.glb", &[("[Color M07]10", RUBBER)]),
    (
        "stage_lab/speaker_stand.glb",
        &[("[Color_008]1", POWDER_COAT)],
    ),
    ("stage_lab/cdj_3000x.glb", CDJ_PARTS),
    ("stage_lab/mixer_djm_a9.glb", MIXER_PARTS),
];

/// A CDJ-3000: rubber feet, the screen, and moulded plastic for the rest —
/// body, top plate print (jog wheel included; the export does not separate
/// it), front panel print, encoder and the labels.
const CDJ_PARTS: &[(&str, Material)] = &[
    ("M08_Obsidian_Black", RUBBER),
    ("[Color M08]29", PLASTIC),
    ("M09_Shadow_Night", PLASTIC),
    ("Screenshot_102", PLASTIC),
    ("<auto>26", PLASTIC),
    ("(21) 99064-4321", PLASTIC),
    ("*248", PLASTIC),
    ("*249", PLASTIC),
    ("CDJ-2000-top1", PLASTIC),
    ("Screenshot_104", SCREEN),
    ("__skp_image_0", PLASTIC),
];

/// A DJM-A9: plastic body, prints and knobs. `<Red>` keeps its authored
/// accent colour.
const MIXER_PARTS: &[(&str, Material)] = &[
    ("DJM-A9-cgi-rear-pc", PLASTIC),
    ("Material220", PLASTIC),
    ("DJM-A9-cgi-front-pc", PLASTIC),
    ("Material218", PLASTIC),
    ("Material217", PLASTIC),
    ("DJM-A9-cgi-top-pc", PLASTIC),
    ("Material219", PLASTIC),
    ("__skp_image_0", PLASTIC),
];

/// Textures whose contrast is compressed toward their mean, by material name,
/// as the exponent applied to each texel's luminance ratio to the mean: 1
/// keeps the map, 0 flattens it.
///
/// The deck carpet is a stock `SketchUp` plush map: 10:1 between its 5th and
/// 95th percentile texels in linear light, far more than a real carpet at
/// arm's length, so the tabletop read as coarse gravel. At 0.4 it is 2.5:1.
const DETAIL: &[(&str, f32)] = &[("[Carpet Plush Charcoal]", 0.4)];

/// The two stage decks share one material set: aluminium legs and frame, a
/// charcoal-painted base frame, and a carpeted top. The top's carpet map and
/// the frame's brushed-metal map stay on as detail.
const DECK_PARTS: &[(&str, Material)] = &[
    ("[Metal Silver]1", ALUMINIUM),
    ("_default_", ALUMINIUM),
    ("[0136_Charcoal]2", POWDER_COAT),
    ("[Carpet Plush Charcoal]", CARPET),
];

/// Every fixture body is one bundled mesh under this directory, and every one
/// is a black powder-coated housing. The lens is not a separate material; its
/// light is the fixture's emissive, which this does not touch.
const FIXTURE_MESHES: &str = "qlc/";

/// The preset for material `name` of the bundled mesh at `asset`, if the table
/// names one.
fn preset(asset: &str, name: Option<&str>) -> Option<Material> {
    if asset.starts_with(FIXTURE_MESHES) {
        return Some(POWDER_COAT);
    }
    let (_, parts) = MESHES.iter().find(|(path, _)| *path == asset)?;
    let name = name?;
    parts
        .iter()
        .find(|(material, _)| *material == name)
        .map(|(_, preset)| *preset)
}

/// Replace the constants of every primitive in `glb` that the table names.
///
/// Applied once, when the library loads the mesh at `asset`, so every draw of
/// it (the lit stage, the builder's ghost, the golden captures) wears the same
/// finish.
pub(crate) fn apply(asset: &str, glb: &mut Glb) {
    let mut flattened = Vec::new();
    for primitive in &glb.primitives {
        let detail = DETAIL
            .iter()
            .find(|(name, _)| primitive.material_name.as_deref() == Some(*name));
        if let (Some(&(_, detail)), Some(image)) = (detail, primitive.base_color_image) {
            if !flattened.contains(&image) {
                flatten(&mut glb.images[image], detail);
                flattened.push(image);
            }
        }
    }
    for primitive in &mut glb.primitives {
        let Some(preset) = preset(asset, primitive.material_name.as_deref()) else {
            continue;
        };
        let texture_mean = primitive
            .base_color_image
            .and_then(|i| glb.images.get(i))
            .map_or(Vec3::ONE, mean_linear);
        primitive.material = Material {
            base_color: preset.base_color / texture_mean.max(Vec3::splat(1e-3)),
            normal_scale: primitive.material.normal_scale,
            occlusion_strength: primitive.material.occlusion_strength,
            ..preset
        };
        if preset.emissive != Vec3::ZERO && primitive.emissive_image.is_none() {
            primitive.emissive_image = primitive.base_color_image;
        }
    }
}

/// Compress `image`'s contrast toward its mean: each texel's luminance ratio
/// to the mean is raised to `detail`, hue kept.
fn flatten(image: &mut Image, detail: f32) {
    let luminance = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    let mean = luminance(mean_linear(image)).max(1e-4);
    let rgba: Vec<u8> = image
        .rgba
        .chunks_exact(4)
        .flat_map(|px| {
            let c = Vec3::new(decode(px[0]), decode(px[1]), decode(px[2]));
            let y = luminance(c).max(1e-5);
            let c = c * (y / mean).powf(detail - 1.0);
            [encode(c.x), encode(c.y), encode(c.z), px[3]]
        })
        .collect();
    image.rgba = rgba.into();
}

fn decode(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// Mean linear RGB of an sRGB-encoded image.
fn mean_linear(image: &Image) -> Vec3 {
    let mut sum = Vec3::ZERO;
    let mut count = 0.0;
    for pixel in image.rgba.chunks_exact(4) {
        sum += Vec3::new(decode(pixel[0]), decode(pixel[1]), decode(pixel[2]));
        count += 1.0;
    }
    if count > 0.0 { sum / count } else { Vec3::ONE }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use glam::Vec3;

    use super::{ALUMINIUM, CARPET, FIXTURE_MESHES, MESHES, POWDER_COAT, mean_linear};
    use crate::assets::Library;

    fn meshes() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes")
    }

    /// A renamed material in a re-exported mesh would otherwise fall back to
    /// its authored constants without anyone noticing.
    #[test]
    fn every_listed_material_exists() {
        for (asset, parts) in MESHES {
            let glb = gltf::Gltf::open(meshes().join(asset)).expect(asset);
            let names: Vec<_> = glb.materials().filter_map(|m| m.name()).collect();
            for (name, _) in *parts {
                assert!(names.contains(name), "{asset} has no material {name:?}");
            }
        }
    }

    /// Every fixture housing is powder coat, whatever its authored material.
    #[test]
    fn fixture_housings_are_powder_coat() {
        let mut library = Library::new(meshes());
        let dir = meshes().join(FIXTURE_MESHES);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let file = entry.unwrap().file_name().into_string().unwrap();
            let glb = library.get(&format!("{FIXTURE_MESHES}{file}")).unwrap();
            for primitive in &glb.primitives {
                assert_eq!(
                    primitive.material.base_color, POWDER_COAT.base_color,
                    "{file}"
                );
                assert!((primitive.material.roughness - POWDER_COAT.roughness).abs() < 1e-6);
            }
        }
    }

    /// A textured deck part keeps its map, and the map's mean lands on the
    /// preset: the carpet is a black deck with a weave, not a grey one.
    #[test]
    fn textured_parts_average_to_their_preset() {
        let mut library = Library::new(meshes());
        let glb = library.get("stage_lab/stage_praticavel_2x1x1.glb").unwrap();
        let mut seen = 0;
        for primitive in &glb.primitives {
            let Some(image) = primitive.base_color_image else {
                continue;
            };
            let mean = primitive.material.base_color * mean_linear(&glb.images[image]);
            let preset = match primitive.material_name.as_deref() {
                Some("[Carpet Plush Charcoal]") => CARPET,
                Some("[Metal Silver]1") => ALUMINIUM,
                other => panic!("unexpected textured deck part {other:?}"),
            };
            assert!(
                (mean - preset.base_color).abs().max_element() < 1e-3,
                "{mean} vs {}",
                preset.base_color
            );
            assert!((primitive.material.metallic - preset.metallic).abs() < 1e-6);
            seen += 1;
        }
        assert_eq!(seen, 2);
    }

    /// A CDJ's screen glows with its own picture; its housing does not glow.
    #[test]
    fn screens_emit_their_print() {
        let mut library = Library::new(meshes());
        let glb = library.get("stage_lab/cdj_3000x.glb").unwrap();
        for primitive in &glb.primitives {
            let screen = primitive.material_name.as_deref() == Some("Screenshot_104");
            assert_eq!(primitive.material.emissive != Vec3::ZERO, screen);
            if screen {
                assert_eq!(primitive.emissive_image, primitive.base_color_image);
            }
        }
    }

    /// The deck carpet's weave is compressed toward its mean.
    #[test]
    fn carpet_detail_is_flattened() {
        let mut library = Library::new(meshes());
        let glb = library.get("stage_lab/stage_praticavel_2x1x1.glb").unwrap();
        let carpet = glb
            .primitives
            .iter()
            .find(|p| p.material_name.as_deref() == Some("[Carpet Plush Charcoal]"))
            .unwrap();
        let image = &glb.images[carpet.base_color_image.unwrap()];
        let mut values: Vec<u8> = image.rgba.chunks_exact(4).map(|px| px[1]).collect();
        values.sort_unstable();
        let at = |q: f32| super::decode(values[(q * (values.len() - 1) as f32) as usize]);
        let spread = at(0.95) / at(0.05);
        assert!(spread < 3.5, "5-95% linear spread {spread}");
    }

    /// Nothing in the table is outside the physically measured ranges: no
    /// dielectric darker than charcoal or brighter than fresh snow, and no
    /// metal with the reflectance of a dielectric.
    #[test]
    fn presets_are_physically_plausible() {
        for (_, parts) in MESHES {
            for (name, m) in *parts {
                let lo = m.base_color.min_element();
                let hi = m.base_color.max_element();
                if m.metallic > 0.5 {
                    assert!(lo >= 0.5, "{name}: metal F0 {lo}");
                } else {
                    assert!((0.01..=0.9).contains(&lo) && hi <= 0.9, "{name}: albedo");
                }
                assert!((0.05..=1.0).contains(&m.roughness), "{name}: roughness");
            }
        }
        assert_eq!(
            mean_linear(&crate::assets::Image {
                width: 1,
                height: 1,
                rgba: std::sync::Arc::from([255u8, 255, 255, 255]),
            }),
            Vec3::ONE
        );
    }
}
