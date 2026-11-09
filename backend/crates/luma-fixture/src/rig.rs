//! The builder's state: a node tree, a current frame, and the meshes and
//! emitters made so far.
//!
//! Geometry is authored in rest-world coordinates (the current frame `t`) and
//! stored in the frame of the node it belongs to, exactly as the reference
//! builder's `Rig` does; nodes open at the current frame.

use std::collections::{HashMap, HashSet};
use std::f64::consts::FRAC_PI_2;

use glam::{DMat4, DQuat, DVec3};

use crate::mesh::{self, Mesh, Sec};
use crate::spec::MaterialSlot;
use crate::{EmitterShape, JointKind};

/// A build failure the reference builder would have answered with its
/// fallback box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError(pub String);

pub(crate) type Res<T = ()> = Result<T, BuildError>;

pub(crate) fn fail<T>(why: impl Into<String>) -> Res<T> {
    Err(BuildError(why.into()))
}

pub(crate) fn tr(x: f64, y: f64, z: f64) -> DMat4 {
    DMat4::from_translation(DVec3::new(x, y, z))
}

pub(crate) fn rot(angle: f64, axis: DVec3) -> DMat4 {
    DMat4::from_axis_angle(axis.normalize(), angle)
}

pub(crate) fn rx(a: f64) -> DMat4 {
    rot(a, DVec3::X)
}

pub(crate) fn ry(a: f64) -> DMat4 {
    rot(a, DVec3::Y)
}

pub(crate) fn cl(v: f64, lo: f64, hi: f64) -> f64 {
    lo.max(hi.min(v))
}

/// Python's `int()` on a float: truncation toward zero.
pub(crate) fn int(v: f64) -> i64 {
    v as i64
}

