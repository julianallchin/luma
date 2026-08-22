//! One golden frame's inputs, resolved from the scene description into
//! world-space draws and lights.
//!
//! Everything the three.js renderer computed per frame in TypeScript —
//! model-kind resolution, physical-dimension scaling, cone geometry, beam axes,
//! strobe gating — happens once here, against pre-resolved data.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat3, Mat4, Vec3};

use crate::assets::{HdrImage, Image, Library, Material, Vertex};
use crate::coords::{euler_xyz, hex_srgb, three_to_world_basis, world_from_three};
use crate::luminaire::{cone_from_opening, is_procedural, luminaire_for, model_kind, PIXEL};
use crate::overlay::Overlay;
use crate::scene_desc::{Definition, PrimitiveState, Scene};

/// Fixture definitions keyed by `fixturePath`, as the catalogue holds them.
pub type Definitions = std::collections::BTreeMap<String, Definition>;

/// Shader cap shared by fixture surface lighting and volumetric transport.
pub const MAX_FIXTURE_CONES: usize = 512;

/// One uploadable triangle list.
pub struct MeshData {
    /// Stable identity of this immutable mesh inside the asset/procedural bank.
    ///
    /// The live renderer uses this to retain GPU buffers across resolved
    /// frames. Geometry with one key must never change during a renderer's
    /// lifetime; a changed asset therefore needs a changed key.
    pub key: String,
    /// Interleaved vertex data.
    pub vertices: Arc<[Vertex]>,
    /// Triangle list into `vertices`.
    pub indices: Arc<[u32]>,
}

/// One mesh instance: what to draw, where, and with which material.
pub struct Draw {
    /// Index into [`Frame::meshes`].
    pub mesh: usize,
    /// Model-to-world transform.
    pub model: Mat4,
    /// Resolved material, after any per-fixture override.
    pub material: Material,
    /// Indices into [`Frame::images`] for the five glTF material-map roles.
    pub textures: MaterialTextures,
    /// Stable authored identity. Several draws may share one object.
    pub editor_object: Option<EditorObject>,
}

/// Authored editor identity carried through frame expansion.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditorObject {
    /// One patched fixture, regardless of its number of mesh primitives.
    Fixture(String),
    /// One authored stage piece, regardless of its glTF node count.
    StagePiece(String),
}

/// Optional glTF material maps in their fixed shader roles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MaterialTextures {
    /// sRGB base-colour map.
    pub base_color: Option<usize>,
    /// Linear tangent-space normal map.
    pub normal: Option<usize>,
    /// Linear packed green-roughness/blue-metallic map.
    pub metallic_roughness: Option<usize>,
    /// Linear red-channel ambient-occlusion map.
    pub occlusion: Option<usize>,
    /// sRGB emissive map.
    pub emissive: Option<usize>,
}

/// One fixture's finite light cone, shared by opaque surface lighting and haze.
/// Field order mirrors the two `SoA` storage buffers the shaders bind: the
/// `position`/`range` pair alone drives the sphere reject.
#[derive(Debug, Clone, Copy)]
pub struct FixtureCone {
    /// Apex of the cone, in world space.
    pub position: Vec3,
    /// Cull radius; the beam tapers to nothing over its last 30%.
    pub range: f32,
    /// Unit beam axis.
    pub direction: Vec3,
    /// Cosine of the half-angle at which intensity falls to 50%.
    pub cos_beam: f32,
    /// Emitted colour, linear.
    pub color: Vec3,
    /// Dimmer times cone gain.
    pub intensity: f32,
    /// Cosine of the half-angle where the profile reaches zero.
    pub cos_field: f32,
    /// 0 for a hard beam, 1 for a near-isotropic wash.
    pub wash: f32,
    /// Bounded procedural gobo: 0 open, 1 radial spokes, 2 breakup grid.
    pub gobo: u32,
    /// Gobo rotation in radians around the beam axis.
    pub gobo_rotation: f32,
}

/// A fixture's face light: lights its own housing from behind the lens and
/// nothing else. Three's punctual falloff with `decay = 2`.
#[derive(Debug, Clone, Copy)]
pub struct PointLight {
    /// World position, just behind the lens.
    pub position: Vec3,
    /// Emitted colour, linear.
    pub color: Vec3,
    /// three's `light.intensity`.
    pub intensity: f32,
    /// three's `light.distance`; the falloff is cut to zero here.
    pub cutoff_distance: f32,
}

