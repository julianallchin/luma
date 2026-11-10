//! Per-frame planning of the lit-interval cache (`interval_cache.rs`): which
//! shadow slots' cached traversal is still exact, and the pools it lives in.

use glam::Mat4;

use crate::frame::Frame;
use crate::haze_compact::HAZE_WORKGROUP;
use crate::light_index::LightRest;

use super::camera::opaque_depth_hash;
use super::uniforms::IntervalCacheUniform;
use super::{Renderer, CAMERA_NEAR};

/// GPU pools of the lit-interval cache. The header holds one word per
/// (8×4 block, shadow slot) region; the payload table holds hashed interval
/// lists for the few pairs that are neither whole-lit nor overflowing.
pub(super) struct IntervalCachePools {
    /// `(blocks per frame, shadow slot capacity)` the header was sized for.
    shape: (u32, usize),
    header: wgpu::Buffer,
    claims: wgpu::Buffer,
    table: wgpu::Buffer,
    /// Hash bucket count minus one.
    bucket_mask: u32,
}

impl Renderer {
    /// Decide which shadow slots' cached traversal output is still exact this
    /// frame, upload the per-slot table and return the compute kernel's cache
    /// bindings. With `active` false (no lit compute haze, or no cached
    /// shadows) every slot is off.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan_interval_cache(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Frame,
        active: bool,
        opaque: usize,
        inv_view_proj: &Mat4,
        camera_far: f32,
        size: (u32, u32),
        haze_size: (u32, u32),
        shadow_slots: &[Option<usize>],
        fixture_cones: &[crate::frame::FixtureCone],
        rests: &[LightRest],
    ) -> (wgpu::Buffer, wgpu::Buffer, wgpu::Buffer, wgpu::Buffer) {
        use crate::interval_cache::{block_rect, FrameKey, SlotKey};
        let blocks = (
            haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
            haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
        );
        let blocks_total = blocks.0 * blocks.1;
        let capacity = shadow_slots.len().min(512);
        let mut uniform = IntervalCacheUniform {
            params: [blocks.0, 0, u32::from(haze_size == size), 0],
            flags: [0, self.interval_cache.epoch(), 0, 0],
            slots: [[0; 4]; 512],
        };
        // Each 8×4 workgroup must lie inside one light-index tile so every
        // lane of a block walks the same light list (`haze_cache.wgsl`):
        // true at native haze resolution, which is the only live setting.
        let active = active && capacity > 0 && haze_size == size;
        if active {
            let limits = self.gpu.device.limits();
            let table_entries = 1u64 << self.gpu.interval_cache_table_bits;
            let entry_words = 2 + 2 * u64::from(crate::interval_cache::CACHE_K);
            // Two header words per (block, slot) record, capped by the binding
            // limit; slots whose regions do not fit simply stay off.
            let limit_words =
                u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size) / 4;
            let header_words =
                (2 * u64::from(blocks_total) * capacity as u64).min(limit_words) as u32;
            let stale = self
                .interval_pools
                .as_ref()
                .is_none_or(|pools| pools.shape != (blocks_total, capacity));
            if stale {
                let buffer = |label: &str, words: u64| {
                    self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(label),
                        size: (words * 4).max(4),
                        usage: wgpu::BufferUsages::STORAGE,
                        mapped_at_creation: false,
                    })
                };
                // Only the header follows the frame's shape. The payload table
                // and its claims are sized by the device, and what they hold
                // stays unreadable across a resize by itself: entries are keyed
                // by generations that are never reissued and claims by epochs
                // that never repeat. So a resize drag, which changes the shape
                // on nearly every frame, keeps them rather than reallocating
                // (and having the kernel map) the largest buffers in the frame.
                // The header is kept while it is big enough and otherwise
                // grows with headroom, for the same drag.
                let previous = self
                    .interval_pools
                    .take()
                    .filter(|pools| pools.table.size() == (table_entries * entry_words * 4).max(4));
                let header = previous
                    .as_ref()
                    .filter(|pools| pools.header.size() >= u64::from(header_words) * 4)
                    .map(|pools| pools.header.clone())
                    .unwrap_or_else(|| {
                        let grown = (u64::from(header_words) * 5 / 4).min(limit_words);
                        buffer("interval-cache-header", grown)
                    });
                let (claims, table) = match previous {
                    Some(pools) => (pools.claims, pools.table),
                    None => (
                        buffer("interval-cache-claims", table_entries),
                        buffer("interval-cache-table", table_entries * entry_words),
                    ),
                };
                self.interval_pools = Some(IntervalCachePools {
                    shape: (blocks_total, capacity),
                    header,
                    claims,
                    table,
                    bucket_mask: (table_entries / 4 - 1) as u32,
                });
                self.interval_cache.resize(header_words / 2);
            }
            let mut camera = [0u32; 25];
            for (target, value) in camera.iter_mut().zip(
                inv_view_proj
                    .to_cols_array()
                    .into_iter()
                    .chain(frame.camera.eye.to_array())
                    .chain([
                        haze_size.0 as f32,
                        haze_size.1 as f32,
                        size.0 as f32,
                        size.1 as f32,
                        CAMERA_NEAR,
                        camera_far,
                    ]),
            ) {
                *target = value.to_bits();
            }
            let frame_key = FrameKey {
                camera,
                depth: opaque_depth_hash(frame, opaque),
            };
            let scale = [
                size.0 as f32 / haze_size.0 as f32,
                size.1 as f32 / haze_size.1 as f32,
            ];
            let rects = self.light_index.source_rects();
            let keys: Vec<Option<SlotKey>> = shadow_slots[..capacity]
                .iter()
                .enumerate()
                .map(|(slot, resident)| {
                    let index = (*resident)?;
                    let shadow = self.fixture_shadow_cache.get(slot).copied().flatten()?;
                    let tiles = rects.get(index).copied().flatten()?;
                    let cone = &fixture_cones[index];
                    Some(SlotKey {
                        shadow,
                        range: cone.range.to_bits(),
                        cos_field: cone.cos_field.to_bits(),
                        lens_distance: rests[index].lens_distance.to_bits(),
                        wash: cone.wash.to_bits(),
                        scatters: rests[index].haze_gain > 0.0,
                        gobo: cone.gobo != 0,
                        rect: block_rect(tiles, scale, [blocks.0, blocks.1]),
                    })
                })
                .collect();
            let entries = self.interval_cache.plan(frame_key, &keys);
            for (target, entry) in uniform.slots.iter_mut().zip(&entries) {
                *target = *entry;
            }
            self.interval_cache_entries = entries;
            let pools = self.interval_pools.as_ref().expect("allocated above");
            uniform.params = [blocks.0, pools.bucket_mask, 1, 0];
        } else {
            self.interval_cache.idle();
            self.interval_cache_entries.clear();
        }
        let uniform_buf = self.storage(
            encoder,
            &[uniform],
            wgpu::BufferUsages::UNIFORM,
            "interval-cache-params",
        );
        let pools = self.interval_pools.get_or_insert_with(|| {
            let buffer = |label: &str| {
                self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: 4,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                })
            };
            IntervalCachePools {
                shape: (0, 0),
                header: buffer("interval-cache-header"),
                claims: buffer("interval-cache-claims"),
                table: buffer("interval-cache-table"),
                bucket_mask: 0,
            }
        });
        (
            uniform_buf,
            pools.header.clone(),
            pools.claims.clone(),
            pools.table.clone(),
        )
    }
}
