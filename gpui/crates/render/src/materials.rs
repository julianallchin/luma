//! Named physically based materials, and which one each bundled mesh wears.
//!
//! Every surface the renderer draws takes its constants from this table: the
//! generated truss, the fixture housings, the venue floor, and the bundled
//! stage-lab meshes. The table is the single source of those numbers. A mesh's
//! own glTF constants are used only where the table names nothing: the DJ
//! gear and speaker art, whose authored values are already plausible and
//! whose logos and screen prints are textures.
//!
//! Values are linear base colour (albedo for a dielectric, F0 for a metal),
//! metalness and perceptual roughness, in the ranges measured for the real
//! finish:
//!
//! | Preset | Base colour | Metallic | Roughness |
//! |---|---|---|---|
//! | [`ALUMINIUM`] | 0.91, 0.92, 0.92 | 1 | 0.38 |
//! | [`STEEL`] | 0.56, 0.57, 0.58 | 1 | 0.5 |
//! | [`POWDER_COAT`] | 0.03 | 0 | 0.5 |
//! | [`DECK`] | 0.05 | 0 | 0.7 |
//! | [`RUBBER`] | 0.02 | 0 | 0.9 |
//! | [`LED_FACE`] | 0.02 | 0 | 0.25 |
//! | [`VENUE_FLOOR`] | 0.035 | 0 | 0.55 |
//! | [`GROUND`] | the sky's ground albedo | 0 | 0.95 |
//!
//! A textured primitive keeps its texture as detail. The preset then sets the
//! texture's *mean*: the factor becomes the preset colour divided by the
//! texture's mean linear colour, so a carpet or brushed-metal map varies the
//! surface around the preset instead of darkening or tinting it.

use glam::Vec3;

use crate::assets::{Glb, Image, Material};

/// Mill-finish aluminium: truss and deck frames. The base colour is
/// aluminium's measured reflectance; the roughness is a brushed, handled
/// surface, not a polished one.
pub const ALUMINIUM: Material = Material {
    base_color: Vec3::new(0.91, 0.92, 0.92),
    metallic: 1.0,
    roughness: 0.38,
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

/// Black painted stage deck top.
pub const DECK: Material = Material {
    base_color: Vec3::splat(0.05),
    roughness: 0.7,
    ..PLAIN
};

/// Black rubber: cable cover bodies.
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
    flat_shading: false,
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
];

/// The two stage decks share one material set: aluminium legs and frame, a
/// charcoal-painted base frame, and a black top. The top's carpet map and
/// the frame's brushed-metal map stay on as detail.
const DECK_PARTS: &[(&str, Material)] = &[
    ("[Metal Silver]1", ALUMINIUM),
    ("_default_", ALUMINIUM),
    ("[0136_Charcoal]2", POWDER_COAT),
    ("[Carpet Plush Charcoal]", DECK),
];

/// Every fixture body is one bundled mesh under this directory, and every one
/// is a black powder-coated housing. The lens is not a separate material; its
/// light is the fixture's face light and emissive, which this does not touch.
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
            // Geometry facts stay the mesh's own: a mesh without normals is
            // flat-shaded whatever its finish.
            flat_shading: primitive.material.flat_shading,
            normal_scale: primitive.material.normal_scale,
            occlusion_strength: primitive.material.occlusion_strength,
            ..preset
        };
    }
}

/// Mean linear RGB of an sRGB-encoded image.
fn mean_linear(image: &Image) -> Vec3 {
    let decode = |c: u8| {
        let c = f32::from(c) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let mut sum = Vec3::ZERO;
    let mut count = 0.0;
    for pixel in image.rgba.chunks_exact(4) {
        sum += Vec3::new(decode(pixel[0]), decode(pixel[1]), decode(pixel[2]));
        count += 1.0;
    }
    if count > 0.0 {
        sum / count
    } else {
        Vec3::ONE
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use glam::Vec3;

    use super::{mean_linear, ALUMINIUM, DECK, FIXTURE_MESHES, MESHES, POWDER_COAT};
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
                Some("[Carpet Plush Charcoal]") => DECK,
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