/// Resolved directional light submitted to the scene and optional shadow pass.
#[derive(Debug, Clone, Copy)]
pub struct DirectionalLight {
    /// Unit world-space direction from the scene toward the light.
    pub direction: Vec3,
    /// Linear RGB colour multiplied by the configured intensity.
    pub radiance: Vec3,
    /// Directional shadow-camera anchor. Shading uses normalized `direction`;
    /// this exists only to retain the captured legacy projection exactly.
    pub shadow_eye: Vec3,
    /// Whether the shadow map is rendered and sampled.
    pub shadows: bool,
    /// Sanitized shadow-filter radius in shadow-map texels.
    pub shadow_softness: f32,
}

/// One base-colour texture, with the identity it was interned under.
///
/// The key travels with the pixels because it is what makes an upload reusable:
/// successive frames of one rig name the same textures, and a renderer that can
/// recognise them uploads each mip chain once instead of once a frame. Without
/// it the only way to know two frames mean the same texture is to compare the
/// bytes.
pub struct Texture {
    /// `"<asset>#img<n>"` — stable for as long as the asset is.
    pub key: String,
    /// The decoded pixels.
    pub image: Image,
}

/// One decoded HDR probe plus its per-frame display controls.
#[derive(Clone)]
pub struct EnvironmentImage {
    /// Stable asset identity used by the renderer's resident cache.
    pub key: String,
    /// Decoded linear source pixels.
    pub image: HdrImage,
    /// Image-based lighting multiplier.
    pub intensity: f32,
    /// Rotation around world Z, in radians.
    pub rotation: f32,
    /// Whether the source is visible behind scene geometry.
    pub visible: bool,
}

/// Everything one output frame needs, resolved to world space.
pub struct Frame {
    /// Deduplicated geometry referenced by [`Draw::mesh`].
    pub meshes: Vec<MeshData>,
    /// Deduplicated material images referenced by [`Draw::textures`].
    pub images: Vec<Texture>,
    /// Opaque draws first, then the trailing `grid_draws` transparent ones.
    /// Depth-only passes take the opaque prefix; the grid never writes depth.
    pub draws: Vec<Draw>,
    /// Count of trailing `Grid`-material draws in `draws` (0 or 1 today).
    pub grid_draws: usize,
    /// Editor affordances, in paint order, after every lit draw.
    pub overlays: Vec<Overlay>,
    /// Fixture face lights, in submission order.
    pub point_lights: Vec<PointLight>,
    /// Resolved fixture cones, capped at [`MAX_FIXTURE_CONES`].
    pub fixture_cones: Vec<FixtureCone>,
    /// Whether fixture cones illuminate opaque surfaces as well as haze.
    pub fixture_surface_lighting: bool,
    /// Whether the surface shader visualizes cluster occupancy.
    pub cluster_debug: bool,
    /// Linear, the `<color attach="background">` value.
    pub clear_color: Vec3,
    /// Linear ambient-light colour multiplied by its intensity.
    pub ambient: Vec3,
    /// Optional resident image-based environment.
    pub environment: Option<EnvironmentImage>,
    /// The independently controlled directional light, if enabled.
    pub directional: Option<DirectionalLight>,
    /// Effective density after the hazer-dimmer scaling; zero disables the pass.
    pub haze_density: f32,
    /// Equiangular samples per beam.
    pub haze_steps: u32,
    /// Fraction of the output resolution the haze pass runs at.
    ///
    /// The haze is a full-screen ray-march and is by far the most expensive
    /// thing in a lit frame, so halving it is a quarter of the work. The
    /// composite's depth-aware bilateral upsample exists for exactly this: the
    /// result is soft where the volume is soft and stays sharp across the
    /// silhouettes plain bilinear would smear. `1.0` is native; the goldens
    /// pin it there.
    pub haze_resolution: f32,
    /// The clock the golden was captured at; drives noise drift and strobe.
    pub time: f32,
    /// Diagnostic output selected by the renderer lab.
    pub debug_view: crate::scene_desc::DebugView,
    /// Where the frame is seen from.
    pub camera: Camera,
}

