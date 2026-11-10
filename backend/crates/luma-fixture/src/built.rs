//! What a build hands Luma: a node tree with meshes, the joints that move it,
//! the canonical cells, the sources and the lenses, all in housing space.

use std::collections::BTreeMap;

use glam::{DMat4, DVec3, Mat4, Vec3};

use crate::mesh::Mesh as Raw;
use crate::rig::{Item, Rig};
use crate::spec::{Drive, Fixture, MaterialSlot, Template};

/// A built fixture. Every matrix, point and direction is `f32` housing space
/// (see the crate docs); nodes are parents-first.
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    /// The node tree. `nodes[0]` is the fixture root, whose frame is housing
    /// space itself.
    pub nodes: Vec<Node>,
    pub joints: Vec<Joint>,
    /// Canonical cells, index `i` at position `i` (CONTROL.md §5).
    pub cells: Vec<Cell>,
    /// One per head: where its beam leaves.
    pub heads: Vec<Head>,
    /// What casts light: one per zone or face, over the cells it covers.
    pub sources: Vec<Source>,
    /// The colour table, with the spec's overrides applied.
    pub materials: Vec<(MaterialSlot, Material)>,
    /// The one transform from the spec's space into housing space.
    pub housing_from_spec: Mat4,
}

/// A node: a rigid frame with meshes, moved by at most one joint.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// The reference builder's node name (`pan`, `tilt`, `head`,
    /// `element_3_tilt`, `spin_0`, ...).
    pub name: String,
    pub parent: Option<usize>,
    /// The node's frame in its parent's, with every joint at 0.
    pub local: Mat4,
    pub meshes: Vec<Mesh>,
    /// The joint that moves this node (an index into [`Built::joints`]).
    pub joint: Option<usize>,
    /// The array element this node belongs to.
    pub element: Option<usize>,
}

/// One drawable mesh in its node's frame: every part of the node in one
/// material, or the emitters of one cell in one material.
#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    pub material: MaterialSlot,
    /// Set on emitter meshes: the canonical cell they light.
    pub cell: Option<u32>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Set on a flat LED panel: the mesh is the panel's flat face, and its
    /// LEDs are this pattern drawn on it (see [`LedPattern`]).
    pub pattern: Option<LedPattern>,
    /// Per vertex, with a `pattern`: where the vertex sits on it, in LED
    /// pitches, `u` along a row from the panel's left edge and `v` down from
    /// its top row. Empty otherwise.
    pub uvs: Vec<[f32; 2]>,
}

/// A flat panel of identical LEDs on a regular grid, drawn as one face that
/// the renderer paints the LEDs onto, rather than as a mesh per LED.
///
/// Many small LEDs with dark gaps between them, each its own mesh, beat
/// against the pixels once an LED and its gap are smaller than one: the face
/// breaks into bands. One face with the layout lets the renderer draw sharp
/// LEDs up close and their average colour once they are too small to see.
/// The cell's [`Emitter`]s still list every LED.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedPattern {
    /// [`EmitterShape::Rect`], [`EmitterShape::Circle`], or
    /// [`EmitterShape::Hex`] (corners along `u`).
    pub shape: EmitterShape,
    /// Each LED's width and height as a share of the pitch along `u` and
    /// `v`, centred in its pitch. A circle or hex fills its share as an
    /// ellipse would in pattern space, which is round on the face.
    pub fill: [f32; 2],
    /// LEDs in a row.
    pub cols: u32,
    /// Odd rows (`v` in 1..2, 3..4, ...) sit half a pitch along `u`.
    pub stagger: bool,
    /// What shows between the LEDs.
    pub gap: MaterialSlot,
}

impl LedPattern {
    /// The share of the face its LEDs cover.
    #[must_use]
    pub fn lit_share(&self) -> f32 {
        let box_share = self.fill[0] * self.fill[1];
        match self.shape {
            EmitterShape::Circle => box_share * std::f32::consts::FRAC_PI_4,
            EmitterShape::Hex => box_share * 3.0 * 3f32.sqrt() / 8.0,
            EmitterShape::Rect | EmitterShape::Tube => box_share,
        }
    }

    /// Whether the point `uv` (in pitches) is on an LED.
    #[must_use]
    pub fn lit(&self, [u, v]: [f32; 2]) -> bool {
        let row = v.floor();
        let u = if self.stagger && row.rem_euclid(2.0) == 1.0 {
            u - 0.5
        } else {
            u
        };
        if u < 0.0 || u >= self.cols as f32 {
            return false;
        }
        let x = (u - u.floor() - 0.5) / (0.5 * self.fill[0]);
        let y = (v - row - 0.5) / (0.5 * self.fill[1]);
        let (x, y) = (x.abs(), y.abs());
        match self.shape {
            EmitterShape::Circle => x * x + y * y <= 1.0,
            EmitterShape::Hex => y.max(0.866_025_4 * x + 0.5 * y) <= 0.866_025_4,
            EmitterShape::Rect | EmitterShape::Tube => x <= 1.0 && y <= 1.0,
        }
    }
}

