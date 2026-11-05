//! GPU buffers: per-frame uploads pooled by label through the staging belt,
//! and the immutable geometry bank resident across frames.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use wgpu::util::DeviceExt;

use crate::frame::Frame;

use super::Renderer;

/// A storage buffer kept across frames and grown only when the contents stop
/// fitting. Cluster rebuilds are frequent — every frame the camera moves — and
/// at 512 cones the index list is tens of megabytes, so reallocating per
/// rebuild costs more than filling the buffer does.
pub(super) struct GrowableStorage {
    buffer: wgpu::Buffer,
    /// Allocation size in bytes, which may exceed the bytes currently live.
    capacity: usize,
}

/// Upload `data`, reusing `slot`'s allocation when it is large enough.
///
/// The buffer may end up larger than the live contents. No shader reads a
/// buffer's own length (counts travel in uniforms and CSR headers), so the
/// stale tail is never addressed. A belt copy is ordered before the passes
/// that read it by being recorded into `encoder` first, so overwriting
/// between frames never races a frame still executing — and growth
/// allocates a fresh buffer, which naturally quarantines the old allocation
/// with whatever submission still reads it.
fn grow_storage<T: Pod>(
    device: &wgpu::Device,
    belt: &mut wgpu::util::StagingBelt,
    encoder: &mut wgpu::CommandEncoder,
    slot: Option<GrowableStorage>,
    data: &[T],
    usage: wgpu::BufferUsages,
    label: &str,
) -> GrowableStorage {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    // A copy region must be word-sized; every upload here is a Pod struct of
    // words, so this holds by construction rather than by padding.
    debug_assert!(bytes.len() % wgpu::COPY_BUFFER_ALIGNMENT as usize == 0);
    match slot {
        Some(store) if store.capacity >= bytes.len() && store.buffer.usage().contains(usage) => {
            // Through the staging belt, not `Queue::write_buffer`: the queue
            // path allocates a fresh staging buffer per call, and each
            // allocation is an IOKit round trip. The belt recycles its
            // chunks — measured at ~1 µs to finish and ~30 µs for a whole
            // frame's uploads, 16 MB cluster indices included.
            if let Some(size) = wgpu::BufferSize::new(bytes.len() as u64) {
                belt.write_buffer(encoder, &store.buffer, 0, size)
                    .copy_from_slice(bytes);
            }
            store
        }
        _ => GrowableStorage {
            buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytes,
                usage: usage | wgpu::BufferUsages::COPY_DST,
            }),
            capacity: bytes.len(),
        },
    }
}

pub(super) struct ResidentGeometry {
    pub(super) keys: Vec<String>,
    pub(super) vertices: wgpu::Buffer,
    pub(super) indices: wgpu::Buffer,
    pub(super) ranges: Vec<(u32, u32, i32)>,
    /// Local-space bounding sphere per mesh, for culling casters against a
    /// cone. Computed with the upload because it depends on the same immutable
    /// vertex data and would otherwise be recomputed every frame.
    pub(super) bounds: Vec<(Vec3, f32)>,
}

impl ResidentGeometry {
    pub(super) fn matches(&self, frame: &Frame) -> bool {
        self.keys.len() == frame.meshes.len()
            && self
                .keys
                .iter()
                .zip(&frame.meshes)
                .all(|(resident, incoming)| resident == &incoming.key)
    }
}

impl Renderer {
    /// Upload one frame's worth of `data` under `label`, reusing the buffer
    /// the previous frame uploaded under the same label.
    ///
    /// The label is the identity: two uploads in one frame under one label
    /// would alias, so anything uploaded in a loop must carry its index in
    /// the label. Buffers only grow — a show's sizes oscillate every frame
    /// and the slack is the point. Measured effect: pooling plus the staging
    /// belt takes all per-frame upload work to ~30 µs; the frame's remaining
    /// CPU encode cost is the CPU binners, not the uploads.
    pub(super) fn storage<T: Pod>(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        data: &[T],
        usage: wgpu::BufferUsages,
        label: &str,
    ) -> wgpu::Buffer {
        let mut pool = self.frame_buffers.borrow_mut();
        let previous = pool.remove(label);
        let store = grow_storage(
            &self.gpu.device,
            &mut self.staging.borrow_mut(),
            encoder,
            previous,
            data,
            usage,
            label,
        );
        let buffer = store.buffer.clone();
        pool.insert(label.to_string(), store);
        buffer
    }

    /// Upload data that lives as long as its owner, not as long as a frame —
    /// mesh vertices and indices, retained per mesh in [`ResidentGeometry`].
    /// Deliberately not pooled: every mesh would share one label.
    pub(super) fn immutable<T: Pod>(
        &self,
        data: &[T],
        usage: wgpu::BufferUsages,
        label: &str,
    ) -> wgpu::Buffer {
        self.gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(data),
                usage,
            })
    }
}

/// Centre and radius of a bounding sphere around a mesh's vertices.
///
/// Centred on the midpoint of the extent rather than the centroid: a mesh with
/// most of its vertices clustered at one end would otherwise get a sphere that
/// has to reach much further to cover the rest.
pub(crate) fn local_bounding_sphere(vertices: &[crate::assets::Vertex]) -> (Vec3, f32) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for vertex in vertices {
        let position = Vec3::from(vertex.position);
        min = min.min(position);
        max = max.max(position);
    }
    if !min.is_finite() || !max.is_finite() {
        return (Vec3::ZERO, 0.0);
    }
    let centre = (min + max) * 0.5;
    let radius = vertices.iter().fold(0.0_f32, |worst, vertex| {
        worst.max((Vec3::from(vertex.position) - centre).length_squared())
    });
    (centre, radius.sqrt())
}

/// wgpu rejects a zero-sized storage buffer; an empty light list still needs a
/// binding.
pub(super) fn pad_at_least_one<T: Pod + Zeroable>(mut v: Vec<T>) -> Vec<T> {
    if v.is_empty() {
        v.push(T::zeroed());
    }
    v
}