/// A look-at camera. The orbit parameterisation of spec §2.4 belongs in
/// `luma-scene`; this is only what the projection needs.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Eye position, world space.
    pub eye: Vec3,
    /// Look-at point, world space.
    pub target: Vec3,
    /// Vertical field of view in degrees.
    pub fov_y_deg: f32,
}

/// Deduplicating upload bank: one copy per (asset, primitive) and per (asset,
/// image), however many fixtures reference it.
#[derive(Default)]
pub(crate) struct Bank {
    meshes: Vec<MeshData>,
    images: Vec<Texture>,
    mesh_keys: HashMap<String, usize>,
    image_keys: HashMap<String, usize>,
}

impl Bank {
    pub(crate) fn insert(&mut self, key: String, build: impl FnOnce() -> MeshData) -> usize {
        let identity = key.clone();
        intern(&mut self.meshes, &mut self.mesh_keys, key, || {
            let mut mesh = build();
            mesh.key = identity;
            mesh
        })
    }

    fn insert_image(&mut self, key: String, build: impl FnOnce() -> Image) -> usize {
        let identity = key.clone();
        intern(&mut self.images, &mut self.image_keys, key, || Texture {
            key: identity,
            image: build(),
        })
    }
}

/// Intern one glTF primitive's geometry and base-colour texture, and emit the
/// draw that references them. `material` is the caller's chance to override the
/// asset's own constants (the near-black fixture housing).
fn glb_draw(
    bank: &mut Bank,
    asset: &str,
    glb: &crate::assets::Glb,
    p: usize,
    model: Mat4,
    material: impl FnOnce(Material) -> Material,
    editor_object: Option<EditorObject>,
) -> Draw {
    let prim = &glb.primitives[p];
    let mesh = bank.insert(format!("{asset}#{p}"), || MeshData {
        key: String::new(),
        vertices: Arc::clone(&prim.vertices),
        indices: Arc::clone(&prim.indices),
    });
    let mut image = |i: usize| {
        bank.insert_image(format!("{asset}#img{i}"), || Image {
            width: glb.images[i].width,
            height: glb.images[i].height,
            rgba: glb.images[i].rgba.clone(),
        })
    };
    let textures = MaterialTextures {
        base_color: prim.base_color_image.map(&mut image),
        normal: prim.normal_image.map(&mut image),
        metallic_roughness: prim.metallic_roughness_image.map(&mut image),
        occlusion: prim.occlusion_image.map(&mut image),
        emissive: prim.emissive_image.map(&mut image),
    };
    Draw {
        mesh,
        model,
        material: material(prim.material),
        textures,
        editor_object,
    }
}

fn intern<T>(
    store: &mut Vec<T>,
    keys: &mut HashMap<String, usize>,
    key: String,
    build: impl FnOnce() -> T,
) -> usize {
    if let Some(&i) = keys.get(&key) {
        return i;
    }
    let i = store.len();
    store.push(build());
    keys.insert(key, i);
    i
}

/// Strobe duty gate. `PrimitiveState.strobe` is a 0..1 rate; the display clock
/// turns it into on/off at 50% duty.
///
/// The two rate constants are the three.js ones — 20 Hz/unit for lensed
/// fixtures, 10 Hz/unit for bar pixels. That is two answers for one concept
/// (spec §3.2 flags it); they are kept apart here only so the goldens
/// reproduce, and should collapse to one when the port lands in the app.
fn strobe_gate(state: PrimitiveState, time: f32, hz_per_unit: f32) -> f32 {
    if state.strobe <= 0.0 {
        return state.dimmer;
    }
    let hz = state.strobe * hz_per_unit;
    if hz <= 0.0 {
        return state.dimmer;
    }
    let period = 1.0 / hz;
    if time.rem_euclid(period) > period * 0.5 {
        0.0
    } else {
        state.dimmer
    }
}

