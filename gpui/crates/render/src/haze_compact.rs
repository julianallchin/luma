//! Residual compaction of the cached compute haze kernel: the per-frame
//! decision of which shadow slots to reclassify, the GPU pools, and the pass
//! chain. `shaders/haze_compact.wgsl` describes the passes themselves.

use std::sync::mpsc;

use bytemuck::{Pod, Zeroable};

/// Pixels of one compute workgroup, and of one interval-cache block.
pub(crate) const HAZE_WORKGROUP: [u32; 2] = [8, 4];
const _: () = assert!(HAZE_WORKGROUP[0] * HAZE_WORKGROUP[1] == 32);

/// Shadow slots the compaction can address, as the interval cache.
const SLOTS: usize = 512;
/// Indirect arguments: `[x, y, 1, 0]` per residual kind, the scatter then hot
/// dispatch, the fused fallback, then the block grid's dimensions.
const ARGS_WORDS: usize = 20;
const HOT_ARGS_OFFSET: u64 = 12 * 4;
const FUSED_ARGS_OFFSET: u64 = 15 * 4;
/// Counters: entries per kind, list length, overflow flag, kind offsets and
/// scatter cursors.
const COUNTER_WORDS: usize = 11;
/// Lanes per workgroup of the single, payload and traverse residual kinds.
/// The traversal runs narrower so its few long entries spread over more
/// cores.
pub(crate) const RESID_LANES: [u32; 3] = [64, 64, 32];
const RESID_LABELS: [&str; 3] = [
    "haze-residual-single",
    "haze-residual-payload",
    "haze-residual-traverse",
];
/// The list starts at this many entries and doubles after an overflow.
const MIN_CAPACITY: u32 = 1 << 20;
const MAX_CAPACITY: u32 = 1 << 27;

/// Mirrors `CompactParams` in `haze_compact.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Uniform {
    /// x: dense slot stride of the planes, y: list capacity, z: blocks per
    /// row, w: maximum workgroups per dispatch dimension.
    params: [u32; 4],
    /// Shadow slot → dense id, `u32::MAX` for an empty slot.
    dense: [u32; SLOTS],
    /// Dense id → this frame's light-index id.
    slot_light: [u32; SLOTS],
    /// One bit per slot: classified this frame.
    dirty: [u32; SLOTS / 32],
    /// The dirty set by light-index id.
    dirty_lights: [u32; 16],
}

/// What the residual compaction did on the last planned frame, plus the
/// most recent counter readback (one or more frames behind).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct CompactStats {
    /// The last planned frame ran the compact haze path.
    pub active: bool,
    /// It ran the classify pass for `dirty_slots` slots; otherwise every
    /// slot's classification was reused as is.
    pub classify: bool,
    /// Slots reclassified this frame.
    pub dirty_slots: u32,
    /// The list was rebuilt from scratch (every live slot reclassified).
    pub rebuilt: bool,
    /// Resident shadow slots with a dense id.
    pub dense_slots: u32,
    /// Residual list capacity in entries.
    pub capacity: u32,
    /// Residual list length at the last readback.
    pub list_count: u32,
    /// Entries per residual kind at the last readback: single, payload,
    /// traverse.
    pub segments: [u32; 3],
    /// That readback's frame overflowed the list and ran the fused kernel.
    pub overflow: bool,
    /// Compact frames planned so far.
    pub frames: u32,
    /// Counter readbacks that have landed.
    pub readbacks: u32,
}

/// The compaction pipeline set over one bind group layout.
pub(crate) struct Pipelines {
    pub(crate) layout: wgpu::BindGroupLayout,
    classify: wgpu::ComputePipeline,
    /// Kind offsets and indirect dispatches after classify.
    prepare_residual: wgpu::ComputePipeline,
    /// Repacks the list into contiguous typed work ranges.
    scatter: wgpu::ComputePipeline,
    /// One per kind: single, payload, traverse.
    residual: [wgpu::ComputePipeline; 3],
    /// Selects the compact hot output or, on an overflow, the fused kernel.
    prepare_output: wgpu::ComputePipeline,
    hot: wgpu::ComputePipeline,
}

