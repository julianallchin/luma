//! A library fixture's housing: its build's own meshes, posed by its joints,
//! with each cell's emitters lit by that cell's primitive.
//!
//! The build is in housing space, the frame a bundled `.qxf` mesh is drawn in,
//! so it hangs off the same mount `base`. Meshes are interned once per library
//! fixture (keyed by its `lib:` path) in the asset library and on the GPU; a
//! frame only poses them.

use glam::{Mat4, Vec3};
use luma_fixture::{Built, MaterialSlot};
use luma_scene::mesh::{LedFace, Material, MeshData, Vertex};

use crate::assets::Library;
use crate::frame::{Bank, Draw, EditorObject, MaterialTextures};

/// Every mesh of `built` under `worlds` (housing-space node frames) at
/// `base`. `light(k, material)` lights the emitters of cell `k`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draws(
    built: &Built,
    key: &str,
    base: Mat4,
    worlds: &[Mat4],
    lib: &mut Library,
    bank: &mut Bank,
    editor_object: Option<&EditorObject>,
    light: impl Fn(u32, &mut Material),
) -> Vec<Draw> {
    let mut out = Vec::new();
    for (n, (node, world)) in built.nodes.iter().zip(worlds).enumerate() {
        let cover = cover(&node.meshes);
        for (m, mesh) in node.meshes.iter().enumerate() {
            let mesh_key = format!("{key}#{n}.{m}");
            let index = bank.insert(mesh_key.clone(), || {
                lib.procedural(&mesh_key, || mesh_data(mesh))
            });
            let mut material = material(built, mesh.material);
            if mesh.cell.is_some() {
                // Off, an emitter is the dark die behind its lens, tinted
                // the colour it makes; on, it is the light it gives.
                material.base_color *= 0.15;
            }
            if let Some(cell) = shows(mesh, cover) {
                light(cell, &mut material);
            }
            if let Some(pattern) = mesh.pattern {
                let gap = self::material(built, pattern.gap);
                material.led = Some(LedFace {
                    pattern,
                    gap_color: gap.base_color,
                    gap_metallic: gap.metallic,
                    gap_roughness: gap.roughness,
                });
            }
            out.push(Draw {
                mesh: index,
                model: base * *world,
                material,
                textures: MaterialTextures::default(),
                editor_object: editor_object.cloned(),
            });
        }
    }
    out
}

/// The cell a diffuser among `meshes` (one node's) covers: the one whose
/// emitters share its node, where they are one cell's. The build does not
/// say which emitters a diffuser is over, and a node of several cells gives
/// no way to tell.
fn cover(meshes: &[luma_fixture::Mesh]) -> Option<u32> {
    meshes
        .iter()
        .any(|mesh| mesh.material == MaterialSlot::Diffuser)
        .then(|| {
            let mut cells = meshes.iter().filter_map(|mesh| mesh.cell);
            let cell = cells.next()?;
            cells.all(|other| other == cell).then_some(cell)
        })
        .flatten()
}

/// The cell whose light `mesh` shows, on a node whose diffuser covers
/// `cover`: a diffuser is the lit face of the cell behind it, and the
/// emitters behind it show nothing.
fn shows(mesh: &luma_fixture::Mesh, cover: Option<u32>) -> Option<u32> {
    match cover {
        Some(cell) => (mesh.material == MaterialSlot::Diffuser).then_some(cell),
        None => mesh.cell,
    }
}

/// The area of cell `cell`'s lit face as its beam leaves it along `along`
/// (housing space, unit), square metres: what the cell's light is spread
/// over, so its radiance is its intensity over this.
pub(crate) fn emitting_area(built: &Built, worlds: &[Mat4], cell: u32, along: Vec3) -> f32 {
    let mut area = 0.0;
    for (node, world) in built.nodes.iter().zip(worlds) {
        let cover = cover(&node.meshes);
        for mesh in node
            .meshes
            .iter()
            .filter(|mesh| shows(mesh, cover) == Some(cell))
        {
            let at = |i: u32| world.transform_point3(Vec3::from(mesh.positions[i as usize]));
            for triangle in mesh.indices.chunks_exact(3) {
                let (a, b, c) = (at(triangle[0]), at(triangle[1]), at(triangle[2]));
                // Half the cross product is the triangle's area along its
                // normal; the faces turned away from the beam give none.
                area += ((b - a).cross(c - a).dot(along) / 2.0).max(0.0);
            }
        }
    }
    area
}