/// Pixel centres of a procedural bar/matrix, in fixture-local three space.
fn pixel_positions(def: &Definition, head_count: usize) -> Vec<Vec3> {
    let phys = def.physical.as_ref();
    // 200 mm here vs `extractPhysicalDimensions`'s 300 mm: two defaults for one
    // concept, ported as-is because the goldens pin real dimensions anyway.
    let dim = |get: fn(&crate::scene_desc::Dimensions) -> f32| {
        phys.and_then(|p| p.dimensions.as_ref())
            .map_or(0.2, |d| get(d).max(1.0) / 1000.0)
    };
    let (width, height, depth) = (dim(|d| d.width), dim(|d| d.height), dim(|d| d.depth));

    let layout = phys.and_then(|p| p.layout.as_ref());
    let (mut lw, mut lh) = layout.map_or((1, 1), |l| (l.width.max(1), l.height.max(1)));
    if lw == 1 && lh == 1 && head_count > 1 {
        lw = head_count as u32;
        lh = 1;
    }
    let (hw, hh) = (width / lw as f32, height / lh as f32);
    let (start_x, start_y) = (-width / 2.0 + hw / 2.0, height / 2.0 - hh / 2.0);
    let mut out = Vec::new();
    for y in 0..lh {
        for x in 0..lw {
            out.push(Vec3::new(
                start_x + x as f32 * hw,
                start_y - y as f32 * hh,
                depth / 2.0 + 0.001,
            ));
        }
    }
    out
}