/// What a joint does to its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JointKind {
    Pan,
    Tilt,
    /// Turns the face about the beam.
    Spin,
    /// Moves the nose along the beam (metres, not degrees).
    Slide,
}

/// A joint: `node.local(θ) = node.local · motion(axis, θ)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Joint {
    pub kind: JointKind,
    pub node: usize,
    /// Unit axis in the node's own frame.
    pub axis: Vec3,
    /// `[lo, hi]`, degrees (metres for a slide); `None` = continuous.
    pub range: Option<[f32; 2]>,
    /// Where the joint sits when nothing drives it. For the tilt of a
    /// luminaire's chain (the fixture's own, or an array element's yoke) it
    /// is the spec's `rest`: degrees from the beam along the pan axis, 90 =
    /// facing front. Everything else rests at 0, as the reference builder
    /// draws it: a deflector's mirror and a bare array head ignore `rest`.
    /// The meshes are built at 0, not here.
    pub rest: f32,
    pub drive: Drive,
    pub per_head: bool,
    /// The array element it moves, for a per-head joint.
    pub element: Option<usize>,
}

/// One canonical cell: what one `fixture:<cell>` primitive lights.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub index: u32,
    /// The node its emitters belong to; `None` for a cell no emitter draws
    /// (an index a face skipped).
    pub node: Option<usize>,
    /// Mean of its emitters' centres, in `node`'s frame.
    pub centre: Vec3,
    /// The way its emitters face.
    pub normal: Vec3,
    /// Extent of its emitters across the face, `[u, v]` metres.
    pub size: [f32; 2],
    pub emitters: Vec<Emitter>,
}

/// One emitter (LED, lens, strip, tube) of a cell, in the cell node's frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Emitter {
    pub centre: Vec3,
    pub normal: Vec3,
    /// The face's `x` (right as seen from the front) at this emitter.
    pub u: Vec3,
    /// `[w, h]` metres along `u` and `normal × u`.
    pub size: [f32; 2],
    pub shape: EmitterShape,
    pub material: MaterialSlot,
    /// The spec's colour (`rgbw`, `red`, `white`, ...), when it names one.
    pub colour: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitterShape {
    Circle,
    Rect,
    Hex,
    /// A cylinder lying along `u`.
    Tube,
}

/// A head's lens: where its beam leaves and which way it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Head {
    pub node: usize,
    pub lens_centre: Vec3,
    /// The beam direction at the lens, in `node`'s frame.
    pub beam: Vec3,
    /// The face's `[w, h]`; a round lens's diameter is `size[0]`.
    pub size: [f32; 2],
    pub round: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub node: usize,
    pub position: Vec3,
    pub cells: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    pub rgb: [f32; 3],
    pub emit: [f32; 3],
    pub metal: f32,
    pub rough: f32,
}

impl Built {
    /// One angle per joint (degrees, metres for a slide): each joint's `rest`.
    #[must_use]
    pub fn rest_angles(&self) -> Vec<f32> {
        self.joints.iter().map(|j| j.rest).collect()
    }

    /// Every node's frame in housing space with joint `i` at `angles[i]`
    /// (missing angles are 0).
    #[must_use]
    pub fn world_matrices(&self, angles: &[f32]) -> Vec<Mat4> {
        let mut out: Vec<Mat4> = Vec::with_capacity(self.nodes.len());
        for n in &self.nodes {
            let motion = n.joint.map_or(Mat4::IDENTITY, |j| {
                let joint = &self.joints[j];
                let a = angles.get(j).copied().unwrap_or(0.0);
                match joint.kind {
                    JointKind::Slide => Mat4::from_translation(joint.axis * a),
                    _ => Mat4::from_axis_angle(joint.axis, a.to_radians()),
                }
            });
            let local = n.local * motion;
            out.push(n.parent.map_or(local, |p| out[p] * local));
        }
        out
    }

    /// The box around every mesh, housing space, every joint at 0. Its `max.y`
    /// is where the clamp goes.
    #[must_use]
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        bounds_at(self, &self.world_matrices(&[]))
    }
}

