//! Experimental shared stage-geometry visibility. The immutable world-space
//! BVH survives light/camera changes; authored stage transforms invalidate it.
//! Fixture housings are excluded, matching the fixture-shadow caster policy.
use std::hash::{Hash, Hasher};

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use wgpu::util::DeviceExt;

use crate::frame::{EditorObject, Frame};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Node {
    lo: [f32; 3],
    escape: u32,
    hi: [f32; 3],
    leaf: u32,
    a: [f32; 4],
    b: [f32; 4],
    c: [f32; 4],
}

#[derive(Clone, Copy)]
struct Triangle([Vec3; 3]);

impl Triangle {
    fn center(self) -> Vec3 {
        (self.0[0] + self.0[1] + self.0[2]) / 3.0
    }
}

fn build(triangles: &mut [Triangle], nodes: &mut Vec<Node>) {
    let index = nodes.len();
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for triangle in triangles.iter() {
        for point in triangle.0 {
            lo = lo.min(point);
            hi = hi.max(point);
        }
    }
    nodes.push(Node {
        lo: lo.to_array(),
        hi: hi.to_array(),
        ..Node::zeroed()
    });
    if triangles.len() == 1 {
        let [a, b, c] = triangles[0].0;
        nodes[index].leaf = 1;
        nodes[index].a = a.extend(0.0).to_array();
        nodes[index].b = (b - a).extend(0.0).to_array();
        nodes[index].c = (c - a).extend(0.0).to_array();
    } else {
        let extent = hi - lo;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        let mid = triangles.len() / 2;
        triangles.select_nth_unstable_by(mid, |a, b| a.center()[axis].total_cmp(&b.center()[axis]));
        let (left, right) = triangles.split_at_mut(mid);
        build(left, nodes);
        build(right, nodes);
    }
    nodes[index].escape = nodes.len() as u32;
}

pub(crate) struct Visibility {
    key: Option<u64>,
    pub buffer: wgpu::Buffer,
}

impl Visibility {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            key: None,
            buffer: upload(device, &[Node::zeroed()]),
        }
    }

    pub fn update(&mut self, device: &wgpu::Device, frame: &Frame, enabled: bool) {
        let casters = || {
            frame
                .draws
                .iter()
                .take(frame.draws.len() - frame.transparent.len())
                .filter(|d| !matches!(d.editor_object, Some(EditorObject::Fixture(_))))
        };
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        enabled.hash(&mut hash);
        if enabled {
            for draw in casters() {
                frame.meshes[draw.mesh].key.hash(&mut hash);
                for v in draw.model.to_cols_array() {
                    v.to_bits().hash(&mut hash);
                }
            }
        }
        let key = hash.finish();
        if self.key == Some(key) {
            return;
        }
        let mut triangles = Vec::new();
        if enabled {
            for draw in casters() {
                let mesh = &frame.meshes[draw.mesh];
                for ids in mesh.indices.chunks_exact(3) {
                    let triangle = Triangle(std::array::from_fn(|i| {
                        draw.model.transform_point3(Vec3::from_array(
                            mesh.vertices[ids[i] as usize].position,
                        ))
                    }));
                    if triangle.0.iter().all(|p| p.is_finite()) {
                        triangles.push(triangle);
                    }
                }
            }
        }
        let mut nodes = Vec::new();
        if triangles.is_empty() {
            nodes.push(Node::zeroed());
        } else {
            build(&mut triangles, &mut nodes);
        }
        self.buffer = upload(device, &nodes);
        self.key = Some(key);
    }
}

fn upload(device: &wgpu::Device, nodes: &[Node]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("stage-visibility-bvh"),
        contents: bytemuck::cast_slice(nodes),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn threaded_tree_skips_whole_subtrees() {
        let mut triangles: Vec<_> = (0..17)
            .map(|i| {
                let p = Vec3::new(i as f32, 0.0, 0.0);
                Triangle([p, p + Vec3::Y, p + Vec3::Z])
            })
            .collect();
        let mut nodes = Vec::new();
        build(&mut triangles, &mut nodes);
        assert_eq!(nodes.len(), 33);
        assert_eq!(nodes[0].escape, 33);
        assert_eq!(nodes.iter().filter(|n| n.leaf == 1).count(), 17);
        for (i, node) in nodes.iter().enumerate() {
            assert!(node.escape as usize > i);
            assert!(node.escape as usize <= nodes.len());
            if node.leaf == 1 {
                assert_eq!(node.escape as usize, i + 1);
            }
        }
    }
}
