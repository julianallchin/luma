//! Exact retained fog-segment visibility cache used by the opt-in prototype.
//!
//! Cached payloads contain the unchanged shadow hierarchy's raw 0..4 response
//! for one 4x4x4 fog block and one complete retained shadow-map identity. Any
//! unavailable resource, key change, fill deferral, or capacity failure leaves
//! a zero directory handle, which makes the grid execute the original function.

use std::collections::HashMap;
use wgpu::util::DeviceExt as _;

pub(crate) const MAX_SHADOW_SLOTS: u32 = 512;
pub(crate) const PAYLOAD_BYTES: u64 = 32;
pub(crate) const TOTAL_BUDGET_BYTES: u64 = 128 << 20;
pub(crate) const PAYLOAD_POOL_BYTES: u64 = 64 << 20;
pub(crate) const DEFAULT_FILL_BUDGET: u32 = 8_192;
pub(crate) const PLAN_BLOCK_WINDOW: u32 = 2_048;
pub(crate) const REQUIRED_STORAGE_BINDINGS: u32 = 9;
pub(crate) const PLAN_WORKGROUP_SIZE: u32 = 64;
pub(crate) const ALGORITHM_VERSION: u32 = 2;
pub(crate) const SHADOW_FORMAT_VERSION: u32 = 1;
const HEADER_WORDS: u64 = 64;
const HEADER_BYTES: u64 = HEADER_WORDS * 4;
const ARGS_BYTES: u64 = 12;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct GlobalKey {
    pub(crate) inv_view_proj: [u32; 16],
    pub(crate) camera_position: [u32; 3],
    /// Raw `HazeUniform.transport.w/z` bits.
    pub(crate) viewport: [u32; 2],
    pub(crate) grid_size: [u32; 3],
    pub(crate) camera_planes: [u32; 2],
    pub(crate) fog_far: u32,
    pub(crate) medium_min: [u32; 3],
    pub(crate) medium_max: [u32; 3],
    pub(crate) tile_size: u32,
    pub(crate) block_side: u32,
    pub(crate) slices: u32,
    pub(crate) algorithm_version: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ShadowContentKey {
    pub(crate) matrix: [u32; 16],
    /// Raw uploaded `FixtureShadowMatrix.params.x/y` bits.
    pub(crate) near: u32,
    pub(crate) far: u32,
    pub(crate) caster_hash: u64,
    pub(crate) format_version: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DeviceLimits {
    pub(crate) max_buffer_size: u64,
    pub(crate) max_storage_buffer_binding_size: u64,
    pub(crate) max_storage_buffers_per_shader_stage: u32,
    pub(crate) max_compute_workgroups_per_dimension: u32,
}

impl From<wgpu::Limits> for DeviceLimits {
    fn from(value: wgpu::Limits) -> Self {
        Self {
            max_buffer_size: value.max_buffer_size,
            max_storage_buffer_binding_size: u64::from(value.max_storage_buffer_binding_size),
            max_storage_buffers_per_shader_stage: value.max_storage_buffers_per_shader_stage,
            max_compute_workgroups_per_dimension: value.max_compute_workgroups_per_dimension,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CacheConfig {
    pub(crate) slot_capacity: u32,
    pub(crate) tile_size: u32,
    pub(crate) block_side: u32,
    pub(crate) slices: u32,
    pub(crate) payload_pool_bytes: u64,
    pub(crate) total_budget_bytes: u64,
    pub(crate) fills_per_frame: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CacheLayout {
    pub(crate) grid_size: [u32; 3],
    pub(crate) blocks: [u32; 3],
    pub(crate) block_count: u32,
    pub(crate) slot_capacity: u32,
    pub(crate) directory_offset_words: u32,
    pub(crate) fill_list_offset_words: u32,
    pub(crate) directory_buffer_bytes: u64,
    pub(crate) payload_buffer_bytes: u64,
    pub(crate) payload_capacity: u32,
    pub(crate) fills_per_frame: u32,
    pub(crate) slot_bytes: u64,
    pub(crate) total_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutError {
    InvalidConfig,
    ArithmeticOverflow,
    TooManyStorageBindings,
    WorkgroupLimit,
    BufferLimit,
    TotalBudget,
    AllocationFailure,
}

impl CacheLayout {
    pub(crate) fn for_viewport(
        viewport: [u32; 2],
        config: CacheConfig,
        limits: DeviceLimits,
    ) -> Result<Self, LayoutError> {
        if viewport.contains(&0)
            || config.tile_size == 0
            || config.block_side == 0
            || config.slices == 0
            || config.slot_capacity == 0
            || config.slot_capacity > MAX_SHADOW_SLOTS
            || config.fills_per_frame == 0
            || config.payload_pool_bytes < PAYLOAD_BYTES
        {
            return Err(LayoutError::InvalidConfig);
        }
        if limits.max_storage_buffers_per_shader_stage < REQUIRED_STORAGE_BINDINGS {
            return Err(LayoutError::TooManyStorageBindings);
        }
        let div_ceil = |n: u32, d: u32| n / d + u32::from(n % d != 0);
        let grid_size = [
            div_ceil(viewport[0], config.tile_size),
            div_ceil(viewport[1], config.tile_size),
            config.slices,
        ];
        let blocks = grid_size.map(|n| div_ceil(n, config.block_side));
        let block_count = blocks
            .into_iter()
            .try_fold(1_u32, u32::checked_mul)
            .ok_or(LayoutError::ArithmeticOverflow)?;
        if block_count
            .min(PLAN_BLOCK_WINDOW)
            .div_ceil(PLAN_WORKGROUP_SIZE)
            > limits.max_compute_workgroups_per_dimension
        {
            return Err(LayoutError::WorkgroupLimit);
        }
        let slot_bytes = u64::from(block_count)
            .checked_mul(4)
            .ok_or(LayoutError::ArithmeticOverflow)?;
        let directory_bytes = slot_bytes
            .checked_mul(u64::from(config.slot_capacity))
            .ok_or(LayoutError::ArithmeticOverflow)?;
        let payload_capacity = u32::try_from(config.payload_pool_bytes / PAYLOAD_BYTES)
            .map_err(|_| LayoutError::ArithmeticOverflow)?;
        let fills_per_frame = config
            .fills_per_frame
            .min(payload_capacity)
            .min(limits.max_compute_workgroups_per_dimension);
        if fills_per_frame == 0 {
            return Err(LayoutError::InvalidConfig);
        }
        let fill_list_bytes = u64::from(fills_per_frame)
            .checked_mul(4)
            .ok_or(LayoutError::ArithmeticOverflow)?;
        let directory_offset_words =
            u32::try_from(HEADER_WORDS).map_err(|_| LayoutError::ArithmeticOverflow)?;
        let fill_list_offset_words = u32::try_from(
            HEADER_BYTES
                .checked_add(directory_bytes)
                .ok_or(LayoutError::ArithmeticOverflow)?
                / 4,
        )
        .map_err(|_| LayoutError::ArithmeticOverflow)?;
        let directory_buffer_bytes = HEADER_BYTES
            .checked_add(directory_bytes)
            .and_then(|bytes| bytes.checked_add(fill_list_bytes))
            .ok_or(LayoutError::ArithmeticOverflow)?;
        let payload_buffer_bytes = u64::from(payload_capacity)
            .checked_mul(PAYLOAD_BYTES)
            .ok_or(LayoutError::ArithmeticOverflow)?;
        let total_bytes = directory_buffer_bytes
            .checked_add(payload_buffer_bytes)
            .and_then(|bytes| bytes.checked_add(slot_bytes))
            .and_then(|bytes| bytes.checked_add(ARGS_BYTES))
            .and_then(|bytes| bytes.checked_add(HEADER_BYTES))
            .ok_or(LayoutError::ArithmeticOverflow)?;
        let binding_limit = limits
            .max_buffer_size
            .min(limits.max_storage_buffer_binding_size);
        if directory_buffer_bytes > binding_limit || payload_buffer_bytes > binding_limit {
            return Err(LayoutError::BufferLimit);
        }
        if total_bytes > config.total_budget_bytes {
            return Err(LayoutError::TotalBudget);
        }
        Ok(Self {
            grid_size,
            blocks,
            block_count,
            slot_capacity: config.slot_capacity,
            directory_offset_words,
            fill_list_offset_words,
            directory_buffer_bytes,
            payload_buffer_bytes,
            payload_capacity,
            fills_per_frame,
            slot_bytes,
            total_bytes,
        })
    }

    fn slot_byte_range(self, slot: u32) -> Option<std::ops::Range<u64>> {
        if slot >= self.slot_capacity {
            return None;
        }
        let start = HEADER_BYTES + u64::from(slot) * self.slot_bytes;
        Some(start..start + self.slot_bytes)
    }
}

#[derive(Debug)]
struct Residency {
    global: Option<GlobalKey>,
    slots: Vec<Option<ShadowContentKey>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResidencyAction {
    ResetAll,
    ClearSlot(u32),
    CopySlot { source: u32, destination: u32 },
}

impl Residency {
    fn new(slot_capacity: u32) -> Self {
        Self {
            global: None,
            slots: vec![None; slot_capacity as usize],
        }
    }

    fn update(
        &mut self,
        global: GlobalKey,
        active: &[(u32, ShadowContentKey)],
    ) -> Vec<ResidencyAction> {
        let mut actions = Vec::new();
        let reset = self.global != Some(global);
        if reset {
            self.global = Some(global);
            self.slots.fill(None);
            actions.push(ResidencyAction::ResetAll);
        }
        let replacements: HashMap<u32, ShadowContentKey> = active.iter().copied().collect();
        let stable_source: HashMap<ShadowContentKey, u32> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(slot, key)| {
                let key = (*key)?;
                let slot = slot as u32;
                let remains = replacements.get(&slot).is_none_or(|next| *next == key);
                remains.then_some((key, slot))
            })
            .collect();
        for &(slot, key) in active {
            let Some(current) = self.slots.get(slot as usize).copied() else {
                continue;
            };
            if current == Some(key) {
                continue;
            }
            if !reset {
                if let Some(&source) = stable_source.get(&key).filter(|&&source| source != slot) {
                    actions.push(ResidencyAction::CopySlot {
                        source,
                        destination: slot,
                    });
                } else {
                    actions.push(ResidencyAction::ClearSlot(slot));
                }
            }
            self.slots[slot as usize] = Some(key);
        }
        actions
    }
}

struct Allocation {
    layout: CacheLayout,
    directory: wgpu::Buffer,
    payload: wgpu::Buffer,
    args: wgpu::Buffer,
    copy_scratch: wgpu::Buffer,
    residency: Residency,
}

/// CPU actions plus the retained GPU header at the explicit readback point.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Stats {
    /// The cache environment switch was requested.
    pub requested: bool,
    /// Device and configuration checks allow the cache pipelines.
    pub enabled: bool,
    /// The current epoch participates in fog-grid shading.
    pub active: bool,
    /// Exact-fallback reason when the requested cache cannot be used.
    pub layout_error: Option<LayoutError>,
    /// Current fog-grid dimensions in cells.
    pub grid_size: [u32; 3],
    /// Current cache dimensions in 4-cubed blocks.
    pub blocks: [u32; 3],
    /// Physical shadow slots represented by the directory.
    pub slot_capacity: u32,
    /// Bytes reserved by all cache-owned GPU buffers.
    pub total_bytes: u64,
    /// Complete directory resets during this renderer's lifetime.
    pub global_resets: u64,
    /// Per-slot directory clears during this renderer's lifetime.
    pub slot_clears: u64,
    /// Physical-slot migrations during this renderer's lifetime.
    pub slot_copies: u64,
    /// Payload records allocated in the current global epoch.
    pub payload_used: u32,
    /// Payload records filled by the most recently completed frame.
    pub fills_this_frame: u32,
    /// Planner block scans stopped early after the per-frame fill budget filled.
    /// This is not a count of exact-fallback records; zero directory handles
    /// continue to use the reference path independently.
    pub deferred_fallbacks: u32,
    /// Current-epoch grid lookups served from cached payloads.
    pub cache_hits: u32,
    /// Current-epoch grid lookups that executed the reference function.
    pub cache_fallbacks: u32,
    /// Current-epoch raw visibility evaluations performed by cache fills.
    pub cold_shadow_calls: u32,
    /// Current-epoch assertion mismatches; sample before every reset.
    pub raw_mismatches: u32,
}

/// Per-renderer buffers and exact key residency. Pipelines remain on `Gpu`.
pub(crate) struct State {
    allocation: Option<Allocation>,
    dummy_directory: wgpu::Buffer,
    dummy_payload: wgpu::Buffer,
    dummy_args: wgpu::Buffer,
    stats: Stats,
}

impl State {
    pub(crate) fn new(
        device: &wgpu::Device,
        requested: bool,
        unavailable: Option<LayoutError>,
    ) -> Self {
        let enabled = requested && unavailable.is_none();
        let zeros = [0_u32; HEADER_WORDS as usize];
        Self {
            allocation: None,
            dummy_directory: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fog-visibility-cache-disabled"),
                contents: bytemuck::cast_slice(&zeros),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            }),
            dummy_payload: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fog-visibility-cache-disabled-payload"),
                contents: bytemuck::bytes_of(&[0_u32; 8]),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            }),
            dummy_args: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fog-visibility-cache-disabled-args"),
                contents: bytemuck::bytes_of(&[0_u32; 3]),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
            }),
            stats: Stats {
                requested,
                enabled,
                layout_error: unavailable,
                ..Stats::default()
            },
        }
    }

    fn allocate(device: &wgpu::Device, layout: CacheLayout) -> Result<Allocation, LayoutError> {
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let allocation = Allocation {
            layout,
            directory: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fog-visibility-cache-directory"),
                size: layout.directory_buffer_bytes,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            payload: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fog-visibility-cache-payload"),
                size: layout.payload_buffer_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            args: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fog-visibility-cache-fill-args"),
                size: ARGS_BYTES,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::INDIRECT
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            copy_scratch: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fog-visibility-cache-slot-copy"),
                size: layout.slot_bytes,
                usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            residency: Residency::new(layout.slot_capacity),
        };
        let out_of_memory = pollster::block_on(out_of_memory.pop());
        let internal = pollster::block_on(internal.pop());
        let validation = pollster::block_on(validation.pop());
        let failed = out_of_memory.is_some() || internal.is_some() || validation.is_some();
        if failed {
            Err(LayoutError::AllocationFailure)
        } else {
            Ok(allocation)
        }
    }

    /// Prepare independent cache resources before plan/fill/grid are encoded.
    /// `frame_active=false` preserves residency and publishes `CACHE_ACTIVE=0`.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        limits: DeviceLimits,
        viewport: [u32; 2],
        slot_capacity: u32,
        frame_active: bool,
        global: GlobalKey,
        active_slots: &[(u32, ShadowContentKey)],
    ) {
        self.stats.active = false;
        self.stats.layout_error = None;
        if !frame_active && self.allocation.is_none() {
            return;
        }
        let config = CacheConfig {
            slot_capacity: slot_capacity.clamp(1, MAX_SHADOW_SLOTS),
            tile_size: crate::fog_grid::tile_size(),
            block_side: crate::fog_grid::BLOCK_SIDE,
            slices: crate::fog_grid::SLICES,
            payload_pool_bytes: PAYLOAD_POOL_BYTES,
            total_budget_bytes: TOTAL_BUDGET_BYTES,
            fills_per_frame: DEFAULT_FILL_BUDGET,
        };
        let layout = match CacheLayout::for_viewport(viewport, config, limits) {
            Ok(layout) => layout,
            Err(error) => {
                self.stats.layout_error = Some(error);
                self.allocation = None;
                return;
            }
        };
        if self
            .allocation
            .as_ref()
            .is_none_or(|allocation| allocation.layout != layout)
        {
            match Self::allocate(device, layout) {
                Ok(allocation) => self.allocation = Some(allocation),
                Err(error) => {
                    self.stats.layout_error = Some(error);
                    self.allocation = None;
                    return;
                }
            }
        }
        let allocation = self.allocation.as_mut().expect("layout allocated");
        encoder.clear_buffer(&allocation.args, 0, None);
        // Fill count is per frame. Other words are cumulative inside one global
        // epoch and are read explicitly after the measured run.
        encoder.clear_buffer(&allocation.directory, 4, Some(4));
        if !frame_active {
            let upload = header_upload(device, &[0]);
            encoder.copy_buffer_to_buffer(&upload, 0, &allocation.directory, 16 * 4, 4);
            return;
        }
        let actions = allocation.residency.update(global, active_slots);
        for action in actions {
            match action {
                ResidencyAction::ResetAll => {
                    encoder.clear_buffer(&allocation.directory, 0, None);
                    self.stats.global_resets += 1;
                }
                ResidencyAction::ClearSlot(slot) => {
                    if let Some(range) = layout.slot_byte_range(slot) {
                        encoder.clear_buffer(
                            &allocation.directory,
                            range.start,
                            Some(range.end - range.start),
                        );
                        self.stats.slot_clears += 1;
                    }
                }
                ResidencyAction::CopySlot {
                    source,
                    destination,
                } => {
                    let Some(source) = layout.slot_byte_range(source) else {
                        continue;
                    };
                    let Some(destination) = layout.slot_byte_range(destination) else {
                        continue;
                    };
                    encoder.copy_buffer_to_buffer(
                        &allocation.directory,
                        source.start,
                        &allocation.copy_scratch,
                        0,
                        layout.slot_bytes,
                    );
                    encoder.copy_buffer_to_buffer(
                        &allocation.copy_scratch,
                        0,
                        &allocation.directory,
                        destination.start,
                        layout.slot_bytes,
                    );
                    self.stats.slot_copies += 1;
                }
            }
        }
        let header = [
            1,
            layout.block_count,
            layout.blocks[0],
            layout.blocks[1],
            layout.blocks[2],
            layout.directory_offset_words,
            layout.fill_list_offset_words,
            layout.payload_capacity,
            layout.fills_per_frame,
        ];
        let upload = header_upload(device, &header);
        encoder.copy_buffer_to_buffer(
            &upload,
            0,
            &allocation.directory,
            16 * 4,
            std::mem::size_of_val(&header) as u64,
        );
        self.stats.active = true;
        self.stats.grid_size = layout.grid_size;
        self.stats.blocks = layout.blocks;
        self.stats.slot_capacity = layout.slot_capacity;
        self.stats.total_bytes = layout.total_bytes;
    }

    pub(crate) fn directory(&self) -> &wgpu::Buffer {
        self.allocation
            .as_ref()
            .map_or(&self.dummy_directory, |allocation| &allocation.directory)
    }

    pub(crate) fn payload(&self) -> &wgpu::Buffer {
        self.allocation
            .as_ref()
            .map_or(&self.dummy_payload, |allocation| &allocation.payload)
    }

    #[cfg(test)]
    pub(crate) fn poison_payload_prefix(&self, queue: &wgpu::Queue, records: u32) -> bool {
        let Some(allocation) = &self.allocation else {
            return false;
        };
        let records = records.min(allocation.layout.payload_capacity);
        if records == 0 {
            return false;
        }
        let poison = vec![u32::MAX; records as usize * (PAYLOAD_BYTES as usize / 4)];
        queue.write_buffer(&allocation.payload, 0, bytemuck::cast_slice(&poison));
        true
    }

    #[cfg(test)]
    pub(crate) fn read_slot_words(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
    ) -> anyhow::Result<Vec<u32>> {
        let allocation = self
            .allocation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("fog visibility cache is not allocated"))?;
        let range = allocation
            .layout
            .slot_byte_range(slot)
            .ok_or_else(|| anyhow::anyhow!("fog visibility slot {slot} is out of range"))?;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fog-visibility-cache-slot-readback"),
            size: allocation.layout.slot_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(
            &allocation.directory,
            range.start,
            &readback,
            0,
            allocation.layout.slot_bytes,
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(anyhow::Error::msg)?;
        receiver.recv().map_err(anyhow::Error::msg)??;
        let mapped = readback
            .slice(..)
            .get_mapped_range()
            .map_err(anyhow::Error::msg)?;
        let words = bytemuck::cast_slice(&mapped).to_vec();
        drop(mapped);
        readback.unmap();
        Ok(words)
    }

    pub(crate) fn args(&self) -> &wgpu::Buffer {
        self.allocation
            .as_ref()
            .map_or(&self.dummy_args, |allocation| &allocation.args)
    }

    pub(crate) fn active(&self) -> bool {
        self.stats.active
    }

    pub(crate) fn plan_workgroups(&self) -> u32 {
        self.stats
            .blocks
            .into_iter()
            .product::<u32>()
            .min(PLAN_BLOCK_WINDOW)
            .div_ceil(PLAN_WORKGROUP_SIZE)
    }

    /// Explicit synchronization point for diagnostic metadata, never called
    /// inside frame encoding or included in performance timings.
    pub(crate) fn read_stats(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<Stats> {
        let mut stats = self.stats.clone();
        let Some(allocation) = &self.allocation else {
            return Ok(stats);
        };
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fog-visibility-cache-stats"),
            size: HEADER_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&allocation.directory, 0, &readback, 0, HEADER_BYTES);
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(anyhow::Error::msg)?;
        receiver.recv().map_err(anyhow::Error::msg)??;
        let mapped = readback
            .slice(..)
            .get_mapped_range()
            .map_err(anyhow::Error::msg)?;
        let words: &[u32] = bytemuck::cast_slice(&mapped);
        stats.payload_used = words[0];
        stats.fills_this_frame = words[1];
        stats.deferred_fallbacks = words[2];
        stats.cache_hits = words[4];
        stats.cache_fallbacks = words[5];
        stats.cold_shadow_calls = words[6];
        stats.raw_mismatches = words[7];
        drop(mapped);
        readback.unmap();
        Ok(stats)
    }
}