/// The box around every mesh under `worlds`.
#[must_use]
pub fn bounds_at(b: &Built, worlds: &[Mat4]) -> Option<(Vec3, Vec3)> {
    let mut acc: Option<(Vec3, Vec3)> = None;
    for (n, w) in b.nodes.iter().zip(worlds) {
        for m in &n.meshes {
            for p in &m.positions {
                let q = w.transform_point3(Vec3::from(*p));
                acc = Some(acc.map_or((q, q), |(lo, hi)| (lo.min(q), hi.max(q))));
            }
        }
    }
    acc
}

/// Slot, base colour, emission, metalness, roughness.
type MaterialRow = (MaterialSlot, [f64; 3], [f64; 3], f64, f64);

const DEFAULTS: [MaterialRow; 12] = [
    (MaterialSlot::Body, [0.2, 0.2, 0.22], [0.0; 3], 0.3, 0.6),
    (MaterialSlot::Body2, [0.26, 0.26, 0.29], [0.0; 3], 0.5, 0.6),
    (MaterialSlot::Metal, [0.55, 0.56, 0.6], [0.0; 3], 0.9, 0.3),
    (MaterialSlot::Lens, [0.10, 0.20, 0.38], [0.0; 3], 0.1, 0.1),
    (
        MaterialSlot::Diffuser,
        [0.55, 0.6, 0.66],
        [0.0; 3],
        0.0,
        0.9,
    ),
    (
        MaterialSlot::Emitter,
        [1.0, 0.96, 0.78],
        [1.0, 0.92, 0.6],
        0.0,
        0.4,
    ),
    (
        MaterialSlot::EmitterW,
        [1.0, 1.0, 1.0],
        [1.0, 1.0, 1.0],
        0.0,
        0.4,
    ),
    (
        MaterialSlot::EmitterR,
        [1.0, 0.2, 0.15],
        [1.0, 0.1, 0.05],
        0.0,
        0.4,
    ),
    (
        MaterialSlot::EmitterG,
        [0.2, 1.0, 0.3],
        [0.1, 1.0, 0.2],
        0.0,
        0.4,
    ),
    (
        MaterialSlot::EmitterB,
        [0.2, 0.35, 1.0],
        [0.1, 0.2, 1.0],
        0.0,
        0.4,
    ),
    (
        MaterialSlot::EmitterA,
        [1.0, 0.65, 0.1],
        [1.0, 0.55, 0.05],
        0.0,
        0.4,
    ),
    (
        MaterialSlot::EmitterUv,
        [0.5, 0.2, 1.0],
        [0.4, 0.1, 1.0],
        0.0,
        0.4,
    ),
];

fn slot_key(s: MaterialSlot) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn materials(f: &Fixture) -> Vec<(MaterialSlot, Material)> {
    let f32x3 = |v: [f64; 3]| v.map(|x| x as f32);
    DEFAULTS
        .iter()
        .map(|&(slot, rgb, emit, metal, rough)| {
            let o = f.materials.as_ref().and_then(|m| m.get(&slot_key(slot)));
            let m = Material {
                rgb: f32x3(o.and_then(|o| o.rgb).unwrap_or(rgb)),
                emit: f32x3(o.and_then(|o| o.emit).unwrap_or(emit)),
                metal: o.and_then(|o| o.metal).unwrap_or(metal) as f32,
                rough: o.and_then(|o| o.rough).unwrap_or(rough) as f32,
            };
            (slot, m)
        })
        .collect()
}

/// Corners whose faces meet at less than this share a normal.
const CREASE_COS: f64 = 0.766; // 40 degrees

/// Indexed f32 geometry with crease-aware normals: smooth across a curved
/// surface, split at an edge.
fn shade(m: &Raw, into: &mut MeshBuf) {
    let fnorm: Vec<DVec3> =
        m.f.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| m.v[i as usize]);
                (b - a).cross(c - a)
            })
            .collect();
    let mut around: Vec<Vec<u32>> = vec![Vec::new(); m.v.len()];
    for (fi, t) in m.f.iter().enumerate() {
        if fnorm[fi].length_squared() > 0.0 {
            for &i in t {
                around[i as usize].push(fi as u32);
            }
        }
    }
    let unit: Vec<DVec3> = fnorm.iter().map(|n| n.normalize_or_zero()).collect();
    // Per vertex, the output vertices made so far: (face set's normal, index).
    let mut made: Vec<Vec<(DVec3, u32)>> = vec![Vec::new(); m.v.len()];
    for (fi, t) in m.f.iter().enumerate() {
        if fnorm[fi].length_squared() == 0.0 {
            continue;
        }
        for &vi in t {
            let mut n = DVec3::ZERO;
            for &g in &around[vi as usize] {
                if unit[g as usize].dot(unit[fi]) > CREASE_COS {
                    n += fnorm[g as usize];
                }
            }
            let n = n.normalize_or_zero();
            let slot = &mut made[vi as usize];
            let index = match slot.iter().find(|(m, _)| *m == n) {
                Some(&(_, i)) => i,
                None => {
                    let i = into.positions.len() as u32;
                    let p = m.v[vi as usize];
                    into.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                    into.normals.push([n.x as f32, n.y as f32, n.z as f32]);
                    slot.push((n, i));
                    i
                }
            };
            into.indices.push(index);
        }
    }
}