/// Axis-aligned box, six quads, flat normals.
fn box_mesh(size: Vec3) -> MeshData {
    let h = size / 2.0;
    let faces: [(Vec3, Vec3, Vec3); 6] = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (-Vec3::X, Vec3::Y, -Vec3::Z),
        (Vec3::Y, -Vec3::X, Vec3::Z),
        (-Vec3::Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (-Vec3::Z, -Vec3::X, Vec3::Y),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (n, u, v) in faces {
        let centre = n * h.dot(n.abs());
        let (du, dv) = (u * h.dot(u.abs()), v * h.dot(v.abs()));
        let base = vertices.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            vertices.push(Vertex {
                position: (centre + du * su + dv * sv).to_array(),
                normal: n.to_array(),
                uv: [0.0, 0.0],
                tangent: u.extend(1.0).to_array(),
            });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    MeshData {
        key: String::new(),
        vertices: vertices.into(),
        indices: indices.into(),
    }
}

/// XY quad centred on the origin, normal +Z — a three `PlaneGeometry` before
/// any rotation.
pub(crate) fn plane_mesh(width: f32, height: f32) -> MeshData {
    let (w, h) = (width / 2.0, height / 2.0);
    let corners = [(-w, -h), (w, -h), (w, h), (-w, h)];
    MeshData {
        key: String::new(),
        vertices: corners
            .iter()
            .map(|&(x, y)| Vertex {
                position: [x, y, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
                tangent: [1.0, 0.0, 0.0, 1.0],
            })
            .collect::<Vec<_>>()
            .into(),
        indices: vec![0, 1, 2, 0, 2, 3].into(),
    }
}

/// Where one frame's per-head state comes from, keyed by `(fixture id, head)`.
///
/// A [`Scene`] carries a pinned `state` map because that is what a golden is:
/// a frame with its inputs frozen. A live viewport has no such map — it has an
/// evaluator that answers the same question per frame. Threading the lookup
/// through [`build_with`] is what lets both callers share one assembly path
/// instead of the live one growing a second, drifting copy of it.
pub type StateSource<'a> = &'a dyn Fn(&str, usize) -> Option<PrimitiveState>;

/// Resolve a scene at one instant into draws and lights, reading head state
/// from the scene's own pinned map.
///
/// # Errors
/// Fails if a referenced mesh is missing from the asset library.
pub fn build(
    scene: &Scene,
    definitions: &Definitions,
    time: f32,
    lib: &mut Library,
) -> anyhow::Result<Frame> {
    build_with(
        scene,
        definitions,
        &|id, head| scene.primitive(id, head),
        time,
        lib,
    )
}

/// [`build`], with head state read from `state` rather than from `scene`.
///
/// Everything else about the scene — rig geometry, camera, render dials — is
/// still the scene's. This is the only per-frame-varying input a live viewport
/// has, which is why it is the only one lifted out.
///
/// # Errors
/// Fails if a referenced mesh is missing from the asset library.
pub fn build_with(
    scene: &Scene,
    definitions: &Definitions,
    state: StateSource<'_>,
    time: f32,
    lib: &mut Library,
) -> anyhow::Result<Frame> {
    let r = three_to_world_basis();
    let mut bank = Bank::default();
    let mut draws = Vec::new();
    let mut point_lights = Vec::new();
    let mut fixture_cones = Vec::new();

    // --- floor -------------------------------------------------------------
    // `<mesh rotation={[-PI/2,0,0]}><planeGeometry args={[200,200]}/>`. Model
    // space stays three-space throughout: every model matrix below is
    // `to_world · (whatever three.js composed)`, so mesh data and local offsets
    // need no per-vertex conversion.
    let to_world = Mat4::from_mat3(r);
    let floor = bank.insert("::floor".into(), || plane_mesh(200.0, 200.0));
    draws.push(Draw {
        mesh: floor,
        textures: MaterialTextures::default(),
        model: to_world * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        material: Material {
            base_color: hex_srgb(0x03_03_03),
            metallic: 0.0,
            roughness: 0.95,
            emissive: Vec3::ZERO,
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            flat_shading: false,
        },
        editor_object: None,
    });

    // --- global haze density ----------------------------------------------
    // Scaled by the strongest hazer's dimmer, with a 0.3 floor.
    let mut hazer_level: f32 = 0.0;
    for f in &scene.fixtures {
        let Some(def) = definitions.get(&f.fixture_path) else {
            continue;
        };
        if model_kind(def).is_some_and(|k| !k.emits_beam()) {
            if let Some(s) = state(&f.id, 0) {
                hazer_level = hazer_level.max(s.dimmer);
            }
        }
    }
    let haze_density = scene.render.haze.density * (0.3 + 0.7 * hazer_level);

    // --- fixtures ----------------------------------------------------------
    for fixture in &scene.fixtures {
        let Some(def) = definitions.get(&fixture.fixture_path) else {
            continue;
        };
        let pos_three = Vec3::new(fixture.pos[0], fixture.pos[2], fixture.pos[1]);
        let rot_three = euler_xyz(fixture.rot[0], fixture.rot[2], fixture.rot[1]);
        let base = to_world * Mat4::from_translation(pos_three) * Mat4::from_mat3(rot_three);

        if is_procedural(def) {
            let head_count = def.head_count(&fixture.mode_name).max(1);
            let pixels = pixel_positions(def, head_count);
            let dims = def.dimensions_m();
            let body = bank.insert(format!("::bar-body:{}", fixture.fixture_path), || {
                box_mesh(Vec3::from(dims))
            });
            draws.push(Draw {
                mesh: body,
                textures: MaterialTextures::default(),
                model: base,
                material: Material {
                    base_color: hex_srgb(0x05_05_05),
                    ..Material::default()
                },
                editor_object: Some(EditorObject::Fixture(fixture.id.clone())),
            });

            let pixels_per_head = pixels.len() as f32 / head_count as f32;
            let (layout_w, layout_h) = layout_of(def, head_count);
            let quad = bank.insert(format!("::bar-pixel:{}", fixture.fixture_path), || {
                plane_mesh(
                    dims[0] / layout_w as f32 * 0.9,
                    dims[1] / layout_h as f32 * 0.9,
                )
            });

            for (i, local) in pixels.iter().enumerate() {
                let head = ((i as f32 / pixels_per_head) as usize).min(head_count - 1);
                let head_state = state(&fixture.id, head).unwrap_or(DARK);
                let intensity = strobe_gate(head_state, time, 10.0);
                draws.push(Draw {
                    mesh: quad,
                    textures: MaterialTextures::default(),
                    model: base * Mat4::from_translation(*local),
                    material: Material {
                        base_color: Vec3::ZERO,
                        metallic: 0.0,
                        roughness: 1.0,
                        emissive: Vec3::from(head_state.color) * intensity * 5.0,
                        normal_scale: 1.0,
                        occlusion_strength: 1.0,
                        flat_shading: false,
                    },
                    editor_object: Some(EditorObject::Fixture(fixture.id.clone())),
                });
            }

            // One haze cone per *head* (not per pixel), fired from the middle
            // pixel of that head's run.
            let cone = cone_from_opening(PIXEL);
            let dir = (r * (rot_three * Vec3::Z)).normalize();
            for head in 0..head_count {
                if fixture_cones.len() >= MAX_FIXTURE_CONES {
                    break;
                }
                let head_state = state(&fixture.id, head).unwrap_or(DARK);
                let intensity = strobe_gate(head_state, time, 10.0);
                if intensity < 0.01 {
                    continue;
                }
                let idx = ((head as f32 * pixels_per_head + pixels_per_head / 2.0) as usize)
                    .min(pixels.len() - 1);
                fixture_cones.push(FixtureCone {
                    position: base.transform_point3(pixels[idx]),
                    range: cone.range,
                    direction: dir,
                    cos_beam: cone.cos_beam,
                    color: Vec3::from(head_state.color),
                    // Normalise by sqrt(emitter count) so a 16-pixel bar isn't
                    // 16x a spot: overlapping pixel cones sum in the haze.
                    intensity: intensity * cone.gain / (head_count as f32).sqrt(),
                    cos_field: cone.cos_field,
                    wash: cone.wash,
                    gobo: head_state.gobo.min(2),
                    gobo_rotation: head_state.gobo_rotation,
                });
            }
            continue;
        }

        let Some(kind) = model_kind(def) else {
            continue;
        };
        let head_state = state(&fixture.id, 0).unwrap_or(DARK);
        let intensity = strobe_gate(head_state, time, 20.0);
        let mesh_rel = format!("qlc/{}", kind.mesh());
        let glb = lib.get(&mesh_rel)?;

        // Per-axis scale to the definition's physical dimensions, measured on
        // the unscaled mesh exactly as `applyPhysicalDimensionScaling` does.
        let extents = {
            let (lo, hi) = glb.bounds();
            hi - lo
        };
        let desired = Vec3::from(def.dimensions_m());
        let axis = |d: f32, e: f32| if e > 0.0 { d / e } else { 1.0 };
        let scale = Vec3::new(
            axis(desired.x, extents.x),
            axis(desired.y, extents.y),
            axis(desired.z, extents.z),
        );

        // Pan/tilt do not move the mesh here: the goldens pin `speed = 0`, and
        // `static-fixture.tsx` freezes articulation when speed is zero.
        let worlds = glb.world_matrices(base * Mat4::from_scale(scale), &HashMap::new());

        for (node, world) in glb.nodes.iter().zip(&worlds) {
            for &p in &node.primitives {
                // Every fixture body is forced near-black so only beams and
                // emissives read (`static-fixture.tsx`). `setRGB` is in the
                // linear working space, so no sRGB decode here.
                draws.push(glb_draw(
                    &mut bank,
                    &mesh_rel,
                    glb,
                    p,
                    *world,
                    |m| Material {
                        base_color: Vec3::splat(0.08),
                        ..m
                    },
                    Some(EditorObject::Fixture(fixture.id.clone())),
                ));
            }
        }

        if kind.emits_beam() {
            // Face light, parented to `head` when the mesh has one.
            let host = glb.node_index("head").unwrap_or(0);
            let local = Vec3::new(0.0, -kind.face_light_offset(), 0.0);
            point_lights.push(PointLight {
                position: worlds[host].transform_point3(local),
                color: Vec3::from(head_state.color),
                intensity: intensity * kind.face_light_intensity(),
                cutoff_distance: (def.dimensions_m()[1] * 0.9).max(0.12),
            });
        }

        if !kind.emits_beam() || intensity < 0.01 || fixture_cones.len() >= MAX_FIXTURE_CONES {
            continue;
        }
        let cone = cone_from_opening(luminaire_for(def, Some(kind)));
        let dir_three = rot_three
            * Mat3::from_rotation_y(head_state.position[0].to_radians())
            * Mat3::from_rotation_x(-head_state.position[1].to_radians())
            * Vec3::NEG_Y;
        fixture_cones.push(FixtureCone {
            position: base.transform_point3(Vec3::ZERO),
            range: cone.range,
            direction: (r * dir_three).normalize(),
            cos_beam: cone.cos_beam,
            color: Vec3::from(head_state.color),
            intensity: intensity * cone.gain,
            cos_field: cone.cos_field,
            wash: cone.wash,
            gobo: head_state.gobo.min(2),
            gobo_rotation: head_state.gobo_rotation,
        });
    }

    // --- stage pieces ------------------------------------------------------
    for piece in &scene.pieces {
        let glb = lib.get(&piece.mesh_path)?;
        let root = to_world
            * Mat4::from_translation(Vec3::new(piece.pos[0], piece.pos[2], piece.pos[1]))
            * Mat4::from_mat3(euler_xyz(piece.rot[0], piece.rot[2], piece.rot[1]))
            * Mat4::from_scale(Vec3::splat(piece.scale));
        let worlds = glb.world_matrices(root, &HashMap::new());
        for (node, world) in glb.nodes.iter().zip(&worlds) {
            for &p in &node.primitives {
                draws.push(glb_draw(
                    &mut bank,
                    &piece.mesh_path,
                    glb,
                    p,
                    *world,
                    |m| m,
                    Some(EditorObject::StagePiece(piece.id.clone())),
                ));
            }
        }
    }

    let camera = Camera {
        eye: world_from_three(Vec3::from(scene.camera.position)),
        target: world_from_three(Vec3::from(scene.camera.target)),
        fov_y_deg: scene.render.fov,
    };

    // The fading grid is an editor affordance, independent of environment
    // lighting, and transparent — hence the tail slot after every opaque draw.
    let grid_draws = usize::from(scene.render.show_grid);
    if scene.render.show_grid {
        draws.push(Draw {
            mesh: floor,
            textures: MaterialTextures::default(),
            model: to_world
                * Mat4::from_translation(Vec3::new(0.0, 0.002, 0.0))
                * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            material: Material::default(),
            editor_object: None,
        });
    }

    let overlays = crate::overlay::build(scene, definitions, &camera, to_world, &mut bank);

    let environment = scene
        .render
        .environment
        .probe
        .as_ref()
        .map(|probe| {
            Ok::<_, anyhow::Error>(EnvironmentImage {
                key: probe.asset.clone(),
                image: lib.environment(&probe.asset)?.clone(),
                intensity: probe.intensity.max(0.0),
                rotation: probe.rotation_deg.to_radians(),
                visible: probe.visible,
            })
        })
        .transpose()?;

    Ok(Frame {
        meshes: bank.meshes,
        images: bank.images,
        draws,
        grid_draws,
        point_lights,
        fixture_cones,
        fixture_surface_lighting: scene.render.fixture_surface_lighting,
        cluster_debug: scene.render.cluster_debug,
        clear_color: Vec3::from(scene.render.environment.background),
        ambient: Vec3::from(scene.render.environment.ambient_color)
            * scene.render.environment.ambient_intensity.max(0.0),
        environment,
        directional: scene.render.sun.and_then(|sun| {
            let direction = Vec3::from(sun.direction).normalize_or_zero();
            (direction != Vec3::ZERO && sun.intensity > 0.0).then_some(DirectionalLight {
                direction,
                radiance: Vec3::from(sun.color).max(Vec3::ZERO) * sun.intensity,
                shadow_eye: scene
                    .render
                    .legacy_shadow_eye
                    .map_or(direction * 244.0_f32.sqrt(), Vec3::from),
                shadows: sun.shadows,
                shadow_softness: if sun.shadow_softness.is_finite() {
                    sun.shadow_softness.clamp(0.0, 3.0)
                } else {
                    1.0
                },
            })
        }),
        haze_density: if scene.render.haze.enabled {
            haze_density
        } else {
            0.0
        },
        haze_steps: scene.render.haze.steps,
        haze_resolution: scene.render.haze.resolution,
        time,
        debug_view: scene.render.debug_view,
        camera,
        overlays,
    })
}

const DARK: PrimitiveState = PrimitiveState {
    dimmer: 0.0,
    color: [0.0; 3],
    strobe: 0.0,
    position: [0.0; 2],
    gobo: 0,
    gobo_rotation: 0.0,
};

fn layout_of(def: &Definition, head_count: usize) -> (u32, u32) {
    let layout = def.physical.as_ref().and_then(|p| p.layout.as_ref());
    let (w, h) = layout.map_or((1, 1), |l| (l.width.max(1), l.height.max(1)));
    if w == 1 && h == 1 && head_count > 1 {
        (head_count as u32, 1)
    } else {
        (w, h)
    }
}
