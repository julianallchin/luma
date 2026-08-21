//! CPU raycasting: a binned-SAH BVH per mesh, built once at asset load.
//!
//! There is no GPU picking buffer and there will not be one (spec §5.1): the
//! query that matters most — "what surface is under this world point,
//! straight down" — has no camera and therefore no answer in screen space.
//! One raycaster, used by picking, the ghost, and the snap surface probe.

use crate::aabb::Aabb;
use crate::graph::{MeshHandle, NodeId};
use glam::{Mat4, Vec3};

/// Where `luma-assets` mesh geometry comes from at raycast time. Implemented
/// by the asset store; this crate only holds the handle.
pub trait MeshSource {
    fn mesh(&self, handle: MeshHandle) -> Option<&TriMesh>;
}

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3,
    /// Unit length in world space. A ray transformed into a scaled node's
    /// local space is *not* unit length, which is exactly why hit distances
    /// are re-measured in world space.
    pub dir: Vec3,
}

impl Ray {
    pub fn new(origin: Vec3, dir: Vec3) -> Self {
        Self {
            origin,
            dir: dir.normalize(),
        }
    }

    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }

    /// Transform into another space. The direction is deliberately left
    /// unnormalized so parameter `t` keeps its meaning across the transform.
    pub fn transformed(&self, m: &Mat4) -> Ray {
        Ray {
            origin: m.transform_point3(self.origin),
            dir: m.transform_vector3(self.dir),
        }
    }

    /// Distance along the ray at which `point` lies.
    pub fn t_of(&self, point: Vec3) -> f32 {
        (point - self.origin).dot(self.dir) / self.dir.length_squared()
    }
}

/// A hit on a scene node, in world space.
#[derive(Clone, Copy, Debug)]
pub struct RayHit {
    pub node: NodeId,
    pub t: f32,
    pub point: Vec3,
    pub face_normal: Vec3,
    pub tri: u32,
}

/// A hit in a mesh's own space.
#[derive(Clone, Copy, Debug)]
pub struct TriHit {
    pub t: f32,
    pub point: Vec3,
    pub normal: Vec3,
    pub tri: u32,
}

#[derive(Clone, Debug)]
struct BvhNode {
    bounds: Aabb,
    /// Leaf: first index into `tri_indices`. Interior: index of the left
    /// child, whose sibling is always the next node.
    first: u32,
    /// Zero for an interior node.
    count: u32,
}

/// Bounding volume hierarchy over one mesh's triangles.
#[derive(Clone, Debug)]
pub struct MeshBvh {
    nodes: Vec<BvhNode>,
    tri_indices: Vec<u32>,
}

/// Triangles per leaf below which splitting stops paying for itself.
const MAX_LEAF_TRIS: u32 = 4;
/// Centroid bins per axis for the SAH sweep.
const BINS: usize = 12;

impl MeshBvh {
    pub fn build(tri_bounds: &[Aabb]) -> MeshBvh {
        let mut bvh = MeshBvh {
            nodes: Vec::new(),
            tri_indices: (0..tri_bounds.len() as u32).collect(),
        };
        if tri_bounds.is_empty() {
            bvh.nodes.push(BvhNode {
                bounds: Aabb::EMPTY,
                first: 0,
                count: 0,
            });
            return bvh;
        }
        bvh.nodes.push(BvhNode {
            bounds: Aabb::EMPTY,
            first: 0,
            count: tri_bounds.len() as u32,
        });
        bvh.split(0, tri_bounds);
        bvh
    }

    pub fn bounds(&self) -> Aabb {
        self.nodes[0].bounds
    }

