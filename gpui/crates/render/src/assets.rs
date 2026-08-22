//! glTF/GLB import: CPU meshes, materials and the node tree, no GPU.
//!
//! Spec §1 gives this its own crate (`luma-assets`) once there is a second
//! consumer; it is a module here so the first one can exist. The node tree is
//! kept rather than flattened because fixture articulation is expressed in it —
//! `arm` takes pan, `head` takes tilt, and the face light hangs off `head`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec3, Vec4};

/// Interleaved position + normal + `TEXCOORD_0` + tangent basis.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Model-space position.
    pub position: [f32; 3],
    /// Model-space normal, unit length.
    pub normal: [f32; 3],
    /// `TEXCOORD_0`, or zero when the primitive carries none.
    pub uv: [f32; 2],
    /// Unit tangent xyz and bitangent handedness in w. Generated when glTF
    /// omits `TANGENT` so normal maps never depend on an optional attribute.
    pub tangent: [f32; 4],
}

/// A decoded `baseColorTexture`, sRGB-encoded RGBA8, tightly packed.
#[derive(Clone)]
pub struct Image {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// `width * height * 4` bytes, sRGB-encoded (the GPU decodes on sample).
    pub rgba: Arc<[u8]>,
}

/// The glTF subset the `Pbr` material path consumes.
#[derive(Debug, Clone, Copy)]
pub struct Material {
    /// glTF `baseColorFactor`, already linear. Multiplied by the primitive's
    /// base-colour texture when it has one, as glTF specifies.
    pub base_color: Vec3,
    /// glTF `metallicFactor`.
    pub metallic: f32,
    /// glTF `roughnessFactor`.
    pub roughness: f32,
    /// glTF `emissiveFactor`, added to the shaded result unlit.
    pub emissive: Vec3,
    /// Scalar applied to the tangent-space normal map's XY channels.
    pub normal_scale: f32,
    /// Strength of the occlusion map's red channel on ambient contribution.
    pub occlusion_strength: f32,
    /// three's `flatShading`. `GLTFLoader` sets it on any primitive that ships
    /// without a `NORMAL` attribute — every `stage_lab` GLB — and the shader
    /// then takes the normal from screen-space derivatives, ignoring whatever
    /// is in the attribute. Smoothing those normals instead is the difference
    /// between a faceted cabinet and a lit blob.
    pub flat_shading: bool,
}

impl Default for Material {
    /// three's `MeshStandardMaterial` defaults, which is what a mesh with no
    /// glTF material gets.
    fn default() -> Self {
        Self {
            base_color: Vec3::ONE,
            metallic: 0.0,
            roughness: 1.0,
            emissive: Vec3::ZERO,
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            flat_shading: false,
        }
    }
}

/// One triangle list with one material — a glTF mesh primitive.
pub struct Primitive {
    /// Interleaved vertex data.
    pub vertices: Arc<[Vertex]>,
    /// Triangle list into `vertices`.
    pub indices: Arc<[u32]>,
    /// Material shared by every triangle here.
    pub material: Material,
    /// Index into [`Glb::images`] of the `baseColorTexture`, if any. A resource
    /// reference, so it lives beside the material constants rather than in
    /// them: the frame re-indexes it into its own image table.
    pub base_color_image: Option<usize>,
    /// Linear normal map image.
    pub normal_image: Option<usize>,
    /// Linear packed map: green roughness, blue metallic.
    pub metallic_roughness_image: Option<usize>,
    /// Linear occlusion map image; red is visibility.
    pub occlusion_image: Option<usize>,
    /// sRGB emissive map image.
    pub emissive_image: Option<usize>,
}

/// One glTF node. Names matter: fixture articulation hangs off `arm`/`head`.
pub struct Node {
    /// glTF node name, when the asset carries one.
    pub name: Option<String>,
    /// Index of the parent node, always lower than this node's own.
    pub parent: Option<usize>,
    /// Transform relative to the parent.
    pub local: Mat4,
    /// Indices into [`Glb::primitives`].
    pub primitives: Vec<usize>,
}

/// One loaded GLB, flattened to a node list in topological order.
pub struct Glb {
    /// Topologically ordered: a parent always precedes its children.
    pub nodes: Vec<Node>,
    /// Every primitive in the file, referenced by index from [`Node`].
    pub primitives: Vec<Primitive>,
    /// Every image in the file, referenced by index from [`Material`].
    pub images: Vec<Image>,
}

