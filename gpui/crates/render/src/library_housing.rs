//! A library fixture's housing: its build's own meshes, posed by its joints,
//! with each cell's emitters lit by that cell's primitive.
//!
//! The build is in housing space, the frame a bundled `.qxf` mesh is drawn in,
//! so it hangs off the same mount `base`. Meshes are interned once per library
//! fixture (keyed by its `lib:` path) in the asset library and on the GPU; a
//! frame only poses them.

use glam::{Mat4, Vec3};
use luma_fixture::{Built, MaterialSlot};
use luma_scene::mesh::{Material, MeshData, Vertex};

use crate::assets::Library;
use crate::frame::{Bank, Draw, EditorObject, MaterialTextures};

/// Every mesh of `built` under `worlds` (housing-space node frames) at
/// `base`. An emitter mesh of cell `k` glows `emissive(k)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draws(
    built: &Built,
    key: &str,
    base: Mat4,
    worlds: &[Mat4],
    lib: &mut Library,
    bank: &mut Bank,
    editor_object: Option<&EditorObject>,
    emissive: impl Fn(u32) -> Vec3,
) -> Vec<Draw> {
    let mut out = Vec::new();
    for (n, (node, world)) in built.nodes.iter().zip(worlds).enumerate() {
        for (m, mesh) in node.meshes.iter().enumerate() {
            let mesh_key = format!("{key}#{n}.{m}");
            let index = bank.insert(mesh_key.clone(), || {
                lib.procedural(&mesh_key, || mesh_data(mesh))
            });
            let mut material = material(built, mesh.material);
            if let Some(cell) = mesh.cell {
                // Off, an emitter is the dark die behind its lens, tinted
                // the colour it makes; on, it is the light it gives.
                material.base_color *= 0.15;
                material.emissive = emissive(cell);
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
/// coordinates, so the tangent is any unit vector across the normal.
fn mesh_data(mesh: &luma_fixture::Mesh) -> MeshData {
    let vertices: Vec<Vertex> = mesh
        .positions
        .iter()
        .zip(&mesh.normals)
        .map(|(p, n)| {
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
                uv: [0.0, 0.0],
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