impl Pipelines {
    /// `source` is the cached kernel's WGSL with `haze_compact.wgsl`
    /// appended; `entries` the fused kernel's group-2 layout. Overrides are
    /// baked into each module's text rather than passed as pipeline
    /// constants: naga's Metal pipeline-constant pass has corrupted these
    /// entry points' expression maps.
    pub(crate) fn new(
        device: &wgpu::Device,
        group_layouts: [&wgpu::BindGroupLayout; 3],
        entries: &[wgpu::BindGroupLayoutEntry],
        source: &str,
        constants: &[(&'static str, f64)],
    ) -> Self {
        let mut entries = entries.to_vec();
        entries.push(crate::gpu::uniform_entry(8, wgpu::ShaderStages::COMPUTE));
        for binding in 9..=15 {
            entries.push(crate::gpu::rw_storage_entry(
                binding,
                wgpu::ShaderStages::COMPUTE,
            ));
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze-compact"),
            entries: &entries,
        });
        let [haze, light_index, grid] = group_layouts;
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("haze-compact"),
            bind_group_layouts: &[Some(haze), Some(light_index), Some(&layout), Some(grid)],
            immediate_size: 0,
        });
        let module = |label: &str, extra: &[(&'static str, f64)]| {
            let mut values = constants.to_vec();
            values.extend_from_slice(extra);
            crate::gpu::shader(
                device,
                label,
                &crate::gpu::specialize_wgsl_overrides(source, &values),
            )
        };
        let make = |label: &'static str, module: &wgpu::ShaderModule, entry: &'static str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let plain = module("haze-compact", &[]);
        // The classify pass keeps the traversal's recorded intervals and
        // ballots; the quadrature it would also run is discarded, so skip it.
        let classify = module("haze-classify", &[("FILL_ONLY", 1.0)]);
        Self {
            classify: make("haze-classify", &classify, "classify_residual"),
            prepare_residual: make("haze-prepare-residual", &plain, "prepare_residual_work"),
            scatter: make("haze-scatter-residual", &plain, "scatter_residual_work"),
            residual: std::array::from_fn(|kind| {
                let module = module(
                    RESID_LABELS[kind],
                    &[
                        ("RESID_KIND", kind as f64),
                        ("RESID_LANES", f64::from(RESID_LANES[kind])),
                    ],
                );
                make(RESID_LABELS[kind], &module, "compute_residual")
            }),
            prepare_output: make("haze-prepare-output", &plain, "prepare_haze_output"),
            hot: make("haze-hot", &plain, "compute_haze_hot"),
            layout,
        }
    }
}

/// GPU pools, sized to the frame's block grid, the dense slot count and the
/// list capacity.
struct Pools {
    blocks: (u32, u32),
    dense: u32,
    capacity: u32,
    /// Two words per (block, dense slot): residual mask and list base.
    planes: wgpu::Buffer,
    list: wgpu::Buffer,
    rgb: wgpu::Buffer,
    work: wgpu::Buffer,
    aux: wgpu::Buffer,
    counters: wgpu::Buffer,
    args: wgpu::Buffer,
    /// Stands in for the indirect-args binding in the passes that dispatch
    /// from it: a buffer cannot be both a writable binding and an indirect
    /// source inside one dispatch.
    args_stub: wgpu::Buffer,
    /// Counter readback, mapped asynchronously after a compact frame.
    readback: wgpu::Buffer,
    pending: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
}

/// One frame's compaction plan: its uniform and which passes run.
pub(crate) struct Plan {
    pub(crate) uniform: Uniform,
    /// Some slot is reclassified: classify, prepare and scatter run.
    pub(crate) classify: bool,
    /// The counters are copied out for the CPU after the hot pass.
    pub(crate) readback: bool,
}

/// Bindings a compaction bind group takes from the rest of the frame.
pub(crate) struct FrameBindings<'a> {
    pub(crate) uniform: &'a wgpu::Buffer,
    /// The lit fog grid for the hot pass; the static density field for the
    /// others, so fog-integrate's write to the grid need not wait for them.
    pub(crate) grid: &'a wgpu::TextureView,
    pub(crate) static_field: &'a wgpu::TextureView,
    pub(crate) output: &'a wgpu::TextureView,
    /// Interval cache: slot table, header words, claims, payload table.
    pub(crate) cache: [&'a wgpu::Buffer; 4],
}