/// How far the body reaches along `along` (housing space, unit), metres
/// from the housing's origin: where its mouth is, for a lamp that looks
/// along `along`.
pub(crate) fn reach(built: &Built, worlds: &[Mat4], along: Vec3) -> f32 {
    built
        .nodes
        .iter()
        .zip(worlds)
        .flat_map(|(node, world)| {
            node.meshes.iter().flat_map(move |mesh| {
                mesh.positions
                    .iter()
                    .map(move |p| world.transform_point3(Vec3::from(*p)).dot(along))
            })
        })
        .fold(f32::MIN, f32::max)
}

/// The build's colour for `slot` as a renderer material. The spec writes
/// display (sRGB) colours; the renderer shades in linear light.
fn material(built: &Built, slot: MaterialSlot) -> Material {
    let Some((_, m)) = built.materials.iter().find(|(s, _)| *s == slot) else {
        return crate::materials::POWDER_COAT;
    };
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Material {
        base_color: Vec3::from(m.rgb.map(linear)),
        metallic: m.metal,
        roughness: m.rough.max(0.05),
        ..Material::default()
    }
}

/// One build mesh as an uploadable triangle list. The build has no texture
/// coordinates but an LED panel's place on its pattern, so the tangent is
/// any unit vector across the normal.
fn mesh_data(mesh: &luma_fixture::Mesh) -> MeshData {
    let vertices: Vec<Vertex> = mesh
        .positions
        .iter()
        .zip(&mesh.normals)
        .enumerate()
        .map(|(i, (p, n))| {
            let normal = Vec3::from(*n);
            let across = if normal.x.abs() < 0.9 {
                Vec3::X
            } else {
                Vec3::Y
            };
            let tangent = across.cross(normal).normalize_or_zero();
            Vertex {
                position: *p,
                normal: *n,
                uv: mesh.uvs.get(i).copied().unwrap_or_default(),
                tangent: tangent.extend(1.0).to_array(),
            }
        })
        .collect();
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: mesh.indices.clone().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lamp with one diffuser over its LEDs shows its light on the
    /// diffuser: that is the face anyone sees, and the dies behind it show
    /// nothing. The house's pendant is such a lamp.
    #[test]
    fn a_diffuser_over_a_cells_emitters_is_its_lit_face() {
        let mut library = Library::new(luma_scene::meshes_root());
        let pendant = library.fixture(crate::house::PENDANT).expect("the pendant");
        let worlds = luma_scene::library::worlds(&pendant, [0.0; 2]);
        let lit: Vec<MaterialSlot> = pendant
            .nodes
            .iter()
            .flat_map(|node| {
                let cover = cover(&node.meshes);
                node.meshes
                    .iter()
                    .filter(move |mesh| shows(mesh, cover).is_some())
                    .map(|mesh| mesh.material)
            })
            .collect();
        assert_eq!(lit, [MaterialSlot::Diffuser]);
        // The face is a disc the size of the lamp's opening, give or take
        // its rim.
        let (_, along) = luma_scene::library::beam(&pendant, &worlds, 0).unwrap();
        let face = emitting_area(&pendant, &worlds, 0, along.normalize());
        let opening = pendant.heads[0].size[0] / 2.0;
        let disc = std::f32::consts::PI * opening * opening;
        assert!(
            (0.5 * disc..=1.01 * disc).contains(&face),
            "the face is {face} m², the opening {disc} m²"
        );
        // And the house lamp's beam leaves through that opening.
        let room = luma_scene::Aabb::EMPTY;
        let lamp = crate::house::lamps(Default::default(), room)[0].cone();
        assert!(
            (0.95 * opening..=1.05 * opening).contains(&lamp.lens.radius),
            "the lamp's lens is {} m in radius, the opening {opening} m",
            lamp.lens.radius
        );
    }
}