    fn split(&mut self, node_index: usize, tri_bounds: &[Aabb]) {
        let (first, count) = {
            let n = &self.nodes[node_index];
            (n.first as usize, n.count as usize)
        };
        let range = &self.tri_indices[first..first + count];

        let mut bounds = Aabb::EMPTY;
        let mut centroid_bounds = Aabb::EMPTY;
        for &t in range {
            bounds.union(&tri_bounds[t as usize]);
            centroid_bounds.expand(tri_bounds[t as usize].center());
        }
        self.nodes[node_index].bounds = bounds;

        if count as u32 <= MAX_LEAF_TRIS {
            return;
        }
        let extent = centroid_bounds.size();
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        if extent[axis] <= 0.0 {
            return; // all centroids coincide — no split can separate them
        }

        // Bin the centroids, then sweep the bin boundaries for the lowest SAH
        // cost.
        let scale = BINS as f32 / extent[axis];
        let mut bin_bounds = [Aabb::EMPTY; BINS];
        let mut bin_counts = [0u32; BINS];
        for &t in range {
            let c = tri_bounds[t as usize].center()[axis];
            let b = (((c - centroid_bounds.min[axis]) * scale) as usize).min(BINS - 1);
            bin_bounds[b].union(&tri_bounds[t as usize]);
            bin_counts[b] += 1;
        }

        let mut best_cost = f32::INFINITY;
        let mut best_split = 0usize;
        for split in 1..BINS {
            let mut left = Aabb::EMPTY;
            let mut left_n = 0u32;
            for b in 0..split {
                left.union(&bin_bounds[b]);
                left_n += bin_counts[b];
            }
            let mut right = Aabb::EMPTY;
            let mut right_n = 0u32;
            for b in split..BINS {
                right.union(&bin_bounds[b]);
                right_n += bin_counts[b];
            }
            if left_n == 0 || right_n == 0 {
                continue;
            }
            let cost = left.surface_area() * left_n as f32 + right.surface_area() * right_n as f32;
            if cost < best_cost {
                best_cost = cost;
                best_split = split;
            }
        }
        if best_split == 0 || best_cost >= bounds.surface_area() * count as f32 {
            return; // a leaf is cheaper than any split we found
        }

        let split_plane = centroid_bounds.min[axis] + best_split as f32 / scale;
        let slice = &mut self.tri_indices[first..first + count];
        let mut mid = 0;
        for i in 0..slice.len() {
            if tri_bounds[slice[i] as usize].center()[axis] < split_plane {
                slice.swap(i, mid);
                mid += 1;
            }
        }
        if mid == 0 || mid == count {
            return;
        }

        let left_index = self.nodes.len();
        self.nodes.push(BvhNode {
            bounds: Aabb::EMPTY,
            first: first as u32,
            count: mid as u32,
        });
        self.nodes.push(BvhNode {
            bounds: Aabb::EMPTY,
            first: (first + mid) as u32,
            count: (count - mid) as u32,
        });
        self.nodes[node_index].first = left_index as u32;
        self.nodes[node_index].count = 0;
        self.split(left_index, tri_bounds);
        self.split(left_index + 1, tri_bounds);
    }
}

/// A CPU triangle mesh with its BVH. `luma-assets` owns these; this crate
/// consumes them.
#[derive(Clone, Debug)]
pub struct TriMesh {
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
    bvh: MeshBvh,
}