/// The other three bind groups every haze compute pass sets.
pub(crate) struct Groups<'a> {
    pub(crate) haze: &'a wgpu::BindGroup,
    pub(crate) light_index: &'a wgpu::BindGroup,
    pub(crate) grid: &'a wgpu::BindGroup,
}

/// Residual compaction state carried across frames.
pub(crate) struct Compaction {
    pools: Option<Pools>,
    /// Sticky dense id per shadow slot, so a residency change elsewhere does
    /// not move a clean slot's planes.
    dense: Vec<Option<u32>>,
    /// Per shadow slot, the interval-cache word (`mode | gen << 8`) its
    /// planes and list entries were classified under; `None` when never
    /// classified into the current list. A slot whose word changed is
    /// reclassified, appending fresh entries; the list is rebuilt from
    /// scratch once appends have grown it past its budget.
    classified: Vec<Option<u32>>,
    /// Entries in the list at its last full rebuild, from the readback.
    pub(crate) full_count: u32,
    /// List capacity for the next pool allocation; doubles after an
    /// overflow.
    pub(crate) capacity: u32,
    /// Width of a residual dispatch in workgroups. The device limit is the
    /// production value; tests lower it to exercise 2D flattening.
    pub(crate) group_columns: u32,
    /// Frames still to run the fused kernel, for tests comparing the two.
    #[cfg(test)]
    pub(crate) suspended: u32,
    stats: CompactStats,
}

