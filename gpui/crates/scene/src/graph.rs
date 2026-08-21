//! Retained scene graph — flat, arena-indexed, Z-up, f32.
//!
//! Not an ECS: there is no gameplay, no systems, and no queries beyond "draw
//! everything" and "raycast". The graph owns the authoritative transforms —
//! nothing downstream may hold a live handle to a node's matrix and mutate it
//! mid-drag, which is the arrangement this replaces.

use crate::aabb::Aabb;
use crate::bvh::{MeshSource, Ray, RayHit};
use glam::{Mat4, Quat, Vec3};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Handle into `luma-assets`' mesh arena. Opaque here — this crate never loads
/// anything, it only routes the handle to a [`MeshSource`] at raycast time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct MeshHandle(pub u32);

/// Handle into `luma-assets`' material arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct MaterialHandle(pub u32);

/// Identifies a light-emitting fixture head to the render side.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct EmitterId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    pub fn from_translation(t: Vec3) -> Self {
        Self {
            translation: t,
            ..Default::default()
        }
    }

    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeContent {
    Empty,
    Mesh {
        mesh: MeshHandle,
        material: MaterialHandle,
    },
    Emitter(EmitterId),
}

/// Bit flags on a node. A newtype rather than a `bitflags` dependency —
/// there are four of them and they will not grow into a language.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeFlags(pub u8);

impl NodeFlags {
    pub const VISIBLE: NodeFlags = NodeFlags(1 << 0);
    pub const PICKABLE: NodeFlags = NodeFlags(1 << 1);
    pub const CASTS_SHADOW: NodeFlags = NodeFlags(1 << 2);
    pub const RECEIVES_SHADOW: NodeFlags = NodeFlags(1 << 3);
    pub const NONE: NodeFlags = NodeFlags(0);
    /// What a piece of stage geometry gets by default.
    pub const DEFAULT: NodeFlags = NodeFlags(0b1111);