fn header_upload(device: &wgpu::Device, words: &[u32]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("fog-visibility-cache-header-upload"),
        contents: bytemuck::cast_slice(words),
        usage: wgpu::BufferUsages::COPY_SRC,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> DeviceLimits {
        DeviceLimits {
            max_buffer_size: 1 << 30,
            max_storage_buffer_binding_size: 1 << 30,
            max_storage_buffers_per_shader_stage: 9,
            max_compute_workgroups_per_dimension: 65_535,
        }
    }

    fn config() -> CacheConfig {
        CacheConfig {
            slot_capacity: 512,
            tile_size: 16,
            block_side: 4,
            slices: 128,
            payload_pool_bytes: PAYLOAD_POOL_BYTES,
            total_budget_bytes: TOTAL_BUDGET_BYTES,
            fills_per_frame: DEFAULT_FILL_BUDGET,
        }
    }

    fn key(value: u32) -> ShadowContentKey {
        ShadowContentKey {
            matrix: [value; 16],
            near: value,
            far: value + 1,
            caster_hash: u64::from(value),
            format_version: SHADOW_FORMAT_VERSION,
        }
    }

    fn global(value: u32) -> GlobalKey {
        GlobalKey {
            inv_view_proj: [value; 16],
            camera_position: [value; 3],
            viewport: [value, value],
            grid_size: [150, 81, 128],
            camera_planes: [value, value + 1],
            fog_far: value,
            medium_min: [value; 3],
            medium_max: [value; 3],
            tile_size: 16,
            block_side: 4,
            slices: 128,
            algorithm_version: ALGORITHM_VERSION,
        }
    }

    #[test]
    fn far52_fits_hard_budget_but_4k_falls_back() {
        let far = CacheLayout::for_viewport([2397, 1292], config(), limits()).unwrap();
        assert_eq!(far.grid_size, [150, 81, 128]);
        assert_eq!(far.blocks, [38, 21, 32]);
        assert_eq!(far.block_count, 25_536);
        assert_eq!(far.payload_capacity, 2_097_152);
        assert!(far.total_bytes < TOTAL_BUDGET_BYTES);
        assert_eq!(
            CacheLayout::for_viewport([3840, 2160], config(), limits()),
            Err(LayoutError::TotalBudget)
        );
    }

    #[test]
    fn adapter_limits_disable_without_reducing_current_rendering() {
        let mut low = limits();
        low.max_storage_buffers_per_shader_stage = 8;
        assert_eq!(
            CacheLayout::for_viewport([2397, 1292], config(), low),
            Err(LayoutError::TooManyStorageBindings)
        );
        let mut small = limits();
        small.max_storage_buffer_binding_size = 32 << 20;
        assert_eq!(
            CacheLayout::for_viewport([2397, 1292], config(), small),
            Err(LayoutError::BufferLimit)
        );
        let mut few_groups = limits();
        few_groups.max_compute_workgroups_per_dimension = 1;
        assert_eq!(
            CacheLayout::for_viewport([2397, 1292], config(), few_groups),
            Err(LayoutError::WorkgroupLimit)
        );
    }

    #[test]
    fn exact_keys_cover_motion_projection_planes_casters_and_global_domain() {
        let base = global(1);
        let mut changed = base;
        changed.inv_view_proj[15] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.camera_position[0] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.viewport[1] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.grid_size[0] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.camera_planes[1] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.medium_min[1] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.medium_max[2] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.fog_far += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.tile_size += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.algorithm_version += 1;
        assert_ne!(base, changed);

        let base = key(7);
        let mut changed = base;
        changed.matrix[5] += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.near += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.far += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.caster_hash += 1;
        assert_ne!(base, changed);
        changed = base;
        changed.format_version += 1;
        assert_ne!(base, changed);
    }

    #[test]
    fn inactive_slots_retain_and_stable_content_can_migrate() {
        let mut residency = Residency::new(4);
        assert_eq!(
            residency.update(global(1), &[(0, key(1))]),
            vec![ResidencyAction::ResetAll]
        );
        assert!(residency.update(global(1), &[(0, key(1))]).is_empty());
        assert!(residency.update(global(1), &[]).is_empty());
        assert_eq!(
            residency.update(global(1), &[(2, key(1))]),
            vec![ResidencyAction::CopySlot {
                source: 0,
                destination: 2,
            }]
        );
        assert_eq!(
            residency.update(global(2), &[(2, key(1))]),
            vec![ResidencyAction::ResetAll]
        );
    }

    #[test]
    fn same_slot_projection_near_far_and_caster_changes_clear_only_that_slot() {
        let mut residency = Residency::new(4);
        let _ = residency.update(global(1), &[(1, key(1)), (2, key(2))]);
        let mut changed = key(1);
        changed.near ^= 1;
        assert_eq!(
            residency.update(global(1), &[(1, changed), (2, key(2))]),
            vec![ResidencyAction::ClearSlot(1)]
        );
        assert!(residency.update(global(1), &[]).is_empty());
    }

    #[test]
    fn camera_motion_resets_all_and_zero_active_lights_preserve_residency() {
        let mut residency = Residency::new(2);
        assert_eq!(
            residency.update(global(1), &[(0, key(1))]),
            vec![ResidencyAction::ResetAll]
        );
        assert!(residency.update(global(1), &[]).is_empty());
        assert_eq!(
            residency.update(global(2), &[]),
            vec![ResidencyAction::ResetAll]
        );
    }

    fn accepted_payload(payload_base: u32, capacity: u32, budget: u32, ticket: u32) -> Option<u32> {
        let limit = budget.min(capacity.saturating_sub(payload_base));
        (ticket < limit).then(|| payload_base + ticket)
    }

    fn plan_window(block_count: u32, cursor: u32) -> (u32, u32, u32) {
        assert!(cursor < block_count);
        let count = PLAN_BLOCK_WINDOW.min(block_count - cursor);
        let next = if cursor + count == block_count {
            0
        } else {
            cursor + count
        };
        (cursor, count, next)
    }

    #[test]
    fn atomic_overshoot_never_publishes_past_frame_or_payload_limit() {
        assert_eq!(accepted_payload(100, 200, 8, 0), Some(100));
        assert_eq!(accepted_payload(100, 200, 8, 7), Some(107));
        assert_eq!(accepted_payload(100, 200, 8, 8), None);
        assert_eq!(accepted_payload(198, 200, 8, 1), Some(199));
        assert_eq!(accepted_payload(198, 200, 8, 2), None);
        assert_eq!(accepted_payload(200, 200, 8, 0), None);
    }

    #[test]
    fn rotating_windows_cover_each_block_once_before_wrap() {
        let block_count = 25_536;
        let mut seen = vec![false; block_count as usize];
        let mut cursor = 0;
        let mut windows = 0;
        loop {
            let (start, count, next) = plan_window(block_count, cursor);
            assert!(count <= PLAN_BLOCK_WINDOW);
            for block in start..start + count {
                assert!(!std::mem::replace(&mut seen[block as usize], true));
            }
            windows += 1;
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        assert_eq!(windows, block_count.div_ceil(PLAN_BLOCK_WINDOW));
        assert!(seen.into_iter().all(|value| value));
    }

    #[test]
    fn plan_dispatch_is_bounded_and_covers_window_padding() {
        for blocks in [1_u32, 63, 64, 65, 2_048, 25_536] {
            let window = blocks.min(PLAN_BLOCK_WINDOW);
            let groups = window.div_ceil(PLAN_WORKGROUP_SIZE);
            assert!(groups * PLAN_WORKGROUP_SIZE >= window);
            assert!(groups * PLAN_WORKGROUP_SIZE - window < PLAN_WORKGROUP_SIZE);
        }
    }

    #[test]
    fn directory_growth_is_charged_to_the_same_hard_budget() {
        let mut small = config();
        small.slot_capacity = 64;
        let small = CacheLayout::for_viewport([2397, 1292], small, limits()).unwrap();
        let full = CacheLayout::for_viewport([2397, 1292], config(), limits()).unwrap();
        assert!(small.total_bytes < full.total_bytes);
        assert_eq!(
            full.total_bytes - small.total_bytes,
            full.slot_bytes * (512 - 64)
        );
    }

    #[test]
    fn shader_contract_keeps_negative_and_unfilled_entries_on_the_oracle() {
        let grid = include_str!("shaders/haze_grid_cache.wgsl");
        assert!(grid.contains("slot_i < 0"));
        assert!(grid.contains("handle == 0u"));
        assert!(
            grid.matches("segment_shadow_visibility(ray_dir, start, end, light)")
                .count()
                >= 2
        );
        let assertion = include_str!("shaders/fog_visibility_cache_assert.wgsl");
        assert!(assertion.contains("cached != reference"));
    }
}