impl Compaction {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        Self {
            pools: None,
            dense: Vec::new(),
            classified: Vec::new(),
            full_count: 0,
            capacity: MIN_CAPACITY,
            group_columns: device.limits().max_compute_workgroups_per_dimension,
            #[cfg(test)]
            suspended: 0,
            stats: CompactStats::default(),
        }
    }

    pub(crate) fn stats(&self) -> CompactStats {
        self.stats
    }

    /// Drop every pool and classification, as a new renderer would start.
    #[cfg(test)]
    pub(crate) fn forget(&mut self) {
        self.pools = None;
        self.dense.clear();
        self.classified.clear();
        self.full_count = 0;
        self.stats = CompactStats::default();
    }

    /// This frame runs no compute haze.
    pub(crate) fn idle(&mut self) {
        self.stats.active = false;
    }

    /// Consume the last compact frame's counter readback, if it has landed.
    fn poll_readback(&mut self, device: &wgpu::Device) {
        let Some(pools) = self.pools.as_mut() else {
            return;
        };
        let Some(rx) = pools.pending.as_ref() else {
            return;
        };
        let _ = device.poll(wgpu::PollType::Poll);
        match rx.try_recv() {
            Ok(Ok(())) => {
                let words: [u32; COUNTER_WORDS] = match pools.readback.slice(..).get_mapped_range() {
                    Ok(data) => {
                        let words: &[u32] = bytemuck::cast_slice(&data);
                        std::array::from_fn(|i| words[i])
                    }
                    Err(_) => [0; COUNTER_WORDS],
                };
                pools.readback.unmap();
                pools.pending = None;
                self.stats.segments = [words[0], words[1], words[2]];
                self.stats.list_count = words[3];
                self.stats.overflow = words[4] != 0;
                self.stats.readbacks += 1;
                if self.full_count == 0 {
                    self.full_count = words[3].max(1);
                }
                if self.stats.overflow {
                    // The next plan reallocates at twice the size and
                    // classifies every slot into the new list.
                    self.capacity = pools.capacity.saturating_mul(2).min(MAX_CAPACITY);
                }
            }
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => pools.pending = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Decide this frame's compaction: assign dense ids, size the pools, and
    /// mark the slots whose classification no longer holds. `wanted` is false
    /// on a frame that runs the fused kernel; `slot_words` is each slot's
    /// interval-cache word (`u32::MAX` when off), `slot_lights` its
    /// light-index id this frame (`u32::MAX` when not lit).
    pub(crate) fn plan(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        wanted: bool,
        haze_size: (u32, u32),
        slot_words: &[u32],
        slot_lights: &[u32],
    ) -> Option<Plan> {
        self.poll_readback(device);
        self.stats.active = false;
        #[cfg(test)]
        if self.suspended > 0 {
            self.suspended -= 1;
            return None;
        }
        if !wanted {
            return None;
        }
        let slots = slot_lights.len().min(SLOTS);
        if self.dense.len() != slots {
            self.dense = vec![None; slots];
            self.classified = vec![None; slots];
        }
        for (slot, light) in slot_lights.iter().take(slots).enumerate() {
            if *light == u32::MAX {
                self.dense[slot] = None;
                self.classified[slot] = None;
            }
        }
        let mut used = [false; SLOTS];
        for id in self.dense.iter().flatten() {
            used[*id as usize] = true;
        }
        for (slot, light) in slot_lights.iter().take(slots).enumerate() {
            if *light != u32::MAX && self.dense[slot].is_none() {
                let id = used.iter().position(|taken| !taken)?;
                used[id] = true;
                self.dense[slot] = Some(id as u32);
                self.classified[slot] = None;
            }
        }
        let mut dense = [u32::MAX; SLOTS];
        let mut slot_light = [u32::MAX; SLOTS];
        let mut count = 0u32;
        for (slot, id) in self.dense.iter().enumerate() {
            if let Some(id) = id {
                dense[slot] = *id;
                count = count.max(id + 1);
                slot_light[*id as usize] = slot_lights[slot];
            }
        }
        let blocks = (
            haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
            haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
        );
        let limits = device.limits();
        let binding_limit =
            limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        // Three raw f32 per entry is the largest per-entry buffer.
        let capacity = self
            .capacity
            .min(u32::try_from(binding_limit / 12).unwrap_or(u32::MAX))
            / 64
            * 64;
        // The plane stride is the allocation, not the live count: a newcomer
        // with a higher dense id must not move every clean slot's planes.
        let dense_needed = count.max(1).next_multiple_of(16);
        let stale = self.pools.as_ref().is_none_or(|pools| {
            pools.blocks != blocks || pools.dense < dense_needed || pools.capacity != capacity
        });
        if stale {
            let plane_bytes = u64::from(blocks.0 * blocks.1) * u64::from(dense_needed) * 8;
            if plane_bytes > binding_limit {
                return None;
            }
            self.pools = Some(Pools::new(device, blocks, dense_needed, capacity));
            self.classified.iter_mut().for_each(|c| *c = None);
            self.full_count = 0;
        }
        let pools = self.pools.as_ref().expect("allocated above");
        let readback = pools.pending.is_none();

        let mut dirty = [0u32; SLOTS / 32];
        let mut dirty_slots = 0u32;
        let mut live_slots = 0u32;
        for slot in 0..slots {
            if self.dense[slot].is_none() {
                continue;
            }
            live_slots += 1;
            let word = slot_words.get(slot).copied().unwrap_or(u32::MAX);
            if self.classified[slot] != Some(word) {
                dirty[slot / 32] |= 1 << (slot % 32);
                dirty_slots += 1;
            }
        }
        // Appends leave a reclassified slot's old entries as unread garbage,
        // so the list is rebuilt once it has grown half again past its last
        // full build, or past three quarters of its capacity; and when half
        // the slots change at once, a rebuild costs no more than the append.
        let list_count = self.stats.list_count;
        let grown = list_count > self.full_count
            && (list_count > self.full_count.saturating_mul(3) / 2
                || list_count > capacity / 4 * 3);
        let rebuild = stale || grown || dirty_slots.saturating_mul(2) >= live_slots;
        if rebuild {
            dirty = [0; SLOTS / 32];
            dirty_slots = 0;
            for slot in 0..slots {
                if self.dense[slot].is_some() {
                    dirty[slot / 32] |= 1 << (slot % 32);
                    dirty_slots += 1;
                }
            }
            self.full_count = 0;
            encoder.clear_buffer(&pools.counters, 0, None);
        }
        let mut dirty_lights = [0u32; 16];
        for slot in 0..slots {
            if dirty[slot / 32] & (1 << (slot % 32)) != 0 {
                self.classified[slot] = Some(slot_words.get(slot).copied().unwrap_or(u32::MAX));
                let li = slot_lights[slot] as usize;
                debug_assert!(li < 512);
                dirty_lights[li / 32] |= 1 << (li % 32);
            }
        }
        let classify = dirty_slots > 0;
        self.stats.active = true;
        self.stats.classify = classify;
        self.stats.dirty_slots = dirty_slots;
        self.stats.rebuilt = rebuild;
        self.stats.dense_slots = count;
        self.stats.capacity = capacity;
        self.stats.frames += 1;
        Some(Plan {
            uniform: Uniform {
                params: [pools.dense, capacity, blocks.0, self.group_columns],
                dense,
                slot_light,
                dirty,
                dirty_lights,
            },
            classify,
            readback,
        })
    }

    /// One compaction bind group. `indirect` binds the real indirect
    /// arguments; the passes that dispatch from them bind a stub.
    pub(crate) fn bind_group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        frame: &FrameBindings<'_>,
        indirect: bool,
        hot: bool,
    ) -> wgpu::BindGroup {
        use wgpu::BindingResource::TextureView;
        let pools = self.pools.as_ref().expect("planned frames have pools");
        let binding = crate::gpu::binding;
        let args = if indirect {
            &pools.args
        } else {
            &pools.args_stub
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("haze-compact"),
            layout,
            entries: &[
                binding(0, TextureView(if hot { frame.grid } else { frame.static_field })),
                binding(1, TextureView(frame.output)),
                binding(4, frame.cache[0].as_entire_binding()),
                binding(5, frame.cache[1].as_entire_binding()),
                binding(6, frame.cache[2].as_entire_binding()),
                binding(7, frame.cache[3].as_entire_binding()),
                binding(8, frame.uniform.as_entire_binding()),
                binding(9, pools.planes.as_entire_binding()),
                binding(10, pools.list.as_entire_binding()),
                binding(11, pools.rgb.as_entire_binding()),
                binding(12, pools.work.as_entire_binding()),
                binding(13, pools.counters.as_entire_binding()),
                binding(14, args.as_entire_binding()),
                binding(15, pools.aux.as_entire_binding()),
            ],
        })
    }

    /// Classify every dirty slot's blocks. Needs the shadow maps, depth and
    /// light index; nothing it writes is read before the residual pass.
    pub(crate) fn encode_classify(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass_queries: &mut crate::pass_profile::PassQueries<'_>,
        pipelines: &Pipelines,
        groups: &Groups<'_>,
        compact: &wgpu::BindGroup,
    ) {
        let pools = self.pools.as_ref().expect("planned frames have pools");
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("haze-classify"),
            timestamp_writes: pass_queries.compute("haze-classify", None),
        });
        pass.set_pipeline(&pipelines.classify);
        groups.set(&mut pass, compact);
        pass.dispatch_workgroups(pools.blocks.0, pools.blocks.1, 1);
    }

    /// Repack the list into typed work ranges (on classify frames) and run
    /// the three residual kinds.
    pub(crate) fn encode_residual(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass_queries: &mut crate::pass_profile::PassQueries<'_>,
        pipelines: &Pipelines,
        groups: &Groups<'_>,
        [prepare, residual]: [&wgpu::BindGroup; 2],
        classify: bool,
    ) {
        let pools = self.pools.as_ref().expect("planned frames have pools");
        if classify {
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("haze-queue-prepare"),
                    timestamp_writes: pass_queries.compute("haze-queue-prepare", None),
                });
                pass.set_pipeline(&pipelines.prepare_residual);
                groups.set(&mut pass, prepare);
                pass.dispatch_workgroups(1, 1, 1);
            }
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze-queue-scatter"),
                timestamp_writes: pass_queries.compute("haze-queue-scatter", None),
            });
            pass.set_pipeline(&pipelines.scatter);
            groups.set(&mut pass, residual);
            pass.dispatch_workgroups_indirect(&pools.args, HOT_ARGS_OFFSET);
        }
        for (kind, label) in RESID_LABELS.iter().enumerate() {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze-residual"),
                timestamp_writes: pass_queries.compute(label, None),
            });
            pass.set_pipeline(&pipelines.residual[kind]);
            groups.set(&mut pass, residual);
            pass.dispatch_workgroups_indirect(&pools.args, kind as u64 * 16);
        }
    }

    /// Select the hot pass or, after an overflow, the fused kernel, and
    /// encode both indirect dispatches; one of them has zero workgroups.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_output<'a>(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass_queries: &mut crate::pass_profile::PassQueries<'a>,
        pipelines: &Pipelines,
        groups: &Groups<'_>,
        [prepare, hot]: [&wgpu::BindGroup; 2],
        fused: (&wgpu::ComputePipeline, &wgpu::BindGroup),
        timestamp_writes: Option<wgpu::ComputePassTimestampWrites<'a>>,
        readback: bool,
    ) {
        let pools = self.pools.as_ref().expect("planned frames have pools");
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze-prepare-output"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipelines.prepare_output);
            groups.set(&mut pass, prepare);
            pass.dispatch_workgroups(1, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze-compute"),
                timestamp_writes: pass_queries.compute("haze-compute", timestamp_writes),
            });
            pass.set_pipeline(&pipelines.hot);
            groups.set(&mut pass, hot);
            pass.dispatch_workgroups_indirect(&pools.args, HOT_ARGS_OFFSET);
            pass.set_pipeline(fused.0);
            groups.set(&mut pass, fused.1);
            pass.dispatch_workgroups_indirect(&pools.args, FUSED_ARGS_OFFSET);
        }
        if readback {
            encoder.copy_buffer_to_buffer(
                &pools.counters,
                0,
                &pools.readback,
                0,
                (COUNTER_WORDS * 4) as u64,
            );
        }
    }

    /// Map the counters copied out by a submitted frame.
    pub(crate) fn start_readback(&mut self) {
        if let Some(pools) = self.pools.as_mut() {
            let (tx, rx) = mpsc::channel();
            pools
                .readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
            pools.pending = Some(rx);
        }
    }
}