impl Glb {
    /// World matrices for every node, parents before children.
    #[must_use]
    pub fn world_matrices(&self, root: Mat4, overrides: &HashMap<usize, Mat4>) -> Vec<Mat4> {
        let mut out = Vec::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            let local = overrides.get(&i).copied().unwrap_or(node.local);
            let parent = node.parent.map_or(root, |p| out[p]);
            out.push(parent * local);
        }
        out
    }

    /// Index of the first node with this glTF name.
    #[must_use]
    pub fn node_index(&self, name: &str) -> Option<usize> {
        self.nodes
            .iter()
            .position(|n| n.name.as_deref() == Some(name))
    }

    /// Axis-aligned bounds of every primitive under `root`, in root space.
    /// Mirrors three's `Box3.setFromObject`, which is what
    /// `applyPhysicalDimensionScaling` measures.
    #[must_use]
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let worlds = self.world_matrices(Mat4::IDENTITY, &HashMap::new());
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for (node, world) in self.nodes.iter().zip(&worlds) {
            for &p in &node.primitives {
                for v in self.primitives[p].vertices.iter() {
                    let w = world.transform_point3(Vec3::from(v.position));
                    lo = lo.min(w);
                    hi = hi.max(w);
                }
            }
        }
        if lo.x > hi.x {
            (Vec3::ZERO, Vec3::ZERO)
        } else {
            (lo, hi)
        }
    }
}

/// Loads and caches GLBs by path so repeated fixtures share one parse.
#[derive(Default)]
pub struct Library {
    root: PathBuf,
    loaded: HashMap<String, Glb>,
}

impl Library {
    /// A library rooted at the directory holding the mesh tree.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            loaded: HashMap::new(),
        }
    }

    /// The parsed asset at `rel`, loading it on first use.
    ///
    /// # Errors
    /// Fails if the file is missing or is not a readable glTF binary.
    pub fn get(&mut self, rel: &str) -> anyhow::Result<&Glb> {
        if !self.loaded.contains_key(rel) {
            let glb = load(&self.root.join(rel))?;
            self.loaded.insert(rel.to_string(), glb);
        }
        Ok(&self.loaded[rel])
    }
}

/// Depth-first append. Pushing before recursing is what guarantees
/// `parent < child` in `nodes`, which the world-matrix pass relies on.
fn walk(
    node: &gltf::Node,
    parent: Option<usize>,
    buffers: &[gltf::buffer::Data],
    nodes: &mut Vec<Node>,
    primitives: &mut Vec<Primitive>,
) {
    let index = nodes.len();
    nodes.push(Node {
        name: node.name().map(str::to_owned),
        parent,
        local: Mat4::from_cols_array_2d(&node.transform().matrix()),
        primitives: Vec::new(),
    });
    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            if let Some(p) = read_primitive(&prim, buffers) {
                nodes[index].primitives.push(primitives.len());
                primitives.push(p);
            }
        }
    }
    for child in node.children() {
        walk(&child, Some(index), buffers, nodes, primitives);
    }
}

fn load(path: &Path) -> anyhow::Result<Glb> {
    let (doc, buffers, images) = gltf::import(path)?;
    let mut primitives = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    for scene in doc.scenes() {
        for node in scene.nodes() {
            walk(&node, None, &buffers, &mut nodes, &mut primitives);
        }
    }
    Ok(Glb {
        nodes,
        primitives,
        images: images.iter().map(to_rgba8).collect(),
    })
}

/// Widen whatever channel layout the decoder produced to RGBA8. glTF images are
/// sRGB-encoded for base colour; the bytes pass through untouched.
fn to_rgba8(data: &gltf::image::Data) -> Image {
    use gltf::image::Format;
    let (stride, alpha) = match data.format {
        Format::R8 => (1, false),
        Format::R8G8 => (2, false),
        Format::R8G8B8 => (3, false),
        Format::R8G8B8A8 => (4, true),
        // 16-bit and float images do not occur in the stage library; a flat
        // white stands in rather than mis-decoding one silently.
        _ => {
            return Image {
                width: 1,
                height: 1,
                rgba: Arc::from([255; 4]),
            }
        }
    };
    let mut rgba = Vec::with_capacity((data.width * data.height * 4) as usize);
    for px in data.pixels.chunks_exact(stride) {
        // One or two channels are greyscale (+ alpha), so red replicates.
        let (r, g, b) = if stride < 3 {
            (px[0], px[0], px[0])
        } else {
            (px[0], px[1], px[2])
        };
        rgba.extend([r, g, b, if alpha { px[3] } else { 255 }]);
    }
    Image {
        width: data.width,
        height: data.height,
        rgba: Arc::from(rgba),
    }
}

fn generated_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let [pa, pb, pc] = [
            Vec3::from(positions[a]),
            Vec3::from(positions[b]),
            Vec3::from(positions[c]),
        ];
        let face = (pb - pa).cross(pc - pa);
        normals[a] += face;
        normals[b] += face;
        normals[c] += face;
    }
    normals
        .into_iter()
        .map(|normal| normal.try_normalize().unwrap_or(Vec3::Z).to_array())
        .collect()
}