impl TriMesh {
    pub fn new(positions: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> TriMesh {
        let tri_bounds: Vec<Aabb> = triangles
            .iter()
            .map(|t| Aabb::from_points(t.iter().map(|&i| positions[i as usize])))
            .collect();
        let bvh = MeshBvh::build(&tri_bounds);
        TriMesh {
            positions,
            triangles,
            bvh,
        }
    }

    pub fn bounds(&self) -> Aabb {
        self.bvh.bounds()
    }

    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// Nearest forward hit, in the mesh's own space. Double-sided: stage GLBs
    /// are not reliably wound, and a back-facing floor still stops a
    /// downward probe.
    pub fn raycast(&self, ray: Ray) -> Option<TriHit> {
        if self.triangles.is_empty() {
            return None;
        }
        let inv_dir = Vec3::new(1.0 / ray.dir.x, 1.0 / ray.dir.y, 1.0 / ray.dir.z);
        let mut best: Option<TriHit> = None;
        let mut stack = vec![0usize];
        while let Some(index) = stack.pop() {
            let node = &self.bvh.nodes[index];
            let limit = best.map_or(f32::INFINITY, |h| h.t);
            if !slab_test(&node.bounds, ray.origin, inv_dir, limit) {
                continue;
            }
            if node.count == 0 {
                stack.push(node.first as usize);
                stack.push(node.first as usize + 1);
                continue;
            }
            for i in node.first..node.first + node.count {
                let tri = self.bvh.tri_indices[i as usize];
                let [a, b, c] = self.triangles[tri as usize];
                let hit = intersect_triangle(
                    ray,
                    self.positions[a as usize],
                    self.positions[b as usize],
                    self.positions[c as usize],
                );
                if let Some(t) = hit {
                    if t < best.map_or(f32::INFINITY, |h| h.t) {
                        let (pa, pb, pc) = (
                            self.positions[a as usize],
                            self.positions[b as usize],
                            self.positions[c as usize],
                        );
                        best = Some(TriHit {
                            t,
                            point: ray.at(t),
                            normal: (pb - pa).cross(pc - pa).normalize_or_zero(),
                            tri,
                        });
                    }
                }
            }
        }
        best
    }
}

/// Slab test against an AABB, accepting any overlap in `[0, limit]`.
fn slab_test(bounds: &Aabb, origin: Vec3, inv_dir: Vec3, limit: f32) -> bool {
    let t0 = (bounds.min - origin) * inv_dir;
    let t1 = (bounds.max - origin) * inv_dir;
    let t_near = t0.min(t1).max_element().max(0.0);
    let t_far = t0.max(t1).min_element();
    t_near <= t_far && t_near <= limit
}

/// Möller–Trumbore, double-sided.
fn intersect_triangle(ray: Ray, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    const EPS: f32 = 1e-8;
    let ab = b - a;
    let ac = c - a;
    let p = ray.dir.cross(ac);
    let det = ab.dot(p);
    if det.abs() < EPS {
        return None;
    }
    let inv_det = 1.0 / det;
    let tv = ray.origin - a;
    let u = tv.dot(p) * inv_det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = tv.cross(ab);
    let v = ray.dir.dot(q) * inv_det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = ac.dot(q) * inv_det;
    (t > EPS).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×2 quad on the z = 0 plane, subdivided into `n × n` cells, so the
    /// BVH actually has to split.
    fn grid_quad(n: usize) -> TriMesh {
        let mut positions = Vec::new();
        let mut triangles = Vec::new();
        for iy in 0..=n {
            for ix in 0..=n {
                positions.push(Vec3::new(
                    -1.0 + 2.0 * ix as f32 / n as f32,
                    -1.0 + 2.0 * iy as f32 / n as f32,
                    0.0,
                ));
            }
        }
        let idx = |ix: usize, iy: usize| (iy * (n + 1) + ix) as u32;
        for iy in 0..n {
            for ix in 0..n {
                triangles.push([idx(ix, iy), idx(ix + 1, iy), idx(ix + 1, iy + 1)]);
                triangles.push([idx(ix, iy), idx(ix + 1, iy + 1), idx(ix, iy + 1)]);
            }
        }
        TriMesh::new(positions, triangles)
    }

    #[test]
    fn hits_the_plane_it_should() {
        let mesh = grid_quad(8);
        let hit = mesh
            .raycast(Ray::new(Vec3::new(0.25, -0.5, 3.0), -Vec3::Z))
            .expect("ray through the quad hits");
        assert!((hit.t - 3.0).abs() < 1e-5);
        assert!(hit.point.abs_diff_eq(Vec3::new(0.25, -0.5, 0.0), 1e-5));
        assert!(hit.normal.abs_diff_eq(Vec3::Z, 1e-5) || hit.normal.abs_diff_eq(-Vec3::Z, 1e-5));
    }

    #[test]
    fn misses_outside_the_quad_and_behind_the_origin() {
        let mesh = grid_quad(8);
        assert!(mesh
            .raycast(Ray::new(Vec3::new(4.0, 0.0, 3.0), -Vec3::Z))
            .is_none());
        assert!(mesh
            .raycast(Ray::new(Vec3::new(0.0, 0.0, 3.0), Vec3::Z))
            .is_none());
    }

    #[test]
    fn bvh_agrees_with_brute_force_over_a_sweep() {
        let mesh = grid_quad(6);
        for i in 0..40 {
            let x = -1.4 + 0.07 * i as f32;
            let ray = Ray::new(Vec3::new(x, 0.13, 2.0), -Vec3::Z);
            let brute = (0..mesh.triangles.len())
                .filter_map(|t| {
                    let [a, b, c] = mesh.triangles[t];
                    intersect_triangle(
                        ray,
                        mesh.positions[a as usize],
                        mesh.positions[b as usize],
                        mesh.positions[c as usize],
                    )
                })
                .fold(f32::INFINITY, f32::min);
            let got = mesh.raycast(ray).map_or(f32::INFINITY, |h| h.t);
            assert!(
                (brute - got).abs() < 1e-5 || (brute.is_infinite() && got.is_infinite()),
                "x={x}: brute {brute} vs bvh {got}"
            );
        }
    }

    #[test]
    fn build_survives_degenerate_input() {
        let empty = TriMesh::new(Vec::new(), Vec::new());
        assert!(empty.raycast(Ray::new(Vec3::ZERO, Vec3::Z)).is_none());
        // Every centroid identical: the SAH sweep must fall back to a leaf.
        let a = Vec3::ZERO;
        let mesh = TriMesh::new(vec![a, Vec3::X, Vec3::Y], vec![[0, 1, 2]; 8]);
        assert_eq!(mesh.triangle_count(), 8);
    }
}