impl Groups<'_> {
    fn set(&self, pass: &mut wgpu::ComputePass<'_>, compact: &wgpu::BindGroup) {
        pass.set_bind_group(0, self.haze, &[]);
        pass.set_bind_group(1, self.light_index, &[]);
        pass.set_bind_group(2, compact, &[]);
        pass.set_bind_group(3, self.grid, &[]);
    }
}

impl Pools {
    fn new(device: &wgpu::Device, blocks: (u32, u32), dense: u32, capacity: u32) -> Self {
        let buffer = |label: &str, bytes: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.max(4),
                usage,
                mapped_at_creation: false,
            })
        };
        let storage = wgpu::BufferUsages::STORAGE;
        let entries = u64::from(capacity) * 4;
        let mut initial = [0u32; ARGS_WORDS];
        initial[18] = blocks.0;
        initial[19] = blocks.1;
        let args = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("haze-resid-args"),
            size: (ARGS_WORDS * 4) as u64,
            usage: storage | wgpu::BufferUsages::INDIRECT,
            mapped_at_creation: true,
        });
        args.slice(..)
            .get_mapped_range_mut()
            .expect("mapped at creation")
            .copy_from_slice(bytemuck::bytes_of(&initial));
        args.unmap();
        Self {
            blocks,
            dense,
            capacity,
            planes: buffer(
                "haze-resid-planes",
                u64::from(blocks.0 * blocks.1) * u64::from(dense) * 8,
                storage,
            ),
            list: buffer("haze-resid-list", entries, storage),
            rgb: buffer("haze-resid-rgb", entries * 3, storage),
            work: buffer("haze-resid-work", entries, storage),
            aux: buffer("haze-resid-aux", entries, storage),
            counters: buffer(
                "haze-resid-counters",
                (COUNTER_WORDS * 4) as u64,
                storage | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            ),
            args,
            args_stub: buffer("haze-resid-args-stub", (ARGS_WORDS * 4) as u64, storage),
            readback: buffer(
                "haze-resid-readback",
                (COUNTER_WORDS * 4) as u64,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            pending: None,
        }
    }
}