/// Generate a stable tangent frame using the glTF reference accumulation
/// method. Degenerate/missing UVs receive a deterministic normal-orthogonal
/// tangent, which keeps neutral normal maps neutral instead of producing NaNs.
fn generated_tangents(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    uvs: &[[f32; 2]],
    indices: &[u32],
) -> Vec<[f32; 4]> {
    let mut tangent = vec![Vec3::ZERO; positions.len()];
    let mut bitangent = vec![Vec3::ZERO; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let [pa, pb, pc] = [
            Vec3::from(positions[a]),
            Vec3::from(positions[b]),
            Vec3::from(positions[c]),
        ];
        let [ta, tb, tc] = [Vec2::from(uvs[a]), Vec2::from(uvs[b]), Vec2::from(uvs[c])];
        let [edge1, edge2] = [pb - pa, pc - pa];
        let [duv1, duv2] = [tb - ta, tc - ta];
        let determinant = duv1.x * duv2.y - duv1.y * duv2.x;
        if determinant.abs() <= 1e-10 {
            continue;
        }
        let reciprocal = determinant.recip();
        let s = (edge1 * duv2.y - edge2 * duv1.y) * reciprocal;
        let t = (edge2 * duv1.x - edge1 * duv2.x) * reciprocal;
        for index in [a, b, c] {
            tangent[index] += s;
            bitangent[index] += t;
        }
    }
    normals
        .iter()
        .zip(tangent)
        .zip(bitangent)
        .map(|((&normal, tangent), bitangent)| {
            let normal = Vec3::from(normal).try_normalize().unwrap_or(Vec3::Z);
            let fallback = if normal.z.abs() < 0.999 {
                normal.cross(Vec3::Z).normalize()
            } else {
                normal.cross(Vec3::Y).normalize()
            };
            let tangent = (tangent - normal * normal.dot(tangent))
                .try_normalize()
                .unwrap_or(fallback);
            let handedness = if normal.cross(tangent).dot(bitangent) < 0.0 {
                -1.0
            } else {
                1.0
            };
            tangent.extend(handedness).to_array()
        })
        .collect()
}

fn read_primitive(prim: &gltf::Primitive, buffers: &[gltf::buffer::Data]) -> Option<Primitive> {
    if prim.mode() != gltf::mesh::Mode::Triangles {
        return None;
    }
    let reader = prim.reader(|b| Some(&buffers[b.index()]));
    let positions: Vec<[f32; 3]> = reader.read_positions()?.collect();
    let indices: Vec<u32> = match reader.read_indices() {
        Some(i) => i.into_u32().collect(),
        None => (0..positions.len() as u32).collect(),
    };
    // No `NORMAL` attribute means flat shading (see `Material::flat_shading`);
    // the attribute is then never read, so it stays zero rather than being
    // filled with a smoothing the shader would discard.
    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(Iterator::collect);
    let flat_shading = normals.is_none();
    let normals = normals.unwrap_or_else(|| generated_normals(&positions, &indices));

    let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
        Some(t) => t.into_f32().collect(),
        None => vec![[0.0, 0.0]; positions.len()],
    };
    let tangents: Vec<[f32; 4]> = reader
        .read_tangents()
        .map(Iterator::collect)
        .unwrap_or_else(|| generated_tangents(&positions, &normals, &uvs, &indices));

    let material = prim.material();
    let pbr = material.pbr_metallic_roughness();
    let base = pbr.base_color_factor();
    let emissive = material.emissive_factor();
    let normal = material.normal_texture();
    let occlusion = material.occlusion_texture();
    Some(Primitive {
        vertices: positions
            .into_iter()
            .zip(normals)
            .zip(uvs)
            .zip(tangents)
            .map(|(((position, normal), uv), tangent)| Vertex {
                position,
                normal,
                uv,
                tangent,
            })
            .collect::<Vec<_>>()
            .into(),
        indices: indices.into(),
        base_color_image: pbr
            .base_color_texture()
            .map(|t| t.texture().source().index()),
        normal_image: normal.as_ref().map(|t| t.texture().source().index()),
        metallic_roughness_image: pbr
            .metallic_roughness_texture()
            .map(|t| t.texture().source().index()),
        occlusion_image: occlusion.as_ref().map(|t| t.texture().source().index()),
        emissive_image: material
            .emissive_texture()
            .map(|t| t.texture().source().index()),
        material: Material {
            base_color: Vec4::from(base).truncate(),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            emissive: Vec3::from(emissive),
            normal_scale: normal.map_or(1.0, |texture| texture.scale()),
            occlusion_strength: occlusion.map_or(1.0, |texture| texture.strength()),
            flat_shading,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::generated_tangents;

    #[test]
    fn generated_tangents_pin_orientation_and_uv_handedness() {
        let positions = [
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ];
        let normals = [[0.0, 0.0, 1.0]; 4];
        let indices = [0, 1, 2, 0, 2, 3];
        let standard = generated_tangents(
            &positions,
            &normals,
            &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            &indices,
        );
        assert!(standard.iter().all(|t| *t == [1.0, 0.0, 0.0, 1.0]));

        let mirrored_uv = generated_tangents(
            &positions,
            &normals,
            &[[1.0, 0.0], [0.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            &indices,
        );
        assert!(mirrored_uv.iter().all(|t| *t == [-1.0, 0.0, 0.0, -1.0]));
    }
}