    pub fn contains(self, other: NodeFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn with(self, other: NodeFlags) -> NodeFlags {
        NodeFlags(self.0 | other.0)
    }

    pub fn without(self, other: NodeFlags) -> NodeFlags {
        NodeFlags(self.0 & !other.0)
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub local: Transform,
    /// Cached world matrix, valid after
    /// [`SceneGraph::update_world_transforms`].
    pub world: Mat4,
    pub content: NodeContent,
    pub flags: NodeFlags,
}

/// Which nodes a raycast may hit.
#[derive(Clone, Copy, Debug)]
pub struct PickFilter {
    pub required: NodeFlags,
    pub exclude: Option<NodeId>,
}

impl Default for PickFilter {
    fn default() -> Self {
        Self {
            required: NodeFlags::VISIBLE.with(NodeFlags::PICKABLE),
            exclude: None,
        }
    }
}

#[derive(Default)]
pub struct SceneGraph {
    nodes: Vec<Node>,
    dirty: Vec<bool>,
    /// Topological: parents before children. Insertion maintains it (a parent
    /// must exist before its child), and [`SceneGraph::set_parent`] rebuilds
    /// it.
    order: Vec<NodeId>,
}

impl SceneGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn insert(
        &mut self,
        parent: Option<NodeId>,
        local: Transform,
        content: NodeContent,
        flags: NodeFlags,
    ) -> NodeId {
        if let Some(p) = parent {
            assert!(
                (p.0 as usize) < self.nodes.len(),
                "parent must be inserted before its child"
            );
        }
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            parent,
            local,
            world: Mat4::IDENTITY,
            content,
            flags,
        });
        self.dirty.push(true);
        self.order.push(id);
        id
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (NodeId(i as u32), n))
    }

    pub fn set_local(&mut self, id: NodeId, local: Transform) {
        self.nodes[id.0 as usize].local = local;
        self.dirty[id.0 as usize] = true;
    }

    pub fn set_flags(&mut self, id: NodeId, flags: NodeFlags) {
        self.nodes[id.0 as usize].flags = flags;
    }

    /// Re-parent a node, keeping its *local* transform. Rebuilds the
    /// topological order, so it is a structural edit, not a per-frame one.
    pub fn set_parent(&mut self, id: NodeId, parent: Option<NodeId>) {
        assert!(parent != Some(id), "a node cannot be its own parent");
        self.nodes[id.0 as usize].parent = parent;
        self.dirty[id.0 as usize] = true;
        self.rebuild_order();
    }

    fn rebuild_order(&mut self) {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut emitted = vec![false; self.nodes.len()];
        // Depth counts as the sort key: a node is emitted once its parent is.
        let mut remaining = self.nodes.len();
        while remaining > 0 {
            let before = remaining;
            for (i, node) in self.nodes.iter().enumerate() {
                if emitted[i] {
                    continue;
                }
                let ready = match node.parent {
                    None => true,
                    Some(p) => emitted[p.0 as usize],
                };
                if ready {
                    emitted[i] = true;
                    order.push(NodeId(i as u32));
                    remaining -= 1;
                }
            }
            assert!(before != remaining, "scene graph has a parent cycle");
        }
        self.order = order;
    }

    /// Single pass over the topological order. Runs once per frame, before
    /// anything reads [`Node::world`].
    pub fn update_world_transforms(&mut self) {
        let mut recomputed = vec![false; self.nodes.len()];
        for &NodeId(i) in &self.order {
            let i = i as usize;
            let parent = self.nodes[i].parent;
            let parent_moved = parent.is_some_and(|p| recomputed[p.0 as usize]);
            if !self.dirty[i] && !parent_moved {
                continue;
            }
            let parent_world = parent.map_or(Mat4::IDENTITY, |p| self.nodes[p.0 as usize].world);
            self.nodes[i].world = parent_world * self.nodes[i].local.matrix();
            self.dirty[i] = false;
            recomputed[i] = true;
        }
    }

    pub fn world(&self, id: NodeId) -> Mat4 {
        self.nodes[id.0 as usize].world
    }

    /// World-space bounds of every mesh node the filter admits. Used by the
    /// camera framing and the marquee.
    pub fn bounds(&self, meshes: &dyn MeshSource, filter: PickFilter) -> Aabb {
        let mut bounds = Aabb::EMPTY;
        for (id, node) in self.iter() {
            let Some(mesh) = self.pickable_mesh(id, node, meshes, filter) else {
                continue;
            };
            let local = mesh.bounds();
            // Transforming the 8 corners is exact enough and cheap at this
            // scene size.
            for i in 0..8 {
                let c = Vec3::new(
                    if i & 1 == 0 { local.min.x } else { local.max.x },
                    if i & 2 == 0 { local.min.y } else { local.max.y },
                    if i & 4 == 0 { local.min.z } else { local.max.z },
                );
                bounds.expand(node.world.transform_point3(c));
            }
        }
        bounds
    }

    fn pickable_mesh<'a>(
        &self,
        id: NodeId,
        node: &Node,
        meshes: &'a dyn MeshSource,
        filter: PickFilter,
    ) -> Option<&'a crate::bvh::TriMesh> {
        if filter.exclude == Some(id) || !node.flags.contains(filter.required) {
            return None;
        }
        match node.content {
            NodeContent::Mesh { mesh, .. } => meshes.mesh(mesh),
            _ => None,
        }
    }

    /// CPU raycast against every admitted mesh node, nearest first.
    ///
    /// The ray is transformed into each candidate's local space rather than
    /// the BVH into world space — cheaper, and it keeps one BVH per mesh
    /// rather than one per instance.
    pub fn raycast(&self, ray: Ray, filter: PickFilter, meshes: &dyn MeshSource) -> Vec<RayHit> {
        let mut hits = Vec::new();
        for (id, node) in self.iter() {
            let Some(mesh) = self.pickable_mesh(id, node, meshes, filter) else {
                continue;
            };
            let inv = node.world.inverse();
            let local_ray = ray.transformed(&inv);
            let Some(hit) = mesh.raycast(local_ray) else {
                continue;
            };
            let point = node.world.transform_point3(hit.point);
            // Normals transform by the inverse-transpose; non-uniform scale on
            // stage pieces is rare but legal.
            let face_normal = (inv.transpose().transform_vector3(hit.normal)).normalize_or_zero();
            hits.push(RayHit {
                node: id,
                t: ray.t_of(point),
                point,
                face_normal,
                tri: hit.tri,
            });
        }
        hits.sort_by(|a, b| a.t.total_cmp(&b.t));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(g: &mut SceneGraph, parent: Option<NodeId>, t: Vec3) -> NodeId {
        g.insert(
            parent,
            Transform::from_translation(t),
            NodeContent::Empty,
            NodeFlags::DEFAULT,
        )
    }

    #[test]
    fn world_transforms_compose_down_the_chain() {
        let mut g = SceneGraph::new();
        let a = leaf(&mut g, None, Vec3::new(1.0, 0.0, 0.0));
        let b = leaf(&mut g, Some(a), Vec3::new(0.0, 2.0, 0.0));
        let c = leaf(&mut g, Some(b), Vec3::new(0.0, 0.0, 3.0));
        g.update_world_transforms();
        assert_eq!(
            g.world(c).transform_point3(Vec3::ZERO),
            Vec3::new(1.0, 2.0, 3.0)
        );
    }

    #[test]
    fn moving_a_parent_moves_its_subtree() {
        let mut g = SceneGraph::new();
        let a = leaf(&mut g, None, Vec3::ZERO);
        let b = leaf(&mut g, Some(a), Vec3::new(0.0, 0.0, 1.0));
        g.update_world_transforms();
        g.set_local(a, Transform::from_translation(Vec3::new(5.0, 0.0, 0.0)));
        g.update_world_transforms();
        assert_eq!(
            g.world(b).transform_point3(Vec3::ZERO),
            Vec3::new(5.0, 0.0, 1.0)
        );
    }

    #[test]
    fn reparenting_rebuilds_the_topological_order() {
        let mut g = SceneGraph::new();
        let a = leaf(&mut g, None, Vec3::new(1.0, 0.0, 0.0));
        let b = leaf(&mut g, None, Vec3::new(0.0, 1.0, 0.0));
        g.set_parent(a, Some(b)); // child now precedes its parent in the arena
        g.update_world_transforms();
        assert_eq!(
            g.world(a).transform_point3(Vec3::ZERO),
            Vec3::new(1.0, 1.0, 0.0)
        );
    }
}