/// A flat mesh as it is, one normal for all of it, with its pattern
/// coordinates.
fn flat(m: &Raw, uv: &[[f32; 2]], into: &mut MeshBuf) {
    let [a, b, c] = m.f[0].map(|i| m.v[i as usize]);
    let n = (b - a)
        .cross(c - a)
        .normalize_or_zero()
        .as_vec3()
        .to_array();
    let base = into.positions.len() as u32;
    for (p, uv) in m.v.iter().zip(uv) {
        into.positions.push(p.as_vec3().to_array());
        into.normals.push(n);
        into.uvs.push(*uv);
    }
    into.indices
        .extend(m.f.iter().flat_map(|t| t.map(|i| base + i)));
}

#[derive(Default)]
struct MeshBuf {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
    uvs: Vec<[f32; 2]>,
}

fn v3(v: DVec3) -> Vec3 {
    v.as_vec3()
}

/// Gather a finished rig into a [`Built`] in housing space.
pub(crate) fn assemble(f: &Fixture, mut b: Rig) -> Built {
    // The spec's own space hangs nothing unless it says so; housing space is
    // always hung. A spec already built hanging needs no turn.
    let hung = match f.template {
        Template::Array => f
            .array
            .as_ref()
            .and_then(|a| a.carrier.hang)
            .unwrap_or(false),
        _ => f.rig.mount.hang.unwrap_or(false),
    };
    let flip = if hung {
        DMat4::IDENTITY
    } else {
        DMat4::from_rotation_z(std::f64::consts::PI)
    };
    let pivot = pivot(f, &b, flip);
    let c = DMat4::from_translation(-pivot) * flip;

    for (i, n) in b.nodes.iter_mut().enumerate() {
        n.world = c * n.world;
        if i == 0 {
            n.local = DMat4::IDENTITY;
        } else if n.parent == Some(0) {
            n.local = c * n.local;
        }
    }
    let to_root = |p: DVec3, node: usize| if node == 0 { c.transform_point3(p) } else { p };
    let dir_root = |d: DVec3, node: usize| if node == 0 { c.transform_vector3(d) } else { d };
    for it in &mut b.items {
        if it.node == 0 {
            it.mesh.transform(&c);
            if let Some(e) = &mut it.emit {
                e.centre = c.transform_point3(e.centre);
                e.normal = c.transform_vector3(e.normal);
                e.u = c.transform_vector3(e.u);
            }
        }
    }

    let mut joints = Vec::new();
    let mut nodes: Vec<Node> = b
        .nodes
        .iter()
        .map(|n| {
            let joint = n.joint.as_ref().map(|j| {
                joints.push(Joint {
                    kind: j.kind,
                    node: joints.len(),
                    axis: v3(j.axis.normalize_or_zero()),
                    range: j.range.map(|r| r.map(|x| x as f32)),
                    rest: j.rest as f32,
                    drive: j.drive,
                    per_head: j.per_head,
                    element: n.element,
                });
                joints.len() - 1
            });
            Node {
                name: n.name.clone(),
                parent: n.parent,
                local: n.local.as_mat4(),
                meshes: Vec::new(),
                joint,
                element: n.element,
            }
        })
        .collect();
    for (i, n) in nodes.iter().enumerate() {
        if let Some(j) = n.joint {
            joints[j].node = i;
        }
    }

    // Meshes: per node, the parts in one material, each cell's emitters in
    // one material, and each cell's LED panels of one pattern.
    let mut patterns: Vec<LedPattern> = Vec::new();
    let mut bufs: BTreeMap<(usize, MaterialSlot, Option<u32>, Option<usize>), MeshBuf> =
        BTreeMap::new();
    let mut emitters: BTreeMap<u32, Vec<(usize, &Item)>> = BTreeMap::new();
    for it in &b.items {
        if let Some(e) = &it.emit {
            emitters.entry(e.cell).or_default().push((it.node, it));
        }
        if it.mesh.f.is_empty() {
            continue;
        }
        let Some(panel) = &it.panel else {
            let cell = it.emit.as_ref().map(|e| e.cell);
            shade(
                &it.mesh,
                bufs.entry((it.node, it.mat, cell, None)).or_default(),
            );
            continue;
        };
        let pattern = match patterns.iter().position(|p| *p == panel.pattern) {
            Some(i) => i,
            None => {
                patterns.push(panel.pattern);
                patterns.len() - 1
            }
        };
        let key = (it.node, it.mat, Some(panel.cell), Some(pattern));
        flat(&it.mesh, &panel.uv, bufs.entry(key).or_default());
    }
    for ((node, material, cell, pattern), buf) in bufs {
        nodes[node].meshes.push(Mesh {
            material,
            cell,
            positions: buf.positions,
            normals: buf.normals,
            indices: buf.indices,
            pattern: pattern.map(|i| patterns[i]),
            uvs: buf.uvs,
        });
    }

    let cells = (0..b.next_cell)
        .map(|k| {
            let Some(list) = emitters.get(&k) else {
                return Cell {
                    index: k,
                    node: None,
                    centre: Vec3::ZERO,
                    normal: Vec3::ZERO,
                    size: [0.0; 2],
                    emitters: Vec::new(),
                };
            };
            let owner = list[0].0;
            let into = b.nodes[owner].world.inverse();
            let ems: Vec<Emitter> = list
                .iter()
                .map(|&(node, it)| {
                    let e = it.emit.as_ref().expect("listed because it emits");
                    let m = into * b.nodes[node].world;
                    Emitter {
                        centre: v3(m.transform_point3(e.centre)),
                        normal: v3(m.transform_vector3(e.normal).normalize_or_zero()),
                        u: v3(m.transform_vector3(e.u).normalize_or_zero()),
                        size: e.size.map(|x| x as f32),
                        shape: e.shape,
                        material: it.mat,
                        colour: e.colour.clone(),
                    }
                })
                .collect();
            let centre = ems.iter().map(|e| e.centre).sum::<Vec3>() / ems.len() as f32;
            let normal = ems
                .iter()
                .map(|e| e.normal)
                .sum::<Vec3>()
                .normalize_or_zero();
            let u = ems[0].u;
            let v = ems[0].normal.cross(u);
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for e in &ems {
                for (axis, half) in [(u, e.size[0] / 2.0), (v, e.size[1] / 2.0)]
                    .into_iter()
                    .enumerate()
                {
                    let at = e.centre.dot(half.0);
                    lo[axis] = lo[axis].min(at - half.1);
                    hi[axis] = hi[axis].max(at + half.1);
                }
            }
            Cell {
                index: k,
                node: Some(owner),
                centre,
                normal,
                size: [hi[0] - lo[0], hi[1] - lo[1]],
                emitters: ems,
            }
        })
        .collect();

    let heads = b
        .lenses
        .iter()
        .map(|l| Head {
            node: l.node,
            lens_centre: v3(to_root(l.centre, l.node)),
            beam: v3(dir_root(l.normal, l.node)),
            size: l.size.map(|x| x as f32),
            round: l.round,
        })
        .collect();
    let sources = b
        .sources
        .iter()
        .map(|s| Source {
            node: s.node,
            position: v3(to_root(s.position, s.node)),
            cells: s.cells.clone(),
        })
        .collect();
    Built {
        nodes,
        joints,
        cells,
        heads,
        sources,
        materials: materials(f),
        housing_from_spec: c.as_mat4(),
    }
}

/// Housing space's origin, in the flipped spec space: the fixture's own
/// pan/tilt pivot (the tilt axis where it has one), else the middle of its
/// box, which is where Luma centres a static housing.
fn pivot(f: &Fixture, b: &Rig, flip: DMat4) -> DVec3 {
    if f.template != Template::Deflector {
        let own = |kind: crate::JointKind| {
            b.nodes
                .iter()
                .find(|n| n.element.is_none() && n.joint.as_ref().is_some_and(|j| j.kind == kind))
        };
        if let Some(n) = own(crate::JointKind::Tilt).or_else(|| own(crate::JointKind::Pan)) {
            return flip.transform_point3(n.world.w_axis.truncate());
        }
    }
    let mut lo = DVec3::splat(f64::MAX);
    let mut hi = DVec3::splat(f64::MIN);
    for it in &b.items {
        let m = flip * b.nodes[it.node].world;
        for &v in &it.mesh.v {
            let p = m.transform_point3(v);
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    if lo.x > hi.x {
        return DVec3::ZERO;
    }
    (lo + hi) / 2.0
}