/// The axis a `cyl` runs along.
#[derive(Clone, Copy)]
pub(crate) enum Axis {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone)]
pub(crate) struct JointRec {
    pub kind: JointKind,
    pub axis: DVec3,
    pub range: Option<[f64; 2]>,
    pub rest: f64,
    pub drive: crate::spec::Drive,
    pub per_head: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct NodeRec {
    pub name: String,
    pub parent: Option<usize>,
    pub local: DMat4,
    pub world: DMat4,
    pub joint: Option<JointRec>,
    pub element: Option<usize>,
}

/// One emitter mesh's description, in its node's frame.
#[derive(Debug, Clone)]
pub(crate) struct EmitRec {
    pub cell: u32,
    pub centre: DVec3,
    pub normal: DVec3,
    pub u: DVec3,
    pub size: [f64; 2],
    pub shape: EmitterShape,
    pub colour: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub name: &'static str,
    pub node: usize,
    pub mat: MaterialSlot,
    pub mesh: Mesh,
    pub emit: Option<EmitRec>,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceRec {
    pub node: usize,
    pub position: DVec3,
    pub cells: Vec<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct LensRec {
    pub node: usize,
    pub centre: DVec3,
    pub normal: DVec3,
    pub size: [f64; 2],
    pub round: bool,
}

/// How an emitter is sized: a diameter, or `(w, h)`.
#[derive(Clone, Copy)]
pub(crate) enum Size {
    D(f64),
    Wh(f64, f64),
}

pub(crate) struct Rig {
    pub t: DMat4,
    pub nodes: Vec<NodeRec>,
    pub cur: usize,
    names: HashSet<String>,
    next_suffix: HashMap<String, u32>,
    pub items: Vec<Item>,
    pub next_cell: u32,
    pub next_src: u32,
    cell_em: HashMap<u32, u32>,
    pub sources: Vec<SourceRec>,
    pub lenses: Vec<LensRec>,
    /// The colour string of the emitter about to be made (zones and nodes know
    /// it; pattern zones do not).
    pub colour: Option<String>,
}

impl Rig {
    pub fn new() -> Self {
        let mut names = HashSet::new();
        names.insert("fixture".to_string());
        Self {
            t: DMat4::IDENTITY,
            nodes: vec![NodeRec {
                name: "fixture".into(),
                parent: None,
                local: DMat4::IDENTITY,
                world: DMat4::IDENTITY,
                joint: None,
                element: None,
            }],
            cur: 0,
            names,
            next_suffix: HashMap::new(),
            items: Vec::new(),
            next_cell: 0,
            next_src: 0,
            cell_em: HashMap::new(),
            sources: Vec::new(),
            lenses: Vec::new(),
            colour: None,
        }
    }

    /// A name no node or mesh has yet: `name`, else `name.1`, `name.2`, ...
    fn uniq(&mut self, name: &str) -> String {
        if self.names.insert(name.to_string()) {
            return name.to_string();
        }
        let k = self.next_suffix.entry(name.to_string()).or_insert(0);
        loop {
            *k += 1;
            let candidate = format!("{name}.{k}");
            if self.names.insert(candidate.clone()) {
                return candidate;
            }
        }
    }

    /// Run `f` with the current frame moved by `m`.
    pub fn at<R>(&mut self, m: DMat4, f: impl FnOnce(&mut Self) -> R) -> R {
        let old = self.t;
        self.t = old * m;
        let out = f(self);
        self.t = old;
        out
    }

    fn open(
        &mut self,
        name: &str,
        joint: Option<JointRec>,
        element: Option<usize>,
        pos: Option<DVec3>,
    ) -> usize {
        let w = self.t * pos.map_or(DMat4::IDENTITY, DMat4::from_translation);
        let name = self.uniq(name);
        let local = self.nodes[self.cur].world.inverse() * w;
        self.nodes.push(NodeRec {
            name,
            parent: Some(self.cur),
            local,
            world: w,
            joint,
            element,
        });
        self.nodes.len() - 1
    }

    /// Open a node at the current frame (plus `pos`) and build inside it.
    pub fn joint<R>(
        &mut self,
        name: &str,
        joint: Option<JointRec>,
        element: Option<usize>,
        pos: Option<DVec3>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let old = self.cur;
        self.cur = self.open(name, joint, element, pos);
        let out = f(self);
        self.cur = old;
        out
    }

    /// A light source over `cells`, at `pos` in the current frame.
    pub fn source(&mut self, pos: DVec3, cells: Vec<u32>) {
        let name = format!("source_{}", self.next_src);
        self.uniq(&name);
        let position = self.to_node(pos);
        self.sources.push(SourceRec {
            node: self.cur,
            position,
            cells,
        });
        self.next_src += 1;
    }

    /// The lens of the head being built: a face `size` at depth `z`.
    pub fn lens(&mut self, z: f64, size: [f64; 2], round: bool) {
        let centre = self.to_node(DVec3::new(0.0, 0.0, z));
        let normal = self.dir_to_node(DVec3::Z);
        self.lenses.push(LensRec {
            node: self.cur,
            centre,
            normal,
            size,
            round,
        });
    }

    fn node_from_rest(&self) -> DMat4 {
        self.nodes[self.cur].world.inverse() * self.t
    }

    fn to_node(&self, p: DVec3) -> DVec3 {
        self.node_from_rest().transform_point3(p)
    }

    fn dir_to_node(&self, d: DVec3) -> DVec3 {
        self.node_from_rest().transform_vector3(d).normalize()
    }

    pub fn add(&mut self, name: &'static str, mut mesh: Mesh, mat: MaterialSlot, local: DMat4) {
        self.add_named(name, &mut mesh, mat, local);
        self.items.push(Item {
            name,
            node: self.cur,
            mat,
            mesh,
            emit: None,
        });
    }

    /// `add` for a mesh named by the spec (`rig.parts[].name`).
    pub fn add_part(&mut self, name: &str, mut mesh: Mesh, mat: MaterialSlot, local: DMat4) {
        self.add_named(name, &mut mesh, mat, local);
        self.items.push(Item {
            name: "part",
            node: self.cur,
            mat,
            mesh,
            emit: None,
        });
    }

    fn add_named(&mut self, name: &str, mesh: &mut Mesh, _mat: MaterialSlot, local: DMat4) {
        mesh.transform(&(self.node_from_rest() * local));
        self.uniq(name);
    }

    // ---- primitives -------------------------------------------------------------------------

    pub fn box_(&mut self, name: &'static str, size: [f64; 3], c: [f64; 3], mat: MaterialSlot) {
        let e = DVec3::from(size).max(DVec3::splat(1e-4));
        self.add(name, mesh::cuboid(e), mat, tr(c[0], c[1], c[2]));
    }

    pub fn ellipse_plate(
        &mut self,
        name: &'static str,
        rx: f64,
        ry: f64,
        t: f64,
        c: [f64; 3],
        mat: MaterialSlot,
    ) {
        let mut m = mesh::cylinder(1.0, t.max(1e-4), 40);
        m.scale(DVec3::new(rx.max(1e-4), ry.max(1e-4), 1.0));
        self.add(name, m, mat, tr(c[0], c[1], c[2]));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cyl(
        &mut self,
        name: &'static str,
        r: f64,
        len: f64,
        c: [f64; 3],
        axis: Axis,
        mat: MaterialSlot,
        sections: usize,
    ) {
        let turn = match axis {
            Axis::X => ry(FRAC_PI_2),
            Axis::Y => rx(-FRAC_PI_2),
            Axis::Z => DMat4::IDENTITY,
        };
        let m = mesh::cylinder(r.max(1e-4), len.max(1e-4), sections);
        self.add(name, m, mat, tr(c[0], c[1], c[2]) * turn);
    }

    pub fn rod(&mut self, name: &'static str, a: DVec3, b: DVec3, r: f64, mat: MaterialSlot) {
        let v = b - a;
        let len = v.length();
        if len < 1e-6 {
            return;
        }
        let turn = DMat4::from_quat(DQuat::from_rotation_arc(DVec3::Z, v / len));
        let c = (a + b) / 2.0;
        self.add(
            name,
            mesh::cylinder(r, len, 12),
            mat,
            tr(c.x, c.y, c.z) * turn,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rbox(
        &mut self,
        name: &'static str,
        size: [f64; 3],
        c: [f64; 3],
        mat: MaterialSlot,
        e: f64,
        nu: usize,
        nv: usize,
    ) {
        self.add(
            name,
            mesh::rbox(DVec3::from(size), e, nu, nv),
            mat,
            tr(c[0], c[1], c[2]),
        );
    }

    pub fn loft(
        &mut self,
        name: &'static str,
        secs: &[Sec],
        mat: MaterialSlot,
        nseg: usize,
        local: DMat4,
    ) {
        self.add(name, mesh::loft(secs, nseg), mat, local);
    }

    /// A loft whose axis is the frame's Y: sections `(y, rx along x, rz along z)`.
    pub fn loft_y(
        &mut self,
        name: &'static str,
        secs: &[Sec],
        mat: MaterialSlot,
        nseg: usize,
        c: [f64; 3],
    ) {
        self.add(
            name,
            mesh::loft(secs, nseg),
            mat,
            tr(c[0], c[1], c[2]) * rx(-FRAC_PI_2),
        );
    }

    pub fn dome(&mut self, name: &'static str, c: [f64; 3], r: f64, h: f64, mat: MaterialSlot) {
        let mut m = mesh::uv_sphere(1.0, [5, 8]);
        m.scale(DVec3::new(r, r, h));
        self.add(name, m, mat, tr(c[0], c[1], c[2]));
    }

    // ---- emitters ---------------------------------------------------------------------------

    /// One emitter of cell `k` at `c`: a square plate or a round/hex disc.
    #[allow(clippy::too_many_arguments)]
    pub fn emit_cell(
        &mut self,
        k: u32,
        c: [f64; 3],
        shape: &str,
        size: Size,
        depth: f64,
        mat: MaterialSlot,
    ) {
        let (m, wh, outline) = if shape == "square" {
            let (w, h) = match size {
                Size::D(d) => (d, d),
                Size::Wh(w, h) => (w, h),
            };
            (
                mesh::cuboid(DVec3::new(w.max(1e-4), h.max(1e-4), depth)),
                [w, h],
                EmitterShape::Rect,
            )
        } else {
            let d = match size {
                Size::D(d) | Size::Wh(d, _) => d,
            };
            let hex = shape == "hex";
            let m = mesh::cylinder((d / 2.0).max(1e-4), depth, if hex { 6 } else { 12 });
            (
                m,
                [d, d],
                if hex {
                    EmitterShape::Hex
                } else {
                    EmitterShape::Circle
                },
            )
        };
        self.emit(k, m, c, mat, wh, outline);
    }

    /// An emitter of cell `k` with its own mesh (a tube).
    pub fn emit_mesh(&mut self, k: u32, m: Mesh, c: [f64; 3], mat: MaterialSlot, dims: [f64; 2]) {
        self.emit(k, m, c, mat, dims, EmitterShape::Tube);
    }

    fn emit(
        &mut self,
        k: u32,
        mut m: Mesh,
        c: [f64; 3],
        mat: MaterialSlot,
        size: [f64; 2],
        shape: EmitterShape,
    ) {
        let j = self.cell_em.entry(k).or_insert(0);
        let name = format!("cell_{k}_em_{j}");
        *j += 1;
        let c = DVec3::from(c);
        let rec = EmitRec {
            cell: k,
            centre: self.to_node(c),
            normal: self.dir_to_node(DVec3::Z),
            u: self.dir_to_node(DVec3::X),
            size,
            shape,
            colour: self.colour.clone(),
        };
        self.add_named(&name, &mut m, mat, DMat4::from_translation(c));
        self.items.push(Item {
            name: "cell_",
            node: self.cur,
            mat,
            mesh: m,
            emit: Some(rec),
        });
    }
}
