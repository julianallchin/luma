//! The wgpu device and the frame's pass chain.
//!
//! Offscreen only: nothing here knows about a window or a swapchain. A frame
//! goes shadow → depth → scene (MSAA) → haze (accumulated) → composite+AgX →
//! editor overlays, and comes out sRGB-encoded 8-bit — either read back as
//! bytes in the caller's [`Channels`] order, or left in memory a compositor
//! addresses directly. That choice is the frame's [`Destination`], and it is
//! the only thing about a frame this module lets a caller vary that is not
//! about the picture.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::assets::Image;
use crate::atmosphere::{AtmosphereCache, AtmospherePipelines};
use crate::environment::{EnvironmentCache, EnvironmentPipelines};
use crate::frame::{Draw, Frame};
use crate::haze_field::HazeField;
use crate::light_index::{
    LightCore, LightIndex, LightIndexInput, LightIndexPipelines, LightIndexStats, LightRest,
};
use crate::overlay::{Overlay, OverlayDepth};
use crate::shadow::{
    assign_shadow_slots, fixture_shadow_caster_hash, fixture_shadow_matrix, fixture_shadow_planes,
    fixture_shadow_texture_array, shadow_matrix_bits, ShadowCacheKey, FIXTURE_SHADOW_SIZE,
    MAX_FIXTURE_SHADOWS,
};
use crate::viewport::{Presented, PRESENTATION_SLOTS};

/// Three bounded layers cover the part of a venue in which directional
/// shadows remain useful. 2048² per layer costs 48 MiB in `Depth32Float`, versus
/// an unbounded camera-sized allocation or 192 MiB for three legacy 4096 maps.
const SHADOW_SIZE: u32 = 2048;
const CASCADE_COUNT: usize = 3;
const CASCADE_SPLITS: [f32; CASCADE_COUNT] = [12.0, 45.0, 180.0];
const CASCADE_BLEND: f32 = 0.1;

const HAZE_WORKGROUP: [u32; 2] = [8, 4];
const _: () = assert!(HAZE_WORKGROUP[0] * HAZE_WORKGROUP[1] == 32);
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Timestamp query layout:
/// 0 first render pass start, 1 scene pass end, 2 haze region end (zero when
/// haze did not run), 3 composite pass end, 4/5 light-index build compute
/// pass; 6/7 shared-volume span, 8 column preparation end, 9 volume lighting end.
///
/// Every sample is a *stage* boundary: a render pass's begin is its vertex
/// stage starting, its end is its fragment stage finishing. On Apple TBDR the
/// vertex stage of one pass runs while the previous pass is still shading, so
/// neither boundary alone bounds a pass. Differencing consecutive *starts*
/// measures scheduling spacing and under-reported fragment-heavy passes by up
/// to three orders of magnitude; differencing a pass's own start and end
/// charges it for everything it waited on (measured composite: 20.2 ms of a
/// 21.0 ms frame it contributed 0.1 ms to). So [`FrameTimings`] cuts its spans
/// at consecutive *end* samples instead, which partitions the frame without
/// double-counting.
///
/// Sample indices themselves are unrestricted
/// (`tests/timestamp_query_contract.rs`), but *when* the set is resolved is
/// not — see the resolve in `submit_readback`.
const QUERY_COUNT: u32 = 10;
const MSAA_SAMPLES: u32 = 4;
const CAMERA_NEAR: f32 = 0.1;
const CAMERA_FAR: f32 = 2000.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_view_proj: [[[f32; 4]; 4]; CASCADE_COUNT],
    camera_pos: [f32; 4],
    camera_forward: [f32; 4],
    cascade_splits: [f32; 4],
    ambient: [f32; 4],
    room: [f32; 4],
    room_falloff: [f32; 4],
    dir_to_light: [f32; 4],
    dir_color: [f32; 4],
    params: [f32; 4],
    medium: crate::medium::Uniform,
    outdoor_sun: [f32; 4],
    /// x: fog-grid radial extent, y: [`SurfaceTransmittance`] mode code,
    /// z: hybrid near limit in metres. See `scene_bindings.wgsl`.
    surface_fog: [f32; 4],
    /// xy: output size in pixels, zw: reciprocal.
    viewport: [f32; 4],
}

/// Where the opaque scene pass takes outdoor camera transmittance from.
/// `LUMA_SURFACE_T_SOURCE=march|grid|hybrid:<metres>`; the default reads the
/// far-field fog grid's camera transmittance (accepted 2026-09-11, scene2);
/// `march` is the older per-fragment `medium_optical_depth` control path. The
/// grid modes read the far-field fog prefix, so the scene pass is encoded
/// after `fog-transmittance` on those frames.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SurfaceTransmittance {
    March,
    Grid,
    Hybrid { near_metres: f32 },
}

impl SurfaceTransmittance {
    fn from_env() -> Self {
        let Some(value) = std::env::var_os("LUMA_SURFACE_T_SOURCE") else {
            return Self::Grid;
        };
        let value = value.to_string_lossy();
        match value.as_ref() {
            "march" | "" => Self::March,
            "grid" => Self::Grid,
            other => match other
                .strip_prefix("hybrid:")
                .and_then(|n| n.parse::<f32>().ok())
            {
                Some(near_metres) if near_metres.is_finite() && near_metres >= 0.0 => {
                    Self::Hybrid { near_metres }
                }
                _ => panic!(
                    "LUMA_SURFACE_T_SOURCE must be march, grid or hybrid:<metres>, got {other:?}"
                ),
            },
        }
    }

    /// Shader code and near limit for `globals.surface_fog.yz`.
    fn shader_mode(self) -> [f32; 2] {
        match self {
            Self::March => [0.0, 0.0],
            Self::Grid => [1.0, 0.0],
            Self::Hybrid { near_metres } => [2.0, near_metres],
        }
    }
}

/// Per-workgroup counts from the latest deterministic compute-haze frame.
/// Each record contains eight sums followed by eight per-pixel maxima.
#[derive(Debug, serde::Serialize)]
pub struct HazeWorkStats {
    /// Haze target dimensions in pixels.
    pub image_size: [u32; 2],
    /// Pixels covered by one compute workgroup.
    pub workgroup_size: [u32; 2],
    /// Number of workgroups along each image axis, including padded edges.
    pub workgroups: [u32; 2],
    /// Counter names in the order used by each record's sums and maxima.
    pub names: [&'static str; 8],
    /// `counts`: eight sums then eight per-pixel maxima. `histogram`: eight
    /// sums of the per-pair lit-interval call histogram (0,1,2,3,4,5-8,9+,
    /// whole-span single call) then eight sums of the per-pixel shadowed-pair
    /// histogram (0,1-2,3-4,5-6,7-8,9-12,13-16,17+).
    pub mode: &'static str,
    /// Row-major workgroup records; padded pixel invocations contribute zero.
    pub groups: Vec<[u32; 16]>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    model: [[f32; 4]; 4],
    normal_matrix: [[f32; 4]; 4],
    base_color: [f32; 4],
    emissive: [f32; 4],
    /// x: `flat_shading`, y: normal-map scale, z: AO strength.
    flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OverlayInstance {
    model: [[f32; 4]; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PointLightGpu {
    position: [f32; 4],
    color: [f32; 4],
}

/// Mirrors `IntervalCacheParams` in `haze.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct IntervalCacheUniform {
    /// x: cache blocks per row, y: hash bucket count minus one.
    params: [u32; 4],
    /// x: bit 0 replays stored interval lists.
    flags: [u32; 4],
    slots: [[u32; 4]; 512],
}

/// Mirrors `CompactParams` in `haze_compact.wgsl`. Twelve copies live in one
/// uniform buffer, one per kind×phase bucket, selected by dynamic offset;
/// the stride is a multiple of the 256-byte uniform offset alignment.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompactUniform {
    /// x: dense slot count, y: list capacity, z: blocks per row, w: segment.
    params: [u32; 4],
    /// xyz: lanes per workgroup per segment.
    seg_lanes: [u32; 4],
    /// x: packed period/phase/flags, y: local transport submission;
    /// z: 2D compute-dispatch row stride, w: direct-arena words.
    target: [u32; 4],
    /// Shadow slot → dense id, `u32::MAX` for an empty slot.
    dense: [u32; 512],
    /// Dense id → this frame's light-index id.
    slot_light: [u32; 512],
    /// One bit per slot: classified this frame.
    dirty: [u32; 16],
    /// The classify (0..16) and fill (16..32) sets by light-index id.
    dirty_lights: [u32; 32],
    /// True resident-transport changes by sticky dense id.
    dirty_dense: [u32; 16],
    /// `[bucket_id, kind, phase, selected_mask_and_flags]`.
    bucket: [u32; 4],
    /// Whole K: capacity, RGB word base, aux word base, packed schedule; four reserved.
    whole: [u32; 8],
    _pad: [u32; 40],
}
/// Width of the residual point target; its height follows the capacity.
const RESID_TARGET_WIDTH: u32 = 4096;
const COMPACT_UNIFORM_STRIDE: u64 = std::mem::size_of::<CompactUniform>() as u64;
const RESID_BUCKETS: usize = 12;
const COMPACT_HOT_ARGS_OFFSET: u64 = 96 * 4;
const COMPACT_FUSED_ARGS_OFFSET: u64 = 99 * 4;
const COMPACT_COUNTER_WORDS: usize = 80;
const WHOLE_COUNTER_BASE: usize = 64;
const WHOLE_ARGS_BASE: usize = 104;
const WHOLE_GC_ARGS_OFFSET: u64 = WHOLE_ARGS_BASE as u64 * 4;
const WHOLE_REFRESH_ARGS_OFFSET: u64 = (WHOLE_ARGS_BASE as u64 + 3) * 4;
const COMPACT_ARGS_WORDS: usize = WHOLE_ARGS_BASE + 6;
const WHOLE_RECORD_BYTES: u64 = 8 * 4 + 4;

fn compact_indirect_args(blocks: (u32, u32)) -> [u32; COMPACT_ARGS_WORDS] {
    let mut args = [0u32; COMPACT_ARGS_WORDS];
    for bucket in 0..RESID_BUCKETS {
        let base = bucket * 8;
        args[base + 1] = 1;
        args[base + 2] = 1;
        args[base + 4] = 6;
    }
    args[102] = blocks.0;
    args[103] = blocks.1;
    args
}

fn compact_dense_capacity(
    required: u32,
    hint: usize,
    blocks_total: u32,
    binding_limit: u64,
) -> Option<u32> {
    const ALIGN: u32 = 16;
    let required = required.max(1).checked_next_multiple_of(ALIGN)?;
    let bytes_per_dense = u64::from(blocks_total).checked_mul(8)?;
    if bytes_per_dense == 0 {
        return None;
    }
    let supported = (binding_limit / bytes_per_dense).min(512) as u32 / ALIGN * ALIGN;
    if required > supported {
        return None;
    }
    let hinted = (hint.min(512) as u32).checked_next_multiple_of(ALIGN)?;
    Some(required.max(hinted.min(supported)))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WholeTailLayout {
    capacity: u32,
    rgb_base_words: u32,
    aux_base_words: u32,
    rgb_bytes: u64,
    aux_bytes: u64,
}

fn whole_tail_layout(
    budget_bytes: u64,
    binding_limit: u64,
    residual_value_words: u64,
    residual_aux_words: u64,
) -> WholeTailLayout {
    let rgb_base_bytes = residual_value_words.saturating_mul(4);
    let aux_base_bytes = residual_aux_words.saturating_mul(4);
    let addressable =
        residual_value_words <= u64::from(u32::MAX) && residual_aux_words <= u64::from(u32::MAX);
    let capacity =
        if !addressable || rgb_base_bytes > binding_limit || aux_base_bytes > binding_limit {
            0
        } else {
            (budget_bytes / WHOLE_RECORD_BYTES)
                .min((binding_limit - rgb_base_bytes) / 32)
                .min((binding_limit - aux_base_bytes) / 4)
                .min(u64::from(u32::MAX - 1)) as u32
        };
    WholeTailLayout {
        capacity,
        rgb_base_words: u32::try_from(residual_value_words).unwrap_or(u32::MAX),
        aux_base_words: u32::try_from(residual_aux_words).unwrap_or(u32::MAX),
        rgb_bytes: rgb_base_bytes + u64::from(capacity) * 32,
        aux_bytes: aux_base_bytes + u64::from(capacity) * 4,
    }
}

fn compact_list_needs_rebuild(list_count: u32, full_count: u32, capacity: u32) -> bool {
    list_count > full_count
        && (list_count > full_count.saturating_mul(3) / 2 || list_count > capacity / 4 * 3)
}

const COMPACT_REBUILD_REUSE_OFF: u32 = 1 << 0;
const COMPACT_REBUILD_POOL_STALE: u32 = 1 << 1;
const COMPACT_REBUILD_COUNTERS_ABSENT: u32 = 1 << 2;
const COMPACT_REBUILD_ARGS_ABSENT: u32 = 1 << 3;
const COMPACT_REBUILD_GROWN: u32 = 1 << 4;
const COMPACT_REBUILD_DIRTY_THRESHOLD: u32 = 1 << 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CompactRebuildInputs {
    reuse_allowed: bool,
    pool_stale: bool,
    counters_present: bool,
    args_present: bool,
    grown: bool,
    dirty_slots: u32,
    live_slots: u32,
}

fn compact_rebuild_reasons(input: CompactRebuildInputs) -> u32 {
    let dirty_threshold = input.dirty_slots.saturating_mul(2) >= input.live_slots;
    u32::from(!input.reuse_allowed) * COMPACT_REBUILD_REUSE_OFF
        | u32::from(input.pool_stale) * COMPACT_REBUILD_POOL_STALE
        | u32::from(!input.counters_present) * COMPACT_REBUILD_COUNTERS_ABSENT
        | u32::from(!input.args_present) * COMPACT_REBUILD_ARGS_ABSENT
        | u32::from(input.grown) * COMPACT_REBUILD_GROWN
        | u32::from(dirty_threshold) * COMPACT_REBUILD_DIRTY_THRESHOLD
}

/// Scalar temporal transport retains one coefficient per residual entry;
/// the control path retains the original RGB result.
fn residual_value_stride(temporal_period: u32) -> u32 {
    if temporal_period == 0 {
        3
    } else {
        1
    }
}

fn residual_value_bytes(capacity: u32, temporal_period: u32) -> u64 {
    u64::from(capacity) * u64::from(residual_value_stride(temporal_period)) * 4
}

#[cfg(test)]
fn phased_residual_groups(count: u32, lanes: u32, period: u32, phase: u32) -> u32 {
    let logical = count.div_ceil(lanes);
    if logical <= phase {
        0
    } else {
        (logical - phase).div_ceil(period)
    }
}

#[cfg(test)]
fn bucket_kind_sums(words: [u32; RESID_BUCKETS]) -> [u32; 3] {
    std::array::from_fn(|kind| words[kind * 4..kind * 4 + 4].iter().sum())
}

#[cfg(test)]
fn bucket_offsets(counts: [u32; RESID_BUCKETS]) -> [u32; RESID_BUCKETS] {
    let mut cursor = 0;
    std::array::from_fn(|bucket| {
        let offset = cursor;
        cursor += counts[bucket];
        offset
    })
}

/// `LUMA_HAZE_COMPACT`: `off` runs the fused cached kernel, `on` compacts on
/// settled frames (some slot in read or write mode) and runs the fused
/// kernel otherwise, `always` also compacts unsettled frames, where every
/// pair traverses and the list holds every active pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HazeCompact {
    Off,
    On,
    Always,
}

impl HazeCompact {
    fn from_env() -> Self {
        match std::env::var("LUMA_HAZE_COMPACT").ok().as_deref() {
            // On by default (lead accepted 2026-09-11): the three compaction
            // pipelines compile the fused kernel's functions separately, and
            // Metal's codegen drift leaves ~2,200 one-code pixels per frame
            // against the fused kernel (harness/perf run-20260911-compact);
            // that is invisible and within the "extremely close" bar.
            None | Some("on") | Some("1") => Self::On,
            Some("off") | Some("0") => Self::Off,
            Some("always") => Self::Always,
            Some(other) => panic!("LUMA_HAZE_COMPACT must be off, on or always, got {other:?}"),
        }
    }
}

/// The compaction pipeline set over one bind group layout.
struct HazeCompactPipelines {
    layout: wgpu::BindGroupLayout,
    fill: wgpu::ComputePipeline,
    classify: wgpu::ComputePipeline,
    /// Computes shared typed offsets and indirect dispatches after classify.
    prepare_residual: wgpu::ComputePipeline,
    /// Repacks the persistent global list into contiguous typed work ranges.
    scatter: wgpu::ComputePipeline,
    /// Selects compact hot output or same-frame fused overflow fallback.
    prepare_output: wgpu::ComputePipeline,
    /// Captures the pre-hot whole descriptor prefix and writes 2D indirect args.
    prepare_whole: wgpu::ComputePipeline,
    /// Refreshes the selected stable whole descriptor phase.
    refresh_whole: wgpu::ComputePipeline,
    /// Clears validated backpointers without reading current light state.
    validate_whole_gc: wgpu::ComputePipeline,
    clear_whole_gc: wgpu::ComputePipeline,
    /// Resets the whole allocator after the ordered GC pass.
    reset_whole: wgpu::ComputePipeline,
    /// One per segment: single, payload, traverse.
    residual: [wgpu::ComputePipeline; 3],
    /// The residual as a tiled draw (`LUMA_HAZE_RESID_STAGE=fragment`): the
    /// render lane runs it beside the grid chain.
    residual_draw: [wgpu::RenderPipeline; 3],
    hot: wgpu::ComputePipeline,
}

/// The residual pass: three indirect segments, as a compute dispatch or as
/// a point-list draw on a scratch attachment.
#[allow(clippy::too_many_arguments)]
fn encode_residual<'a>(
    encoder: &mut wgpu::CommandEncoder,
    pass_queries: &mut crate::pass_profile::PassQueries<'a>,
    pipes: &HazeCompactPipelines,
    haze_bg: &wgpu::BindGroup,
    light_index_bg: &wgpu::BindGroup,
    prepare_bg: &wgpu::BindGroup,
    residual_bg: &wgpu::BindGroup,
    grid_bg: &wgpu::BindGroup,
    args: &wgpu::Buffer,
    target: &wgpu::TextureView,
    fragment: [bool; 3],
    prepare: bool,
    repack: bool,
) {
    const LABELS: [&str; 3] = [
        "haze-residual-single",
        "haze-residual-payload",
        "haze-residual-traverse",
    ];
    if prepare {
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze-queue-prepare"),
                timestamp_writes: pass_queries.compute("haze-queue-prepare", None),
            });
            pass.set_pipeline(&pipes.prepare_residual);
            pass.set_bind_group(0, haze_bg, &[]);
            pass.set_bind_group(1, light_index_bg, &[]);
            pass.set_bind_group(2, prepare_bg, &[0]);
            pass.set_bind_group(3, grid_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        if repack {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze-queue-scatter"),
                timestamp_writes: pass_queries.compute("haze-queue-scatter", None),
            });
            pass.set_pipeline(&pipes.scatter);
            pass.set_bind_group(0, haze_bg, &[]);
            pass.set_bind_group(1, light_index_bg, &[]);
            pass.set_bind_group(2, residual_bg, &[0]);
            pass.set_bind_group(3, grid_bg, &[]);
            pass.dispatch_workgroups_indirect(args, COMPACT_HOT_ARGS_OFFSET);
        }
    }
    // Twelve stable queues: kind × list-index phase. Keep the four phase
    // dispatches for each kind inside one pass; each dynamic uniform names
    // the bucket whose count and offset were produced on-GPU earlier in this
    // same submission.
    for kind in 0..3_u32 {
        if fragment[kind as usize] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("haze-residual"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: pass_queries.render(LABELS[kind as usize], None),
                ..Default::default()
            });
            pass.set_bind_group(0, haze_bg, &[]);
            pass.set_bind_group(1, light_index_bg, &[]);
            pass.set_bind_group(3, grid_bg, &[]);
            pass.set_pipeline(&pipes.residual_draw[kind as usize]);
            for phase in 0..4_u32 {
                let bucket = kind * 4 + phase;
                pass.set_bind_group(2, residual_bg, &[bucket * COMPACT_UNIFORM_STRIDE as u32]);
                pass.draw_indirect(args, u64::from(bucket) * 32 + 16);
            }
        } else {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("haze-residual"),
                timestamp_writes: pass_queries.compute(LABELS[kind as usize], None),
            });
            pass.set_pipeline(&pipes.residual[kind as usize]);
            pass.set_bind_group(0, haze_bg, &[]);
            pass.set_bind_group(1, light_index_bg, &[]);
            pass.set_bind_group(3, grid_bg, &[]);
            for phase in 0..4_u32 {
                let bucket = kind * 4 + phase;
                pass.set_bind_group(2, residual_bg, &[bucket * COMPACT_UNIFORM_STRIDE as u32]);
                pass.dispatch_workgroups_indirect(args, u64::from(bucket) * 32);
            }
        }
    }
}

/// One frame's compaction decision and per-frame buffers.
struct CompactFrame {
    uniform: wgpu::Buffer,
    counters: wgpu::Buffer,
    args: wgpu::Buffer,
    fill: bool,
    classify: bool,
    /// Scalar temporal modes prepare fresh phased indirect arguments even
    /// when the persistent typed queue needs no repack.
    prepare_residual: bool,
    /// The bounded whole-scalar pool is enabled for this frame.
    whole_enabled: bool,
    after_integrate: bool,
    /// The counters are copied out for the CPU after the hot pass.
    readback: bool,
}

/// GPU pools of the residual compaction, sized to the frame's block count,
/// the dense slot count and the list capacity.
struct CompactPools {
    /// `(blocks per frame, dense slot capacity)` the planes were sized for.
    shape: (u32, u32),
    whole: WholeTailLayout,
    planes: wgpu::Buffer,
    list: wgpu::Buffer,
    rgb: wgpu::Buffer,
    work: wgpu::Buffer,
    aux: wgpu::Buffer,
    /// Counter readback, mapped asynchronously after each compact frame.
    readback: wgpu::Buffer,
    pending: Option<mpsc::Receiver<Result<(), String>>>,
    /// Stands in for the indirect-args binding in the residual and hot
    /// passes, which never touch it: a buffer cannot be both a writable
    /// binding and an indirect source inside one dispatch.
    args_stub: wgpu::Buffer,
    /// Scratch attachment for the point-list residual; never stored.
    target: wgpu::TextureView,
}

/// What the residual compaction did on the last planned frame, plus the
/// most recent counter readback (one or more frames behind).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct CompactStats {
    /// Cue-independent venue lighting support was selected for this CPU frame.
    pub venue_domain_selected: bool,
    /// Exact float bits of the selected medium XYZ bounds and `fog_far`, after
    /// all production and diagnostic domain selection for this CPU frame.
    pub selected_domain_words: [u32; 7],
    /// The last planned frame ran the compact haze path.
    pub active: bool,
    /// It also ran the fill pass (some slot was in write mode).
    pub fill: bool,
    /// It ran the classify pass for `dirty_slots` slots; otherwise every
    /// slot's classification was reused as is.
    pub classify: bool,
    /// Slots reclassified this frame, after a rebuild expands dirty to all live slots.
    pub dirty_slots: u32,
    /// Dirty slots before the rebuild decision changed the dirty mask.
    pub pre_rebuild_dirty_slots: u32,
    /// Live compact residents before the rebuild decision.
    pub pre_rebuild_live_slots: u32,
    /// Dirty slots with no classification in the current list. This includes
    /// first use, reactivation after ordinary residency removal, and pool reset.
    pub pre_rebuild_unclassified_slots: u32,
    /// Dirty slots whose available interval-cache generation changed.
    pub pre_rebuild_generation_slots: u32,
    /// Dirty slots that changed between an available generation and off.
    pub pre_rebuild_availability_slots: u32,
    /// Otherwise-clean slots dirtied only because list reuse was disabled.
    pub pre_rebuild_reuse_forced_slots: u32,
    /// Slots dirtied by a resident transport-key or mapping-validity change.
    pub pre_rebuild_resident_transport_slots: u32,
    /// CPU-visible completed GPU list count used by the rebuild predicate.
    pub pre_rebuild_list_count: u32,
    /// CPU-side full-list reference used by the rebuild predicate. When zero,
    /// a later asynchronous completed readback supplies the next reference.
    pub pre_rebuild_full_count: u32,
    /// Bitmask: reuse-off, pool-stale, counters-absent, args-absent, grown,
    /// dirty-threshold occupy bits 0 through 5 respectively.
    pub rebuild_reasons: u32,
    /// The list was rebuilt from scratch (every live slot reclassified).
    pub rebuilt: bool,
    /// Resident shadow slots given a dense id this frame.
    pub dense_slots: u32,
    /// Venue-derived dense-column request before adapter clamping.
    pub requested_dense_hint: u32,
    /// Dense columns required by current sticky slot assignments.
    pub required_dense_slots: u32,
    /// Dense columns reserved in the compact plane allocation.
    pub reserved_dense_slots: u32,
    /// The compact plane allocation was absent, resized, or too narrow.
    pub pool_stale: bool,
    /// Residual list capacity in entries.
    pub capacity: u32,
    /// Residual entries the last read-back compact frame appended.
    pub list_count: u32,
    /// Per-segment counts of that frame: single, payload, traverse.
    pub segments: [u32; 3],
    /// GPU-computed offsets of the twelve stable kind×phase work ranges.
    pub queue_offsets: [u32; RESID_BUCKETS],
    /// Entries scattered into each stable work range.
    pub queue_cursors: [u32; RESID_BUCKETS],
    /// Renderer-local scalar-transport submission planned on this CPU frame.
    pub transport_submission: u32,
    /// Submission id carried by the most recent asynchronous GPU readback.
    pub readback_transport_submission: u32,
    /// Scalar refresh period carried by the most recent GPU readback.
    pub readback_temporal_period: u32,
    /// Primary scalar refresh phase carried by the most recent GPU readback.
    pub readback_temporal_phase: u32,
    /// Actual eight-bit phase mask carried by the most recent GPU readback.
    pub readback_temporal_selected_mask: u32,
    /// The readback origin launched every bucket to service dirty residents.
    pub readback_resident_dirty_dispatch: bool,
    /// Scalar refresh period and selected phase of the planned CPU frame.
    pub temporal_period: u32,
    /// Primary scalar refresh phase of the planned CPU frame.
    pub temporal_phase: u32,
    /// Actual eight-bit phase mask planned on the CPU frame.
    pub temporal_selected_mask: u32,
    /// The planned frame launched every bucket to service dirty residents.
    pub resident_dirty_dispatch: bool,
    /// Why this frame refreshed every residual group.
    pub temporal_reset_reasons: u32,
    /// Oldest retained scalar value after this frame, in microseconds.
    pub temporal_max_age_us: u32,
    /// Logical and physically dispatched groups from the readback origin.
    pub temporal_logical_groups: [u32; 3],
    /// Physical group counts carried by the most recent GPU readback.
    pub temporal_physical_groups: [u32; 3],
    /// Direct interval-arena capacity in u32 words; zero when disabled.
    pub direct_arena_capacity_words: u32,
    /// Direct interval-arena words requested since the last full rebuild.
    pub direct_arena_requested_words: u32,
    /// Cache-eligible direct records requested since the last full rebuild.
    pub direct_arena_requested_records: u32,
    /// Direct records stored since the last full rebuild.
    pub direct_arena_stored_records: u32,
    /// Direct arena words stored since the last full rebuild.
    pub direct_arena_stored_words: u32,
    /// Records that fell back because the direct arena was full.
    pub direct_arena_failed_records: u32,
    /// Direct arena words rejected because the arena was full.
    pub direct_arena_failed_words: u32,
    /// Global-table misses with more intervals than the direct prototype stores.
    pub direct_arena_over_k: u32,
    /// Eligible misses whose two recorded tail intervals are both empty.
    pub direct_arena_fully_dark: u32,
    /// Sum of interval counts for cache-eligible direct-record requests.
    pub direct_arena_requested_intervals: u32,
    /// Sum of non-empty interval counts for direct-record requests.
    pub direct_arena_requested_nonempty: u32,
    /// Uniform whole-lit compact planes reached by the measured hot pass.
    pub whole_lit_planes: u32,
    /// Lanes that executed the whole-lit hot path in those planes.
    pub whole_lit_hot_lanes: u32,
    /// Whole-lit lanes that also executed the exact own-ray remainder.
    pub whole_lit_remainder_lanes: u32,
    /// Bounded whole scalar-K descriptor capacity.
    pub whole_k_capacity: u32,
    /// Whole descriptors allocated, including stale append garbage.
    pub whole_k_count: u32,
    /// Whole allocations rejected by the hard capacity.
    pub whole_k_failed: u32,
    /// Valid whole descriptors refreshed by the readback frame.
    pub whole_k_refreshed: u32,
    /// Whole descriptors rejected before current-light reads.
    pub whole_k_stale: u32,
    /// Whole transport submission planned on the current CPU frame.
    pub whole_k_submission: u32,
    /// Planned whole scalar refresh period and phase.
    pub whole_k_temporal_period: u32,
    /// Planned whole scalar refresh phase.
    pub whole_k_temporal_phase: u32,
    /// Why the current whole plan refreshed all descriptors.
    pub whole_k_temporal_reset_reasons: u32,
    /// Oldest retained whole scalar after the current plan, in microseconds.
    pub whole_k_temporal_max_age_us: u32,
    /// Renderer-local whole submission from the latest readback.
    pub whole_k_readback_submission: u32,
    /// Packed period/phase/full/GC flags from the latest whole readback.
    pub whole_k_readback_temporal: u32,
    /// Descriptor prefix considered by the readback-origin frame.
    pub whole_k_logical_groups: u32,
    /// Whole refresh workgroups actually launched by that frame; zero on GC.
    pub whole_k_physical_groups: u32,
    /// Whole pool epoch after independent GC.
    pub whole_k_epoch: u32,
    /// Independent whole GC passes completed.
    pub whole_k_gc_count: u32,
    /// The live population saturated this epoch; exact fallback remains active.
    pub whole_k_saturated: bool,
    /// That frame overflowed the list or a segment; the fused kernel runs
    /// for the following frames.
    pub overflow: bool,
    /// Frames the fused kernel will still run because of an overflow.
    pub suspended: u32,
    /// Compact frames planned so far.
    pub frames: u32,
    /// Counter readbacks that have landed.
    pub readbacks: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HazeUniform {
    inv_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    params: [f32; 4],
    tuning: [f32; 4],
    transport: [f32; 4],
    tiles: [f32; 4],
    depth: [f32; 4],
    /// x: shadowed fixture count, y: shadow texel size.
    shadow: [f32; 4],
    medium: crate::medium::Uniform,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SurfaceClusterUniform {
    /// x: surface lighting enabled, y: occupancy debug, z: surface-depth mask.
    /// The culling structure itself is the shared light index.
    flags: [f32; 4],
    /// x: shadowed fixture count, y: shadow texel size, z: beam gain.
    shadow: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FixtureShadowMatrix {
    view_proj: [[f32; 4]; 4],
    /// x: projection near plane, y: far plane, metres. The shaders linearise
    /// shadow depths with these so occlusion slack is metric — a constant raw
    /// reverse-Z bias is centimetres near the light but metres at range, and
    /// read as beams spilling through occluders.
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniform {
    inv_view_proj: [[f32; 4]; 4],
    params: [f32; 4],
    depth: [f32; 4],
    background: [f32; 4],
    medium: crate::medium::Uniform,
    camera_pos: [f32; 4],
    outdoor_sun: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TemporalUniform {
    /// x: history weight, y: history valid, z: depth rejection threshold.
    params: [f32; 4],
}

/// Transport constants the look was dialled in on (spec §3.1). They are baked:
/// the keyboard dials that produced them do not port.
struct Transport;
impl Transport {
    const BEAM_GAIN: f32 = 180.0;
    const WHITE_LEAK: f32 = 0.03;
    const PHASE_G: f32 = 0.6;
    const NEAR_CLAMP: f32 = 0.06;
    /// Extinction per unit haze density, in 1/metres. One σ serves the whole
    /// transport: the haze pass integrates in-scatter against it and the
    /// composite attenuates the scene by it — if they disagree, surfaces and
    /// the medium disagree about how much fog sits in front of them and
    /// geometry silhouettes through the fog.
    const EXTINCTION: f32 = 0.06;
}

/// Byte order a readback is written in.
///
/// This is the *output texture's* format, not a post-processing step: the
/// composite pass writes straight into the order the caller asked for, so the
/// readback is a row memcpy either way. Swizzling on the CPU instead cost about
/// a millisecond per megapixel, for a choice the sampler makes for free.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channels {
    /// What a PNG wants.
    Rgba,
    /// What `gpui::RenderImage` wants.
    Bgra,
}

/// Who reads a finished frame, which is what decides where it is written.
///
/// This is not a quality or a format switch — both destinations run the same
/// passes and produce the same picture. It decides only whether the frame is
/// staged for the CPU or left in memory the window compositor can address, and
/// therefore whether a copy is encoded at all.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Destination {
    /// The caller, as bytes in this order. An export, a golden, a video frame.
    Bytes(Channels),
    /// The window compositor, in place. Falls back to `Bytes(Bgra)` on a
    /// platform or adapter with no shareable memory, so a caller asking for
    /// this never has to know whether it got it.
    Compositor,
}

impl Destination {
    fn channels(self) -> Channels {
        match self {
            Self::Bytes(channels) => channels,
            Self::Compositor => Channels::Bgra,
        }
    }
}

/// Cumulative immutable-resource uploads made by a renderer instance.
///
/// This is an acceptance probe rather than a timing estimate: a steady-state
/// live rig must leave both values unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadStats {
    /// Combined geometry-bank uploads.
    pub geometry: u64,
    /// Material-map uploads, counted per source/color-space role.
    pub textures: u64,
    /// HDR environment uploads and GPU preprocessing runs.
    pub environments: u64,
}

/// Adapter identity attached to volumetric timing evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct RendererProfile {
    /// Human-readable adapter name.
    pub name: String,
    /// Graphics backend reported by wgpu.
    pub backend: String,
    /// Adapter class (integrated, discrete, CPU, and so on).
    pub device_type: String,
    /// Driver name.
    pub driver: String,
    /// Driver detail string.
    pub driver_info: String,
    /// Smallest subgroup width reported by wgpu for this adapter.
    pub subgroup_min_size: u32,
    /// Largest subgroup width reported by wgpu for this adapter.
    pub subgroup_max_size: u32,
    /// Whether the adapter can provide hardware timestamp queries.
    pub timestamp_query_supported: bool,
    /// Nanoseconds represented by one timestamp tick, when supported.
    pub timestamp_period_ns: Option<f32>,
}

impl RendererProfile {
    /// Read off the device rather than the adapter for the timestamp feature:
    /// an adopted device may have declined what its adapter offers.
    fn of(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let info = adapter.get_info();
        let timestamp_query_supported = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        Self {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            driver_info: info.driver_info,
            subgroup_min_size: info.subgroup_min_size,
            subgroup_max_size: info.subgroup_max_size,
            timestamp_query_supported,
            timestamp_period_ns: timestamp_query_supported.then(|| queue.get_timestamp_period()),
        }
    }
}

fn supports_haze_subgroups(
    features: wgpu::Features,
    subgroup_min_size: u32,
    metal_backend: bool,
    native_apple_silicon: bool,
) -> bool {
    features.contains(wgpu::Features::SUBGROUP)
        && (subgroup_min_size >= 32 || (metal_backend && native_apple_silicon))
}

/// GPU-timeline timings for one production live render.
///
/// The three region spans are cut at consecutive fragment-stage completions,
/// so they partition [`Self::gpu_total_ms`] exactly rather than overlapping —
/// see `QUERY_COUNT` for why a pass's own begin-to-end bracket is not the
/// honest cut. What a partition cannot express is that the scene and haze
/// passes have no dependency on each other and really do run concurrently:
/// their shared time is charged to whichever completes first, and a haze pass
/// that finishes before the scene pass reports zero rather than negative. Only
/// the composite pass, which samples both of their outputs, has spans that are
/// exclusively its own.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FrameTimings {
    /// Optional overlapping pass brackets, enabled by LUMA_PROFILE_DETAIL=1.
    pub passes: Vec<crate::pass_profile::GpuPassTiming>,
    /// CPU phases from the same submission; durations serialize as secs/nanos.
    pub cpu: CpuSpans,
    /// The whole frame on the GPU timeline: first render pass start through
    /// composite completion, the composite pass being the sink every other
    /// pass feeds. Within ~10% of the wall clock around submit → queue drained
    /// on the workloads in `tests/timestamp_lie.rs`.
    pub gpu_total_ms: f64,
    /// Scene completion through the last haze pass's (or the temporal
    /// resolve's) completion. Zero when haze did not run — and also when it
    /// ran entirely alongside the scene pass and finished first, in which case
    /// its cost is inside [`Self::gpu_scene_ms`].
    pub gpu_volumetric_ms: f64,
    /// Frame start through scene completion: the light-index build, the
    /// shadow and depth passes, the scene pass, and whatever haze work overlapped
    /// them. Zero redrawn maps (`ShadowStats::redrawn_maps`) makes this the
    /// depth prepass and the scene pass alone.
    pub gpu_scene_ms: f64,
    /// The composite pass alone: its two inputs' completion through its own.
    /// Editor overlays run after the last sample, so they are outside every
    /// span here.
    pub gpu_composite_ms: f64,
    /// The light-index build compute pass. Compute has no vertex/fragment
    /// split, so this one is a true begin-to-end bracket — but the pass
    /// overlaps the start of the render passes, so it is a component of
    /// [`Self::gpu_scene_ms`] rather than an addition to it.
    pub gpu_index_ms: f64,
    /// First shared lighting and prefix-integration span, including dependency waits.
    pub gpu_fog_grid_ms: f64,
    /// Conservative camera-column preparation; part of `gpu_fog_grid_ms`.
    pub gpu_fog_prepare_ms: f64,
    /// Shared volume lighting after column preparation.
    pub gpu_fog_light_ms: f64,
    /// Density quadrature and prefix integration after shared lighting.
    pub gpu_fog_integrate_ms: f64,
    /// CPU time for scene preparation, command encoding and queue submission.
    pub cpu_encode_submit_ms: f64,
    /// CPU time spent preparing the deterministic surface-light index.
    pub cpu_cluster_ms: f64,
}

/// Where the CPU time between claiming a presentation slot and handing the
/// frame to Metal went.
///
/// A slot reads `Rendering` from the moment it is claimed, which is *before*
/// this span runs — so a worker blocked anywhere in here shows two slots
/// rendering, no work on the GPU, and a healthy UI thread. That is the
/// signature every stall in this investigation has had, and it was invisible
/// because the only bracket over this span, `FrameTimings::cpu_encode_submit_ms`,
/// exists solely on the one frame per cycle that carries GPU timestamps —
/// absent on precisely the frames that stall.
///
/// So this is carried on every frame. The five phases are disjoint and sum to
/// [`Self::total`], which is what makes a long total attributable to a step
/// rather than merely visible.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct CpuSpans {
    /// Camera matrices, cone sanitising, shadow-slot assignment, light arrays.
    pub prepare: Duration,
    /// Clustered-light index preparation.
    pub clusters: Duration,
    /// Uniform and instance uploads, and the bind groups over them.
    pub upload: Duration,
    /// Acquiring this slot's presentation target — including, on macOS, the
    /// shared `IOSurface` the window compositor samples. The prime suspect for
    /// a stall that only exists with a real compositor.
    pub targets: Duration,
    /// Command encoding, through `queue.submit`.
    pub encode: Duration,
    /// Entry to submitted.
    pub total: Duration,
    /// Nested fixture-shadow work, already included in `upload` and `encode`.
    /// Do not add these spans to the five top-level phases.
    pub fixture_shadows: FixtureShadowCpuSpans,
    /// Nested command finalization and submission, already included in `encode`.
    pub submission: SubmissionCpuSpans,
}

/// CPU work at the end of encoding, nested within [`CpuSpans::encode`].
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct SubmissionCpuSpans {
    /// Staging-buffer completion and recall registration.
    pub staging: Duration,
    /// Finalizing the command encoder. Backend work may be deferred here.
    pub finish: Duration,
    /// Submitting the finished command buffer to the queue.
    pub submit: Duration,
}

/// CPU work for fixture shadows within a single submission. These spans are
/// disjoint from each other, but nested within [`CpuSpans`]'s top-level phases.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct FixtureShadowCpuSpans {
    /// Dirty-map detection and projection uniform uploads; within `upload`.
    pub globals: Duration,
    /// World-space bounds and per-light caster selection; within `encode`.
    pub cull: Duration,
    /// Stable mesh buckets and caster-instance indices; within `encode`.
    pub buckets: Duration,
    /// Caster-instance upload and map bind groups; within `encode`.
    pub resources: Duration,
    /// Depth-map render commands; within `encode`.
    pub maps: Duration,
    /// Conservative hierarchy rebuild commands; within `encode`.
    pub hierarchy: Duration,
}

impl CpuSpans {
    /// The longest phase and what it cost, for a report that has room for one
    /// number rather than six.
    #[must_use]
    pub fn worst(&self) -> (&'static str, Duration) {
        [
            ("prepare", self.prepare),
            ("clusters", self.clusters),
            ("upload", self.upload),
            ("targets", self.targets),
            ("encode", self.encode),
        ]
        .into_iter()
        .max_by_key(|(_, span)| *span)
        .expect("the phase list is not empty")
    }
}

/// What the fixture-shadow passes actually submitted last frame.
///
/// `caster_draws` is the metric that matters and milliseconds are not a
/// substitute for it: the cost of this path is draws, and a scene sparse enough
/// to make the milliseconds look fine can hide an unculled term that explodes
/// the moment the geometry is real. Recorded so a 17-draw benchmark can never
/// hide it again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ShadowStats {
    /// Shadow maps redrawn this frame.
    pub redrawn_maps: usize,
    /// Shadow-map layers whose conservative depth hierarchy was rebuilt.
    pub hierarchy_layers: usize,
    /// Total caster draws submitted across those maps.
    pub caster_draws: usize,
    /// Draws that would have been submitted with no culling — `redrawn_maps`
    /// times the opaque draw count.
    pub unculled_draws: usize,
    /// Instanced draws actually encoded — one per (map, distinct mesh). When
    /// `caster_draws / mesh_draws` approaches 1 the venue has many distinct
    /// meshes rather than many copies, mesh grouping has stopped paying, and
    /// the merged-index-buffer escalation (`shadows-phase3.md` §5.3) is due.
    pub mesh_draws: usize,
}

impl Channels {
    fn format(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Bgra => wgpu::TextureFormat::Bgra8UnormSrgb,
        }
    }

    /// Slot in [`Renderer::composite_pipelines`].
    fn index(self) -> usize {
        match self {
            Self::Rgba => 0,
            Self::Bgra => 1,
        }
    }
}

/// One scene's worth of GPU state: everything a frame writes, plus the caches
/// that keep it from being rebuilt every frame.
///
/// The device and the pipelines are *not* here — they are [`Gpu`], shared by
/// every renderer in the process. What is left is exactly the state two
/// renderers could not share without drawing into each other's picture: the
/// shadow atlases, the sized render targets, the temporal history and the
/// resident-asset caches. One instance renders any number of frames at any
/// number of sizes; render targets are reallocated on size change.
pub struct Renderer {
    gpu: Arc<Gpu>,
    /// Which environment probe this renderer has resident. The pipelines that
    /// produce it are shared; the probe is not, because it is uploaded from
    /// whatever scene *this* renderer was last asked to draw.
    environment: EnvironmentCache,
    atmosphere: AtmosphereCache,
    shadow_map: wgpu::TextureView,
    shadow_layers: [wgpu::TextureView; CASCADE_COUNT],
    fixture_shadow_map: wgpu::TextureView,
    fixture_shadow_map_extra: wgpu::TextureView,
    fixture_shadow_layers: Vec<wgpu::TextureView>,
    fixture_shadow_cache: Vec<Option<ShadowCacheKey>>,
    cascade_shadow_cache: [Option<ShadowCacheKey>; CASCADE_COUNT],
    /// Which cone occupies each shadow slot, carried across frames so
    /// [`assign_shadow_slots`] can keep a resident rather than reshuffling.
    fixture_shadow_slots: Vec<Option<usize>>,
    /// Uploaded images by stable source identity and color-space role.
    texture_views: HashMap<TextureKey, wgpu::TextureView>,
    /// Material bind groups by their five immutable map identities.
    ///
    /// A frame of a live rig names the same textures as the frame before it,
    /// and each upload carries a full mip chain built on the CPU. Keeping them
    /// is the difference between paying that once and paying it sixty times a
    /// second. Entries the current frame does not name are dropped, so a venue
    /// change does not accumulate the old venue's textures.
    materials: HashMap<MaterialKey, wgpu::BindGroup>,
    /// Immutable scene geometry resident across live frames. A resolved frame
    /// rebuilds transforms and light state, but its asset/procedural mesh keys
    /// stay stable until the venue changes.
    geometry: Option<ResidentGeometry>,
    visibility: crate::visibility::Visibility,
    geometry_shadows: bool,
    visibility_reference: bool,
    geometry_shadow_samples: u32,
    wide_light_group: u32,
    grid_fog: bool,
    /// Single-pass deterministic transport writes its outputs directly on Metal.
    haze_compute: bool,
    surface_transmittance: SurfaceTransmittance,
    haze_work_counts_valid: bool,
    fog_blocks_valid: bool,
    surface_depth_cull: bool,
    upload_stats: UploadStats,
    targets: Option<Targets>,
    haze_history_valid: bool,
    haze_history_index: usize,
    haze_history_key: Option<HazeHistoryKey>,
    medium_cache: crate::medium::Cache,
    shadow_hierarchy: crate::shadow_hierarchy::Targets,
    last_live_time: Option<f32>,
    live_noise_frame: u32,
    profiler: Option<ProfilerResources>,
    light_index: LightIndex,
    shadow_stats: ShadowStats,
    /// Lit-interval cache bookkeeping and pools; see `interval_cache.rs`.
    interval_cache: crate::interval_cache::IntervalCacheState,
    interval_pools: Option<IntervalCachePools>,
    /// Residual compaction pools and bookkeeping; see `haze_compact.wgsl`.
    compact_pools: Option<CompactPools>,
    compact_stats: CompactStats,
    fog_visibility: crate::fog_visibility_cache::State,
    /// Wall-clock phase residency for the opt-in scalar residual transport.
    residual_temporal: ResidualTemporalState,
    /// Independent phase residency for whole-plane scalar transport.
    whole_temporal: ResidualTemporalState,
    /// Local id written into GPU counters so delayed readbacks can be joined
    /// to the CPU frame that planned their phased dispatch.
    transport_submission: u32,
    whole_submission: u32,
    /// Key and GC fact for delayed whole counter readback origins.
    whole_submission_keys: VecDeque<(u32, WholeOriginKey, bool)>,
    /// One reclamation requested after an ordinary saturation.
    whole_gc_pending: bool,
    /// A GC under this key still saturated, proving live-over-cap.
    whole_saturated_key: Option<WholeOriginKey>,
    /// Changes when per-resident topology/K ownership changes. This does not
    /// invalidate clean K; it only permits one post-saturation reclamation.
    whole_resident_epoch: u32,
    /// Per shadow slot, the `mode | gen << 8` word its planes and list
    /// entries were classified under; `None` when never classified into the
    /// current list. A slot whose word changed is reclassified, appending
    /// fresh entries; the list is rebuilt from scratch when appends have
    /// grown it past its budget.
    compact_classified: Vec<Option<u32>>,
    /// Per-slot transport identity, independent of sorted light id/radiance.
    compact_resident_keys: Vec<Option<ResidualResidentKey>>,
    /// Whether each physical shadow slot had a resident. Used only to permit
    /// bounded whole-pool reclamation after saturation; sorted/source index
    /// churn and animated scalar keys deliberately do not advance it.
    compact_resident_occupancy: Vec<bool>,
    /// Whether that resident had a valid sorted mapping on the previous frame.
    compact_mapping_valid: Vec<bool>,
    /// Sticky dense id per shadow slot, so a residency change elsewhere does
    /// not move a clean slot's planes.
    compact_dense: Vec<Option<u32>>,
    /// Entries in the list at its last full rebuild, from the readback.
    compact_full_count: u32,
    /// Monotonic identity for list-index/phase reuse across full rebuilds.
    compact_queue_epoch: u32,
    /// Width of a residual compute dispatch in workgroups. The device limit
    /// is the production value; tests lower it to exercise 2D flattening.
    compact_group_columns: u32,
    /// The per-slot records uploaded by the last `plan_interval_cache`.
    interval_cache_entries: Vec<crate::interval_cache::SlotEntry>,
    /// Staging memory for per-frame uploads, recycled across frames. One
    /// belt chunk serves many uploads, where each `Queue::write_buffer` call
    /// allocates (and kernel-registers) a staging buffer of its own.
    staging: std::cell::RefCell<wgpu::util::StagingBelt>,
    /// Per-frame GPU inputs by label, retained across frames and overwritten
    /// in place — see [`Renderer::storage`].
    ///
    /// `RefCell` because uploads happen deep inside the encode path,
    /// interleaved with shared borrows of the caches above, and threading
    /// `&mut self` through there would push the pool's existence onto every
    /// caller. The renderer lives on one thread; the borrow is never held
    /// across a call out.
    frame_buffers: std::cell::RefCell<HashMap<String, GrowableStorage>>,
}

/// A storage buffer kept across frames and grown only when the contents stop
/// fitting. Cluster rebuilds are frequent — every frame the camera moves — and
/// at 512 cones the index list is tens of megabytes, so reallocating per
/// rebuild costs more than filling the buffer does.
struct GrowableStorage {
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

struct ProfilerResources {
    /// One set of timestamp resources per presentation slot.
    ///
    /// Sized by [`PRESENTATION_SLOTS`] and not by a literal: `submit_readback`
    /// asserts a slot is in range against that constant and then indexes this
    /// array with it, so a second, smaller answer to "how many slots are there"
    /// turns the assert into a lie and the index into a panic on the renderer
    /// thread — which presents as the stage silently freezing.
    slots: [ProfilerSlot; PRESENTATION_SLOTS],
    timestamp_period_ns: f32,
}

struct ProfilerSlot {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HazeHistoryKey {
    output: [u32; 4],
    camera: [u32; 6],
    fov: u32,
    density: u32,
    topology: u64,
    shadows: [bool; 2],
    casters: u64,
}

const RESID_TEMPORAL_RESET_INIT: u32 = 1 << 0;
const RESID_TEMPORAL_RESET_CLASSIFY: u32 = 1 << 1;
const RESID_TEMPORAL_RESET_KEY: u32 = 1 << 2;
const RESID_TEMPORAL_RESET_TIME: u32 = 1 << 3;
const RESID_TEMPORAL_RESET_INACTIVE: u32 = 1 << 4;
const RESID_TEMPORAL_RESET_OVERFLOW: u32 = 1 << 5;

/// Frame-wide inputs shared by every scalar residual. Radiance and medium
/// time stay out: hot applies current radiance and phased refresh approximates
/// only the animated medium time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResidualGlobalKey {
    output: [u32; 4],
    camera: [u32; 8],
    medium: [u32; 16],
    fog_far: u32,
    /// Original ordered cone topology when per-resident validity is disabled.
    legacy_topology: Vec<u32>,
    shadows: [bool; 2],
    casters: u64,
    depth: u64,
    shadow_samples: u32,
}

/// Per-resident inputs that change scalar transport. The identity is keyed by
/// physical shadow slot; sorted light ids and radiance are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResidualResidentKey {
    cone: [u32; 4],
    rest: [u32; 10],
    shadow: Option<ShadowCacheKey>,
    projection: [u32; 2],
    interval: [u32; 4],
    scatters: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WholeOriginKey {
    global: ResidualGlobalKey,
    resident_epoch: u32,
}

fn normalized_interval_identity(entry: crate::interval_cache::SlotEntry) -> [u32; 4] {
    let available = u32::from(entry[3] & 0xFF != crate::interval_cache::MODE_OFF);
    [
        entry[0],
        entry[1],
        entry[2],
        (entry[3] >> 8) << 1 | available,
    ]
}

fn residual_resident_key(
    cone: &crate::frame::FixtureCone,
    rest: &LightRest,
    shadow: Option<ShadowCacheKey>,
    interval: crate::interval_cache::SlotEntry,
) -> ResidualResidentKey {
    let (near, far) = fixture_shadow_planes(cone);
    ResidualResidentKey {
        cone: [
            cone.position.x.to_bits(),
            cone.position.y.to_bits(),
            cone.position.z.to_bits(),
            cone.range.to_bits(),
        ],
        rest: [
            rest.direction[0].to_bits(),
            rest.direction[1].to_bits(),
            rest.direction[2].to_bits(),
            rest.cos_beam.to_bits(),
            rest.cos_field.to_bits(),
            rest.wash.to_bits(),
            rest.gobo.to_bits(),
            rest.gobo_rotation.to_bits(),
            rest.inverse_right_length.to_bits(),
            rest.field_tangent.to_bits(),
        ],
        shadow,
        projection: [near.to_bits(), far.to_bits()],
        interval: normalized_interval_identity(interval),
        scatters: rest.haze_gain > 0.0,
    }
}

fn resident_transport_dirty(
    previous: &Option<ResidualResidentKey>,
    previous_mapping_valid: bool,
    current: &Option<ResidualResidentKey>,
    current_mapping_valid: bool,
) -> bool {
    previous != current || (!previous_mapping_valid && current_mapping_valid)
}

/// Explicit, fixed sampling domain for the outdoor fog-grid quality probe.
///
/// This changes the grid's sampling coordinates and the boundary where surface
/// transmittance switches from sampled clouds to its analytic mean-density
/// tail. It neither retains dark fixtures nor changes the active fixture list.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DiagnosticLightingDomain {
    min: Vec3,
    max: Vec3,
    fog_far: f32,
}

impl DiagnosticLightingDomain {
    const ENV: &'static str = "LUMA_HAZE_LIGHTING_DOMAIN";

    fn from_env() -> anyhow::Result<Option<Self>> {
        let Some(raw) = std::env::var_os(Self::ENV) else {
            return Ok(None);
        };
        let raw = raw
            .into_string()
            .map_err(|_| anyhow::anyhow!("{} must be UTF-8", Self::ENV))?;
        let values = raw
            .split(',')
            .map(str::trim)
            .map(str::parse::<f32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| anyhow::anyhow!("invalid {}: {error}", Self::ENV))?;
        anyhow::ensure!(
            values.len() == 7,
            "{} requires min_x,min_y,min_z,max_x,max_y,max_z,fog_far",
            Self::ENV
        );
        anyhow::ensure!(
            values.iter().all(|value| value.is_finite()),
            "{} values must be finite",
            Self::ENV
        );
        let domain = Self {
            min: Vec3::new(values[0], values[1], values[2]),
            max: Vec3::new(values[3], values[4], values[5]),
            fog_far: values[6],
        };
        anyhow::ensure!(
            domain.min.cmplt(domain.max).all(),
            "{} min must be strictly below max on every axis",
            Self::ENV
        );
        anyhow::ensure!(
            domain.fog_far > 0.0,
            "{} fog_far must be positive",
            Self::ENV
        );
        Ok(Some(domain))
    }

    fn select(
        self,
        frame: &Frame,
        mut medium: crate::medium::Uniform,
        active_fog_far: f32,
        camera_far: f32,
    ) -> (crate::medium::Uniform, f32) {
        assert!(
            frame.sky.is_some(),
            "{} is an outdoor-only diagnostic",
            Self::ENV
        );
        let active_min = Vec3::from_array(medium.min[..3].try_into().unwrap());
        let active_max = Vec3::from_array(medium.max[..3].try_into().unwrap());
        if !frame.fixture_cones.is_empty() {
            assert!(
                self.min.cmple(active_min).all() && self.max.cmpge(active_max).all(),
                "{} must enclose the current active lighting bounds",
                Self::ENV
            );
        }
        assert!(
            self.fog_far >= active_fog_far && self.fog_far <= camera_far,
            "{} fog_far must enclose active lights without crossing camera_far",
            Self::ENV
        );
        medium.min[..3].copy_from_slice(&self.min.to_array());
        medium.max[..3].copy_from_slice(&self.max.to_array());
        (medium, self.fog_far)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ResidualTemporalSchedule {
    period: u32,
    phase: u32,
    /// Up to eight phases refreshed in this submission.
    selected_mask: u32,
    full: bool,
    reset_reasons: u32,
    max_age: f32,
}

struct ResidualTemporalState {
    key: Option<ResidualGlobalKey>,
    phase_times: [Option<f32>; 8],
    last_time: Option<f32>,
    next_phase: u32,
    pending_reset: u32,
    valid: bool,
}

impl Default for ResidualTemporalState {
    fn default() -> Self {
        Self {
            key: None,
            phase_times: [None; 8],
            last_time: None,
            next_phase: 0,
            pending_reset: RESID_TEMPORAL_RESET_INIT,
            valid: false,
        }
    }
}

impl ResidualTemporalState {
    fn invalidate(&mut self, reason: u32) {
        self.pending_reset |= reason;
        self.valid = false;
        self.last_time = None;
        self.phase_times = [None; 8];
        self.next_phase = 0;
    }

    fn plan(
        &mut self,
        configured_period: u32,
        max_age: f32,
        time: f32,
        key: ResidualGlobalKey,
        classify: bool,
        masked_catchup: bool,
    ) -> ResidualTemporalSchedule {
        debug_assert!(matches!(configured_period, 1 | 4 | 8));
        let mut reasons = self.pending_reset;
        if classify {
            reasons |= RESID_TEMPORAL_RESET_CLASSIFY;
        }
        if self.key.as_ref() != Some(&key) {
            reasons |= RESID_TEMPORAL_RESET_KEY;
        }
        let time_continuous = time.is_finite()
            && self
                .last_time
                .is_none_or(|previous| time >= previous && time - previous <= max_age);
        if !time_continuous {
            reasons |= RESID_TEMPORAL_RESET_TIME;
        }
        if !self.valid {
            reasons |= RESID_TEMPORAL_RESET_INIT;
        }

        self.key = Some(key);
        self.pending_reset = 0;
        if configured_period == 1 || reasons != 0 {
            if time.is_finite() {
                self.valid = true;
                self.last_time = Some(time);
                self.phase_times = [Some(time); 8];
            } else {
                self.invalidate(RESID_TEMPORAL_RESET_TIME);
            }
            self.next_phase = 0;
            return ResidualTemporalSchedule {
                period: 1,
                phase: 0,
                // The physical queue partition always has four buckets. A P8
                // reset must additionally certify both semantic halves.
                selected_mask: (1u32 << configured_period.max(4)) - 1,
                full: true,
                reset_reasons: reasons,
                max_age: 0.0,
            };
        }

        let phase = self.next_phase;
        let mut selected_mask = 1u32 << phase;
        let mut projected = self.phase_times;
        projected[phase as usize] = Some(time);
        let active_phases = &projected[..configured_period as usize];
        let projected_max_age = active_phases
            .iter()
            .flatten()
            .map(|refreshed| time - refreshed)
            .fold(0.0_f32, f32::max);
        if !masked_catchup
            && (active_phases.iter().any(Option::is_none) || projected_max_age > max_age)
        {
            self.valid = true;
            self.last_time = Some(time);
            self.phase_times = [Some(time); 8];
            self.next_phase = 0;
            return ResidualTemporalSchedule {
                period: 1,
                phase: 0,
                selected_mask: (1u32 << configured_period.max(4)) - 1,
                full: true,
                reset_reasons: RESID_TEMPORAL_RESET_TIME,
                max_age: 0.0,
            };
        }
        // Catch up only the phases that would exceed the age budget. This
        // preserves the stable queue identity and avoids an avoidable all-K
        // reset under variable but continuous frame cadence.
        if masked_catchup {
            for candidate in 0..configured_period as usize {
                if projected[candidate].is_none_or(|refreshed| time - refreshed > max_age) {
                    selected_mask |= 1u32 << candidate;
                    projected[candidate] = Some(time);
                }
            }
        }
        let projected_max_age = projected[..configured_period as usize]
            .iter()
            .flatten()
            .map(|refreshed| time - refreshed)
            .fold(0.0_f32, f32::max);

        self.valid = true;
        self.last_time = Some(time);
        self.phase_times = projected;
        self.next_phase = (phase + 1) % configured_period;
        ResidualTemporalSchedule {
            period: configured_period,
            phase,
            selected_mask,
            full: false,
            reset_reasons: 0,
            max_age: projected_max_age,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TextureEncoding {
    Srgb,
    Linear,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TextureKey {
    source: String,
    encoding: TextureEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MaterialKey {
    base_color: Option<TextureKey>,
    normal: Option<TextureKey>,
    metallic_roughness: Option<TextureKey>,
    occlusion: Option<TextureKey>,
    emissive: Option<TextureKey>,
}

impl MaterialKey {
    fn of(draw: &Draw, frame: &Frame) -> Self {
        let key = |index: Option<usize>, encoding| {
            index.map(|index| TextureKey {
                source: frame.images[index].key.clone(),
                encoding,
            })
        };
        Self {
            base_color: key(draw.textures.base_color, TextureEncoding::Srgb),
            normal: key(draw.textures.normal, TextureEncoding::Linear),
            metallic_roughness: key(draw.textures.metallic_roughness, TextureEncoding::Linear),
            occlusion: key(draw.textures.occlusion, TextureEncoding::Linear),
            emissive: key(draw.textures.emissive, TextureEncoding::Srgb),
        }
    }

    fn textures(&self) -> impl Iterator<Item = &TextureKey> {
        [
            self.base_color.as_ref(),
            self.normal.as_ref(),
            self.metallic_roughness.as_ref(),
            self.occlusion.as_ref(),
            self.emissive.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
}

struct MaterialDefaults {
    base_color: wgpu::TextureView,
    normal: wgpu::TextureView,
    metallic_roughness: wgpu::TextureView,
    occlusion: wgpu::TextureView,
    emissive: wgpu::TextureView,
}

struct ResidentGeometry {
    keys: Vec<String>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    ranges: Vec<(u32, u32, i32)>,
    /// Local-space bounding sphere per mesh, for culling casters against a
    /// cone. Computed with the upload because it depends on the same immutable
    /// vertex data and would otherwise be recomputed every frame.
    bounds: Vec<(Vec3, f32)>,
}

impl ResidentGeometry {
    fn matches(&self, frame: &Frame) -> bool {
        self.keys.len() == frame.meshes.len()
            && self
                .keys
                .iter()
                .zip(&frame.meshes)
                .all(|(resident, incoming)| resident == &incoming.key)
    }
}

/// GPU pools of the lit-interval cache. The header holds one word per
/// (8×4 block, shadow slot) region; the payload table holds hashed interval
/// lists for the few pairs that are neither whole-lit nor overflowing.
struct IntervalCachePools {
    /// `(blocks per frame, shadow slot capacity)` the header was sized for.
    shape: (u32, usize),
    header: wgpu::Buffer,
    claims: wgpu::Buffer,
    table: wgpu::Buffer,
    /// Hash bucket count minus one.
    bucket_mask: u32,
}

/// Hash of every opaque draw that writes the camera depth buffer: mesh
/// identity and model matrix bits, in draw order. Fixture bodies count here,
/// unlike `fixture_shadow_caster_hash`, because the haze ray stops at them.
fn opaque_depth_hash(frame: &Frame, opaque: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut push_byte = |byte: u8| {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    };
    for draw in frame.draws.iter().take(opaque) {
        for byte in frame.meshes[draw.mesh].key.as_bytes() {
            push_byte(*byte);
        }
        push_byte(0);
        for value in draw.model.to_cols_array() {
            for byte in value.to_bits().to_le_bytes() {
                push_byte(byte);
            }
        }
    }
    hash
}

struct Targets {
    width: u32,
    height: u32,
    /// Size of the haze target, which runs at `haze_resolution` of the output.
    haze_width: u32,
    haze_height: u32,
    destination: Destination,
    msaa_color: wgpu::TextureView,
    msaa_surface_depth: Option<wgpu::TextureView>,
    msaa_depth: wgpu::TextureView,
    scene: wgpu::TextureView,
    depth: wgpu::TextureView,
    haze: wgpu::TextureView,
    haze_sampled: wgpu::TextureView,
    haze_work_counts: wgpu::Buffer,
    /// Counted compaction: classify, residual and hot each sum into their
    /// own records so the fused kernel's totals can be checked for identity.
    haze_work_counts_compact: [wgpu::Buffer; 3],
    fog: crate::fog_grid::Targets,
    haze_history: [wgpu::TextureView; 2],
    /// Independent presentation resources, one per in-flight frame.
    /// Intermediate passes may be shared because queue submissions execute in
    /// order; each submission's final output must remain private until the
    /// frame it holds is off the screen.
    presentations: [PresentationTarget; PRESENTATION_SLOTS],
    /// 256-byte-aligned row pitch of a staged target's readback buffer.
    bytes_per_row: u32,
}

/// Where one in-flight frame is written.
enum PresentationTarget {
    /// Staged for the CPU: the composite pass writes the texture, an encoded
    /// copy moves it into the buffer, and the caller maps the buffer.
    Staged {
        output: wgpu::Texture,
        view: wgpu::TextureView,
        readback: wgpu::Buffer,
    },
    /// Written where the compositor can already see it. There is no second
    /// copy, so there is nothing to map and nothing to own afterwards.
    Shared(crate::share::Shared),
}

impl PresentationTarget {
    fn view(&self) -> &wgpu::TextureView {
        match self {
            Self::Staged { view, .. } => view,
            Self::Shared(shared) => shared.view(),
        }
    }
}

/// How one submission's pixels reach its caller, resolved when the frame is
/// encoded so the rest of encoding never asks again.
enum Finish {
    /// Copy the texture into the buffer, then map the buffer.
    Copy(wgpu::Texture, wgpu::Buffer),
    /// Hand back the surface that was written directly.
    Share(crate::share::Surface),
}

/// A submitted frame that has not finished.
///
/// Both destinations wait for the GPU; they differ in what arrives when it is
/// done. A staged frame's bytes arrive with its buffer map; a shared frame's
/// pixels were never anywhere else, so only the completion signal is awaited.
pub(crate) struct PendingFrame {
    completion: Completion,
    width: u32,
    height: u32,
    started: Instant,
    profile: Option<PendingProfile>,
    /// What the fixture-shadow passes submitted for this frame.
    ///
    /// Carried with the frame rather than read off the renderer later: by the
    /// time a frame completes, the renderer has moved on and its counter
    /// describes some other frame. Unlike `profile` this is always present —
    /// it is counted while encoding, not measured by the adapter.
    shadows: ShadowStats,
    /// The clustered-light index this frame shaded against.
    ///
    /// Carried with the frame for the same reason as `shadows`: the renderer
    /// has moved on by the time a frame completes, and its live counter then
    /// describes a different frame.
    clusters: LightIndexStats,
    /// How long the request waited before this frame was started.
    queued: Duration,
    /// Where the CPU time from claiming this slot to submitting went. Always
    /// present, unlike `profile` — see [`CpuSpans`].
    cpu: CpuSpans,
    /// When the driver said this frame was done, if it has.
    signalled: Option<Instant>,
}

enum Completion {
    Staged {
        readback: wgpu::Buffer,
        mapped: mpsc::Receiver<(Instant, Result<(), String>)>,
        mapped_result: Option<Result<(), String>>,
        bytes_per_row: u32,
    },
    Shared {
        surface: crate::share::Surface,
        done: mpsc::Receiver<Instant>,
        finished: bool,
    },
}

/// The frame's timestamps, resolved once the frame itself has finished.
///
/// `resolve_query_set` runs on the blit engine concurrently with the render
/// stream, so a resolve encoded into the frame's own command buffer reads the
/// query set before the GPU has finished writing it — dropping the tail
/// samples, deterministically, at some viewport sizes and not others. Nothing
/// orderable *inside* the command buffer fixes that: not a trailing render
/// pass, not sequencing the resolve behind a copy of the frame's own output.
/// So the resolve waits for the frame's completion signal — the image map on
/// the readback path, the queue callback on the shared-surface one — and goes
/// in a command buffer of its own. Both signals already exist, which is what
/// keeps this off the critical path: registering a second one costs a
/// queue-wide drain per frame and halves live throughput.
struct PendingProfile {
    device: wgpu::Device,
    queue: wgpu::Queue,
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// `None` until the frame completed and the resolve was submitted.
    mapped: Option<mpsc::Receiver<Result<(), String>>>,
    mapped_result: Option<Result<(), String>>,
    timestamp_period_ns: f32,
    cpu_encode_submit: Duration,
    cpu_cluster: Duration,
    strict_timestamps: bool,
    grid_fog: bool,
    query_count: u32,
    passes: Vec<(&'static str, u32, u32)>,
}

impl PendingProfile {
    /// Submit the resolve, once, after the frame it belongs to has finished.
    fn resolve_after_frame(&mut self) {
        if self.mapped.is_some() {
            return;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("luma-profile-resolve"),
            });
        encoder.resolve_query_set(&self.query_set, 0..self.query_count, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(
            &self.resolve,
            0,
            &self.readback,
            0,
            u64::from(self.query_count) * 8,
        );
        self.queue.submit([encoder.finish()]);
        let (mapped_tx, mapped) = mpsc::sync_channel(1);
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = mapped_tx.send(result.map_err(|error| error.to_string()));
            });
        self.mapped = Some(mapped);
    }
}

pub(crate) struct CompletedFrame {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) image: Presented,
    pub(crate) draw_time: Duration,
    pub(crate) profile: Option<FrameTimings>,
    pub(crate) shadows: ShadowStats,
    pub(crate) clusters: LightIndexStats,
    pub(crate) queued: Duration,
    /// Where the CPU time before this frame reached Metal went.
    pub(crate) cpu: CpuSpans,
    /// Submit to the driver's completion callback — the GPU's own share of
    /// `draw_time`, including any wait to begin executing.
    pub(crate) until_signalled: Option<Duration>,
    /// Completion callback to this poll observing it — the worker's share.
    /// A large value here means nobody was looking, not that anything was slow.
    pub(crate) until_noticed: Option<Duration>,
}

/// The one wgpu device this process renders through, and everything compiled
/// against it.
///
/// # Why this is process-wide
///
/// A device is not a per-view resource. Every pipeline in here is a shader
/// compile, and every renderer that acquired its own device paid for all of
/// them again — for a second copy of pipelines that are byte-identical, since
/// nothing here varies per viewport. Sharing them is not an optimisation
/// bolted onto the renderer; it is the correct ownership, and it is what lets
/// the compile happen once at launch where a user can be told it is happening.
///
/// # What is *not* in here
///
/// Anything a frame writes. The shadow atlases, the render targets, the
/// temporal history and every cache are [`Renderer`] state, because two
/// renderers sharing one of those would draw into each other's picture. The
/// rule that keeps this honest: a field belongs here only if no frame ever
/// mutates it.
///
/// # Lifetime
///
/// Held outside the supervised render worker deliberately. A worker panic is
/// recoverable precisely because restarting it does not have to rebuild any of
/// this. Device loss is the one event that does, and [`Gpu::shared`] is where
/// that rebuild happens.
struct FogVisibilityPipelines {
    plan_layout: wgpu::BindGroupLayout,
    fill_layout: wgpu::BindGroupLayout,
    plan: wgpu::ComputePipeline,
    finalize: wgpu::ComputePipeline,
    fill: wgpu::ComputePipeline,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_profile: RendererProfile,
    environment: EnvironmentPipelines,
    atmosphere: AtmospherePipelines,
    scene_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    cluster_layout: wgpu::BindGroupLayout,
    haze_layout: wgpu::BindGroupLayout,
    medium_cache_layout: wgpu::BindGroupLayout,
    medium_cache_pipeline: wgpu::ComputePipeline,
    shadow_hierarchy_pipelines: crate::shadow_hierarchy::Pipelines,
    /// The baked volumetric density field. Belongs here rather than on the
    /// renderer because no frame mutates it — it is a function of the device
    /// and nothing else.
    haze_field: HazeField,
    light_index_pipelines: LightIndexPipelines,
    temporal_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    overlay_layout: wgpu::BindGroupLayout,
    scene_pipeline: wgpu::RenderPipeline,
    surface_depth_pipeline: wgpu::RenderPipeline,
    depth_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    fixture_shadow_layout: wgpu::BindGroupLayout,
    fixture_shadow_pipeline: wgpu::RenderPipeline,
    haze_pipeline: wgpu::RenderPipeline,
    haze_grid_pipeline: wgpu::RenderPipeline,
    haze_compute_pipeline: wgpu::ComputePipeline,
    haze_compute_layout: wgpu::BindGroupLayout,
    haze_work_counts: bool,
    /// `LUMA_HAZE_WORK_COUNTS=2`: the counter records carry histograms.
    haze_work_hist: bool,
    /// The native compute kernel caches its shadow-traversal output
    /// (`interval_cache.rs`). Needs subgroup ballots; `LUMA_INTERVAL_CACHE=0`
    /// switches it off for A/B runs.
    interval_cache: bool,
    /// log2 of the payload hash table's entry count.
    interval_cache_table_bits: u32,
    /// Residual compaction of the cached kernel (`haze_compact.wgsl`).
    haze_compact: HazeCompact,
    haze_compact_pipelines: Option<HazeCompactPipelines>,
    /// `LUMA_HAZE_RESID_AFTER=transmittance|integrate`: whether the residual
    /// dispatch waits for the lit grid or only for the transmittance prefix
    /// (bit-identical alpha), which lets it overlap the scene pass.
    haze_resid_after_integrate: bool,
    /// Per segment, whether the residual runs as a draw on the render lane.
    haze_resid_fragment: [bool; 3],
    /// `LUMA_HAZE_RESID_CAP`: residual list capacity in entries.
    haze_resid_capacity: u32,
    /// `LUMA_HAZE_DIRECT_ARENA_MB`: direct interval-arena capacity in MiB.
    /// Zero by default; this is an explicit diagnostic prototype.
    haze_direct_arena_words: u32,
    /// `LUMA_HAZE_WHOLE_K_MB`: combined K-plus-descriptor budget, capped at 128 MiB.
    haze_whole_k_bytes: u64,
    /// Lanes per workgroup of each segment's compute dispatch
    /// (`LUMA_HAZE_RESID_LANES=64,64,32`); counted runs use 64.
    haze_resid_lanes: [u32; 3],
    /// Scalar residual refresh period: 0 keeps RGB, 1 is the arithmetic
    /// control, and 4 rotates physical residual workgroups.
    haze_resid_temporal: u32,
    /// Opt-in stable phase queues with per-resident transport validity.
    haze_resid_per_resident: bool,
    /// Maximum retained scalar age in renderer medium-clock seconds.
    haze_resid_max_age: f32,
    /// Opt-in production-derived fixture lighting domain diagnostic.
    venue_lighting_domain: bool,
    diagnostic_lighting_domain: Option<DiagnosticLightingDomain>,
    profile_repeat: Option<String>,
    fog_grid_pipeline: wgpu::ComputePipeline,
    fog_visibility_requested: bool,
    fog_visibility_unavailable: Option<crate::fog_visibility_cache::LayoutError>,
    /// Independent exact fog-shadow cache experiment. It never shares the
    /// residual queue bind group or its indirect-argument buffer.
    fog_visibility: Option<FogVisibilityPipelines>,
    /// `LUMA_FOG_GRID_COUNTS=1`: the fog-grid kernel's outcome counters
    /// (`haze_grid_counted.wgsl`), cleared before each dispatch. Diagnostic;
    /// the frame is performance-ineligible.
    fog_grid_counts: Option<wgpu::Buffer>,
    fog_prepare_pipeline: wgpu::ComputePipeline,
    fog_prepare_layout: wgpu::BindGroupLayout,
    fog_classify_layout: wgpu::BindGroupLayout,
    fog_classify_pipeline: wgpu::ComputePipeline,
    fog_integrate_pipeline: wgpu::ComputePipeline,
    /// `integrate_grid` with `TRANSMITTANCE_ONLY`: the camera prefix the
    /// surface pass reads, encoded right after `fog-prepare`.
    fog_transmittance_pipeline: wgpu::ComputePipeline,
    /// `fog-integrate` reading the per-slice τ that `fog-transmittance`
    /// recorded, for frames where that prefix ran (grid-sourced surface
    /// transmittance); otherwise `fog_integrate_pipeline` taps the density.
    fog_integrate_reuse_pipeline: wgpu::ComputePipeline,
    fog_integrate_layout: wgpu::BindGroupLayout,
    fog_tau_write_layout: wgpu::BindGroupLayout,
    fog_tau_read_layout: wgpu::BindGroupLayout,
    fog_grid_write_layout: wgpu::BindGroupLayout,
    fog_grid_read_layout: wgpu::BindGroupLayout,
    temporal_pipeline: wgpu::RenderPipeline,
    /// Indexed by [`Channels::index`]: the same pass, targeting each output
    /// format.
    composite_pipelines: [wgpu::RenderPipeline; 2],
    grid_pipeline: wgpu::RenderPipeline,
    compass_pipeline: wgpu::RenderPipeline,
    cable_pipeline: wgpu::RenderPipeline,
    /// Indexed by [`overlay_pipeline_index`]: the two output formats crossed
    /// with two topologies and two depth behaviours.
    overlay_pipelines: [wgpu::RenderPipeline; 8],
    hard_shadow_sampler: wgpu::Sampler,
    shadow_sampler: wgpu::Sampler,
    /// A 1x1 depth array bound where a pass has no real shadow map to offer.
    dummy_shadow: wgpu::TextureView,
    linear_sampler: wgpu::Sampler,
    texture_sampler: wgpu::Sampler,
    /// Neutral glTF maps, bound by procedural/depth-only draws.
    white_material: wgpu::BindGroup,
    material_defaults: MaterialDefaults,
    /// Set from the driver's device-lost callback; see [`Gpu::shared`].
    lost: Arc<AtomicBool>,
    /// Whether `device` is the window compositor's; see [`crate::device::DeviceContext::adopt`].
    adopted: bool,
    /// How long [`Gpu::build`] took. Kept because it is the number the launch
    /// indicator is reporting on, and reading it from the device means the
    /// answer is the same whoever triggered the build.
    built_in: Duration,
}

/// The process-wide device, once it has been built.
///
/// A `Mutex<Option<..>>` rather than a `OnceLock` because it has exactly one
/// reason to be replaced — see [`Gpu::shared`] — and a cell that can never be
/// refilled could not express it.
static SHARED: Mutex<Option<Arc<Gpu>>> = Mutex::new(None);

impl Gpu {
    /// The process-wide device and its pipelines, building them on first use.
    ///
    /// Callers that would rather not pay the build inside a frame should call
    /// this from [`crate::warm`] at launch; everything else can simply ask.
    ///
    /// A lost device is the one and only reason a second build happens. Not a
    /// renderer panic, not a failed frame, not a resize: those leave the device
    /// intact, and rebuilding on them would throw away every compiled pipeline
    /// to fix something that was never the device's fault. Renderers still
    /// holding the lost `Arc` keep it and keep failing, which is what their
    /// own error path is for — this only decides what the *next* renderer gets.
    ///
    /// The lock is deliberately held across the build. Two threads arriving at
    /// once — the launch warmup and a stage that opened before it finished —
    /// must produce one device between them, and the second waiting is the
    /// whole point: it is waiting for the device it was about to build itself.
    ///
    /// # Errors
    /// Fails when no wgpu adapter or device can be acquired, which is the
    /// honest answer on a machine with no GPU.
    pub fn shared() -> anyhow::Result<Arc<Self>> {
        let mut slot = SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(gpu) = slot.as_ref() {
            if !gpu.is_lost() {
                return Ok(Arc::clone(gpu));
            }
            *slot = None;
        }
        let gpu = Arc::new(Self::build()?);
        *slot = Some(Arc::clone(&gpu));
        Ok(gpu)
    }

    fn profile_copies(&self, pass: &str) -> u32 {
        1 + u32::from(self.profile_repeat.as_deref() == Some(pass))
    }

    /// Whether the driver has told us this device is gone.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// How long this device and its pipelines took to build.
    #[must_use]
    pub fn built_in(&self) -> Duration {
        self.built_in
    }

    /// The process-wide device, only if it already exists.
    ///
    /// Never builds one. This is the question a progress indicator asks — "is
    /// there anything to wait for" — and answering it by building the thing
    /// would be the one call that makes the answer yes.
    #[must_use]
    pub fn built() -> Option<Arc<Self>> {
        SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|gpu| !gpu.is_lost())
            .map(Arc::clone)
    }

    /// Adapter identity, for attaching to timing evidence.
    #[must_use]
    pub fn adapter_profile(&self) -> &RendererProfile {
        &self.adapter_profile
    }

    pub(crate) fn build() -> anyhow::Result<Self> {
        let started = Instant::now();
        let context = crate::device::DeviceContext::shared()?;
        let profile = RendererProfile::of(&context.adapter, &context.device, &context.queue);
        Self::on(
            context.device.clone(),
            context.queue.clone(),
            profile,
            context.lost.clone(),
            context.adopted,
            started,
        )
    }

    /// Every pipeline, on a device somebody has already made.
    fn on(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_profile: RendererProfile,
        lost: Arc<AtomicBool>,
        adopted: bool,
        started: Instant,
    ) -> anyhow::Result<Self> {
        let profile_omit = std::env::var("LUMA_PROFILE_OMIT").unwrap_or_default();
        anyhow::ensure!(
            matches!(
                profile_omit.as_str(),
                "" | "surface-clouds"
                    | "surface-lighting"
                    | "surface-shadows"
                    | "face-lights"
                    | "native-shadows"
                    | "native-integrals"
                    | "native-clouds"
                    | "native-light-depth"
                    | "native-camera-depth"
                    | "grid-shadow-tests"
            ),
            "unknown LUMA_PROFILE_OMIT component: {profile_omit}"
        );
        let omitted = |name: &str| f64::from(u8::from(profile_omit == name));
        let profile_repeat = std::env::var("LUMA_PROFILE_REPEAT").ok();
        let diagnostic_lighting_domain = DiagnosticLightingDomain::from_env()?;
        anyhow::ensure!(
            profile_repeat.as_deref().is_none_or(|name| matches!(
                name,
                "scene"
                    | "medium-cache"
                    | "fog-prepare"
                    | "fog-classify"
                    | "fog-grid"
                    | "fog-integrate"
                    | "haze-compute"
            )),
            "unknown LUMA_PROFILE_REPEAT pass: {profile_repeat:?}"
        );

        let environment = EnvironmentPipelines::new(&device, &queue);
        let atmosphere = AtmospherePipelines::new(&device, &queue);
        let mut scene_entries = vec![
            uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
            storage_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT),
            storage_entry(2, wgpu::ShaderStages::FRAGMENT),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ];
        scene_entries.extend(crate::atmosphere::AerialTextures::layout_entries());
        scene_entries.extend([
            wgpu::BindGroupLayoutEntry {
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                ..texture_entry(10)
            },
            wgpu::BindGroupLayoutEntry {
                binding: 11,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ]);
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &scene_entries,
        });
        // Bindings 2 and 3 held the CSR cluster lists before the unified
        // light index (8–10) replaced them; the numbers stay reserved so the
        // surviving slots keep their shader-side ids. (A profiler counter
        // briefly lived at 2 as a read-write binding — bound across the hot
        // passes it serialised them on Metal, ~20× wall per frame at high
        // draw counts. Profiler accumulation is a separate compute pass now;
        // never bind a read-write buffer here.)
        let cluster_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("surface-clusters"),
            entries: &[
                storage_entry(0, wgpu::ShaderStages::FRAGMENT),
                storage_entry(1, wgpu::ShaderStages::FRAGMENT),
                uniform_entry(4, wgpu::ShaderStages::FRAGMENT),
                storage_entry(5, wgpu::ShaderStages::FRAGMENT),
                depth_array_entry(6, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform_entry(8, wgpu::ShaderStages::FRAGMENT),
                storage_entry(9, wgpu::ShaderStages::FRAGMENT),
                storage_entry(10, wgpu::ShaderStages::FRAGMENT),
                storage_entry(11, wgpu::ShaderStages::FRAGMENT),
                depth_array_entry(12, wgpu::ShaderStages::FRAGMENT),
                // Per-tile surface depth split of the light index (binding 14
                // is declared alongside 13 in `scene_bindings.wgsl`).
                // The far-field fog prefix, for grid-sourced surface transmittance.
                wgpu::BindGroupLayoutEntry {
                    binding: 13,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                storage_entry(14, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        // Group 1 is the per-draw material texture. glTF's `baseColorTexture`
        // is sRGB-encoded, so the view format decodes on sample and the shader
        // multiplies in linear space, as three does.
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let haze_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze"),
            entries: &[
                uniform_entry(
                    0,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                storage_entry(
                    1,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                storage_entry(
                    2,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // 4 and 5 held the per-pass haze tile list before the unified
                // light index (group 1) replaced it; they now carry the baked
                // density field (`haze_field.rs`), which is what the volumetric
                // integrand's `haze_noise` reads.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage_entry(
                    6,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                depth_array_entry(
                    7,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                storage_entry(
                    8,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                depth_array_entry(
                    9,
                    wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 12,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let haze_field = HazeField::bake(&device, &queue);

        // The unified light index: build pipelines plus the consumer layout
        // the haze pipeline binds as group 1.
        let light_index_pipelines = LightIndexPipelines::new(&device);

        let temporal_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze-temporal"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
            ],
        });

        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Both scene-geometry shaders open with the shared bind-group
        // declarations; see `scene_bindings.wgsl`.
        let bindings = format!(
            "{}{}{}{}{}{}{}",
            crate::haze_field::prelude(),
            include_str!("shaders/medium.wgsl"),
            include_str!("shaders/scene_bindings.wgsl"),
            crate::atmosphere::surface_prelude(),
            include_str!("shaders/horizon.wgsl"),
            include_str!("shaders/haze_daylight.wgsl"),
            include_str!("shaders/outdoor_surface.wgsl"),
        );
        let fixture_light = include_str!("shaders/fixture_light.wgsl");
        let visibility = include_str!("shaders/visibility.wgsl");
        let haze_visibility = visibility.replace("@group(3) @binding(11)", "@group(0) @binding(8)");
        // The light-index prelude is authored against group 1 (the haze
        // pass's slot); the surface pass carries the same bindings inside its
        // group 3, so its copy is rebound by this one documented replace.
        let light_index_prelude = include_str!("shaders/light_index.wgsl");
        let scene_light_index_prelude = light_index_prelude.replace("@group(1)", "@group(3)");
        let scene_module = shader(
            &device,
            "scene",
            &format!(
                "{bindings}{scene_light_index_prelude}{fixture_light}{visibility}{}",
                include_str!("shaders/scene.wgsl")
            ),
        );
        // The transport (ray reconstruction + per-light integral + group-0
        // layout) is one file both volumetric passes prepend, so they cannot
        // draw two different beams.
        let beam_transport = format!(
            "{}{}{}",
            include_str!("shaders/medium.wgsl"),
            crate::fog_grid::prelude(),
            include_str!("shaders/beam_transport.wgsl")
        );
        // The density field's dimensions are compile-time properties of
        // `haze_field`, so they arrive as injected constants rather than as
        // uniform members nobody could see drift.
        let haze_field_prelude = crate::haze_field::prelude();
        // The lit-interval cache writes one header word per 32-thread workgroup
        // from a subgroup ballot. A narrower subgroup would elect multiple
        // writers and its first ballot word would not cover the workgroup.
        // wgpu's Metal limits are conservative, while native Apple GPUs have a
        // documented 32-lane SIMD-group, so preserve that platform fast path.
        let interval_cache = supports_haze_subgroups(
            device.features(),
            adapter_profile.subgroup_min_size,
            adapter_profile.backend == "Metal",
            cfg!(all(target_os = "macos", target_arch = "aarch64")),
        ) && !std::env::var_os("LUMA_INTERVAL_CACHE")
            .is_some_and(|v| v == "0");
        let interval_cache_table_bits = std::env::var("LUMA_INTERVAL_CACHE_TABLE_BITS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(21)
            .clamp(10, 24);
        // Quadrature quad-sharing inside the cached kernel (`haze_cache.wgsl`,
        // `QUADSHARE`): off is the control arithmetic; `centre` and `own` are
        // the two approximations. Needs the cache's subgroup ops.
        let quadshare = match std::env::var("LUMA_HAZE_QUADSHARE").ok().as_deref() {
            None => 1.0,
            Some("off") | Some("0") => 0.0,
            Some("centre") | Some("center") | Some("1") => 1.0,
            Some("own") | Some("2") => 2.0,
            Some(other) => panic!("LUMA_HAZE_QUADSHARE must be off, centre or own, got {other:?}"),
        };
        let quadshare = if interval_cache { quadshare } else { 0.0 };
        let quad_env = |key: &str, default: f64| -> f64 {
            std::env::var(key)
                .ok()
                .map(|v| {
                    v.parse::<f64>()
                        .unwrap_or_else(|_| panic!("{key} must be a number, got {v:?}"))
                })
                .unwrap_or(default)
        };
        let quad_uniform = quad_env("LUMA_HAZE_QUAD_UNIFORM", 1.0);
        let quad_apex_px = quad_env("LUMA_HAZE_QUAD_APEX", 32.0);
        let quad_gate_m = quad_env("LUMA_HAZE_QUAD_GATE", 0.25);
        let quad_gate_frac = quad_env("LUMA_HAZE_QUAD_GATE_FRAC", 0.01);
        let quad_span_mean = quad_env("LUMA_HAZE_QUAD_SPAN_MEAN", 1.0);
        let quad_split_fetch = quad_env("LUMA_HAZE_QUAD_SPLIT_FETCH", 1.0);
        let quad_diag = quad_env("LUMA_HAZE_QUAD_DIAG", 0.0);
        let quad_interp = quad_env("LUMA_HAZE_QUAD_INTERP", 1.0);
        let haze_cache = if interval_cache {
            include_str!("shaders/haze_cache.wgsl")
        } else {
            include_str!("shaders/haze_cache_stub.wgsl")
        };
        let haze_compact = if interval_cache {
            HazeCompact::from_env()
        } else {
            HazeCompact::Off
        };
        let haze_compact_text = if interval_cache {
            include_str!("shaders/haze_compact.wgsl")
        } else {
            ""
        };
        let whole_scalar_text = if interval_cache {
            include_str!("shaders/whole_lit_scalar_k.wgsl")
        } else {
            ""
        };
        let haze_base_text = format!(
            "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}{haze_cache}",
            include_str!("shaders/haze.wgsl")
        );
        let haze_module = shader(
            &device,
            "haze",
            &format!("{haze_base_text}{haze_compact_text}{whole_scalar_text}"),
        );
        let medium_cache_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("medium-cache"),
                entries: &[
                    uniform_entry(0, wgpu::ShaderStages::COMPUTE),
                    storage_entry(1, wgpu::ShaderStages::COMPUTE),
                    storage_entry(2, wgpu::ShaderStages::COMPUTE),
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D3,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba16Float,
                            view_dimension: wgpu::TextureViewDimension::D3,
                        },
                        count: None,
                    },
                ],
            });
        let medium_cache_module = shader(
            &device,
            "medium-cache",
            &format!(
                "{}{}{}",
                crate::haze_field::prelude(),
                include_str!("shaders/medium.wgsl"),
                include_str!("shaders/medium_cache.wgsl")
            ),
        );
        let medium_cache_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("medium-cache"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("medium-cache"),
                        bind_group_layouts: &[Some(&medium_cache_layout)],
                        immediate_size: 0,
                    }),
                ),
                module: &medium_cache_module,
                entry_point: Some("build_cache"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let temporal_module = shader(
            &device,
            "haze-temporal",
            include_str!("shaders/haze_temporal.wgsl"),
        );
        let composite_module = shader(
            &device,
            "composite",
            &format!(
                "{}{}{}{}{}",
                crate::haze_field::prelude(),
                include_str!("shaders/medium.wgsl"),
                crate::atmosphere::composite_prelude(),
                include_str!("shaders/haze_daylight.wgsl"),
                include_str!("shaders/composite.wgsl")
            ),
        );
        let grid_module = shader(
            &device,
            "grid",
            &format!("{bindings}{}", include_str!("shaders/grid.wgsl")),
        );
        let cable_module = shader(
            &device,
            "cables",
            &format!("{bindings}{}", include_str!("shaders/cables.wgsl")),
        );

        let scene_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("scene"),
                bind_group_layouts: &[
                    Some(&scene_layout),
                    Some(&material_layout),
                    Some(environment.scene_layout()),
                    Some(&cluster_layout),
                ],
                immediate_size: 0,
            });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: 48,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 12,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 24,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 3,
                },
            ],
        };

        let scene_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_main"),
                buffers: &[Some(vertex_layout.clone())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &scene_module,
                entry_point: Some("fs_main"),
                targets: &[Some(SCENE_FORMAT.into())],
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[
                        ("PROFILE_SKIP_SURFACE_CLOUDS", omitted("surface-clouds")),
                        ("PROFILE_SKIP_FIXTURES", omitted("surface-lighting")),
                        ("PROFILE_SKIP_SURFACE_SHADOWS", omitted("surface-shadows")),
                        ("PROFILE_SKIP_FACE_LIGHTS", omitted("face-lights")),
                    ],
                    ..Default::default()
                },
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let surface_depth_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("surface-depth"),
                layout: Some(&scene_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &scene_module,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(vertex_layout.clone())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &scene_module,
                    entry_point: Some("fs_surface_depth"),
                    targets: &[Some(wgpu::TextureFormat::R16Float.into())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(depth_state(true)),
                multisample: wgpu::MultisampleState {
                    count: MSAA_SAMPLES,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            });

        let depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("depth-prepass"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_main"),
                buffers: &[Some(vertex_layout.clone())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let grid_vertex_layout = vertex_layout.clone();
        let cable_vertex_layout = vertex_layout.clone();
        // The fixture shadow pass binds only what its vertex stage reads:
        // per-map globals, the instance table, and the mesh-bucketed caster
        // index list. Borrowing the full scene layout meant one material
        // bind per caster the pass never sampled.
        let fixture_shadow_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fixture-shadow"),
                entries: &[
                    uniform_entry(0, wgpu::ShaderStages::VERTEX),
                    storage_entry(1, wgpu::ShaderStages::VERTEX),
                    storage_entry(5, wgpu::ShaderStages::VERTEX),
                ],
            });
        let fixture_shadow_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fixture-shadow"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("fixture-shadow"),
                        bind_group_layouts: &[Some(&fixture_shadow_layout)],
                        immediate_size: 0,
                    }),
                ),
                vertex: wgpu::VertexState {
                    module: &scene_module,
                    entry_point: Some("vs_fixture_shadow"),
                    buffers: &[Some(vertex_layout.clone())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: None,
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(depth_state(true)),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &scene_module,
                entry_point: Some("vs_depth"),
                buffers: &[Some(vertex_layout)],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let fog_column_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let fog_prepare_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-prepare"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                }],
            });
        let fog_prepare_module = shader(
            &device,
            "fog-prepare",
            &format!(
            "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}",
            include_str!("shaders/haze_prepare.wgsl")
            ),
        );
        let fog_prepare_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fog-prepare"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("fog-prepare"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&fog_prepare_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &fog_prepare_module,
                entry_point: Some("prepare_columns"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[(
                        "DEPTH_CULL",
                        if std::env::var_os("LUMA_FOG_DEPTH_CULL").is_some_and(|v| v == "0") {
                            0.0
                        } else {
                            1.0
                        },
                    )],
                    ..Default::default()
                },
                cache: None,
            });
        let fog_classify_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-classify"),
                entries: &[
                    fog_column_entry(0),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let fog_classify_module = shader(
            &device,
            "fog-classify",
            &format!(
            "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}",
            include_str!("shaders/haze_classify.wgsl")
            ),
        );
        let fog_classify_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fog-classify"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("fog-classify"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&fog_classify_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &fog_classify_module,
                entry_point: Some("classify_blocks"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let fog_grid_counted = std::env::var_os("LUMA_FOG_GRID_COUNTS").is_some_and(|v| v == "1");
        let fog_visibility_requested =
            std::env::var_os("LUMA_FOG_VISIBILITY_CACHE").is_some_and(|value| value == "1");
        let fog_visibility_assert =
            std::env::var_os("LUMA_FOG_VISIBILITY_ASSERT").is_some_and(|value| value == "1");
        let fog_visibility_unavailable = fog_visibility_requested
            .then(|| {
                if fog_grid_counted
                    || !crate::fog_grid::block_visibility()
                    || profile_omit == "grid-shadow-tests"
                {
                    Some(crate::fog_visibility_cache::LayoutError::InvalidConfig)
                } else if device.limits().max_storage_buffers_per_shader_stage
                    < crate::fog_visibility_cache::REQUIRED_STORAGE_BINDINGS
                {
                    Some(crate::fog_visibility_cache::LayoutError::TooManyStorageBindings)
                } else {
                    None
                }
            })
            .flatten();
        let fog_visibility_enabled =
            fog_visibility_requested && fog_visibility_unavailable.is_none();
        let mut fog_grid_write_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: SCENE_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D3,
                },
                count: None,
            },
            fog_column_entry(1),
            storage_entry(2, wgpu::ShaderStages::COMPUTE),
        ];
        if fog_grid_counted {
            fog_grid_write_entries.push(wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        } else if fog_visibility_enabled {
            fog_grid_write_entries.push(if fog_visibility_assert {
                rw_storage_entry(3, wgpu::ShaderStages::COMPUTE)
            } else {
                storage_entry(3, wgpu::ShaderStages::COMPUTE)
            });
            fog_grid_write_entries.push(storage_entry(4, wgpu::ShaderStages::COMPUTE));
        }
        let fog_grid_write_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-grid-write"),
                entries: &fog_grid_write_entries,
            });
        let fog_grid_counts = fog_grid_counted.then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fog-grid-counts"),
                size: 64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let fog_integrate_entries = |tau: Option<wgpu::BindingType>| {
            let mut entries = vec![
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: SCENE_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D3,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                fog_column_entry(2),
            ];
            entries.extend(tau.map(|ty| wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty,
                count: None,
            }));
            entries
        };
        let fog_integrate_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-integrate"),
                entries: &fog_integrate_entries(None),
            });
        // `fog-transmittance` records per-slice τ; the reusing `fog-integrate`
        // reads it back (see `FOG_TAU_MODE` in `haze_integrate.wgsl`).
        let fog_tau_write_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-transmittance"),
                entries: &fog_integrate_entries(Some(wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::R32Float,
                    view_dimension: wgpu::TextureViewDimension::D3,
                })),
            });
        let fog_tau_read_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-integrate-reuse"),
                entries: &fog_integrate_entries(Some(wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                })),
            });
        let fog_integrate_module = |mode: u32| {
            let tau = match mode {
                0 => {
                    "fn fog_tau_read(column: vec2<u32>, slice: u32) -> f32 { return 0.0; }\n\
                      fn fog_tau_write(column: vec2<u32>, slice: u32, value: f32) {}\n"
                }
                1 => {
                    "@group(2) @binding(3) var fog_tau: texture_storage_3d<r32float, write>;\n\
                      fn fog_tau_read(column: vec2<u32>, slice: u32) -> f32 { return 0.0; }\n\
                      fn fog_tau_write(column: vec2<u32>, slice: u32, value: f32) {\n\
                          textureStore(fog_tau, vec3<i32>(vec3<u32>(column, slice)), vec4<f32>(value, 0.0, 0.0, 0.0));\n\
                      }\n"
                }
                _ => {
                    "@group(2) @binding(3) var fog_tau: texture_3d<f32>;\n\
                      fn fog_tau_read(column: vec2<u32>, slice: u32) -> f32 {\n\
                          return textureLoad(fog_tau, vec3<i32>(vec3<u32>(column, slice)), 0).r;\n\
                      }\n\
                      fn fog_tau_write(column: vec2<u32>, slice: u32, value: f32) {}\n"
                }
            };
            shader(
                &device,
                "fog-integrate",
                &format!(
                "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}const FOG_TAU_MODE: u32 = {mode}u;\n{tau}{}",
                include_str!("shaders/haze_integrate.wgsl")
                ),
            )
        };
        let fog_integrate_pipeline_with =
            |label: &str,
             module: &wgpu::ShaderModule,
             layout: &wgpu::BindGroupLayout,
             transmittance_only: bool| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: Some(
                        &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: Some(label),
                            bind_group_layouts: &[
                                Some(&haze_layout),
                                Some(light_index_pipelines.layout()),
                                Some(layout),
                            ],
                            immediate_size: 0,
                        }),
                    ),
                    module,
                    entry_point: Some("integrate_grid"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: if transmittance_only {
                            &[("TRANSMITTANCE_ONLY", 1.0)]
                        } else {
                            &[]
                        },
                        ..Default::default()
                    },
                    cache: None,
                })
            };
        let fog_integrate_pipeline = fog_integrate_pipeline_with(
            "fog-integrate",
            &fog_integrate_module(0),
            &fog_integrate_layout,
            false,
        );
        let fog_transmittance_pipeline = fog_integrate_pipeline_with(
            "fog-transmittance",
            &fog_integrate_module(1),
            &fog_tau_write_layout,
            true,
        );
        let fog_integrate_reuse_pipeline = fog_integrate_pipeline_with(
            "fog-integrate-reuse",
            &fog_integrate_module(2),
            &fog_tau_read_layout,
            false,
        );
        let fog_grid_read_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-grid-read"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
        let fog_grid_body = if fog_grid_counted {
            include_str!("shaders/haze_grid_counted.wgsl").to_owned()
        } else if fog_visibility_enabled {
            format!(
                "{}{}",
                if fog_visibility_assert {
                    include_str!("shaders/fog_visibility_cache_assert.wgsl")
                } else {
                    include_str!("shaders/fog_visibility_cache_plain.wgsl")
                },
                include_str!("shaders/haze_grid_cache.wgsl")
            )
        } else {
            include_str!("shaders/haze_grid.wgsl").to_owned()
        };
        let fog_grid_module = shader(
            &device,
            "fog-grid",
            &format!(
                "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{fog_grid_body}"
            ),
        );
        let mut fog_grid_constants = vec![
            (
                "BLOCK_VISIBILITY",
                f64::from(u8::from(crate::fog_grid::block_visibility())),
            ),
            (
                "PROFILE_SKIP_GRID_SHADOW_TESTS",
                omitted("grid-shadow-tests"),
            ),
        ];
        if fog_visibility_enabled {
            fog_grid_constants.push((
                "FOG_VISIBILITY_ASSERT",
                f64::from(u8::from(fog_visibility_assert)),
            ));
        }
        let fog_grid_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fog-grid"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("fog-grid"),
                    bind_group_layouts: &[
                        Some(&haze_layout),
                        Some(light_index_pipelines.layout()),
                        Some(&fog_grid_write_layout),
                    ],
                    immediate_size: 0,
                }),
            ),
            module: &fog_grid_module,
            entry_point: Some("light_grid"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &fog_grid_constants,
                ..Default::default()
            },
            cache: None,
        });

        let fog_visibility = fog_visibility_enabled.then(|| {
            let plan_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-visibility-plan"),
                entries: &[
                    storage_entry(0, wgpu::ShaderStages::COMPUTE),
                    rw_storage_entry(1, wgpu::ShaderStages::COMPUTE),
                    rw_storage_entry(2, wgpu::ShaderStages::COMPUTE),
                ],
            });
            let fill_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-visibility-fill"),
                entries: &[
                    fog_column_entry(0),
                    rw_storage_entry(1, wgpu::ShaderStages::COMPUTE),
                    rw_storage_entry(2, wgpu::ShaderStages::COMPUTE),
                ],
            });
            let make = |label: &str,
                        source: &str,
                        layout: &wgpu::BindGroupLayout,
                        entry: &str| {
                let module = shader(
                    &device,
                    label,
                    &format!(
                        "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{source}"
                    ),
                );
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: Some(&device.create_pipeline_layout(
                        &wgpu::PipelineLayoutDescriptor {
                            label: Some(label),
                            bind_group_layouts: &[
                                Some(&haze_layout),
                                Some(light_index_pipelines.layout()),
                                Some(layout),
                            ],
                            immediate_size: 0,
                        },
                    )),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                })
            };
            let plan_source = include_str!("shaders/fog_visibility_cache_plan.wgsl");
            FogVisibilityPipelines {
                plan: make("fog-visibility-plan", plan_source, &plan_layout, "plan_visibility"),
                finalize: make(
                    "fog-visibility-finalize",
                    plan_source,
                    &plan_layout,
                    "finalize_visibility_plan",
                ),
                fill: make(
                    "fog-visibility-fill",
                    include_str!("shaders/fog_visibility_cache_fill.wgsl"),
                    &fill_layout,
                    "fill_visibility",
                ),
                plan_layout,
                fill_layout,
            }
        });

        let make_haze_pipeline = |grid: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("haze"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("haze"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&fog_grid_read_layout),
                            Some(&fog_grid_read_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                vertex: wgpu::VertexState {
                    module: &haze_module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &haze_module,
                    entry_point: Some("fs_main"),
                    // Subframes accumulate additively; each carries weight 1/K.
                    targets: &std::array::from_fn::<_, 2, _>(|_| {
                        Some(wgpu::ColorTargetState {
                            format: SCENE_FORMAT,
                            blend: Some(wgpu::BlendState {
                                color: ADD,
                                alpha: ADD,
                            }),
                            write_mask: wgpu::ColorWrites::ALL,
                        })
                    }),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &[
                            ("GRID_FOG", f64::from(u8::from(grid))),
                            ("PROFILE_SKIP_NATIVE_SHADOWS", omitted("native-shadows")),
                            ("PROFILE_SKIP_NATIVE_INTEGRALS", omitted("native-integrals")),
                            ("PROFILE_SKIP_NATIVE_CLOUDS", omitted("native-clouds")),
                            (
                                "PROFILE_SKIP_NATIVE_LIGHT_DEPTH",
                                omitted("native-light-depth"),
                            ),
                            (
                                "PROFILE_SKIP_NATIVE_CAMERA_DEPTH",
                                omitted("native-camera-depth"),
                            ),
                        ],
                        ..Default::default()
                    },
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let haze_output_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: SCENE_FORMAT,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let mut haze_compute_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            haze_output_entry(1),
            haze_output_entry(2),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ];
        if interval_cache {
            // Lit-interval cache: slot table, header words, payload claims and
            // payload entries. Writable bindings, but this layout belongs to
            // the compute kernel alone, so no other pass is serialized against
            // them.
            haze_compute_entries.extend([
                uniform_entry(4, wgpu::ShaderStages::COMPUTE),
                rw_storage_entry(5, wgpu::ShaderStages::COMPUTE),
                rw_storage_entry(6, wgpu::ShaderStages::COMPUTE),
                rw_storage_entry(7, wgpu::ShaderStages::COMPUTE),
            ]);
        }
        let haze_compute_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("haze-compute"),
                entries: &haze_compute_entries,
            });
        let haze_work_mode = std::env::var("LUMA_HAZE_WORK_COUNTS").ok();
        let haze_work_counts = haze_work_mode
            .as_deref()
            .is_some_and(|v| v == "1" || v == "2");
        let haze_work_hist = haze_work_mode.as_deref() == Some("2");
        let haze_resid_temporal = std::env::var("LUMA_HAZE_RESID_TEMPORAL")
            .ok()
            .map(|value| {
                let period = value
                    .parse::<u32>()
                    .expect("LUMA_HAZE_RESID_TEMPORAL must be 0, 1, 4 or 8");
                assert!(
                    matches!(period, 0 | 1 | 4 | 8),
                    "LUMA_HAZE_RESID_TEMPORAL must be 0, 1, 4 or 8"
                );
                period
            })
            .unwrap_or(0);
        let haze_resid_per_resident =
            std::env::var_os("LUMA_HAZE_RESID_PER_RESIDENT").is_some_and(|value| value == "1");
        assert!(
            !haze_resid_per_resident || matches!(haze_resid_temporal, 1 | 4 | 8),
            "LUMA_HAZE_RESID_PER_RESIDENT requires LUMA_HAZE_RESID_TEMPORAL=1, 4 or 8"
        );
        assert!(
            haze_resid_temporal != 8 || haze_resid_per_resident,
            "LUMA_HAZE_RESID_TEMPORAL=8 requires LUMA_HAZE_RESID_PER_RESIDENT=1"
        );
        let haze_resid_max_age = std::env::var("LUMA_HAZE_RESID_MAX_AGE_MS")
            .ok()
            .map(|value| {
                value
                    .parse::<f32>()
                    .expect("LUMA_HAZE_RESID_MAX_AGE_MS must be finite milliseconds")
                    / 1000.0
            })
            .unwrap_or(0.065);
        assert!(
            haze_resid_max_age.is_finite() && haze_resid_max_age > 0.0,
            "LUMA_HAZE_RESID_MAX_AGE_MS must be positive and finite"
        );
        let venue_lighting_domain =
            std::env::var_os("LUMA_HAZE_VENUE_DOMAIN").is_some_and(|value| value == "1");
        let haze_scalar_k = f64::from(u8::from(haze_resid_temporal != 0));
        let haze_resid_value_stride = f64::from(residual_value_stride(haze_resid_temporal));
        let direct_arena_bytes = std::env::var("LUMA_HAZE_DIRECT_ARENA_MB")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|mib| mib.checked_mul(1 << 20))
            .unwrap_or(0);
        let binding_limit = u64::from(device.limits().max_storage_buffer_binding_size)
            .min(device.limits().max_buffer_size);
        let haze_direct_arena_words = if direct_arena_bytes <= binding_limit
            && direct_arena_bytes / 4 < u64::from(0x7FFF_FFFFu32)
        {
            (direct_arena_bytes / 4) as u32
        } else {
            0
        };
        let haze_whole_k_bytes = std::env::var("LUMA_HAZE_WHOLE_K_MB")
            .ok()
            .map(|value| {
                value
                    .parse::<u64>()
                    .expect("LUMA_HAZE_WHOLE_K_MB must be 0..128")
            })
            .unwrap_or(0);
        assert!(
            haze_whole_k_bytes <= 128,
            "LUMA_HAZE_WHOLE_K_MB must be 0..128"
        );
        assert!(
            haze_resid_temporal != 8 || haze_whole_k_bytes == 0,
            "experimental residual P8 requires LUMA_HAZE_WHOLE_K_MB=0"
        );
        let haze_whole_k_bytes = haze_whole_k_bytes * (1 << 20);
        let haze_constants = |extra: &[(&'static str, f64)]| -> Vec<(&'static str, f64)> {
            let mut constants = vec![
                ("NATIVE_DETERMINISTIC", 1.0),
                ("HAZE_WORK_COUNTS", f64::from(u8::from(haze_work_counts))),
                ("HAZE_WORK_HIST", f64::from(u8::from(haze_work_hist))),
                ("INTERVAL_CACHE", f64::from(u8::from(interval_cache))),
                ("QUADSHARE", quadshare),
                ("QUAD_UNIFORM", quad_uniform),
                ("QUAD_APEX_PX", quad_apex_px),
                ("QUAD_GATE_M", quad_gate_m),
                ("QUAD_GATE_FRAC", quad_gate_frac),
                ("QUAD_SPAN_MEAN", quad_span_mean),
                ("QUAD_SPLIT_FETCH", quad_split_fetch),
                ("QUAD_DIAG", quad_diag),
                ("QUAD_INTERP", quad_interp),
                ("HAZE_GROUP_X", f64::from(HAZE_WORKGROUP[0])),
                ("HAZE_GROUP_Y", f64::from(HAZE_WORKGROUP[1])),
                ("GRID_FOG", 1.0),
                ("PROFILE_SKIP_NATIVE_SHADOWS", omitted("native-shadows")),
                ("PROFILE_SKIP_NATIVE_INTEGRALS", omitted("native-integrals")),
                ("PROFILE_SKIP_NATIVE_CLOUDS", omitted("native-clouds")),
                (
                    "PROFILE_SKIP_NATIVE_LIGHT_DEPTH",
                    omitted("native-light-depth"),
                ),
                (
                    "PROFILE_SKIP_NATIVE_CAMERA_DEPTH",
                    omitted("native-camera-depth"),
                ),
            ];
            constants.extend_from_slice(extra);
            constants
        };
        let haze_resid_lanes: [u32; 3] = if haze_work_counts {
            [64; 3]
        } else {
            let lanes: Vec<u32> = std::env::var("LUMA_HAZE_RESID_LANES")
                .ok()
                .map(|v| {
                    v.split(',')
                        .map(|n| {
                            n.trim()
                                .parse::<u32>()
                                .expect("LUMA_HAZE_RESID_LANES: three lane counts")
                        })
                        .collect()
                })
                .unwrap_or_else(|| vec![64, 64, 32]);
            assert!(
                lanes.len() == 3 && lanes.iter().all(|l| [16, 32, 64, 128].contains(l)),
                "LUMA_HAZE_RESID_LANES must be three of 16, 32, 64, 128"
            );
            [lanes[0], lanes[1], lanes[2]]
        };
        let haze_compact_pipelines = (haze_compact != HazeCompact::Off).then(|| {
            // Naga 30's Metal pipeline-constant pass corrupts its expression
            // map while compacting the counted hot entry point. Hot has no
            // dependency on temporal scheduling, so give it a small module
            // with every override baked to this device's selected value.
            let (hot_text, _) = haze_compact_text
                .split_once("// --- temporal residual scheduling")
                .expect("haze compact hot/scheduling marker");
            let hot_constants = haze_constants(&[
                ("RESID_SCALAR_HOT", haze_scalar_k),
                // The whole hot remainder must retain the full RGB transport.
                ("RESID_SCALAR_K", 0.0),
                ("RESID_VALUE_STRIDE", haze_resid_value_stride),
            ]);
            let hot_source = specialize_wgsl_overrides(
                &format!("{haze_base_text}{hot_text}{whole_scalar_text}"),
                &hot_constants,
            );
            let haze_hot_module = shader(&device, "haze-hot", &hot_source);
            // The fused kernel's bindings plus the compaction's own. Writable
            // bindings, but only the compaction passes bind this layout
            // and they are already ordered by their data dependencies.
            let mut entries = haze_compute_entries.clone();
            let stages = wgpu::ShaderStages::COMPUTE | wgpu::ShaderStages::FRAGMENT;
            for entry in &mut entries {
                entry.visibility = stages;
            }
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: 8,
                // The residual point draw's vertex stage reads the target size.
                visibility: stages | wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: None,
                },
                count: None,
            });
            for binding in 9..=15 {
                entries.push(rw_storage_entry(binding, stages));
            }
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("haze-compact"),
                entries: &entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("haze-compact"),
                bind_group_layouts: &[
                    Some(&haze_layout),
                    Some(light_index_pipelines.layout()),
                    Some(&layout),
                    Some(&fog_grid_read_layout),
                ],
                immediate_size: 0,
            });
            let make = |label: &'static str, entry: &'static str, extra: &[(&'static str, f64)]| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pipeline_layout),
                    module: &haze_module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &haze_constants(extra),
                        ..Default::default()
                    },
                    cache: None,
                })
            };
            let counted = |plain: &'static str, counted: &'static str| {
                if haze_work_counts {
                    counted
                } else {
                    plain
                }
            };
            let residual_draw = std::array::from_fn(|segment| {
                let constants = haze_constants(&[
                    ("RESID_KIND", segment as f64),
                    ("RESID_SCALAR_K", haze_scalar_k),
                    ("RESID_VALUE_STRIDE", haze_resid_value_stride),
                ]);
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("haze-residual-draw"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &haze_module,
                        entry_point: Some("vs_residual"),
                        buffers: &[],
                        compilation_options: wgpu::PipelineCompilationOptions {
                            constants: &constants,
                            ..Default::default()
                        },
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &haze_module,
                        entry_point: Some("fs_residual"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::R8Unorm,
                            blend: None,
                            write_mask: wgpu::ColorWrites::empty(),
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions {
                            constants: &constants,
                            ..Default::default()
                        },
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            });
            HazeCompactPipelines {
                residual_draw,
                fill: make("haze-fill", "fill_cache", &[("FILL_ONLY", 1.0)]),
                classify: make(
                    "haze-classify",
                    counted("classify_residual", "classify_residual_counted"),
                    // The merged fill keeps the traversal's recorded
                    // intervals and ballots; the quadrature it would also
                    // run is discarded, so skip it.
                    &[("FILL_ONLY", 1.0)],
                ),
                prepare_residual: make("haze-prepare-residual", "prepare_residual_work", &[]),
                scatter: make("haze-scatter-residual", "scatter_residual_work", &[]),
                prepare_output: make("haze-prepare-output", "prepare_haze_output", &[]),
                prepare_whole: make(
                    "haze-prepare-whole-scalar-k",
                    "prepare_whole_scalar_k",
                    &[("RESID_SCALAR_K", 0.0)],
                ),
                refresh_whole: make(
                    "haze-refresh-whole-scalar-k",
                    "refresh_whole_scalar_k",
                    &[("RESID_SCALAR_K", 0.0)],
                ),
                validate_whole_gc: make(
                    "haze-validate-whole-scalar-k-gc",
                    "validate_whole_scalar_k_gc",
                    &[("RESID_SCALAR_K", 0.0)],
                ),
                clear_whole_gc: make(
                    "haze-clear-whole-scalar-k-gc",
                    "clear_whole_scalar_k_gc",
                    &[("RESID_SCALAR_K", 0.0)],
                ),
                reset_whole: make(
                    "haze-reset-whole-scalar-k",
                    "reset_whole_after_gc",
                    &[("RESID_SCALAR_K", 0.0)],
                ),
                residual: std::array::from_fn(|segment| {
                    make(
                        "haze-residual",
                        counted("compute_residual", "compute_residual_counted"),
                        &[
                            ("RESID_KIND", segment as f64),
                            ("RESID_LANES", f64::from(haze_resid_lanes[segment])),
                            ("RESID_SCALAR_K", haze_scalar_k),
                            ("RESID_VALUE_STRIDE", haze_resid_value_stride),
                        ],
                    )
                }),
                hot: device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("haze-hot"),
                    layout: Some(&pipeline_layout),
                    module: &haze_hot_module,
                    entry_point: Some(counted("compute_haze_hot", "compute_haze_hot_counted")),
                    compilation_options: Default::default(),
                    cache: None,
                }),
                layout,
            }
        });
        // Counted runs keep the compute residual: its per-workgroup reduction
        // has no fragment equivalent.
        // Per segment (single, payload, traverse): true runs it as a draw on
        // the render lane. `mixed` draws only the traversal segment, whose
        // few long entries are latency-bound and hide beside the others.
        let haze_resid_fragment = match std::env::var("LUMA_HAZE_RESID_STAGE").ok().as_deref() {
            None | Some("compute") => [false; 3],
            Some("mixed") => [false, false, !haze_work_counts],
            Some("fragment") => [!haze_work_counts; 3],
            Some(other) => {
                panic!("LUMA_HAZE_RESID_STAGE must be mixed, fragment or compute, got {other:?}")
            }
        };
        // After the lit grid by default: the grid chain then fills the window
        // beside the scene pass (it tolerates that contention), and the
        // residual runs uncontended behind it. `transmittance` runs the
        // residual right after the classify pass, off the transmittance-only
        // prefix (bit-identical alpha), which measured 0.1 ms slower on the
        // 3 s replay.
        let haze_resid_after_integrate =
            match std::env::var("LUMA_HAZE_RESID_AFTER").ok().as_deref() {
                None | Some("integrate") => true,
                Some("transmittance") => false,
                Some(other) => panic!(
                    "LUMA_HAZE_RESID_AFTER must be transmittance or integrate, got {other:?}"
                ),
            };
        let haze_resid_capacity = std::env::var("LUMA_HAZE_RESID_CAP")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(8 << 20)
            .clamp(1 << 16, 1 << 27)
            / 64
            * 64;
        let haze_compute_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("haze-compute"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("haze-compute"),
                        bind_group_layouts: &[
                            Some(&haze_layout),
                            Some(light_index_pipelines.layout()),
                            Some(&haze_compute_layout),
                            Some(&fog_grid_read_layout),
                        ],
                        immediate_size: 0,
                    }),
                ),
                module: &haze_module,
                entry_point: Some(if haze_work_counts {
                    "compute_haze_counted"
                } else {
                    "compute_haze"
                }),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[
                        ("NATIVE_DETERMINISTIC", 1.0),
                        ("HAZE_WORK_COUNTS", f64::from(u8::from(haze_work_counts))),
                        ("HAZE_WORK_HIST", f64::from(u8::from(haze_work_hist))),
                        ("INTERVAL_CACHE", f64::from(u8::from(interval_cache))),
                        ("QUADSHARE", quadshare),
                        ("QUAD_UNIFORM", quad_uniform),
                        ("QUAD_APEX_PX", quad_apex_px),
                        ("QUAD_GATE_M", quad_gate_m),
                        ("QUAD_GATE_FRAC", quad_gate_frac),
                        ("QUAD_SPAN_MEAN", quad_span_mean),
                        ("QUAD_SPLIT_FETCH", quad_split_fetch),
                        ("QUAD_DIAG", quad_diag),
                        ("QUAD_INTERP", quad_interp),
                        ("HAZE_GROUP_X", f64::from(HAZE_WORKGROUP[0])),
                        ("HAZE_GROUP_Y", f64::from(HAZE_WORKGROUP[1])),
                        ("GRID_FOG", 1.0),
                        ("PROFILE_SKIP_NATIVE_SHADOWS", omitted("native-shadows")),
                        ("PROFILE_SKIP_NATIVE_INTEGRALS", omitted("native-integrals")),
                        ("PROFILE_SKIP_NATIVE_CLOUDS", omitted("native-clouds")),
                        (
                            "PROFILE_SKIP_NATIVE_LIGHT_DEPTH",
                            omitted("native-light-depth"),
                        ),
                        (
                            "PROFILE_SKIP_NATIVE_CAMERA_DEPTH",
                            omitted("native-camera-depth"),
                        ),
                    ],
                    ..Default::default()
                },
                cache: None,
            });

        let haze_pipeline = make_haze_pipeline(false);
        let haze_grid_pipeline = make_haze_pipeline(true);

        let temporal_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("haze-temporal"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("haze-temporal"),
                    bind_group_layouts: &[Some(&temporal_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &temporal_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &temporal_module,
                entry_point: Some("fs_main"),
                targets: &[Some(SCENE_FORMAT.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("composite"),
                bind_group_layouts: &[
                    Some(&composite_layout),
                    Some(environment.scene_layout()),
                    Some(atmosphere.composite_layout()),
                ],
                immediate_size: 0,
            });
        let composite_pipelines = [Channels::Rgba, Channels::Bgra].map(|channels| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("composite"),
                layout: Some(&composite_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &composite_module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(channels.format().into())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        let grid_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &grid_module,
                entry_point: Some("vs_main"),
                buffers: &[Some(grid_vertex_layout.clone())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &grid_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            // `depthWrite: false` — the grid tests against the stage but never
            // occludes it.
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        // The grid's own state, with the grid's own module, down to a second
        // fragment entry point: the compass is the same quad, blended the same
        // way, and a divergent depth or blend choice here would be a second
        // answer to a settled question.
        let compass_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("compass"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &grid_module,
                entry_point: Some("vs_main"),
                buffers: &[Some(grid_vertex_layout.clone())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &grid_module,
                entry_point: Some("fs_compass"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        // The grid's own state, with the grid's shader swapped out: both are
        // transparent scene-space affordances that test depth and never write
        // it, and a second blend or depth choice here would be a second answer
        // to the same question.
        let cable_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cables"),
            layout: Some(&scene_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &cable_module,
                entry_point: Some("vs_main"),
                buffers: &[Some(cable_vertex_layout)],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &cable_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(false)),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLES,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX),
                storage_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT),
            ],
        });
        let overlay_module = shader(&device, "overlay", include_str!("shaders/overlay.wgsl"));
        let overlay_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay"),
                bind_group_layouts: &[Some(&overlay_layout)],
                immediate_size: 0,
            });
        let overlay_position_layout = wgpu::VertexBufferLayout {
            array_stride: 48,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            }],
        };
        let overlay_pipelines = std::array::from_fn(|i| {
            let channels = if i < 4 {
                Channels::Rgba
            } else {
                Channels::Bgra
            };
            let variant = i % 4;
            let lines = variant & 1 == 1;
            let free = variant & 2 == 2;
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("overlay"),
                layout: Some(&overlay_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &overlay_module,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(overlay_position_layout.clone())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &overlay_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: channels.format(),
                        blend: free.then_some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: if lines {
                        wgpu::PrimitiveTopology::LineList
                    } else {
                        wgpu::PrimitiveTopology::TriangleList
                    },
                    // three's `MeshBasicMaterial` is `FrontSide`; the gizmo's
                    // plane quads are one-sided and the hide rules assume it.
                    cull_mode: (!lines).then_some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(if free {
                    wgpu::DepthStencilState {
                        depth_compare: Some(wgpu::CompareFunction::Always),
                        ..depth_state(false)
                    }
                } else {
                    depth_state(true)
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        let (dummy_shadow, _) = shadow_texture_array(&device, 1, 1, 1, "shadow-placeholder");
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::GreaterEqual),
            ..Default::default()
        });
        let hard_shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hard-shadow"),
            compare: Some(wgpu::CompareFunction::GreaterEqual),
            ..Default::default()
        });
        // glTF sampler `wrapS/wrapT = REPEAT`, trilinear — three's default for
        // an imported texture, and the mip chain is what keeps the deck's wood
        // grain from aliasing into a bright fizz at grazing angles.
        let texture_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let one_pixel = |rgba: [u8; 4], encoding| {
            upload_texture_view(
                &device,
                &queue,
                &Image {
                    width: 1,
                    height: 1,
                    rgba: std::sync::Arc::from(rgba),
                },
                encoding,
            )
        };
        let material_defaults = MaterialDefaults {
            base_color: one_pixel([255; 4], TextureEncoding::Srgb),
            normal: one_pixel([128, 128, 255, 255], TextureEncoding::Linear),
            metallic_roughness: one_pixel([255; 4], TextureEncoding::Linear),
            occlusion: one_pixel([255; 4], TextureEncoding::Linear),
            // The glTF identity for an absent emissive texture is white. The
            // factor, not a synthetic black map, is what disables emission.
            // This also preserves procedural emissive materials, which never
            // carry a glTF image.
            emissive: one_pixel([255; 4], TextureEncoding::Srgb),
        };
        let white_material = material_bind_group(
            &device,
            &material_layout,
            &texture_sampler,
            &material_defaults.base_color,
            &material_defaults.normal,
            &material_defaults.metallic_roughness,
            &material_defaults.occlusion,
            &material_defaults.emissive,
        );

        let shadow_hierarchy_pipelines = crate::shadow_hierarchy::Pipelines::new(&device);
        Ok(Self {
            device,
            queue,
            adapter_profile,
            environment,
            atmosphere,
            scene_layout,
            material_layout,
            cluster_layout,
            haze_layout,
            medium_cache_layout,
            medium_cache_pipeline,
            shadow_hierarchy_pipelines,
            haze_field,
            light_index_pipelines,
            temporal_layout,
            composite_layout,
            overlay_layout,
            scene_pipeline,
            surface_depth_pipeline,
            depth_pipeline,
            shadow_pipeline,
            fixture_shadow_layout,
            fixture_shadow_pipeline,
            haze_pipeline,
            haze_grid_pipeline,
            haze_compute_pipeline,
            haze_compute_layout,
            haze_work_counts,
            haze_work_hist,
            interval_cache,
            interval_cache_table_bits,
            haze_compact,
            haze_compact_pipelines,
            haze_resid_after_integrate,
            haze_resid_fragment,
            haze_resid_capacity,
            haze_direct_arena_words,
            haze_whole_k_bytes,
            haze_resid_lanes,
            haze_resid_temporal,
            haze_resid_per_resident,
            haze_resid_max_age,
            venue_lighting_domain,
            diagnostic_lighting_domain,
            profile_repeat,
            fog_grid_pipeline,
            fog_visibility_requested,
            fog_visibility_unavailable,
            fog_visibility,
            fog_grid_counts,
            fog_prepare_pipeline,
            fog_prepare_layout,
            fog_classify_layout,
            fog_classify_pipeline,
            fog_integrate_pipeline,
            fog_transmittance_pipeline,
            fog_integrate_reuse_pipeline,
            fog_integrate_layout,
            fog_tau_write_layout,
            fog_tau_read_layout,
            fog_grid_write_layout,
            fog_grid_read_layout,
            temporal_pipeline,
            composite_pipelines,
            grid_pipeline,
            compass_pipeline,
            cable_pipeline,
            overlay_pipelines,
            hard_shadow_sampler,
            shadow_sampler,
            dummy_shadow,
            linear_sampler,
            texture_sampler,
            white_material,
            material_defaults,
            lost,
            adopted,
            built_in: started.elapsed(),
        })
    }

    /// The device every renderer draws with.
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Its queue.
    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Whether this device is the window compositor's — see [`crate::device::DeviceContext::adopt`].
    #[must_use]
    pub fn is_adopted(&self) -> bool {
        self.adopted
    }
}

impl Renderer {
    /// Build a renderer on the process-wide device.
    ///
    /// # Errors
    /// Fails when no wgpu adapter or device can be acquired.
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self::on(Gpu::shared()?))
    }

    /// Build a renderer that records hardware timestamps.
    ///
    /// Separate from [`Self::new`] because the query sets and their readback
    /// buffers are per-renderer and a production viewport should not allocate
    /// them. The device feature itself is always on where the adapter offers
    /// it, so this no longer decides which device gets acquired.
    ///
    /// # Errors
    /// Fails when no GPU exists or the adapter has no timestamp-query support.
    /// Profiling never substitutes CPU wall time for GPU evidence.
    pub fn new_profiled() -> anyhow::Result<Self> {
        Self::profiling_on(Gpu::shared()?)
    }

    /// Build a renderer on a device somebody else acquired.
    ///
    /// Infallible, and that is the point of the shared device: once a [`Gpu`]
    /// exists, nothing about opening another view onto it can fail.
    #[must_use]
    pub fn on(gpu: Arc<Gpu>) -> Self {
        Self::build_on(gpu, false)
    }

    /// [`Self::on`], recording hardware timestamps.
    ///
    /// # Errors
    /// Fails when the adapter has no timestamp-query support. Profiling never
    /// substitutes CPU wall time for GPU evidence.
    pub fn profiling_on(gpu: Arc<Gpu>) -> anyhow::Result<Self> {
        anyhow::ensure!(
            gpu.adapter_profile.timestamp_query_supported,
            "selected GPU adapter does not support timestamp queries"
        );
        Ok(Self::build_on(gpu, true))
    }

    /// The precondition on `profiled` is [`Self::profiling_on`]'s to check, so
    /// nothing here can fail: on an existing device this only allocates.
    fn build_on(gpu: Arc<Gpu>, profiled: bool) -> Self {
        let staging_device = gpu.device.clone();
        let (device, queue) = (&gpu.device, &gpu.queue);
        let query_capacity = if std::env::var_os("LUMA_PROFILE_DETAIL").is_some_and(|v| v == "1") {
            crate::pass_profile::QUERY_CAPACITY
        } else {
            QUERY_COUNT
        };
        let profiler = profiled.then(|| ProfilerResources {
            slots: std::array::from_fn(|_| ProfilerSlot {
                query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("luma-profile-timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: query_capacity,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("luma-profile-resolve"),
                    size: query_capacity as u64 * 8,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("luma-profile-readback"),
                    size: query_capacity as u64 * 8,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
            }),
            timestamp_period_ns: queue.get_timestamp_period(),
        });
        let (shadow_map, shadow_layers) = shadow_texture_array(
            device,
            SHADOW_SIZE,
            SHADOW_SIZE,
            CASCADE_COUNT as u32,
            "shadow-cascades",
        );
        let (fixture_shadow_map, fixture_shadow_layers) =
            fixture_shadow_texture_array(device, MAX_FIXTURE_SHADOWS as u32);
        let (fixture_shadow_map_extra, _) = fixture_shadow_texture_array(device, 1);
        let light_index = LightIndex::new(device);
        let visibility = crate::visibility::Visibility::new(device);

        let medium_cache = crate::medium::Cache::new(&gpu.device);
        let shadow_hierarchy = crate::shadow_hierarchy::Targets::new(
            &gpu.device,
            FIXTURE_SHADOW_SIZE,
            MAX_FIXTURE_SHADOWS,
        );
        let haze_compute = std::env::var_os("LUMA_HAZE_COMPUTE")
            .map_or(gpu.adapter_profile.backend == "Metal", |v| v == "1");
        let compact_group_columns = gpu.device.limits().max_compute_workgroups_per_dimension;
        let fog_visibility = crate::fog_visibility_cache::State::new(
            device,
            gpu.fog_visibility_requested,
            gpu.fog_visibility_unavailable,
        );
        Self {
            gpu,
            haze_compute,
            surface_transmittance: SurfaceTransmittance::from_env(),
            haze_work_counts_valid: false,
            fog_blocks_valid: false,
            environment: EnvironmentCache::default(),
            atmosphere: AtmosphereCache::default(),
            shadow_map,
            shadow_layers,
            fixture_shadow_map,
            fixture_shadow_map_extra,
            fixture_shadow_layers,
            fixture_shadow_cache: vec![None; MAX_FIXTURE_SHADOWS],
            cascade_shadow_cache: [None; CASCADE_COUNT],
            fixture_shadow_slots: vec![None; MAX_FIXTURE_SHADOWS],
            texture_views: HashMap::new(),
            materials: HashMap::new(),
            geometry: None,
            visibility,
            geometry_shadows: false,
            visibility_reference: std::env::var_os("LUMA_VISIBILITY_REFERENCE")
                .is_some_and(|v| v == "1"),
            surface_depth_cull: !std::env::var_os("LUMA_SURFACE_DEPTH_CULL")
                .is_some_and(|v| v == "0"),
            grid_fog: !std::env::var_os("LUMA_GRID_FOG").is_some_and(|v| v == "0"),
            wide_light_group: std::env::var("LUMA_WIDE_LIGHT_GROUP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8)
                .clamp(1, 8),
            geometry_shadow_samples: std::env::var("LUMA_GEOMETRY_SHADOW_SAMPLES")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(2)
                .clamp(2, 32),
            upload_stats: UploadStats {
                geometry: 0,
                textures: 0,
                environments: 0,
            },
            targets: None,
            haze_history_valid: false,
            haze_history_index: 0,
            haze_history_key: None,
            medium_cache,
            shadow_hierarchy,
            last_live_time: None,
            live_noise_frame: 0,
            profiler,
            light_index,
            shadow_stats: ShadowStats::default(),
            interval_cache: crate::interval_cache::IntervalCacheState::new(0),
            interval_pools: None,
            compact_pools: None,
            compact_stats: CompactStats::default(),
            fog_visibility,
            residual_temporal: ResidualTemporalState::default(),
            whole_temporal: ResidualTemporalState::default(),
            transport_submission: 0,
            whole_submission: 0,
            whole_submission_keys: VecDeque::new(),
            whole_gc_pending: false,
            whole_saturated_key: None,
            whole_resident_epoch: 0,
            compact_classified: Vec::new(),
            compact_resident_keys: Vec::new(),
            compact_resident_occupancy: Vec::new(),
            compact_mapping_valid: Vec::new(),
            compact_dense: Vec::new(),
            compact_full_count: 0,
            compact_queue_epoch: 0,
            compact_group_columns,
            interval_cache_entries: Vec::new(),
            // Sized for a frame's typical upload total; an oversized upload
            // (a cluster index rebuild) gets a dedicated chunk that recycles
            // like any other.
            staging: std::cell::RefCell::new(wgpu::util::StagingBelt::new(staging_device, 4 << 20)),
            frame_buffers: std::cell::RefCell::default(),
        }
    }

    fn targets(
        &mut self,
        width: u32,
        height: u32,
        haze: (u32, u32),
        destination: Destination,
        grid_fog: bool,
        surface_depth_cull: bool,
    ) -> &Targets {
        let stale = self.targets.as_ref().is_none_or(|t| {
            t.width != width
                || t.height != height
                || t.destination != destination
                || (t.haze_width, t.haze_height) != haze
        });
        let channels = destination.channels();
        if stale {
            self.haze_history_valid = false;
            self.haze_history_key = None;
            let color = |w, h, samples, usage, label| {
                self.gpu
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format: SCENE_FORMAT,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            let bytes_per_row = (width * 4).div_ceil(256) * 256;
            let presentations = std::array::from_fn(|_| {
                if destination == Destination::Compositor {
                    if let Some(shared) = crate::share::Shared::new(&self.gpu, width, height) {
                        return PresentationTarget::Shared(shared);
                    }
                }
                let output = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("output"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: channels.format(),
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let view = output.create_view(&wgpu::TextureViewDescriptor::default());
                PresentationTarget::Staged {
                    output,
                    view,
                    readback: self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("readback"),
                        size: u64::from(bytes_per_row * height),
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                }
            });
            self.targets = Some(Targets {
                width,
                height,
                haze_width: haze.0,
                haze_height: haze.1,
                destination,
                msaa_color: color(
                    width,
                    height,
                    MSAA_SAMPLES,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                    "scene-msaa",
                ),
                msaa_surface_depth: None,
                msaa_depth: depth_texture(
                    &self.gpu.device,
                    width,
                    height,
                    MSAA_SAMPLES,
                    "depth-msaa",
                ),
                scene: color(
                    width,
                    height,
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    "scene",
                ),
                depth: depth_texture(&self.gpu.device, width, height, 1, "depth"),
                haze: color(
                    haze.0,
                    haze.1,
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING,
                    "haze",
                ),
                fog: crate::fog_grid::Targets::new(&self.gpu.device, None),
                haze_sampled: color(
                    haze.0,
                    haze.1,
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING,
                    "haze-sampled",
                ),
                haze_work_counts: self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("haze-work-counts"),
                    size: if self.gpu.haze_work_counts {
                        u64::from(haze.0.div_ceil(HAZE_WORKGROUP[0]))
                            * u64::from(haze.1.div_ceil(HAZE_WORKGROUP[1]))
                            * 16
                            * 4
                    } else {
                        4
                    },
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                haze_work_counts_compact: {
                    let blocks = u64::from(haze.0.div_ceil(HAZE_WORKGROUP[0]))
                        * u64::from(haze.1.div_ceil(HAZE_WORKGROUP[1]));
                    // Twelve kind×phase buckets each round up to a counted
                    // 64-lane record, at most eleven beyond the rounded total.
                    let residual = u64::from(self.gpu.haze_resid_capacity.div_ceil(64) + 11);
                    let make = |label: &str, records: u64| {
                        self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some(label),
                            size: if self.gpu.haze_work_counts {
                                records * 16 * 4
                            } else {
                                4
                            },
                            usage: wgpu::BufferUsages::STORAGE
                                | wgpu::BufferUsages::COPY_SRC
                                | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        })
                    };
                    [
                        make("haze-work-counts-classify", blocks),
                        make("haze-work-counts-residual", residual),
                        make("haze-work-counts-hot", blocks),
                    ]
                },
                haze_history: std::array::from_fn(|_| {
                    color(
                        haze.0,
                        haze.1,
                        1,
                        wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        "haze-history",
                    )
                }),
                presentations,
                bytes_per_row,
            });
        }
        if surface_depth_cull {
            let targets = self.targets.as_mut().expect("just populated");
            if targets.msaa_surface_depth.is_none() {
                targets.msaa_surface_depth = Some(
                    self.gpu
                        .device
                        .create_texture(&wgpu::TextureDescriptor {
                            label: Some("surface-depth-msaa"),
                            size: wgpu::Extent3d {
                                width,
                                height,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: MSAA_SAMPLES,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::R16Float,
                            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                                | wgpu::TextureUsages::TEXTURE_BINDING,
                            view_formats: &[],
                        })
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                );
            }
        }
        if grid_fog {
            self.targets
                .as_mut()
                .expect("just populated")
                .fog
                .ensure(&self.gpu.device, [width, height]);
        }
        self.targets.as_ref().expect("just populated")
    }

    /// Render one frame and read it back as sRGB-encoded RGBA8, row-major, no
    /// padding.
    ///
    /// `subframes` is the jitter-accumulation count (spec §6): the same jitter
    /// primitive the live temporal pass uses, applied deterministically.
    ///
    /// The frame stands alone: the temporal history is bypassed and reset, so
    /// the image is a function of this `frame` and `subframes` alone. Sampling
    /// a sequence of *consecutive* moments is [`Renderer::render_next`].
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub fn render(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
    ) -> anyhow::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.render_into(frame, width, height, subframes, Channels::Rgba, &mut out)?;
        Ok(out)
    }

    /// Exhaustive per-ray reference capture, with no light selection, shared
    /// volume interpolation, or spatial denoising. The caller controls pixel
    /// resolution and sample count through `frame` and `subframes`.
    /// This converges the renderer's transport model, not an independent tracer.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub fn render_reference(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
    ) -> anyhow::Result<Vec<u8>> {
        let group = std::mem::replace(&mut self.wide_light_group, 1);
        let result = self.render(frame, width, height, subframes);
        self.wide_light_group = group;
        result
    }

    /// Render the *next* frame of a sequence and read it back as
    /// sRGB-encoded RGBA8, row-major, no padding.
    ///
    /// Deterministic lighting is evaluated once at the current scene time.
    /// Stochastic transport accumulates the requested subframes and, when
    /// valid, temporal history. Changes to camera, medium, lighting, geometry,
    /// or time continuity reject that history. [`Self::render`] captures an
    /// isolated moment without history; [`Self::render_reference`] additionally
    /// disables shared volume interpolation and light selection.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub fn render_next(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
    ) -> anyhow::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.render_live_into(frame, width, height, subframes, Channels::Rgba, &mut out)?;
        Ok(out)
    }

    /// Measure one frame through the production temporal/live pass chain.
    ///
    /// # Errors
    /// Fails if this renderer was not created by [`Self::new_profiled`] or if
    /// either GPU readback cannot be mapped.
    pub fn profile_live_frame(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
    ) -> anyhow::Result<FrameTimings> {
        self.profile_live_into(frame, width, height, subframes, &mut Vec::new())
    }

    /// Time a live frame using the same output surface as the viewport.
    ///
    /// Shared-surface devices avoid the full-image CPU readback performed by
    /// [`Self::profile_live_frame`]. Completion and timestamp readback still
    /// block; this measures serial renderer work, not display presentation.
    /// Devices without surface sharing use the viewport's pixel fallback.
    /// Keep this destination throughout a sequence: switching to a pixel
    /// capture reallocates targets and resets temporal history.
    ///
    /// # Errors
    /// Fails when profiling is disabled, completion fails, or timestamps are
    /// missing or invalid. Unlike interactive diagnostics, no sample is dropped.
    pub fn profile_live_surface(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
    ) -> anyhow::Result<FrameTimings> {
        anyhow::ensure!(
            self.profiler.is_some(),
            "renderer was not created for profiling"
        );
        let mut pending = self.submit_readback(
            frame,
            width.max(1),
            height.max(1),
            subframes,
            Destination::Compositor,
            0,
            true,
            true,
        );
        pending
            .complete_blocking(&self.gpu.device)?
            .profile
            .ok_or_else(|| anyhow::anyhow!("profile query resources were unavailable"))
    }

    /// Capture RGBA pixels and timings from the same production live frame.
    /// The existing profiling readback supplies the pixels; no extra frame or
    /// GPU copy is submitted. Useful when changed shadows must be audited.
    ///
    /// # Errors
    /// Fails if profiling is disabled or either GPU readback cannot be mapped.
    pub fn profile_live_into(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        pixels: &mut Vec<u8>,
    ) -> anyhow::Result<FrameTimings> {
        anyhow::ensure!(
            self.profiler.is_some(),
            "renderer was not created for profiling"
        );
        let mut pending = self.submit_readback(
            frame,
            width.max(1),
            height.max(1),
            subframes,
            Destination::Bytes(Channels::Rgba),
            0,
            true,
            true,
        );
        let completed = pending.complete_blocking(&self.gpu.device)?;
        let timing = completed
            .profile
            .ok_or_else(|| anyhow::anyhow!("profile query resources were unavailable"))?;
        *pixels = completed
            .image
            .into_pixels()
            .expect("a Bytes destination reads its pixels back");
        Ok(timing)
    }

    /// Read cumulative immutable-resource upload counts.
    #[must_use]
    pub fn upload_stats(&self) -> UploadStats {
        self.upload_stats
    }

    /// Read metrics for the most recently submitted surface cluster grid.
    #[must_use]
    pub fn light_index_stats(&self) -> LightIndexStats {
        self.light_index.stats()
    }

    /// Count `(geometry pixels, original index candidates)` for the latest
    /// submitted frame, independent of whether its timings were measured.
    ///
    /// This explicitly requested diagnostic dispatches, copies and blocks.
    /// It never runs automatically inside a timed frame. It counts the
    /// original ray-mask/Z-bin index, not the refined surface mask or MSAA
    /// invocations. Returns `None` before rendering or with no geometry pixels.
    pub fn fragment_stats(&mut self) -> anyhow::Result<Option<(u64, u64)>> {
        let Some(targets) = self.targets.as_ref() else {
            return Ok(None);
        };
        let counters = self.light_index.bindings().fragment_counters;
        let readback = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fragment-counters-readback"),
            size: 8,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("fragment-counters"),
            });
        self.light_index.record_fragment_count(
            &self.gpu.light_index_pipelines,
            &self.gpu.device,
            &mut encoder,
            &targets.depth,
        );
        encoder.copy_buffer_to_buffer(&counters, 0, &readback, 0, 8);
        self.gpu.queue.submit([encoder.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |result| {
            if let Err(error) = result {
                eprintln!("fragment counter map failed: {error}");
            }
        });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(anyhow::Error::msg)?;
        let view = readback
            .slice(..)
            .get_mapped_range()
            .map_err(anyhow::Error::msg)?;
        let words: &[u32] = bytemuck::cast_slice(&view);
        let (fragments, candidates) = (u64::from(words[0]), u64::from(words[1]));
        drop(view);
        Ok((fragments > 0).then_some((fragments, candidates)))
    }

    /// Read the fog-grid outcome counters of the last submitted frame
    /// (`LUMA_FOG_GRID_COUNTS=1`, `haze_grid_counted.wgsl` for the layout).
    pub fn fog_grid_counts(&self) -> anyhow::Result<Option<[u32; 16]>> {
        let Some(source) = self.gpu.fog_grid_counts.as_ref() else {
            return Ok(None);
        };
        let readback = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fog-grid-counts-readback"),
            size: 64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(source, 0, &readback, 0, 64);
        self.gpu.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(anyhow::Error::msg)?;
        receiver.recv().map_err(anyhow::Error::msg)??;
        let data = readback
            .slice(..)
            .get_mapped_range()
            .map_err(anyhow::Error::msg)?;
        let words: &[u32] = bytemuck::cast_slice(&data);
        let out: [u32; 16] = std::array::from_fn(|i| words[i]);
        drop(data);
        Ok(Some(out))
    }

    /// Read the latest submitted compute-haze work counts. Enable explicitly
    /// with `LUMA_HAZE_WORK_COUNTS=1` before device creation. This diagnostic
    /// adds shader work, and its frame timings must not be used as benchmarks.
    /// Returns `None` before an eligible frame or after a non-compute frame.
    pub fn haze_work_stats(&self) -> anyhow::Result<Option<HazeWorkStats>> {
        if !self.haze_work_counts_valid {
            return Ok(None);
        }
        let targets = self.targets.as_ref().expect("counted frame has targets");
        let read_records = |source: &wgpu::Buffer| -> anyhow::Result<Vec<[u32; 16]>> {
            let readback = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("haze-work-readback"),
                size: source.size(),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(source, 0, &readback, 0, source.size());
            self.gpu.queue.submit([encoder.finish()]);
            let (sender, receiver) = mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            self.gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(anyhow::Error::msg)?;
            receiver.recv().map_err(anyhow::Error::msg)??;
            let data = readback
                .slice(..)
                .get_mapped_range()
                .map_err(anyhow::Error::msg)?;
            let groups = bytemuck::cast_slice::<u8, [u32; 16]>(&data).to_vec();
            drop(data);
            Ok(groups)
        };
        let compact = self.compact_stats.active;
        let groups = if compact {
            // Classify and hot share the block layout: sums add, maxima take
            // the larger. The residual's 1D records are folded into the first
            // block's sums so the totals over all records stay comparable
            // with the fused kernel's.
            let classify = read_records(&targets.haze_work_counts_compact[0])?;
            let residual = read_records(&targets.haze_work_counts_compact[1])?;
            let hot = read_records(&targets.haze_work_counts_compact[2])?;
            let mut groups: Vec<[u32; 16]> = classify
                .iter()
                .zip(&hot)
                .map(|(c, h)| {
                    std::array::from_fn(|i| {
                        if i < 8 {
                            c[i].wrapping_add(h[i])
                        } else {
                            c[i].max(h[i])
                        }
                    })
                })
                .collect();
            if let Some(first) = groups.first_mut() {
                for record in &residual {
                    for i in 0..8 {
                        first[i] = first[i].wrapping_add(record[i]);
                    }
                }
            }
            groups
        } else {
            read_records(&targets.haze_work_counts)?
        };
        Ok(Some(HazeWorkStats {
            image_size: [targets.haze_width, targets.haze_height],
            workgroup_size: HAZE_WORKGROUP,
            workgroups: [
                targets.haze_width.div_ceil(HAZE_WORKGROUP[0]),
                targets.haze_height.div_ceil(HAZE_WORKGROUP[1]),
            ],
            names: if self.gpu.haze_work_hist {
                [
                    "pair_calls_0",
                    "pair_calls_1",
                    "pair_calls_2",
                    "pair_calls_3",
                    "pair_calls_4",
                    "pair_calls_5_8",
                    "pair_calls_9_plus",
                    "pair_whole_span",
                ]
            } else {
                [
                    "candidates",
                    "intersections",
                    "shadow_segments",
                    "depth_reads",
                    "lit_intervals",
                    "quadrature_taps",
                    "shadow_rays",
                    "first_block_whole_lit",
                ]
            },
            mode: if compact {
                "counts-compact"
            } else if self.gpu.haze_work_hist {
                "histogram"
            } else {
                "counts"
            },
            groups,
        }))
    }

    /// Read the latest submitted grid classification without adding shader work.
    /// This copies/maps the existing masks on request, outside frame submission.
    /// Returns `None` before classification or after a frame that did not use it.
    pub fn fog_block_stats(&self) -> anyhow::Result<Option<crate::fog_grid::FogBlockStats>> {
        if !self.fog_blocks_valid {
            return Ok(None);
        }
        let targets = self.targets.as_ref().expect("classified frame has targets");
        targets
            .fog
            .block_stats(&self.gpu.device, &self.gpu.queue)
            .map(Some)
    }

    /// Enable retained shadow maps for every fixture. Surfaces and haze share
    /// them; the software tracer is reserved for the reference harness.
    pub fn set_geometry_shadows(&mut self, enabled: bool) {
        if self.geometry_shadows != enabled {
            self.geometry_shadows = enabled;
            self.haze_history_valid = false;
        }
    }

    /// Number of fixtures with resident shadow maps on the last frame.
    #[must_use]
    pub fn shadowed_fixture_count(&self) -> usize {
        self.fixture_shadow_slots.iter().flatten().count()
    }

    /// What the fixture-shadow passes submitted on the last frame.
    #[must_use]
    pub fn shadow_stats(&self) -> ShadowStats {
        self.shadow_stats
    }

    /// Shadow slots read from, written to, or bypassing the lit-interval
    /// cache on the last frame.
    #[must_use]
    pub fn interval_cache_stats(&self) -> crate::interval_cache::IntervalCacheStats {
        self.interval_cache.stats()
    }

    /// What the residual compaction did on the last planned frame and its
    /// most recent counter readback.
    #[must_use]
    pub fn compact_stats(&self) -> CompactStats {
        self.compact_stats
    }

    /// Explicit, timing-ineligible readback of the opt-in fog visibility
    /// cache's allocation, fallback, and raw-response counters.
    pub fn fog_visibility_cache_stats(&self) -> anyhow::Result<crate::FogVisibilityCacheStats> {
        self.fog_visibility
            .read_stats(&self.gpu.device, &self.gpu.queue)
    }

    /// The device this renderer draws through, shared with every other.
    ///
    /// Deliberately the whole [`Gpu`] rather than a forwarded
    /// `adapter_profile()`: a method here that only reads a field there is a
    /// pass-through, and it would have to grow a twin for every fact about the
    /// device a caller ever wants.
    #[must_use]
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// [`Self::render`], reading back into a caller-owned buffer in a caller-
    /// chosen channel order.
    ///
    /// Both exist because a viewport draws sixty of these a second: `out` is
    /// reused rather than reallocated, and the swizzle a presentation layer
    /// would otherwise do in a second pass over 25 MB happens inside the one
    /// copy that was already walking the rows.
    ///
    /// # Errors
    /// Fails if the readback buffer cannot be mapped.
    pub(crate) fn render_into(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        channels: Channels,
        out: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        let mut pending = self.submit_readback(
            frame,
            width,
            height,
            subframes,
            Destination::Bytes(channels),
            0,
            false,
            false,
        );
        let completed = pending.complete_blocking(&self.gpu.device)?;
        *out = completed
            .image
            .into_pixels()
            .expect("a Bytes destination reads its pixels back");
        Ok(())
    }

    pub(crate) fn render_live_into(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        channels: Channels,
        out: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        let mut pending = self.submit_readback(
            frame,
            width,
            height,
            subframes,
            Destination::Bytes(channels),
            0,
            true,
            false,
        );
        let completed = pending.complete_blocking(&self.gpu.device)?;
        *out = completed
            .image
            .into_pixels()
            .expect("a Bytes destination reads its pixels back");
        Ok(())
    }

    pub(crate) fn submit_live(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        slot: usize,
        measure: bool,
        queued: Duration,
    ) -> PendingFrame {
        let mut pending = self.submit_readback(
            frame,
            width,
            height,
            subframes,
            Destination::Compositor,
            slot,
            true,
            measure,
        );
        // Profiling is diagnostic in the interactive viewport. A driver may
        // occasionally return an incomplete timestamp set while several GPU
        // test/device streams are active; that must not discard an otherwise
        // valid rendered frame. The dedicated profiler remains strict.
        if let Some(profile) = &mut pending.profile {
            profile.strict_timestamps = false;
        }
        pending.queued = queued;
        pending
    }

    pub(crate) fn poll_live(&self) -> anyhow::Result<()> {
        self.gpu.device.poll(wgpu::PollType::Poll)?;
        Ok(())
    }

    fn submit_readback(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        destination: Destination,
        slot: usize,
        temporal: bool,
        measure: bool,
    ) -> PendingFrame {
        assert!(
            slot < PRESENTATION_SLOTS,
            "presentation slot is bounded to the target count"
        );
        self.haze_work_counts_valid = false;
        self.fog_blocks_valid = false;
        let channels = destination.channels();
        let started = Instant::now();
        // Created before the first upload rather than before the first pass:
        // every `storage` upload records a staging-belt copy into this
        // encoder, and a copy is ordered before the passes that read it by
        // being recorded first.
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let profile_resources = self.profiler.as_ref().filter(|_| measure).map(|profile| {
            let resources = &profile.slots[slot];
            (
                resources.query_set.clone(),
                resources.resolve.clone(),
                resources.readback.clone(),
                profile.timestamp_period_ns,
            )
        });
        let mut pass_queries = crate::pass_profile::PassQueries::new(
            profile_resources
                .as_ref()
                .filter(|(q, ..)| q.count() > QUERY_COUNT)
                .map(|(q, ..)| q),
        );
        let camera_far = if frame.sky.is_some() {
            crate::atmosphere::MAX_DISTANCE_M
        } else {
            CAMERA_FAR
        };
        let aspect = width as f32 / height as f32;
        // Passing the bounds in reverse order produces a finite reverse-Z
        // projection: near maps to one and the bounded far plane maps to zero.
        let proj = Mat4::perspective_rh(
            frame.camera.fov_y_deg.to_radians(),
            aspect,
            camera_far,
            CAMERA_NEAR,
        );
        let view = Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z);
        let view_proj = proj * view;

        let camera_forward = (frame.camera.target - frame.camera.eye).normalize_or(Vec3::Y);
        let light_view_proj = frame
            .directional
            .map_or([Mat4::IDENTITY; CASCADE_COUNT], |light| {
                cascade_matrices(
                    frame.camera.eye,
                    camera_forward,
                    frame.camera.fov_y_deg.to_radians(),
                    aspect,
                    light.direction,
                )
            });

        let point_lights: Vec<PointLightGpu> = frame
            .point_lights
            .iter()
            .filter(|l| l.intensity > 0.0)
            .map(|l| PointLightGpu {
                position: l.position.extend(l.cutoff_distance).to_array(),
                color: (l.color * l.intensity).extend(0.0).to_array(),
            })
            .collect();

        let fixture_cones: Vec<_> = frame
            .fixture_cones
            .iter()
            .take(crate::frame::MAX_FIXTURE_CONES)
            .map(sanitize_fixture_cone)
            .collect();
        let opaque = frame.draws.len() - frame.transparent.len();
        let caster_hash = fixture_shadow_caster_hash(frame, opaque);
        let cached_shadows =
            (self.geometry_shadows || frame.geometry_shadows) && !self.visibility_reference;
        let shadow_capacity = frame
            .fixture_shadow_capacity_hint
            .min(crate::frame::MAX_FIXTURE_CONES)
            .max(fixture_cones.len())
            .next_power_of_two()
            .max(MAX_FIXTURE_SHADOWS);
        if cached_shadows
            && frame.fixture_shadows
            && self.fixture_shadow_cache.len() < shadow_capacity
        {
            // Two banks stay inside the default 256-array-layer device limit,
            // including devices shared with GPUI's compositor.
            let (first, mut layers) =
                fixture_shadow_texture_array(&self.gpu.device, shadow_capacity.min(256) as u32);
            let extra_count = shadow_capacity.saturating_sub(256);
            let (second, extra) =
                fixture_shadow_texture_array(&self.gpu.device, extra_count.max(1) as u32);
            if extra_count > 0 {
                layers.extend(extra);
            }
            self.shadow_hierarchy = crate::shadow_hierarchy::Targets::new(
                &self.gpu.device,
                FIXTURE_SHADOW_SIZE,
                shadow_capacity,
            );
            self.fixture_shadow_map = first;
            self.fixture_shadow_map_extra = second;
            self.fixture_shadow_layers = layers;
            self.fixture_shadow_cache = vec![None; shadow_capacity];
            self.fixture_shadow_slots = vec![None; shadow_capacity];
        }
        // Full mode retains every projection; the legacy comparison keeps
        // its priority-based 16-map selection.
        let shadow_slots = if frame.fixture_shadows {
            if cached_shadows {
                crate::shadow::assign_cached_slots(
                    &fixture_cones,
                    &self.fixture_shadow_cache,
                    caster_hash,
                )
            } else {
                let previous = std::array::from_fn(|i| self.fixture_shadow_slots[i]);
                assign_shadow_slots(&fixture_cones, frame.camera.eye, &previous).to_vec()
            }
        } else {
            vec![None; self.fixture_shadow_cache.len()]
        };
        self.fixture_shadow_slots = shadow_slots.clone();
        let fixture_shadow_count = shadow_slots.iter().filter(|slot| slot.is_some()).count();
        let cores: Vec<LightCore> = fixture_cones
            .iter()
            .map(|light| LightCore {
                position: light.position.to_array(),
                range: light.range.clamp(0.05, 100.0),
            })
            .collect();
        let mut slot_of = vec![-1.0_f32; fixture_cones.len()];
        for (slot, resident) in shadow_slots.iter().enumerate() {
            if let Some(index) = resident {
                slot_of[*index] = slot as f32;
            }
        }
        let rests: Vec<LightRest> = fixture_cones
            .iter()
            .enumerate()
            .map(|(index, light)| {
                let direction = light.direction.try_normalize().unwrap_or(Vec3::NEG_Y);
                let helper = if direction.z.abs() > 0.98 {
                    Vec3::Y
                } else {
                    Vec3::Z
                };
                let field = light.cos_field.clamp(-1.0, 1.0);
                LightRest {
                    direction: direction.to_array(),
                    cos_beam: light.cos_beam.clamp(-1.0, 1.0),
                    color: light.color.to_array(),
                    intensity: light.intensity.clamp(0.0, 100.0),
                    cos_field: light.cos_field.clamp(-1.0, 1.0),
                    wash: light.wash.clamp(0.0, 1.0),
                    gobo: light.gobo.min(2) as f32,
                    gobo_rotation: light.gobo_rotation.rem_euclid(std::f32::consts::TAU),
                    shadow_slot: slot_of[index],
                    haze_gain: light.haze_gain.clamp(0.0, 1.0),
                    inverse_right_length: direction.cross(helper).length().recip(),
                    field_tangent: (1.0 - field * field).max(0.0).sqrt() / field.max(0.05),
                }
            })
            .collect();
        let fixture_shadow_matrices: Vec<FixtureShadowMatrix> = shadow_slots
            .iter()
            .map(|resident| match resident {
                Some(index) => {
                    let cone = &fixture_cones[*index];
                    let (near, far) = fixture_shadow_planes(cone);
                    FixtureShadowMatrix {
                        view_proj: fixture_shadow_matrix(cone).to_cols_array_2d(),
                        params: [near, far, 0.0, 0.0],
                    }
                }
                None => FixtureShadowMatrix {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                    params: [0.1, 1.0, 0.0, 0.0],
                },
            })
            .collect();
        // Build the unified light index for this frame: sanitise + depth-sort +
        // Z-bins on the CPU, tile masks in two compute dispatches, and the
        // reordered light SoA upload. Rebuilt every frame — no cache, no key:
        // a camera-derived key is unsound for a static camera with moving
        // lights, and the build is two small dispatches plus a 512-cone sort.
        let cluster_started = Instant::now();
        let light_index_bg = self
            .light_index
            .build(
                &self.gpu.light_index_pipelines,
                &self.gpu.device,
                &self.gpu.queue,
                &mut encoder,
                &LightIndexInput {
                    cones: &fixture_cones,
                    camera: frame.camera,
                    viewport: [width, height],
                    near: CAMERA_NEAR,
                    far: camera_far,
                },
                &cores,
                &rests,
                pass_queries.compute(
                    "light-index",
                    profile_resources.as_ref().map(|(queries, ..)| {
                        wgpu::ComputePassTimestampWrites {
                            query_set: queries,
                            beginning_of_pass_write_index: Some(4),
                            end_of_pass_write_index: Some(5),
                        }
                    }),
                ),
            )
            .clone();
        let clusters_done = Instant::now();
        let cpu_cluster = clusters_done - cluster_started;

        let haze_density = frame
            .haze_density
            .is_finite()
            .then_some(frame.haze_density.clamp(0.0, 4.0))
            .unwrap_or(0.0);
        let haze_time = frame.time.is_finite().then_some(frame.time).unwrap_or(0.0);
        let medium =
            crate::medium::Uniform::new(frame, haze_density * Transport::EXTINCTION, haze_time);
        let outdoor_sun = frame
            .sky
            .map_or(Vec3::ZERO, |sky| sky.sun_radiance)
            .extend(Transport::PHASE_G)
            .to_array();
        // Transport quality is a render setting, independent of how many
        // emitters a dimmer cue happens to leave active.
        let sampled_haze = cached_shadows && self.wide_light_group > 1;
        // Captures accumulating several subframes retain full per-ray detail.
        let grid_fog =
            sampled_haze && self.grid_fog && haze_density >= 0.001 && (temporal || subframes <= 1);
        // The far-field grid's radial extent: the haze uniform's `shadow.w`,
        // and the surface pass's `surface_fog.x` when it reads that grid.
        let fog_far = fixture_cones
            .iter()
            .map(|c| frame.camera.eye.distance(c.position) + c.range)
            .fold(1.0_f32, f32::max)
            .min(camera_far);
        let (medium, fog_far, venue_domain_selected) = select_fixture_lighting_domain(
            self.gpu.venue_lighting_domain,
            frame,
            fixture_cones.is_empty(),
            medium,
            fog_far,
            camera_far,
        );
        self.compact_stats.venue_domain_selected = venue_domain_selected;
        // The immutable diagnostic domain remains an independent explicit
        // override for causal A/B. Both selectors reject non-enclosing bounds.
        let (medium, fog_far) = if fixture_cones.is_empty() {
            (medium, fog_far)
        } else {
            self.gpu
                .diagnostic_lighting_domain
                .map_or((medium, fog_far), |domain| {
                    domain.select(frame, medium, fog_far, camera_far)
                })
        };
        self.compact_stats.selected_domain_words = [
            medium.min[0].to_bits(),
            medium.min[1].to_bits(),
            medium.min[2].to_bits(),
            medium.max[0].to_bits(),
            medium.max[1].to_bits(),
            medium.max[2].to_bits(),
            fog_far.to_bits(),
        ];
        // The grid only exists on frames that run the fog chain; otherwise
        // every mode marches, and the scene pass keeps its usual slot.
        let surface_mode = if grid_fog {
            self.surface_transmittance.shader_mode()
        } else {
            SurfaceTransmittance::March.shader_mode()
        };
        let scene_after_fog = surface_mode[0] > 0.0;
        let globals = Globals {
            surface_fog: [fog_far, surface_mode[0], surface_mode[1], 0.0],
            viewport: [
                width as f32,
                height as f32,
                1.0 / width as f32,
                1.0 / height as f32,
            ],
            medium,
            outdoor_sun,
            view_proj: view_proj.to_cols_array_2d(),
            light_view_proj: light_view_proj.map(|matrix| matrix.to_cols_array_2d()),
            camera_pos: frame.camera.eye.extend(1.0).to_array(),
            camera_forward: camera_forward.extend(0.0).to_array(),
            cascade_splits: [
                CASCADE_SPLITS[0],
                CASCADE_SPLITS[1],
                CASCADE_SPLITS[2],
                CASCADE_BLEND,
            ],
            ambient: frame.ambient.extend(0.0).to_array(),
            room: frame.room.map_or([0.0; 4], |room| {
                [room.centre.x, room.centre.y, room.half.x, room.half.y]
            }),
            room_falloff: [frame.room.map_or(0.0, |room| room.margin), 0.0, 0.0, 0.0],
            dir_to_light: frame
                .directional
                .map_or(Vec4::ZERO, |light| light.direction.extend(1.0))
                .to_array(),
            dir_color: frame
                .directional
                .map_or(Vec4::ZERO, |light| {
                    light.radiance.extend(light.shadow_softness)
                })
                .to_array(),
            params: [
                point_lights.len() as f32,
                1.0 / SHADOW_SIZE as f32,
                f32::from(u8::from(
                    frame.directional.is_some_and(|light| light.shadows),
                )),
                frame.debug_view.shader_code() as f32,
            ],
        };
        if self.environment.prepare(
            &self.gpu.environment,
            &self.gpu.device,
            &self.gpu.queue,
            frame.environment.as_ref(),
        ) {
            self.upload_stats.environments += 1;
        }
        let ambient_buf = self.environment.ambient(&self.gpu.device);
        let mut ambient_probe = self.environment.irradiance(&self.gpu.environment).clone();
        let (_environment_uniform, mut environment_bg) = self.environment.bind_group(
            &self.gpu.environment,
            &self.gpu.device,
            frame.environment.as_ref(),
            &ambient_buf,
        );
        // The sky, when there is one, is both the background the composite
        // resolves and the probe the scene pass is lit by. Its tables are
        // rebuilt only when the sun moves, so this is a cache hit for every
        // frame of a fixed hour.
        let (sky_bg, sky_probe) = self.atmosphere.prepare(
            &self.gpu.atmosphere,
            &self.gpu.environment,
            &self.gpu.device,
            &mut encoder,
            frame.sky.as_ref(),
        );
        let aerial = self.atmosphere.prepare_aerial(
            &self.gpu.atmosphere,
            &self.gpu.device,
            &mut encoder,
            frame.sky.as_ref(),
            frame.camera.eye.z,
            &mut pass_queries,
        );
        if let Some(probe) = &sky_probe {
            environment_bg =
                self.gpu
                    .environment
                    .sky_bind_group(&self.gpu.device, probe, &ambient_buf);
            ambient_probe = probe.0.clone();
        }
        // Frame-constant probe mean, computed once here instead of by six cube
        // samples in every surface and composite fragment. Encoded after the
        // probe it reads and before every pass that reads the result.
        self.gpu.environment.dispatch_ambient(
            &self.gpu.device,
            &mut encoder,
            &ambient_probe,
            &ambient_buf,
        );

        // --- resident geometry ----------------------------------------------
        // Frame assembly is intentionally cheap and ephemeral, but the meshes
        // it names are immutable. Upload the combined bank only when those
        // stable identities change (normally a venue/asset change), never for
        // camera, transport or fixture-state updates.
        if self
            .geometry
            .as_ref()
            .is_none_or(|geometry| !geometry.matches(frame))
        {
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            let mut ranges = Vec::new();
            let mut bounds = Vec::new();
            for mesh in &frame.meshes {
                bounds.push(local_bounding_sphere(&mesh.vertices));
                let base_vertex = vertices.len() as i32;
                let first_index = indices.len() as u32;
                vertices.extend_from_slice(&mesh.vertices);
                indices.extend(mesh.indices.iter().copied());
                ranges.push((first_index, indices.len() as u32, base_vertex));
            }
            let geometry = ResidentGeometry {
                keys: frame.meshes.iter().map(|mesh| mesh.key.clone()).collect(),
                vertices: self.immutable(&vertices, wgpu::BufferUsages::VERTEX, "vertices"),
                indices: self.immutable(&indices, wgpu::BufferUsages::INDEX, "indices"),
                ranges,
                bounds,
            };
            self.geometry = Some(geometry);
            self.upload_stats.geometry += 1;
        }
        let geometry = self.geometry.as_ref().expect("frame geometry uploaded");
        let vertex_buf = geometry.vertices.clone();
        let index_buf = geometry.indices.clone();
        let ranges = geometry.ranges.clone();
        let mesh_bounds = geometry.bounds.clone();

        let instances: Vec<Instance> = frame.draws.iter().map(instance_of).collect();
        let overlay_instances: Vec<OverlayInstance> = frame
            .overlays
            .iter()
            .map(|o| OverlayInstance {
                model: o.model.to_cols_array_2d(),
                color: o.color.extend(o.opacity).to_array(),
            })
            .collect();
        // Upload each immutable image once per color-space role. A source can
        // legitimately be used as both base color (sRGB) and a data map
        // (linear), and those require different GPU formats.
        let material_keys: Vec<MaterialKey> = frame
            .draws
            .iter()
            .map(|draw| MaterialKey::of(draw, frame))
            .collect();
        for (draw, key) in frame.draws.iter().zip(&material_keys) {
            let roles = [
                (draw.textures.base_color, key.base_color.as_ref()),
                (draw.textures.normal, key.normal.as_ref()),
                (
                    draw.textures.metallic_roughness,
                    key.metallic_roughness.as_ref(),
                ),
                (draw.textures.occlusion, key.occlusion.as_ref()),
                (draw.textures.emissive, key.emissive.as_ref()),
            ];
            for (index, texture_key) in roles {
                let Some((index, texture_key)) = index.zip(texture_key) else {
                    continue;
                };
                if !self.texture_views.contains_key(texture_key) {
                    let view = upload_texture_view(
                        &self.gpu.device,
                        &self.gpu.queue,
                        &frame.images[index].image,
                        texture_key.encoding,
                    );
                    self.texture_views.insert(texture_key.clone(), view);
                    self.upload_stats.textures += 1;
                }
            }
        }
        let used_textures: HashSet<&TextureKey> = material_keys
            .iter()
            .flat_map(MaterialKey::textures)
            .collect();
        self.texture_views
            .retain(|key, _| used_textures.contains(key));
        self.materials
            .retain(|key, _| material_keys.iter().any(|used| used == key));
        for key in &material_keys {
            if self.materials.contains_key(key) {
                continue;
            }
            let view = |texture: Option<&TextureKey>, default| {
                texture.map_or(default, |key| &self.texture_views[key])
            };
            let bind_group = material_bind_group(
                &self.gpu.device,
                &self.gpu.material_layout,
                &self.gpu.texture_sampler,
                view(
                    key.base_color.as_ref(),
                    &self.gpu.material_defaults.base_color,
                ),
                view(key.normal.as_ref(), &self.gpu.material_defaults.normal),
                view(
                    key.metallic_roughness.as_ref(),
                    &self.gpu.material_defaults.metallic_roughness,
                ),
                view(
                    key.occlusion.as_ref(),
                    &self.gpu.material_defaults.occlusion,
                ),
                view(key.emissive.as_ref(), &self.gpu.material_defaults.emissive),
            );
            self.materials.insert(key.clone(), bind_group);
        }
        let materials: Vec<wgpu::BindGroup> = material_keys
            .iter()
            .map(|key| self.materials[key].clone())
            .collect();

        let instance_buf = self.storage(
            &mut encoder,
            &pad_at_least_one(instances),
            wgpu::BufferUsages::STORAGE,
            "instances",
        );
        let point_buf = self.storage(
            &mut encoder,
            &pad_at_least_one(point_lights),
            wgpu::BufferUsages::STORAGE,
            "point-lights",
        );
        let globals_buf = self.storage(
            &mut encoder,
            &[globals],
            wgpu::BufferUsages::UNIFORM,
            "globals",
        );
        let fixture_shadow_matrix_buf = self.storage(
            &mut encoder,
            &pad_at_least_one(fixture_shadow_matrices.clone()),
            wgpu::BufferUsages::STORAGE,
            "fixture-shadow-matrices",
        );
        // Avoid an extra geometry pass for short light lists.
        let surface_depth_cull = self.surface_depth_cull
            && frame.fixture_surface_lighting
            && self.light_index.stats().lights_on_screen >= 32;
        let surface_split = if surface_depth_cull {
            self.gpu.light_index_pipelines.surface_split() as f32
        } else {
            0.0
        };
        let cluster_uniform = self.storage(
            &mut encoder,
            &[SurfaceClusterUniform {
                flags: [
                    f32::from(u8::from(frame.fixture_surface_lighting)),
                    f32::from(u8::from(frame.cluster_debug)),
                    f32::from(u8::from(surface_depth_cull)),
                    surface_split,
                ],
                shadow: [
                    fixture_shadow_count as f32,
                    1.0 / FIXTURE_SHADOW_SIZE as f32,
                    // A cone's `intensity` is a 0..1 dimmer times its optic
                    // gain, not radiance — the absolute scale is the transport
                    // beam gain, and the surface pass must apply the *same*
                    // scale the haze march does or the two halves of one beam
                    // disagree about how bright it is (bright shaft over a
                    // black pool).
                    Transport::BEAM_GAIN,
                    0.0,
                ],
            }],
            wgpu::BufferUsages::UNIFORM,
            "surface-cluster-uniform",
        );
        self.visibility.update(
            &self.gpu.device,
            frame,
            self.visibility_reference
                && (self.geometry_shadows || frame.geometry_shadows)
                && frame.fixture_shadows,
        );
        let index_bindings = self.light_index.bindings();
        let overlay_buf = self.storage(
            &mut encoder,
            &pad_at_least_one(overlay_instances),
            wgpu::BufferUsages::STORAGE,
            "overlays",
        );
        let overlay_bg = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("overlay"),
                layout: &self.gpu.overlay_layout,
                entries: &[
                    binding(0, globals_buf.as_entire_binding()),
                    binding(1, overlay_buf.as_entire_binding()),
                ],
            });

        let hard_shadows = frame
            .directional
            .is_some_and(|light| light.shadow_softness == 0.0);
        let lit_bg = self.scene_bind_group(
            &globals_buf,
            &instance_buf,
            &point_buf,
            true,
            hard_shadows,
            &aerial,
        );
        let unlit_bg = self.scene_bind_group(
            &globals_buf,
            &instance_buf,
            &point_buf,
            false,
            false,
            &aerial,
        );
        let shadow_bgs: Vec<_> = light_view_proj
            .iter()
            .enumerate()
            .map(|(cascade, matrix)| {
                let mut shadow_globals = globals;
                shadow_globals.light_view_proj[0] = matrix.to_cols_array_2d();
                // Indexed label: one pooled buffer per cascade — see `storage`.
                let buffer = self.storage(
                    &mut encoder,
                    &[shadow_globals],
                    wgpu::BufferUsages::UNIFORM,
                    &format!("shadow-globals-{cascade}"),
                );
                (
                    buffer.clone(),
                    self.scene_bind_group(
                        &buffer,
                        &instance_buf,
                        &point_buf,
                        false,
                        false,
                        &aerial,
                    ),
                )
            })
            .collect();
        let shadow_globals_started = Instant::now();
        let fixture_shadow_keys: Vec<_> = fixture_shadow_matrices
            .iter()
            .map(|matrix| ShadowCacheKey {
                matrix_bits: shadow_matrix_bits(&matrix.view_proj),
                caster_hash,
            })
            .collect();
        let fixture_shadow_dirty: Vec<_> = fixture_shadow_keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                // Vacant slots retain their previous depth and cache key for
                // reuse. Their placeholder matrix does not invalidate either
                // the map or its hierarchy: no light reads that slot now.
                shadow_slots[index].is_some() && self.fixture_shadow_cache[index] != Some(*key)
            })
            .collect();
        // Only a map that is about to be redrawn needs a projection uniform and
        // a bind group. Building all 128 regardless was most of this frame's
        // encode time on the shadowed preset.
        let fixture_shadow_bgs: Vec<_> = fixture_shadow_matrices
            .iter()
            .zip(&fixture_shadow_dirty)
            .enumerate()
            .map(|(slot, (matrix, dirty))| {
                dirty.then(|| {
                    let mut shadow_globals = globals;
                    shadow_globals.light_view_proj[0] = matrix.view_proj;
                    // Indexed label: one pooled buffer per shadow slot.
                    let buffer = self.storage(
                        &mut encoder,
                        &[shadow_globals],
                        wgpu::BufferUsages::UNIFORM,
                        &format!("fixture-shadow-globals-{slot}"),
                    );
                    buffer
                })
            })
            .collect();

        let mut shadow_cpu = FixtureShadowCpuSpans {
            globals: shadow_globals_started.elapsed(),
            ..Default::default()
        };

        let (t_width, t_height) = (width, height);
        // Below a quarter the bilateral upsample has too few taps per output
        // pixel to anchor on and beams start to crawl; above native there is
        // nothing left to gain. `max(1)` covers an element only a few pixels
        // tall, where the ratio alone would round the target away.
        let scale = frame.haze_resolution.clamp(0.25, 1.0);
        let haze_size = (
            ((width as f32 * scale).round() as u32).max(1),
            ((height as f32 * scale).round() as u32).max(1),
        );
        let targets_started = Instant::now();
        let (
            msaa_color,
            msaa_surface_depth,
            msaa_depth,
            scene_view,
            depth_view,
            haze_view,
            haze_sampled,
            haze_work_counts,
            haze_work_counts_compact,
            fog_grid,
            fog_integral,
            fog_transmittance,
            fog_tau,
            fog_columns,
            fog_candidates,
            haze_history,
            output_view,
            finish,
            bytes_per_row,
        ) = {
            let t = self.targets(
                t_width,
                t_height,
                haze_size,
                destination,
                grid_fog,
                surface_depth_cull,
            );
            let presentation = &t.presentations[slot];
            let finish = match presentation {
                PresentationTarget::Staged {
                    output, readback, ..
                } => Finish::Copy(output.clone(), readback.clone()),
                PresentationTarget::Shared(shared) => Finish::Share(shared.surface()),
            };
            (
                t.msaa_color.clone(),
                t.msaa_surface_depth.clone(),
                t.msaa_depth.clone(),
                t.scene.clone(),
                t.depth.clone(),
                t.haze.clone(),
                t.haze_sampled.clone(),
                t.haze_work_counts.clone(),
                t.haze_work_counts_compact.clone(),
                t.fog.incident.clone(),
                t.fog.integral.clone(),
                t.fog.transmittance.clone(),
                t.fog.tau.clone(),
                t.fog.columns.clone(),
                t.fog.candidates.clone(),
                t.haze_history.clone(),
                presentation.view().clone(),
                finish,
                t.bytes_per_row,
            )
        };
        let targets_done = Instant::now();
        // Built after the targets: binding 13 is this frame's fog prefix.
        let cluster_bg = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("surface-clusters"),
                layout: &self.gpu.cluster_layout,
                entries: &[
                    binding(0, index_bindings.core.as_entire_binding()),
                    binding(1, index_bindings.rest.as_entire_binding()),
                    binding(4, cluster_uniform.as_entire_binding()),
                    binding(5, fixture_shadow_matrix_buf.as_entire_binding()),
                    binding(
                        6,
                        wgpu::BindingResource::TextureView(&self.fixture_shadow_map),
                    ),
                    binding(7, wgpu::BindingResource::Sampler(&self.gpu.linear_sampler)),
                    binding(8, index_bindings.params.as_entire_binding()),
                    binding(9, index_bindings.tile_masks.as_entire_binding()),
                    binding(10, index_bindings.z_bins.as_entire_binding()),
                    binding(11, self.visibility.buffer.as_entire_binding()),
                    binding(
                        12,
                        wgpu::BindingResource::TextureView(&self.fixture_shadow_map_extra),
                    ),
                    binding(13, wgpu::BindingResource::TextureView(&fog_transmittance)),
                    binding(14, index_bindings.surface_splits.as_entire_binding()),
                ],
            });
        let history_key =
            haze_history_key(frame, width, height, haze_size, haze_density, caster_hash);
        let time_continuous = self
            .last_live_time
            .is_some_and(|previous| (-0.01..=0.25).contains(&(frame.time - previous)));
        let history_valid = temporal
            && self.haze_history_valid
            && time_continuous
            && self.haze_history_key.as_ref() == Some(&history_key);
        let noise_seed = if temporal && history_valid {
            self.live_noise_frame
        } else {
            0
        };
        if temporal {
            self.haze_history_key = Some(history_key);
            self.last_live_time = Some(frame.time);
            self.live_noise_frame = if history_valid {
                self.live_noise_frame.wrapping_add(1)
            } else {
                1
            };
        } else {
            self.haze_history_valid = false;
            self.haze_history_key = None;
            self.last_live_time = None;
        }
        // This block runs to the end of the function: `encode_scene` below
        // captures its draw closures and is called either here or, when the
        // surface pass reads the fog grid, after `fog-integrate`.
        {
            let mut pending_start = profile_resources.as_ref().map(|(queries, ..)| queries);
            let all_opaque: Vec<usize> = (0..opaque).collect();
            let transparent: Vec<usize> = (opaque..frame.draws.len()).collect();
            // Which casters each shadow map actually needs.
            //
            // A fixture lights a cone a few metres long; the venue around it is
            // not in that cone and contributes nothing to its depth map. Drawing
            // all of it anyway made the shadow passes the whole frame — at a
            // realistic draw count they submitted 16 x every opaque draw, and
            // cost more than everything else put together.
            //
            // Only computed for maps that are about to render: a clean map
            // keeps the caster set it was drawn with, because the same key that
            // says the depth is still valid says the caster set is too. A moved
            // head is dirty by definition, so its set is never stale.
            let has_fixture_shadow_pass = fixture_shadow_dirty.iter().any(|dirty| *dirty);
            let shadow_cull_started = Instant::now();
            // A draw's world-space sphere is shared by every light in this
            // submission. Transform it once, using the same largest-axis
            // scale as the cone test used per light. Rebuild on each dirty
            // frame so moved or rescaled casters cannot leave stale bounds.
            let caster_bounds: Vec<_> = if has_fixture_shadow_pass {
                frame.draws[..opaque]
                    .iter()
                    .map(|draw| {
                        let (local, radius) = mesh_bounds[draw.mesh];
                        let scale = draw
                            .model
                            .to_scale_rotation_translation()
                            .0
                            .abs()
                            .max_element();
                        (draw.model.transform_point3(local), radius * scale)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let shadow_casters: Vec<Vec<usize>> = shadow_slots
                .iter()
                .zip(&fixture_shadow_dirty)
                .map(|(resident, dirty)| {
                    let Some(cone) = resident
                        .filter(|_| *dirty)
                        .map(|index| &fixture_cones[index])
                    else {
                        return Vec::new();
                    };
                    let direction = cone.direction.try_normalize().unwrap_or(Vec3::NEG_Z);
                    all_opaque
                        .iter()
                        .copied()
                        .filter(|&index| {
                            let (center, radius) = caster_bounds[index];
                            crate::light_index::cone_reaches_sphere(
                                cone.position,
                                direction,
                                cone.range,
                                cone.cos_field,
                                center,
                                radius,
                            )
                        })
                        .collect()
                })
                .collect();
            shadow_cpu.cull = shadow_cull_started.elapsed();
            let shadow_buckets_started = Instant::now();
            let redrawn_maps = shadow_casters
                .iter()
                .zip(&fixture_shadow_bgs)
                .filter(|(_, prepared)| prepared.is_some())
                .count();
            self.shadow_stats = ShadowStats {
                redrawn_maps,
                hierarchy_layers: 0,
                caster_draws: shadow_casters
                    .iter()
                    .zip(&fixture_shadow_bgs)
                    .filter(|(_, prepared)| prepared.is_some())
                    .map(|(casters, _)| casters.len())
                    .sum(),
                unculled_draws: redrawn_maps * opaque,
                // Filled below once the buckets exist.
                mesh_draws: 0,
            };
            // One instanced draw per (map, distinct mesh): bucket each map's
            // casters by mesh and concatenate the draw indices into one
            // storage buffer the fixture-shadow vertex stage indexes through
            // `instance_index`. Fixture bodies are excluded here (they used
            // to be skipped at draw time): a luminaire sits at the apex of
            // its own cone and would shadow every sample.
            let mut caster_instance_data: Vec<u32> = Vec::new();
            let shadow_mesh_buckets: Vec<Vec<(usize, u32, u32)>> = shadow_casters
                .iter()
                .map(|casters| {
                    let mut sorted: Vec<usize> = casters
                        .iter()
                        .copied()
                        .filter(|&index| {
                            !matches!(
                                &frame.draws[index].editor_object,
                                Some(crate::frame::EditorObject::Fixture(_))
                            )
                        })
                        .collect();
                    // Mesh-major, then draw order, so the buffer contents are
                    // reproducible across frames.
                    sorted.sort_by_key(|&index| (frame.draws[index].mesh, index));
                    let mut buckets = Vec::new();
                    let mut cursor = 0;
                    while cursor < sorted.len() {
                        let mesh = frame.draws[sorted[cursor]].mesh;
                        let first = caster_instance_data.len() as u32;
                        let mut count = 0;
                        while cursor < sorted.len() && frame.draws[sorted[cursor]].mesh == mesh {
                            caster_instance_data.push(sorted[cursor] as u32);
                            count += 1;
                            cursor += 1;
                        }
                        buckets.push((mesh, first, count));
                    }
                    buckets
                })
                .collect();
            let mesh_draws = shadow_mesh_buckets
                .iter()
                .map(|buckets| buckets.len())
                .sum();
            self.shadow_stats.mesh_draws = mesh_draws;
            shadow_cpu.buckets = shadow_buckets_started.elapsed();
            let shadow_resources_started = Instant::now();
            let caster_instance_buf = self.storage(
                &mut encoder,
                &pad_at_least_one(caster_instance_data),
                wgpu::BufferUsages::STORAGE,
                "caster-instances",
            );
            let fixture_shadow_pass_bgs: Vec<Option<wgpu::BindGroup>> = fixture_shadow_bgs
                .iter()
                .map(|prepared| {
                    prepared.as_ref().map(|globals_buf| {
                        self.gpu
                            .device
                            .create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("fixture-shadow"),
                                layout: &self.gpu.fixture_shadow_layout,
                                entries: &[
                                    binding(0, globals_buf.as_entire_binding()),
                                    binding(1, instance_buf.as_entire_binding()),
                                    binding(5, caster_instance_buf.as_entire_binding()),
                                ],
                            })
                    })
                })
                .collect();
            shadow_cpu.resources = shadow_resources_started.elapsed();
            let draw_range = |pass: &mut wgpu::RenderPass, range: &[usize]| {
                pass.set_vertex_buffer(0, vertex_buf.slice(..));
                pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                let mut bound = None;
                for &i in range {
                    let draw = &frame.draws[i];
                    if bound != Some(&material_keys[i]) {
                        bound = Some(&material_keys[i]);
                        pass.set_bind_group(1, &materials[i], &[]);
                    }
                    let (first, last, base) = ranges[draw.mesh];
                    pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                }
            };
            // Depth shaders do not sample materials. Consecutive instances
            // of one mesh can share a draw without changing primitive order,
            // including the order of coplanar receivers in the MSAA pass.
            let draw_depth = |pass: &mut wgpu::RenderPass| {
                pass.set_vertex_buffer(0, vertex_buf.slice(..));
                pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                pass.set_bind_group(1, &self.gpu.white_material, &[]);
                let mut start = 0;
                while start < opaque {
                    let mesh = frame.draws[start].mesh;
                    let mut end = start + 1;
                    while end < opaque && frame.draws[end].mesh == mesh {
                        end += 1;
                    }
                    let (first, last, base) = ranges[mesh];
                    pass.draw_indexed(first..last, base, start as u32..end as u32);
                    start = end;
                }
            };

            if has_fixture_shadow_pass {
                let shadow_maps_started = Instant::now();
                for ((layer, prepared), buckets) in self
                    .fixture_shadow_layers
                    .iter()
                    .zip(&fixture_shadow_pass_bgs)
                    .zip(&shadow_mesh_buckets)
                {
                    // A slot has a bind group exactly when it is both occupied
                    // and dirty, so the two cannot disagree about which passes
                    // to encode. Slots are sparse — hysteresis leaves gaps —
                    // so occupancy is per slot, not a prefix count.
                    let Some(bind_group) = prepared else {
                        continue;
                    };

                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("fixture-shadow"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(depth_attachment(layer)),
                        timestamp_writes: pass_queries
                            .render("fixture-shadow", claim_start_timestamp(&mut pending_start)),
                        ..Default::default()
                    });
                    // An empty caster list still runs: the attachment's clear
                    // is what makes the map read as unoccluded, and skipping the
                    // pass would leave whatever the slot's previous tenant wrote.
                    pass.set_pipeline(&self.gpu.fixture_shadow_pipeline);
                    pass.set_bind_group(0, bind_group, &[]);
                    pass.set_vertex_buffer(0, vertex_buf.slice(..));
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(mesh, first, count) in buckets {
                        let (first_index, last_index, base) = ranges[mesh];
                        pass.draw_indexed(first_index..last_index, base, first..first + count);
                    }
                }
                shadow_cpu.maps = shadow_maps_started.elapsed();
                let shadow_hierarchy_started = Instant::now();
                self.shadow_stats.hierarchy_layers = self.shadow_hierarchy.record(
                    &self.gpu.shadow_hierarchy_pipelines,
                    &self.gpu.device,
                    &mut encoder,
                    [&self.fixture_shadow_map, &self.fixture_shadow_map_extra],
                    &fixture_shadow_dirty,
                    &mut pass_queries,
                );
                shadow_cpu.hierarchy = shadow_hierarchy_started.elapsed();
                for (index, key) in fixture_shadow_keys.iter().copied().enumerate() {
                    if fixture_shadow_dirty[index] {
                        self.fixture_shadow_cache[index] = Some(key);
                    }
                }
            }

            // A cascade's depth map only changes when its projection or its
            // casters do, exactly as for a fixture map. Redrawing all three
            // every frame was the sun path missing the check the fixture path
            // already had.
            let cascade_keys: [ShadowCacheKey; CASCADE_COUNT] =
                light_view_proj.map(|matrix| ShadowCacheKey {
                    matrix_bits: shadow_matrix_bits(&matrix.to_cols_array_2d()),
                    caster_hash,
                });
            if frame.directional.is_some_and(|light| light.shadows) {
                for (cascade, layer) in self.shadow_layers.iter().enumerate() {
                    if self.cascade_shadow_cache[cascade] == Some(cascade_keys[cascade]) {
                        continue;
                    }
                    self.cascade_shadow_cache[cascade] = Some(cascade_keys[cascade]);
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("shadow-cascade"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(depth_attachment(layer)),
                        timestamp_writes: pass_queries
                            .render("shadow-cascade", claim_start_timestamp(&mut pending_start)),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.shadow_pipeline);
                    pass.set_bind_group(0, &shadow_bgs[cascade].1, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_depth(&mut pass);
                }
            }

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("depth-prepass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(depth_attachment(&depth_view)),
                    timestamp_writes: pass_queries
                        .render("depth-prepass", claim_start_timestamp(&mut pending_start)),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.depth_pipeline);
                pass.set_bind_group(0, &unlit_bg, &[]);
                pass.set_bind_group(2, &environment_bg, &[]);
                pass.set_bind_group(3, &cluster_bg, &[]);
                draw_depth(&mut pass);
            }

            if surface_depth_cull {
                let msaa_surface_depth = msaa_surface_depth
                    .as_ref()
                    .expect("allocated for surface culling");
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("surface-depth"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: msaa_surface_depth,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(depth_attachment_transient(&msaa_depth)),
                        timestamp_writes: pass_queries.render("surface-depth", None),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.surface_depth_pipeline);
                    pass.set_bind_group(0, &lit_bg, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_depth(&mut pass);
                }
                self.light_index.refine_surface(
                    &self.gpu.light_index_pipelines,
                    &self.gpu.device,
                    &mut encoder,
                    msaa_surface_depth,
                    pass_queries.compute("surface-light-index", None),
                );
            }

            // The scene pass reads nothing the haze chain writes unless the
            // surface transmittance comes from the fog grid; then it is
            // encoded after `fog-transmittance` instead of here, still ahead
            // of the haze pass, so the legacy timestamp slots keep their order.
            let encode_scene = scene_encoder(|encoder, pass_queries, gpu, queries| {
                let repeat_count = gpu.profile_copies("scene");
                for repeat_index in 0..repeat_count {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("scene"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &msaa_color,
                            resolve_target: Some(&scene_view),
                            depth_slice: None,
                            // Premultiplied radiance and coverage, including each
                            // surface's horizon fade. Transparent draws blend over
                            // them before the composite supplies the background.
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(depth_attachment_transient(&msaa_depth)),
                        timestamp_writes: pass_queries.render(
                            "scene",
                            queries
                                .filter(|_| repeat_index + 1 == repeat_count)
                                .map(|queries| wgpu::RenderPassTimestampWrites {
                                    query_set: queries,
                                    beginning_of_pass_write_index: None,
                                    end_of_pass_write_index: Some(1),
                                }),
                        ),
                        ..Default::default()
                    });
                    pass.set_pipeline(&gpu.scene_pipeline);
                    pass.set_bind_group(0, &lit_bg, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_range(&mut pass, &all_opaque);
                    for (slot, kind) in frame.transparent.iter().enumerate() {
                        pass.set_pipeline(match kind {
                            crate::frame::Transparent::Grid => &gpu.grid_pipeline,
                            crate::frame::Transparent::Compass => &gpu.compass_pipeline,
                            crate::frame::Transparent::Cables => &gpu.cable_pipeline,
                        });
                        draw_range(&mut pass, &transparent[slot..=slot]);
                    }
                }
            });
            if !scene_after_fog {
                encode_scene(
                    &mut encoder,
                    &mut pass_queries,
                    &self.gpu,
                    profile_resources.as_ref().map(|(queries, ..)| queries),
                );
            }

            // --- haze ------------------------------------------------------------
            // The light index was built (and its SoA uploaded) before the scene
            // passes; the haze passes bind the same frame's index as group 1.
            let inv_view_proj = view_proj.inverse();
            let subframes = subframes.max(1);
            let weight = 1.0 / subframes as f32;
            let grid_prepare = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-prepare"),
                    layout: &self.gpu.fog_prepare_layout,
                    entries: &[binding(0, wgpu::BindingResource::TextureView(&fog_columns))],
                });
            let grid_classify = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-classify"),
                    layout: &self.gpu.fog_classify_layout,
                    entries: &[
                        binding(0, wgpu::BindingResource::TextureView(&fog_columns)),
                        binding(1, fog_candidates.as_entire_binding()),
                    ],
                });
            let grid_read = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-grid-read"),
                    layout: &self.gpu.fog_grid_read_layout,
                    entries: &[binding(
                        0,
                        wgpu::BindingResource::TextureView(&fog_integral),
                    )],
                });
            let grid_integrate = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-integrate"),
                    layout: &self.gpu.fog_integrate_layout,
                    entries: &[
                        binding(0, wgpu::BindingResource::TextureView(&fog_integral)),
                        binding(1, wgpu::BindingResource::TextureView(&fog_grid)),
                        binding(2, wgpu::BindingResource::TextureView(&fog_columns)),
                    ],
                });
            let grid_transmittance =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-transmittance"),
                        layout: &self.gpu.fog_tau_write_layout,
                        entries: &[
                            binding(0, wgpu::BindingResource::TextureView(&fog_transmittance)),
                            binding(1, wgpu::BindingResource::TextureView(&fog_grid)),
                            binding(2, wgpu::BindingResource::TextureView(&fog_columns)),
                            binding(3, wgpu::BindingResource::TextureView(&fog_tau)),
                        ],
                    });
            let grid_integrate_reuse =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-integrate-reuse"),
                        layout: &self.gpu.fog_tau_read_layout,
                        entries: &[
                            binding(0, wgpu::BindingResource::TextureView(&fog_integral)),
                            binding(1, wgpu::BindingResource::TextureView(&fog_grid)),
                            binding(2, wgpu::BindingResource::TextureView(&fog_columns)),
                            binding(3, wgpu::BindingResource::TextureView(&fog_tau)),
                        ],
                    });
            if self.gpu.fog_visibility.is_some() {
                let global = crate::fog_visibility_cache::GlobalKey {
                    inv_view_proj: shadow_matrix_bits(&inv_view_proj.to_cols_array_2d()),
                    camera_position: frame.camera.eye.to_array().map(f32::to_bits),
                    viewport: [
                        (haze_size.0 as f32).to_bits(),
                        (haze_size.1 as f32).to_bits(),
                    ],
                    grid_size: [
                        width.div_ceil(crate::fog_grid::tile_size()),
                        height.div_ceil(crate::fog_grid::tile_size()),
                        crate::fog_grid::SLICES,
                    ],
                    camera_planes: [CAMERA_NEAR.to_bits(), camera_far.to_bits()],
                    fog_far: fog_far.to_bits(),
                    medium_min: std::array::from_fn(|index| medium.min[index].to_bits()),
                    medium_max: std::array::from_fn(|index| medium.max[index].to_bits()),
                    tile_size: crate::fog_grid::tile_size(),
                    block_side: crate::fog_grid::BLOCK_SIDE,
                    slices: crate::fog_grid::SLICES,
                    algorithm_version: crate::fog_visibility_cache::ALGORITHM_VERSION,
                };
                let active_slots: Vec<_> = shadow_slots
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, resident)| {
                        resident.map(|_| {
                            let matrix = &fixture_shadow_matrices[slot];
                            (
                                slot as u32,
                                crate::fog_visibility_cache::ShadowContentKey {
                                    matrix: shadow_matrix_bits(&matrix.view_proj),
                                    near: matrix.params[0].to_bits(),
                                    far: matrix.params[1].to_bits(),
                                    caster_hash: fixture_shadow_keys[slot].caster_hash,
                                    format_version:
                                        crate::fog_visibility_cache::SHADOW_FORMAT_VERSION,
                                },
                            )
                        })
                    })
                    .collect();
                self.fog_visibility.prepare(
                    &self.gpu.device,
                    &mut encoder,
                    crate::fog_visibility_cache::DeviceLimits::from(self.gpu.device.limits()),
                    [width, height],
                    shadow_slots
                        .len()
                        .min(crate::fog_visibility_cache::MAX_SHADOW_SLOTS as usize)
                        as u32,
                    grid_fog && frame.fixture_shadows && cached_shadows && fixture_shadow_count > 0,
                    global,
                    &active_slots,
                );
            }
            let grid_write = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-grid-write"),
                    layout: &self.gpu.fog_grid_write_layout,
                    entries: &{
                        let mut entries = vec![
                            binding(0, wgpu::BindingResource::TextureView(&fog_grid)),
                            binding(1, wgpu::BindingResource::TextureView(&fog_columns)),
                            binding(2, fog_candidates.as_entire_binding()),
                        ];
                        if self.gpu.fog_visibility.is_some() {
                            entries.push(binding(
                                3,
                                self.fog_visibility.directory().as_entire_binding(),
                            ));
                            entries.push(binding(
                                4,
                                self.fog_visibility.payload().as_entire_binding(),
                            ));
                        } else if let Some(counts) = &self.gpu.fog_grid_counts {
                            entries.push(binding(3, counts.as_entire_binding()));
                        }
                        entries
                    },
                });
            let fog_visibility_groups = self.gpu.fog_visibility.as_ref().map(|pipelines| {
                let plan = self
                    .gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-visibility-plan"),
                        layout: &pipelines.plan_layout,
                        entries: &[
                            binding(0, fog_candidates.as_entire_binding()),
                            binding(1, self.fog_visibility.directory().as_entire_binding()),
                            binding(2, self.fog_visibility.args().as_entire_binding()),
                        ],
                    });
                let fill = self
                    .gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-visibility-fill"),
                        layout: &pipelines.fill_layout,
                        entries: &[
                            binding(0, wgpu::BindingResource::TextureView(&fog_columns)),
                            binding(1, self.fog_visibility.directory().as_entire_binding()),
                            binding(2, self.fog_visibility.payload().as_entire_binding()),
                        ],
                    });
                (plan, fill)
            });
            self.medium_cache
                .ensure(&self.gpu.device, fixture_cones.len());
            let medium_cache_group = if haze_density > 0.0 && !fixture_cones.is_empty() {
                let cache_uniform = crate::medium::CacheUniform {
                    medium,
                    count: [fixture_cones.len() as u32, 0, 0, 0],
                };
                let buffer = self.storage(
                    &mut encoder,
                    &[cache_uniform],
                    wgpu::BufferUsages::UNIFORM,
                    "medium-cache-uniform",
                );
                Some(
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("medium-cache"),
                            layout: &self.gpu.medium_cache_layout,
                            entries: &[
                                binding(0, buffer.as_entire_binding()),
                                binding(1, index_bindings.core.as_entire_binding()),
                                binding(2, index_bindings.rest.as_entire_binding()),
                                binding(
                                    3,
                                    wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                                ),
                                binding(
                                    4,
                                    wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                                ),
                                binding(
                                    5,
                                    wgpu::BindingResource::TextureView(&self.medium_cache.view),
                                ),
                            ],
                        }),
                )
            } else {
                None
            };
            let medium_cache_dispatch = [
                medium.shape[3] as u32,
                medium.shape[3] as u32,
                fixture_cones.len() as u32,
            ];
            let encode_medium_cache = |encoder: &mut wgpu::CommandEncoder,
                                       pass_queries: &mut crate::pass_profile::PassQueries<'_>,
                                       gpu: &Gpu| {
                let Some(group) = medium_cache_group.as_ref() else {
                    return;
                };
                let repeat_count = gpu.profile_copies("medium-cache");
                for _repeat_index in 0..repeat_count {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("medium-cache"),
                        timestamp_writes: pass_queries.compute("medium-cache", None),
                    });
                    pass.set_pipeline(&gpu.medium_cache_pipeline);
                    pass.set_bind_group(0, group, &[]);
                    pass.dispatch_workgroups(
                        medium_cache_dispatch[0],
                        medium_cache_dispatch[1],
                        medium_cache_dispatch[2],
                    );
                }
            };
            // Compute-lane order (`LUMA_LANE_LATE=0` restores the older one): on a
            // grid-fed surface frame the medium cache and the compaction classify
            // are encoded after the scene pass. Nothing ahead of the scene reads
            // either — `fog-prepare` and `fog-transmittance` bind the cache slot
            // but never sample it, and `fog-grid`, the residual and the hot
            // kernel all follow the scene — while ahead of `fog-prepare` they only
            // delay the transmittance prefix the scene's fragment stage waits on.
            let lane_late = !std::env::var_os("LUMA_LANE_LATE").is_some_and(|v| v == "0");
            let medium_cache_late = lane_late && scene_after_fog && grid_fog;
            if !medium_cache_late {
                encode_medium_cache(&mut encoder, &mut pass_queries, &self.gpu);
            }

            let stochastic_lights = fixture_cones.iter().any(|c| c.gobo != 0);
            let complete_shadow_coverage = fixture_shadow_count == 0
                || rests
                    .iter()
                    .all(|r| r.shadow_slot >= 0.0 || r.haze_gain <= 0.0);
            let haze_passes = if grid_fog && !stochastic_lights {
                1
            } else {
                subframes
            };
            let resolve_haze = (temporal || sampled_haze) && (!grid_fog || stochastic_lights);
            let native_compute =
                grid_fog && !stochastic_lights && complete_shadow_coverage && self.haze_compute;
            let retain_inactive =
                std::env::var_os("LUMA_INTERVAL_RETAIN_INACTIVE").is_some_and(|value| value == "1");
            let interval_cache_bindings = self.plan_interval_cache(
                &mut encoder,
                frame,
                native_compute
                    && cached_shadows
                    && frame.fixture_shadows
                    && !fixture_cones.is_empty(),
                opaque,
                &inv_view_proj,
                camera_far,
                (width, height),
                haze_size,
                &shadow_slots,
                &fixture_cones,
                &rests,
                retain_inactive,
            );
            // Shadow slot → this frame's light-index id, for the residual list.
            let slot_lights: Vec<u32> = {
                let source_to_sorted = self.light_index.source_to_sorted();
                shadow_slots
                    .iter()
                    .map(|resident| {
                        resident
                            .and_then(|index| source_to_sorted.get(index).copied().flatten())
                            .unwrap_or(u32::MAX)
                    })
                    .collect()
            };
            let transport_key = (self.gpu.haze_resid_temporal != 0).then(|| {
                residual_transport_key(
                    frame,
                    width,
                    height,
                    haze_size,
                    medium,
                    fog_far,
                    camera_far,
                    caster_hash,
                    opaque_depth_hash(frame, opaque),
                    cached_shadows,
                    self.geometry_shadow_samples,
                    self.gpu.haze_resid_per_resident,
                    &fixture_cones,
                    &rests,
                )
            });
            let resident_keys: Vec<Option<ResidualResidentKey>> = shadow_slots
                .iter()
                .enumerate()
                .map(|(slot, resident)| {
                    let index = (*resident)?;
                    Some(residual_resident_key(
                        fixture_cones.get(index)?,
                        rests.get(index)?,
                        self.fixture_shadow_cache.get(slot).copied().flatten(),
                        self.interval_cache_entries.get(slot).copied().unwrap_or([
                            0,
                            0,
                            0,
                            crate::interval_cache::MODE_OFF,
                        ]),
                    ))
                })
                .collect();
            let compact_frame = self.plan_compact(
                &mut encoder,
                native_compute && interval_cache_bindings.is_some(),
                haze_size,
                (width, height),
                &shadow_slots,
                &slot_lights,
                &resident_keys,
                fixture_cones.len(),
                frame.fixture_shadow_capacity_hint,
                scene_after_fog,
                frame.time,
                transport_key,
                retain_inactive,
            );
            // Cloned out of the pools so the bind-group closure below borrows no
            // renderer field across the encode loop.
            let compact_buffers = compact_frame.as_ref().map(|cf| {
                let pools = self
                    .compact_pools
                    .as_ref()
                    .expect("planned frames have pools");
                (
                    cf.uniform.clone(),
                    pools.planes.clone(),
                    pools.list.clone(),
                    pools.rgb.clone(),
                    pools.work.clone(),
                    pools.aux.clone(),
                    cf.counters.clone(),
                    cf.args.clone(),
                    pools.readback.clone(),
                    pools.args_stub.clone(),
                    pools.target.clone(),
                )
            });
            let compact_pipes = self.gpu.haze_compact_pipelines.as_ref();
            let compact_late = std::env::var_os("LUMA_HAZE_COMPACT_LATE").is_some_and(|v| v == "1");
            // The classify pass fills write-mode slots itself; a separate fill
            // pass ahead of it is a diagnostic (`LUMA_HAZE_FILL=separate`), and
            // classify then finds those slots' headers already written (its
            // in-register fill produces the same words).
            let separate_fill = std::env::var_os("LUMA_HAZE_FILL").is_some_and(|v| v == "separate");
            let scene_late = compact_frame.is_some()
                && !compact_late
                && std::env::var_os("LUMA_HAZE_SCENE_LATE").is_some_and(|v| v == "1");
            // Only the hot pass samples the lit grid; the others bind the static
            // density field in its place so fog-integrate's write to the grid
            // does not have to wait for them (nor they for it).
            let compact_bind_group =
                |work_counts: &wgpu::Buffer, indirect: bool, grid: bool| -> wgpu::BindGroup {
                    let (uniform, planes, list, rgb, work, aux, counters, args, _, args_stub, _) =
                        compact_buffers.as_ref().expect("compact frame");
                    let args = if indirect { args } else { args_stub };
                    let grid_view = if grid {
                        &fog_integral
                    } else {
                        &self.gpu.haze_field.view
                    };
                    let (cache_uniform, header, claims, table) = interval_cache_bindings
                        .as_ref()
                        .expect("compact frames have cache bindings");
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze-compact"),
                            layout: &compact_pipes.expect("compact frame").layout,
                            entries: &[
                                binding(0, wgpu::BindingResource::TextureView(grid_view)),
                                binding(1, wgpu::BindingResource::TextureView(&haze_view)),
                                binding(2, wgpu::BindingResource::TextureView(&haze_sampled)),
                                binding(3, work_counts.as_entire_binding()),
                                binding(4, cache_uniform.as_entire_binding()),
                                binding(5, header.as_entire_binding()),
                                binding(6, claims.as_entire_binding()),
                                binding(7, table.as_entire_binding()),
                                binding(
                                    8,
                                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                        buffer: uniform,
                                        offset: 0,
                                        size: wgpu::BufferSize::new(COMPACT_UNIFORM_STRIDE),
                                    }),
                                ),
                                binding(9, planes.as_entire_binding()),
                                binding(10, list.as_entire_binding()),
                                binding(11, rgb.as_entire_binding()),
                                binding(12, work.as_entire_binding()),
                                binding(13, counters.as_entire_binding()),
                                binding(14, args.as_entire_binding()),
                                binding(15, aux.as_entire_binding()),
                            ],
                        })
                };
            let grid_read_transmittance =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fog-grid-read-transmittance"),
                        layout: &self.gpu.fog_grid_read_layout,
                        entries: &[binding(
                            0,
                            wgpu::BindingResource::TextureView(&fog_transmittance),
                        )],
                    });
            for k in 0..haze_passes {
                let uniform = HazeUniform {
                    medium,
                    inv_view_proj: inv_view_proj.to_cols_array_2d(),
                    camera_pos: frame.camera.eye.extend(1.0).to_array(),
                    params: [
                        fixture_cones.len() as f32,
                        haze_density,
                        frame.haze_steps as f32,
                        // Feeding track time makes the noise drift identical on
                        // every run (spec §6).
                        haze_time,
                    ],
                    tuning: [
                        (noise_seed.wrapping_add(k) & 4095) as f32,
                        weight,
                        Transport::NEAR_CLAMP,
                        Transport::BEAM_GAIN,
                    ],
                    transport: [
                        Transport::WHITE_LEAK,
                        Transport::PHASE_G,
                        haze_size.1 as f32,
                        haze_size.0 as f32,
                    ],
                    // The light index lives in full-resolution pixel space; xy
                    // scales this pass's fragment coordinate up to it.
                    tiles: [
                        width as f32 / haze_size.0 as f32,
                        height as f32 / haze_size.1 as f32,
                        if grid_fog && k == 0 { 2.0 } else { 1.0 },
                        0.0,
                    ],
                    depth: [
                        CAMERA_NEAR,
                        camera_far,
                        haze_density * Transport::EXTINCTION,
                        if sampled_haze {
                            self.wide_light_group as f32
                        } else {
                            1.0
                        },
                    ],
                    shadow: [
                        fixture_shadow_count as f32,
                        1.0 / FIXTURE_SHADOW_SIZE as f32,
                        if self.visibility_reference
                            && (self.geometry_shadows || frame.geometry_shadows)
                            && frame.fixture_shadows
                        {
                            self.geometry_shadow_samples as f32
                        } else {
                            0.0
                        },
                        fog_far,
                    ],
                };
                // Indexed label: every subframe's upload lands before the one
                // submit, so one shared label would leave all of them reading the
                // last subframe's jitter seed.
                let haze_buf = self.storage(
                    &mut encoder,
                    &[uniform],
                    wgpu::BufferUsages::UNIFORM,
                    &format!("haze-{k}"),
                );
                let bind_group = self
                    .gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("haze"),
                        layout: &self.gpu.haze_layout,
                        entries: &[
                            binding(0, haze_buf.as_entire_binding()),
                            binding(
                                10,
                                wgpu::BindingResource::TextureView(&self.medium_cache.view),
                            ),
                            binding(
                                11,
                                wgpu::BindingResource::TextureView(&self.shadow_hierarchy.views[0]),
                            ),
                            binding(
                                12,
                                wgpu::BindingResource::TextureView(&self.shadow_hierarchy.views[1]),
                            ),
                            binding(1, index_bindings.core.as_entire_binding()),
                            binding(2, index_bindings.rest.as_entire_binding()),
                            binding(3, wgpu::BindingResource::TextureView(&depth_view)),
                            binding(
                                4,
                                wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                            ),
                            binding(
                                5,
                                wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                            ),
                            binding(6, fixture_shadow_matrix_buf.as_entire_binding()),
                            binding(
                                7,
                                wgpu::BindingResource::TextureView(&self.fixture_shadow_map),
                            ),
                            binding(8, self.visibility.buffer.as_entire_binding()),
                            binding(
                                9,
                                wgpu::BindingResource::TextureView(&self.fixture_shadow_map_extra),
                            ),
                        ],
                    });
                // The fill and classify passes never sample the medium cache, but
                // a bind group that carries it would make them wait for the
                // medium-cache pass; this one binds the static density field in
                // its place so they can start as soon as the shadow maps, the
                // hierarchy and the light index are done.
                let bind_group_early =
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze-early"),
                            layout: &self.gpu.haze_layout,
                            entries: &[
                                binding(0, haze_buf.as_entire_binding()),
                                binding(
                                    10,
                                    wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                                ),
                                binding(
                                    11,
                                    wgpu::BindingResource::TextureView(
                                        &self.shadow_hierarchy.views[0],
                                    ),
                                ),
                                binding(
                                    12,
                                    wgpu::BindingResource::TextureView(
                                        &self.shadow_hierarchy.views[1],
                                    ),
                                ),
                                binding(1, index_bindings.core.as_entire_binding()),
                                binding(2, index_bindings.rest.as_entire_binding()),
                                binding(3, wgpu::BindingResource::TextureView(&depth_view)),
                                binding(
                                    4,
                                    wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                                ),
                                binding(
                                    5,
                                    wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                                ),
                                binding(6, fixture_shadow_matrix_buf.as_entire_binding()),
                                binding(
                                    7,
                                    wgpu::BindingResource::TextureView(&self.fixture_shadow_map),
                                ),
                                binding(8, self.visibility.buffer.as_entire_binding()),
                                binding(
                                    9,
                                    wgpu::BindingResource::TextureView(
                                        &self.fixture_shadow_map_extra,
                                    ),
                                ),
                            ],
                        });
                if grid_fog && k == 0 {
                    let encode_classify = |encoder: &mut wgpu::CommandEncoder,
                                           pass_queries: &mut crate::pass_profile::PassQueries<
                        '_,
                    >| {
                        let Some(cf) = compact_frame.as_ref().filter(|_| !compact_late) else {
                            return;
                        };
                        // Fill needs shadow maps and depth; classify additionally
                        // the fill's headers. Neither samples the medium cache or
                        // the grid, so they overlap the medium-cache pass and the
                        // scene, ahead of everything the residual needs.
                        let pipes = compact_pipes.expect("compact frame");
                        let classify_group =
                            compact_bind_group(&haze_work_counts_compact[0], true, false);
                        if cf.fill && separate_fill {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("haze-fill"),
                                    timestamp_writes: pass_queries.compute("haze-fill", None),
                                });
                            pass.set_pipeline(&pipes.fill);
                            pass.set_bind_group(0, &bind_group_early, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &classify_group, &[0]);
                            pass.set_bind_group(3, &grid_read_transmittance, &[]);
                            pass.dispatch_workgroups(
                                haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
                                haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
                                1,
                            );
                        }
                        if cf.classify {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("haze-classify"),
                                    timestamp_writes: pass_queries.compute("haze-classify", None),
                                });
                            pass.set_pipeline(&pipes.classify);
                            pass.set_bind_group(0, &bind_group_early, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &classify_group, &[0]);
                            pass.set_bind_group(3, &grid_read_transmittance, &[]);
                            pass.dispatch_workgroups(
                                haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
                                haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
                                1,
                            );
                        }
                    };
                    // Encoded after the scene on a grid-fed surface frame: the
                    // classify (with its fill) writes only the interval cache and
                    // the residual planes/list, whose first readers are the
                    // residual and hot kernels behind `fog-integrate`.
                    let classify_late = lane_late && scene_after_fog && !scene_late;
                    if !classify_late {
                        encode_classify(&mut encoder, &mut pass_queries);
                    }
                    // The prefix passes never sample the medium cache; the early
                    // group binds the static field in its slot so they do not wait
                    // for the medium-cache pass.
                    let prefix_group = if lane_late {
                        &bind_group_early
                    } else {
                        &bind_group
                    };
                    let repeat_count = self.gpu.profile_copies("fog-prepare");
                    for repeat_index in 0..repeat_count {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("fog-grid"),
                            timestamp_writes: pass_queries.compute(
                                "fog-prepare",
                                profile_resources.as_ref().and_then(|(queries, ..)| {
                                    (k == 0).then_some(wgpu::ComputePassTimestampWrites {
                                        query_set: queries,
                                        beginning_of_pass_write_index: (repeat_index == 0)
                                            .then_some(6),
                                        end_of_pass_write_index: (repeat_index + 1 == repeat_count)
                                            .then_some(8),
                                    })
                                }),
                            ),
                        });
                        pass.set_pipeline(&self.gpu.fog_prepare_pipeline);
                        pass.set_bind_group(0, prefix_group, &[]);
                        pass.set_bind_group(1, &light_index_bg, &[]);
                        pass.set_bind_group(2, &grid_prepare, &[]);
                        pass.dispatch_workgroups(
                            width.div_ceil(crate::fog_grid::tile_size()).div_ceil(8),
                            height.div_ceil(crate::fog_grid::tile_size()).div_ceil(8),
                            1,
                        );
                        drop(pass);
                    }
                    if scene_after_fog {
                        // Grid-sourced surface transmittance: the camera prefix
                        // needs only the columns, so the surface pass follows it
                        // here and overlaps the lit grid as it used to overlap the
                        // whole chain.
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("fog-transmittance"),
                            timestamp_writes: pass_queries.compute("fog-transmittance", None),
                        });
                        pass.set_pipeline(&self.gpu.fog_transmittance_pipeline);
                        pass.set_bind_group(0, prefix_group, &[]);
                        pass.set_bind_group(1, &light_index_bg, &[]);
                        pass.set_bind_group(2, &grid_transmittance, &[]);
                        pass.dispatch_workgroups(
                            width.div_ceil(crate::fog_grid::tile_size()),
                            height.div_ceil(crate::fog_grid::tile_size()),
                            1,
                        );
                        drop(pass);
                        if !scene_late {
                            encode_scene(
                                &mut encoder,
                                &mut pass_queries,
                                &self.gpu,
                                profile_resources.as_ref().map(|(queries, ..)| queries),
                            );
                            if classify_late {
                                encode_classify(&mut encoder, &mut pass_queries);
                            }
                        }
                    }
                    if medium_cache_late {
                        encode_medium_cache(&mut encoder, &mut pass_queries, &self.gpu);
                    }
                    if let Some(cf) = compact_frame.as_ref().filter(|_| !compact_late) {
                        let pipes = compact_pipes.expect("compact frame");
                        let (.., args, _, _, resid_target) =
                            compact_buffers.as_ref().expect("compact frame");
                        if !cf.after_integrate {
                            // The transmittance-only prefix carries the same alpha
                            // as the lit grid, so the residual quadrature can run
                            // here, beside the scene pass.
                            if self.gpu.haze_work_counts {
                                encoder.clear_buffer(&haze_work_counts_compact[1], 0, None);
                            }
                            let prepare_group =
                                compact_bind_group(&haze_work_counts_compact[1], true, false);
                            let residual_group =
                                compact_bind_group(&haze_work_counts_compact[1], false, false);
                            encode_residual(
                                &mut encoder,
                                &mut pass_queries,
                                pipes,
                                &bind_group,
                                &light_index_bg,
                                &prepare_group,
                                &residual_group,
                                &grid_read_transmittance,
                                args,
                                resid_target,
                                self.gpu.haze_resid_fragment,
                                cf.prepare_residual,
                                cf.classify,
                            );
                        }
                    }
                    if scene_after_fog && scene_late {
                        // Diagnostic order (`LUMA_HAZE_SCENE_LATE=1`): the surface
                        // pass encoded after the compaction passes, so the render
                        // lane's fragment work overlaps the lit grid and the hot
                        // pass instead of the classify chain.
                        encode_scene(
                            &mut encoder,
                            &mut pass_queries,
                            &self.gpu,
                            profile_resources.as_ref().map(|(queries, ..)| queries),
                        );
                    }
                    if crate::fog_grid::block_visibility() {
                        self.fog_blocks_valid = true;
                        let repeat_count = self.gpu.profile_copies("fog-classify");
                        for _repeat_index in 0..repeat_count {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("fog-classify"),
                                    timestamp_writes: pass_queries.compute("fog-classify", None),
                                });
                            pass.set_pipeline(&self.gpu.fog_classify_pipeline);
                            pass.set_bind_group(0, &bind_group, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &grid_classify, &[]);
                            pass.dispatch_workgroups(
                                width
                                    .div_ceil(crate::fog_grid::tile_size())
                                    .div_ceil(crate::fog_grid::BLOCK_SIDE),
                                height
                                    .div_ceil(crate::fog_grid::tile_size())
                                    .div_ceil(crate::fog_grid::BLOCK_SIDE),
                                crate::fog_grid::SLICES.div_ceil(crate::fog_grid::BLOCK_SIDE),
                            );
                        }
                    }
                    if k == 0 && self.fog_visibility.active() {
                        let pipelines = self
                            .gpu
                            .fog_visibility
                            .as_ref()
                            .expect("active fog-visibility cache has pipelines");
                        let (plan_group, fill_group) = fog_visibility_groups
                            .as_ref()
                            .expect("active fog-visibility cache has groups");
                        {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("fog-visibility-plan"),
                                    timestamp_writes: pass_queries
                                        .compute("fog-visibility-plan", None),
                                });
                            pass.set_pipeline(&pipelines.plan);
                            pass.set_bind_group(0, &bind_group_early, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, plan_group, &[]);
                            pass.dispatch_workgroups(self.fog_visibility.plan_workgroups(), 1, 1);
                        }
                        {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("fog-visibility-finalize"),
                                    timestamp_writes: pass_queries
                                        .compute("fog-visibility-finalize", None),
                                });
                            pass.set_pipeline(&pipelines.finalize);
                            pass.set_bind_group(0, &bind_group_early, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, plan_group, &[]);
                            pass.dispatch_workgroups(1, 1, 1);
                        }
                        {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("fog-visibility-fill"),
                                    timestamp_writes: pass_queries
                                        .compute("fog-visibility-fill", None),
                                });
                            pass.set_pipeline(&pipelines.fill);
                            pass.set_bind_group(0, &bind_group_early, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, fill_group, &[]);
                            pass.dispatch_workgroups_indirect(self.fog_visibility.args(), 0);
                        }
                    }
                    if let Some(counts) = &self.gpu.fog_grid_counts {
                        encoder.clear_buffer(counts, 0, None);
                    }
                    let repeat_count = self.gpu.profile_copies("fog-grid");
                    for repeat_index in 0..repeat_count {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("fog-grid"),
                            timestamp_writes: pass_queries.compute(
                                "fog-grid",
                                profile_resources
                                    .as_ref()
                                    .filter(|_| repeat_index + 1 == repeat_count)
                                    .map(|(queries, ..)| wgpu::ComputePassTimestampWrites {
                                        query_set: queries,
                                        beginning_of_pass_write_index: None,
                                        end_of_pass_write_index: Some(9),
                                    }),
                            ),
                        });
                        pass.set_pipeline(&self.gpu.fog_grid_pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.set_bind_group(1, &light_index_bg, &[]);
                        pass.set_bind_group(2, &grid_write, &[]);
                        pass.dispatch_workgroups(
                            width
                                .div_ceil(crate::fog_grid::tile_size())
                                .div_ceil(crate::fog_grid::BLOCK_SIDE),
                            height
                                .div_ceil(crate::fog_grid::tile_size())
                                .div_ceil(crate::fog_grid::BLOCK_SIDE),
                            crate::fog_grid::SLICES.div_ceil(crate::fog_grid::BLOCK_SIDE),
                        );
                        drop(pass);
                    }
                    let repeat_count = self.gpu.profile_copies("fog-integrate");
                    for repeat_index in 0..repeat_count {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("fog-integrate"),
                            timestamp_writes: pass_queries.compute(
                                "fog-integrate",
                                profile_resources
                                    .as_ref()
                                    .filter(|_| repeat_index + 1 == repeat_count)
                                    .and_then(|(queries, ..)| {
                                        (k == 0).then_some(wgpu::ComputePassTimestampWrites {
                                            query_set: queries,
                                            beginning_of_pass_write_index: None,
                                            end_of_pass_write_index: Some(7),
                                        })
                                    }),
                            ),
                        });
                        // `fog-transmittance` ran this frame iff the surface reads
                        // the grid; its per-slice τ then replaces the density taps.
                        if scene_after_fog {
                            pass.set_pipeline(&self.gpu.fog_integrate_reuse_pipeline);
                        } else {
                            pass.set_pipeline(&self.gpu.fog_integrate_pipeline);
                        }
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.set_bind_group(1, &light_index_bg, &[]);
                        pass.set_bind_group(
                            2,
                            if scene_after_fog {
                                &grid_integrate_reuse
                            } else {
                                &grid_integrate
                            },
                            &[],
                        );
                        pass.dispatch_workgroups(
                            width.div_ceil(crate::fog_grid::tile_size()),
                            height.div_ceil(crate::fog_grid::tile_size()),
                            1,
                        );
                    }
                    if let Some(cf) = compact_frame.as_ref().filter(|_| compact_late) {
                        // Diagnostic placement (`LUMA_HAZE_COMPACT_LATE=1`): every
                        // compaction pass after the grid chain, uncontended, so
                        // their brackets read as real costs.
                        let pipes = compact_pipes.expect("compact frame");
                        let (.., args, _, _, _) = compact_buffers.as_ref().expect("compact frame");
                        let classify_group =
                            compact_bind_group(&haze_work_counts_compact[0], true, false);
                        if cf.fill && separate_fill {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("haze-fill"),
                                    timestamp_writes: pass_queries.compute("haze-fill", None),
                                });
                            pass.set_pipeline(&pipes.fill);
                            pass.set_bind_group(0, &bind_group, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &classify_group, &[0]);
                            pass.set_bind_group(3, &grid_read, &[]);
                            pass.dispatch_workgroups(
                                haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
                                haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
                                1,
                            );
                        }
                        if cf.classify {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("haze-classify"),
                                    timestamp_writes: pass_queries.compute("haze-classify", None),
                                });
                            pass.set_pipeline(&pipes.classify);
                            pass.set_bind_group(0, &bind_group, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &classify_group, &[0]);
                            pass.set_bind_group(3, &grid_read, &[]);
                            pass.dispatch_workgroups(
                                haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
                                haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
                                1,
                            );
                        }
                        let _ = args;
                    }
                    if let Some(cf) = compact_frame
                        .as_ref()
                        .filter(|cf| cf.after_integrate || compact_late)
                    {
                        let pipes = compact_pipes.expect("compact frame");
                        let (.., args, _, _, resid_target) =
                            compact_buffers.as_ref().expect("compact frame");
                        if self.gpu.haze_work_counts {
                            encoder.clear_buffer(&haze_work_counts_compact[1], 0, None);
                        }
                        let prepare_group =
                            compact_bind_group(&haze_work_counts_compact[1], true, false);
                        let residual_group =
                            compact_bind_group(&haze_work_counts_compact[1], false, false);
                        encode_residual(
                            &mut encoder,
                            &mut pass_queries,
                            pipes,
                            &bind_group,
                            &light_index_bg,
                            &prepare_group,
                            &residual_group,
                            &grid_read,
                            args,
                            resid_target,
                            self.gpu.haze_resid_fragment,
                            cf.prepare_residual,
                            cf.classify,
                        );
                    }
                }
                // Deterministic grid transport emits each pixel exactly once.
                // Direct stores preserve its output without fragment blending;
                // stochastic subframes still need the accumulation pass below.
                // The specialized kernel contains no MIS fallback. An unmapped
                // scattering source keeps the generic fragment path, including
                // its established arithmetic for partially dark rigs.
                if native_compute {
                    self.haze_work_counts_valid = self.gpu.haze_work_counts;
                    if let Some(cf) = compact_frame.as_ref() {
                        let pipes = compact_pipes.expect("compact frame");
                        let (.., counters, args, readback, _, _) =
                            compact_buffers.as_ref().expect("compact frame");
                        let prepare_group =
                            compact_bind_group(&haze_work_counts_compact[2], true, false);
                        let hot_group =
                            compact_bind_group(&haze_work_counts_compact[2], false, true);
                        if cf.whole_enabled {
                            {
                                let mut pass =
                                    encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("haze-prepare-whole-scalar-k"),
                                        timestamp_writes: pass_queries
                                            .compute("haze-prepare-whole-scalar-k", None),
                                    });
                                pass.set_pipeline(&pipes.prepare_whole);
                                pass.set_bind_group(0, &bind_group, &[]);
                                pass.set_bind_group(1, &light_index_bg, &[]);
                                pass.set_bind_group(2, &prepare_group, &[0]);
                                pass.set_bind_group(3, &grid_read, &[]);
                                pass.dispatch_workgroups(1, 1, 1);
                            }
                            {
                                let mut pass =
                                    encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                        label: Some("haze-whole-scalar-k"),
                                        timestamp_writes: pass_queries
                                            .compute("haze-whole-scalar-k", None),
                                    });
                                pass.set_bind_group(0, &bind_group, &[]);
                                pass.set_bind_group(1, &light_index_bg, &[]);
                                pass.set_bind_group(2, &hot_group, &[0]);
                                pass.set_bind_group(3, &grid_read, &[]);
                                pass.set_pipeline(&pipes.validate_whole_gc);
                                pass.dispatch_workgroups_indirect(args, WHOLE_GC_ARGS_OFFSET);
                                pass.set_pipeline(&pipes.clear_whole_gc);
                                pass.dispatch_workgroups_indirect(args, WHOLE_GC_ARGS_OFFSET);
                                pass.set_pipeline(&pipes.reset_whole);
                                pass.dispatch_workgroups(1, 1, 1);
                                pass.set_pipeline(&pipes.refresh_whole);
                                pass.dispatch_workgroups_indirect(args, WHOLE_REFRESH_ARGS_OFFSET);
                            }
                        }
                        {
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("haze-prepare-output"),
                                    timestamp_writes: None,
                                });
                            pass.set_pipeline(&pipes.prepare_output);
                            pass.set_bind_group(0, &bind_group, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &prepare_group, &[0]);
                            pass.set_bind_group(3, &grid_read, &[]);
                            pass.dispatch_workgroups(1, 1, 1);
                        }
                        let (cache_uniform, header, claims, table) = interval_cache_bindings
                            .as_ref()
                            .expect("compact frames have cache bindings");
                        let fallback_group =
                            self.gpu
                                .device
                                .create_bind_group(&wgpu::BindGroupDescriptor {
                                    label: Some("haze-compact-fused-fallback"),
                                    layout: &self.gpu.haze_compute_layout,
                                    entries: &[
                                        binding(
                                            0,
                                            wgpu::BindingResource::TextureView(&fog_integral),
                                        ),
                                        binding(1, wgpu::BindingResource::TextureView(&haze_view)),
                                        binding(
                                            2,
                                            wgpu::BindingResource::TextureView(&haze_sampled),
                                        ),
                                        binding(3, haze_work_counts_compact[2].as_entire_binding()),
                                        binding(4, cache_uniform.as_entire_binding()),
                                        binding(5, header.as_entire_binding()),
                                        binding(6, claims.as_entire_binding()),
                                        binding(7, table.as_entire_binding()),
                                    ],
                                });
                        let repeat_count = self.gpu.profile_copies("haze-compute");
                        for repeat_index in 0..repeat_count {
                            if self.gpu.haze_work_counts && repeat_index + 1 == repeat_count {
                                // These spare words are per-output-frame whole-lit
                                // diagnostics. Clear immediately before the final
                                // measured repeat so PROFILE_REPEAT cannot multiply
                                // the population counts.
                                encoder.clear_buffer(counters, 55 * 4, Some(12));
                            }
                            let mut pass =
                                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                                    label: Some("haze-compute"),
                                    timestamp_writes: pass_queries.compute(
                                        "haze-compute",
                                        profile_resources
                                            .as_ref()
                                            .filter(|_| repeat_index + 1 == repeat_count)
                                            .map(|(queries, ..)| {
                                                wgpu::ComputePassTimestampWrites {
                                                    query_set: queries,
                                                    beginning_of_pass_write_index: None,
                                                    end_of_pass_write_index: Some(2),
                                                }
                                            }),
                                    ),
                                });
                            pass.set_pipeline(&pipes.hot);
                            pass.set_bind_group(0, &bind_group, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &hot_group, &[0]);
                            pass.set_bind_group(3, &grid_read, &[]);
                            pass.dispatch_workgroups_indirect(args, COMPACT_HOT_ARGS_OFFSET);
                            pass.set_pipeline(&self.gpu.haze_compute_pipeline);
                            pass.set_bind_group(0, &bind_group, &[]);
                            pass.set_bind_group(1, &light_index_bg, &[]);
                            pass.set_bind_group(2, &fallback_group, &[]);
                            pass.set_bind_group(3, &grid_read, &[]);
                            pass.dispatch_workgroups_indirect(args, COMPACT_FUSED_ARGS_OFFSET);
                        }
                        if cf.readback {
                            encoder.copy_buffer_to_buffer(
                                counters,
                                0,
                                readback,
                                0,
                                (COMPACT_COUNTER_WORDS * 4) as u64,
                            );
                        }
                        continue;
                    }
                    let mut output_entries = vec![
                        binding(0, wgpu::BindingResource::TextureView(&fog_integral)),
                        binding(1, wgpu::BindingResource::TextureView(&haze_view)),
                        binding(2, wgpu::BindingResource::TextureView(&haze_sampled)),
                        binding(3, haze_work_counts.as_entire_binding()),
                    ];
                    if let Some((uniform, header, claims, table)) = &interval_cache_bindings {
                        output_entries.extend([
                            binding(4, uniform.as_entire_binding()),
                            binding(5, header.as_entire_binding()),
                            binding(6, claims.as_entire_binding()),
                            binding(7, table.as_entire_binding()),
                        ]);
                    }
                    let output = self
                        .gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze-compute"),
                            layout: &self.gpu.haze_compute_layout,
                            entries: &output_entries,
                        });
                    let repeat_count = self.gpu.profile_copies("haze-compute");
                    for repeat_index in 0..repeat_count {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("haze-compute"),
                            timestamp_writes: pass_queries.compute(
                                "haze-compute",
                                profile_resources
                                    .as_ref()
                                    .filter(|_| repeat_index + 1 == repeat_count)
                                    .map(|(queries, ..)| wgpu::ComputePassTimestampWrites {
                                        query_set: queries,
                                        beginning_of_pass_write_index: None,
                                        end_of_pass_write_index: Some(2),
                                    }),
                            ),
                        });
                        pass.set_pipeline(&self.gpu.haze_compute_pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.set_bind_group(1, &light_index_bg, &[]);
                        pass.set_bind_group(2, &output, &[]);
                        pass.set_bind_group(3, &grid_read, &[]);
                        pass.dispatch_workgroups(
                            haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
                            haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
                            1,
                        );
                    }
                    continue;
                }
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("haze"),
                    color_attachments: &[&haze_view, &haze_sampled].map(|view| {
                        Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: if k == 0 {
                                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                                } else {
                                    wgpu::LoadOp::Load
                                },
                                store: wgpu::StoreOp::Store,
                            },
                        })
                    }),
                    depth_stencil_attachment: None,
                    // The haze region's end lands on the temporal resolve when one
                    // runs, else on the last accumulation pass.
                    timestamp_writes: pass_queries.render(
                        "haze",
                        profile_resources.as_ref().and_then(|(queries, ..)| {
                            (!resolve_haze && k + 1 == haze_passes).then_some(
                                wgpu::RenderPassTimestampWrites {
                                    query_set: queries,
                                    beginning_of_pass_write_index: None,
                                    end_of_pass_write_index: Some(2),
                                },
                            )
                        }),
                    ),
                    ..Default::default()
                });
                pass.set_pipeline(if grid_fog {
                    &self.gpu.haze_grid_pipeline
                } else {
                    &self.gpu.haze_pipeline
                });
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_bind_group(1, &light_index_bg, &[]);
                pass.set_bind_group(2, &grid_read, &[]);
                pass.set_bind_group(3, &grid_read, &[]);
                pass.draw(0..3, 0..1);
            }

            let composite_haze = if resolve_haze {
                let read = self.haze_history_index;
                let write = 1 - read;
                let temporal_uniform = TemporalUniform {
                    // Reject history when the represented surface moves by more
                    // than 25 cm in linear view space.
                    params: [
                        0.82,
                        f32::from(u8::from(history_valid)),
                        0.25,
                        f32::from(u8::from(sampled_haze && !grid_fog)),
                    ],
                };
                let uniform_buf = self.storage(
                    &mut encoder,
                    &[temporal_uniform],
                    wgpu::BufferUsages::UNIFORM,
                    "haze-temporal",
                );
                let bind_group = self
                    .gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("haze-temporal"),
                        layout: &self.gpu.temporal_layout,
                        entries: &[
                            binding(0, uniform_buf.as_entire_binding()),
                            binding(1, wgpu::BindingResource::TextureView(&haze_view)),
                            binding(2, wgpu::BindingResource::TextureView(&haze_history[read])),
                            binding(3, wgpu::BindingResource::TextureView(&haze_sampled)),
                        ],
                    });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("haze-temporal"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &haze_history[write],
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: pass_queries.render(
                        "haze-temporal",
                        profile_resources.as_ref().map(|(queries, ..)| {
                            wgpu::RenderPassTimestampWrites {
                                query_set: queries,
                                beginning_of_pass_write_index: None,
                                end_of_pass_write_index: Some(2),
                            }
                        }),
                    ),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.temporal_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
                drop(pass);
                self.haze_history_index = write;
                self.haze_history_valid = temporal;
                haze_history[write].clone()
            } else {
                self.haze_history_valid = false;
                haze_view.clone()
            };

            // --- composite + readback --------------------------------------------
            let composite_uniform = CompositeUniform {
                medium,
                outdoor_sun,
                camera_pos: frame.camera.eye.extend(1.0).to_array(),
                inv_view_proj: inv_view_proj.to_cols_array_2d(),
                params: [
                    haze_size.0 as f32,
                    haze_size.1 as f32,
                    // Bilateral sigma in metres of linear view depth.
                    0.25,
                    frame.debug_view.shader_code() as f32,
                ],
                depth: [
                    CAMERA_NEAR,
                    camera_far,
                    haze_density * Transport::EXTINCTION,
                    Transport::PHASE_G,
                ],
                background: frame.clear_color.extend(1.0).to_array(),
            };
            let composite_buf = self.storage(
                &mut encoder,
                &[composite_uniform],
                wgpu::BufferUsages::UNIFORM,
                "composite",
            );
            let bind_group = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("composite"),
                    layout: &self.gpu.composite_layout,
                    entries: &[
                        binding(0, composite_buf.as_entire_binding()),
                        binding(1, wgpu::BindingResource::TextureView(&scene_view)),
                        binding(2, wgpu::BindingResource::TextureView(&composite_haze)),
                        binding(
                            5,
                            wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
                        ),
                        binding(
                            6,
                            wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
                        ),
                        binding(3, wgpu::BindingResource::Sampler(&self.gpu.linear_sampler)),
                        binding(4, wgpu::BindingResource::TextureView(&depth_view)),
                    ],
                });
            {
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("composite"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &output_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: pass_queries.render(
                            "composite",
                            profile_resources.as_ref().map(|(queries, ..)| {
                                wgpu::RenderPassTimestampWrites {
                                    query_set: queries,
                                    beginning_of_pass_write_index: None,
                                    end_of_pass_write_index: Some(3),
                                }
                            }),
                        ),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.composite_pipelines[channels.index()]);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &environment_bg, &[]);
                    pass.set_bind_group(2, &sky_bg, &[]);
                    pass.draw(0..3, 0..1);
                }

                // Editor affordances are display UI, not scene radiance. Drawing
                // into the final sRGB target after AgX makes authored colours
                // independent of stage lighting and exposure. Cages load the
                // full-resolution reverse-Z prepass depth; free gizmos use Always.
                if !frame.overlays.is_empty() {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("editor-overlays"),
                        timestamp_writes: pass_queries.render("editor-overlays", None),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &output_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(depth_attachment_load(&depth_view)),
                        ..Default::default()
                    });
                    pass.set_bind_group(0, &overlay_bg, &[]);
                    pass.set_vertex_buffer(0, vertex_buf.slice(..));
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for (i, overlay) in frame.overlays.iter().enumerate() {
                        pass.set_pipeline(
                            &self.gpu.overlay_pipelines[overlay_pipeline_index(overlay, channels)],
                        );
                        let (first, last, base) = ranges[overlay.mesh];
                        pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                    }
                }
                if let Finish::Copy(output, readback) = &finish {
                    encoder.copy_texture_to_buffer(
                        output.as_image_copy(),
                        wgpu::TexelCopyBufferInfo {
                            buffer: readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(bytes_per_row),
                                rows_per_image: Some(t_height),
                            },
                        },
                        wgpu::Extent3d {
                            width: t_width,
                            height: t_height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                if let Some((queries, resolve, profile_readback, _)) = &profile_resources {
                    encoder.resolve_query_set(queries, 0..pass_queries.count, resolve, 0);
                    encoder.copy_buffer_to_buffer(
                        resolve,
                        0,
                        profile_readback,
                        0,
                        u64::from(pass_queries.count) * 8,
                    );
                }
            }

            let staging_started = Instant::now();
            self.staging
                .borrow_mut()
                .finish_and_recall_on_submit(&encoder);
            let finish_started = Instant::now();
            let commands = encoder.finish();
            let submit_started = Instant::now();
            self.gpu.queue.submit([commands]);
            if let Some(cf) = compact_frame.as_ref().filter(|cf| cf.readback) {
                let _ = cf;
                if let Some(pools) = self.compact_pools.as_mut() {
                    let (tx, rx) = mpsc::channel();
                    pools
                        .readback
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            let _ = tx.send(result.map_err(|error| error.to_string()));
                        });
                    pools.pending = Some(rx);
                }
            }
            let submitted = Instant::now();
            let cpu_encode_submit = submitted - started;
            let cpu = CpuSpans {
                prepare: cluster_started - started,
                clusters: clusters_done - cluster_started,
                upload: targets_started - clusters_done,
                targets: targets_done - targets_started,
                encode: cpu_encode_submit - (targets_done - started),
                total: cpu_encode_submit,
                fixture_shadows: shadow_cpu,
                submission: SubmissionCpuSpans {
                    staging: finish_started - staging_started,
                    finish: submit_started - finish_started,
                    submit: submitted - submit_started,
                },
            };
            let completion = match finish {
                Finish::Copy(_, readback) => {
                    let (mapped_tx, mapped) = mpsc::sync_channel(1);
                    readback
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            let _ = mapped_tx
                                .send((Instant::now(), result.map_err(|error| error.to_string())));
                        });
                    Completion::Staged {
                        readback,
                        mapped,
                        mapped_result: None,
                        bytes_per_row,
                    }
                }
                // Without a readback there is no map to wait on, so the queue itself
                // has to say when the surface is safe to sample. On Metal this is
                // the only fence between the two devices; on a shared device it is
                // only the completion stamp.
                Finish::Share(surface) => {
                    let (done_tx, done) = mpsc::sync_channel(1);
                    // Stamped in the callback, not where it is noticed: the whole
                    // question is how much of `draw_time` is the GPU finishing and
                    // how much is the worker getting round to looking.
                    self.gpu.queue.on_submitted_work_done(move || {
                        let _ = done_tx.send(Instant::now());
                    });
                    Completion::Shared {
                        surface,
                        done,
                        finished: false,
                    }
                }
            };
            let query_count = pass_queries.count;
            let passes = pass_queries.spans;
            let pending_profile =
                profile_resources.map(|(query_set, resolve, readback, timestamp_period_ns)| {
                    PendingProfile {
                        device: self.gpu.device.clone(),
                        queue: self.gpu.queue.clone(),
                        query_set,
                        resolve,
                        readback,
                        mapped: None,
                        mapped_result: None,
                        timestamp_period_ns,
                        cpu_encode_submit,
                        cpu_cluster,
                        strict_timestamps: true,
                        grid_fog,
                        query_count,
                        passes,
                    }
                });
            PendingFrame {
                completion,
                width: t_width,
                height: t_height,
                started,
                profile: pending_profile,
                shadows: self.shadow_stats,
                clusters: self.light_index.stats(),
                // Overwritten by `submit_live`, which is the only caller that has
                // a queue to have waited in; the profiler submits directly.
                queued: Duration::ZERO,
                cpu,
                signalled: None,
            }
        }
    }

    /// Every slot's classification key from the last interval-cache plan:
    /// its generation while cached (a write frame and the read frames after
    /// it share the header and payload state that classification depends on).
    /// `u32::MAX` means invalid or unwritten; an ordinary retained inactive
    /// slot keeps its generation.
    fn interval_cache_slot_words(&self, retain_inactive: bool) -> Vec<u32> {
        if retain_inactive {
            self.interval_cache.retained_generations()
        } else {
            self.interval_cache_entries
                .iter()
                .map(|entry| {
                    if entry[3] & 0xFF == 0 {
                        u32::MAX
                    } else {
                        entry[3] >> 8
                    }
                })
                .collect()
        }
    }

    /// Consume the last compact frame's counter readback, if it has landed.
    fn poll_compact_readback(&mut self) {
        let Some(pools) = self.compact_pools.as_mut() else {
            return;
        };
        let Some(rx) = pools.pending.as_ref() else {
            return;
        };
        let _ = self.gpu.device.poll(wgpu::PollType::Poll);
        match rx.try_recv() {
            Ok(Ok(())) => {
                let words: [u32; COMPACT_COUNTER_WORDS] = {
                    let data = pools.readback.slice(..).get_mapped_range();
                    match data {
                        Ok(data) => {
                            let words: &[u32] = bytemuck::cast_slice(&data);
                            std::array::from_fn(|i| words[i])
                        }
                        Err(_) => [0; COMPACT_COUNTER_WORDS],
                    }
                };
                pools.readback.unmap();
                pools.pending = None;
                self.compact_stats.segments = [words[0], words[1], words[2]];
                self.compact_stats.list_count = words[3];
                self.compact_stats.direct_arena_requested_words = words[5];
                self.compact_stats.direct_arena_requested_records = words[6];
                self.compact_stats.direct_arena_stored_records = words[7];
                self.compact_stats.direct_arena_failed_records = words[8];
                self.compact_stats.direct_arena_stored_words = words[9];
                self.compact_stats.direct_arena_failed_words = words[10];
                self.compact_stats.direct_arena_over_k = words[11];
                self.compact_stats.direct_arena_fully_dark = words[12];
                self.compact_stats.direct_arena_requested_intervals = words[13];
                self.compact_stats.direct_arena_requested_nonempty = words[14];
                self.compact_stats.whole_lit_planes = words[55];
                self.compact_stats.queue_offsets = std::array::from_fn(|i| words[28 + i]);
                self.compact_stats.queue_cursors = std::array::from_fn(|i| words[40 + i]);
                self.compact_stats.readback_transport_submission = words[52];
                self.compact_stats.readback_temporal_selected_mask = words[53] & 0xFF;
                self.compact_stats.readback_resident_dirty_dispatch = words[53] & 0x100 != 0;
                let readback_period = (words[53] >> 16) & 0xFF;
                self.compact_stats.readback_temporal_period =
                    u32::from(matches!(readback_period, 1 | 4 | 8)) * readback_period;
                self.compact_stats.readback_temporal_phase = (words[53] >> 24) & 0xFF;
                self.compact_stats.temporal_logical_groups = [words[58], words[59], words[60]];
                self.compact_stats.temporal_physical_groups = [words[61], words[62], words[63]];
                self.compact_stats.whole_lit_hot_lanes = words[56];
                self.compact_stats.whole_lit_remainder_lanes = words[57];
                self.compact_stats.whole_k_count = words[WHOLE_COUNTER_BASE];
                self.compact_stats.whole_k_failed = words[WHOLE_COUNTER_BASE + 1];
                self.compact_stats.whole_k_refreshed = words[WHOLE_COUNTER_BASE + 2];
                self.compact_stats.whole_k_stale = words[WHOLE_COUNTER_BASE + 3];
                self.compact_stats.whole_k_readback_submission = words[WHOLE_COUNTER_BASE + 4];
                self.compact_stats.whole_k_readback_temporal = words[WHOLE_COUNTER_BASE + 5];
                self.compact_stats.whole_k_logical_groups = words[WHOLE_COUNTER_BASE + 6];
                self.compact_stats.whole_k_physical_groups = words[WHOLE_COUNTER_BASE + 7];
                self.compact_stats.whole_k_epoch = words[WHOLE_COUNTER_BASE + 8];
                self.compact_stats.whole_k_gc_count = words[WHOLE_COUNTER_BASE + 9];
                self.compact_stats.whole_k_saturated = words[WHOLE_COUNTER_BASE + 10] != 0;
                let whole_origin = self
                    .whole_submission_keys
                    .iter()
                    .find(|(submission, ..)| {
                        *submission == self.compact_stats.whole_k_readback_submission
                    })
                    .cloned();
                while self
                    .whole_submission_keys
                    .front()
                    .is_some_and(|(submission, ..)| {
                        *submission <= self.compact_stats.whole_k_readback_submission
                    })
                {
                    self.whole_submission_keys.pop_front();
                }
                if let Some((_, key, collected)) = whole_origin {
                    if self.compact_stats.whole_k_saturated {
                        if collected {
                            self.whole_saturated_key = Some(key);
                            self.whole_gc_pending = false;
                        } else if self.whole_saturated_key.as_ref() != Some(&key) {
                            self.whole_gc_pending = true;
                        }
                    } else if collected && self.whole_saturated_key.as_ref() == Some(&key) {
                        self.whole_saturated_key = None;
                    }
                }
                if self.compact_full_count == 0 {
                    self.compact_full_count = words[3].max(1);
                }
                self.compact_stats.overflow = words[4] != 0;
                self.compact_stats.readbacks += 1;
                if words[4] != 0 {
                    self.compact_stats.suspended = 60;
                    self.residual_temporal
                        .invalidate(RESID_TEMPORAL_RESET_OVERFLOW);
                }
            }
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => pools.pending = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// Residual compaction of the cached kernel: decide whether this frame
    /// compacts, size the pools and upload the per-segment parameters.
    #[allow(clippy::too_many_arguments)]
    fn plan_compact(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cache_active: bool,
        haze_size: (u32, u32),
        size: (u32, u32),
        shadow_slots: &[Option<usize>],
        slot_lights: &[u32],
        resident_keys: &[Option<ResidualResidentKey>],
        light_count: usize,
        dense_capacity_hint: usize,
        scene_after_fog: bool,
        medium_time: f32,
        transport_key: Option<ResidualGlobalKey>,
        retain_inactive: bool,
    ) -> Option<CompactFrame> {
        self.poll_compact_readback();
        let suspended = self.compact_stats.suspended;
        self.compact_stats.suspended = suspended.saturating_sub(1);
        self.compact_stats.active = false;
        self.compact_stats.fill = false;
        self.compact_stats.transport_submission = 0;
        self.compact_stats.temporal_period = 0;
        self.compact_stats.temporal_phase = 0;
        self.compact_stats.temporal_selected_mask = 0;
        self.compact_stats.resident_dirty_dispatch = false;
        self.compact_stats.temporal_reset_reasons = 0;
        self.compact_stats.temporal_max_age_us = 0;
        self.compact_stats.whole_k_submission = 0;
        self.compact_stats.whole_k_temporal_period = 0;
        self.compact_stats.whole_k_temporal_phase = 0;
        self.compact_stats.whole_k_temporal_reset_reasons = 0;
        self.compact_stats.whole_k_temporal_max_age_us = 0;
        self.compact_stats.pre_rebuild_dirty_slots = 0;
        self.compact_stats.pre_rebuild_live_slots = 0;
        self.compact_stats.pre_rebuild_unclassified_slots = 0;
        self.compact_stats.pre_rebuild_generation_slots = 0;
        self.compact_stats.pre_rebuild_availability_slots = 0;
        self.compact_stats.pre_rebuild_reuse_forced_slots = 0;
        self.compact_stats.pre_rebuild_resident_transport_slots = 0;
        self.compact_stats.pre_rebuild_list_count = 0;
        self.compact_stats.pre_rebuild_full_count = 0;
        self.compact_stats.rebuild_reasons = 0;
        self.compact_stats.requested_dense_hint = 0;
        self.compact_stats.required_dense_slots = 0;
        self.compact_stats.reserved_dense_slots = 0;
        self.compact_stats.pool_stale = false;
        self.gpu.haze_compact_pipelines.as_ref()?;
        let stats = self.interval_cache.stats();
        let settled = stats.read + stats.write > 0;
        let wanted = match self.gpu.haze_compact {
            HazeCompact::Off => false,
            HazeCompact::On => settled,
            HazeCompact::Always => true,
        };
        let active = cache_active
            && wanted
            && suspended == 0
            && haze_size == size
            && u64::from(haze_size.0) * u64::from(haze_size.1) <= 1 << 22
            && light_count <= 1024;
        if !active {
            if self.gpu.haze_resid_temporal != 0 {
                self.residual_temporal
                    .invalidate(RESID_TEMPORAL_RESET_INACTIVE);
                self.whole_temporal
                    .invalidate(RESID_TEMPORAL_RESET_INACTIVE);
            }
            return None;
        }
        let per_resident = self.gpu.haze_resid_per_resident;
        // Sticky dense ids: a slot keeps its id while resident; freed ids
        // are handed to newcomers lowest first.
        let slots = shadow_slots.len().min(512);
        if self.compact_dense.len() != slots {
            self.compact_dense = vec![None; slots];
            self.compact_classified = vec![None; slots];
            self.compact_resident_keys = vec![None; slots];
            self.compact_resident_occupancy = vec![false; slots];
            self.compact_mapping_valid = vec![false; slots];
        }
        let mut resident_topology_changed = false;
        for (slot, resident) in shadow_slots.iter().take(slots).enumerate() {
            let occupied = resident.is_some();
            resident_topology_changed |=
                per_resident && self.compact_resident_occupancy[slot] != occupied;
            self.compact_resident_occupancy[slot] = occupied;
            if resident.is_none() && !retain_inactive {
                self.compact_dense[slot] = None;
                self.compact_classified[slot] = None;
                self.compact_resident_keys[slot] = None;
                self.compact_mapping_valid[slot] = false;
            }
        }
        let mut used: Vec<bool> = vec![false; 512];
        for id in self.compact_dense.iter().flatten() {
            used[*id as usize] = true;
        }
        for (slot, resident) in shadow_slots.iter().take(slots).enumerate() {
            if resident.is_some() && self.compact_dense[slot].is_none() {
                let Some(id) = used.iter().position(|taken| !taken) else {
                    // More resident slots than the compact table can encode.
                    return None;
                };
                used[id] = true;
                self.compact_dense[slot] = Some(id as u32);
                self.compact_classified[slot] = None;
                self.compact_resident_keys[slot] = None;
                self.compact_mapping_valid[slot] = false;
            }
        }
        let mut dense = [u32::MAX; 512];
        let mut slot_light = [u32::MAX; 512];
        let mut count = 0u32;
        for (slot, id) in self.compact_dense.iter().enumerate() {
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
        let blocks_total = blocks.0 * blocks.1;
        let capacity = self.gpu.haze_resid_capacity;
        let arena_words = self.gpu.haze_direct_arena_words;
        let limits = self.gpu.device.limits();
        let binding_limit =
            u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size);
        let reserve_hint =
            std::env::var_os("LUMA_HAZE_COMPACT_RESERVE_HINT").is_some_and(|value| value == "1");
        let hint = if reserve_hint {
            dense_capacity_hint
        } else {
            count as usize
        };
        self.compact_stats.requested_dense_hint = dense_capacity_hint.min(512) as u32;
        self.compact_stats.required_dense_slots = count;
        let dense_cap = compact_dense_capacity(count, hint, blocks_total, binding_limit)?;
        let entry_bytes = u64::from(capacity) * 4;
        let residual_value_words = residual_value_bytes(capacity, self.gpu.haze_resid_temporal) / 4;
        let residual_aux_words = u64::from(capacity) + u64::from(arena_words);
        let whole = whole_tail_layout(
            if self.gpu.haze_resid_temporal == 0 {
                0
            } else {
                self.gpu.haze_whole_k_bytes
            },
            binding_limit,
            residual_value_words,
            residual_aux_words,
        );
        let value_bytes = whole.rgb_bytes;
        let aux_bytes = whole.aux_bytes;
        if value_bytes > binding_limit
            || entry_bytes > binding_limit
            || aux_bytes > binding_limit
            || capacity.div_ceil(RESID_TARGET_WIDTH) > limits.max_texture_dimension_2d
        {
            return None;
        }
        let stale = self.compact_pools.as_ref().is_none_or(|pools| {
            pools.shape.0 != blocks_total || pools.shape.1 < dense_cap || pools.whole != whole
        });
        let reserved_dense = self
            .compact_pools
            .as_ref()
            .filter(|_| !stale)
            .map_or(dense_cap, |pools| pools.shape.1);
        self.compact_stats.reserved_dense_slots = reserved_dense;
        self.compact_stats.pool_stale = stale;
        if stale {
            let plane_bytes = u64::from(blocks_total) * u64::from(dense_cap) * 8;
            let binding_limit =
                u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size);
            if plane_bytes > binding_limit {
                return None;
            }
            self.compact_classified.iter_mut().for_each(|c| *c = None);
            self.whole_temporal.invalidate(RESID_TEMPORAL_RESET_INIT);
            self.whole_gc_pending = false;
            self.whole_saturated_key = None;
            self.whole_submission_keys.clear();
            let buffer = |label: &str, bytes: u64, usage: wgpu::BufferUsages| {
                self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: bytes.max(4),
                    usage,
                    mapped_at_creation: false,
                })
            };
            let storage = wgpu::BufferUsages::STORAGE;
            self.compact_pools = Some(CompactPools {
                shape: (blocks_total, dense_cap),
                whole,
                planes: buffer("haze-resid-planes", plane_bytes, storage),
                list: buffer("haze-resid-list", u64::from(capacity) * 4, storage),
                rgb: buffer("haze-resid-rgb", value_bytes, storage),
                work: buffer("haze-resid-work", u64::from(capacity) * 4, storage),
                aux: buffer("haze-resid-aux", aux_bytes, storage),
                readback: buffer(
                    "haze-resid-readback",
                    (COMPACT_COUNTER_WORDS * 4) as u64,
                    wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                ),
                pending: None,
                args_stub: buffer(
                    "haze-resid-args-stub",
                    (COMPACT_ARGS_WORDS * 4) as u64,
                    storage,
                ),
                target: self
                    .gpu
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("haze-resid-target"),
                        size: wgpu::Extent3d {
                            width: RESID_TARGET_WIDTH,
                            height: capacity.div_ceil(RESID_TARGET_WIDTH),
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::R8Unorm,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                    .create_view(&Default::default()),
            });
        }
        let readback = self
            .compact_pools
            .as_ref()
            .is_some_and(|pools| pools.pending.is_none());
        // A slot's classification depends on its mode and generation (header
        // and payload state) and on nothing else that can change while those
        // hold; a slot whose word changed is reclassified and its entries
        // appended. Appends leave the old entries as unread garbage, so the
        // list is rebuilt once it has grown past its budget.
        let words = self.interval_cache_slot_words(retain_inactive);
        let reuse_allowed = !std::env::var_os("LUMA_HAZE_LIST_REUSE").is_some_and(|v| v == "0");
        let pooled = |label: &str| {
            self.frame_buffers
                .borrow()
                .get(label)
                .map(|store| store.buffer.clone())
        };
        let mut dirty = [0u32; 16];
        let mut dirty_dense = [0u32; 16];
        let mut dirty_slots = 0u32;
        let mut live_slots = 0u32;
        let mut unclassified_slots = 0u32;
        let mut generation_slots = 0u32;
        let mut availability_slots = 0u32;
        let mut reuse_forced_slots = 0u32;
        let mut resident_transport_slots = 0u32;
        for slot in 0..slots {
            let Some(dense_id) = self.compact_dense[slot] else {
                continue;
            };
            let mapping_valid = slot_lights[slot] != u32::MAX;
            let resident_dirty = per_resident
                && resident_transport_dirty(
                    &self.compact_resident_keys[slot],
                    self.compact_mapping_valid[slot],
                    &resident_keys[slot],
                    mapping_valid,
                );
            self.compact_resident_keys[slot] = resident_keys[slot].clone();
            self.compact_mapping_valid[slot] = mapping_valid;
            if resident_dirty {
                dirty_dense[dense_id as usize / 32] |= 1 << (dense_id % 32);
                resident_transport_slots += 1;
            }

            if !mapping_valid {
                // A retained interval classification is independent of the
                // current sorted-light mapping. The scalar transports are
                // refreshed when this resident becomes mapped again.
                if !per_resident {
                    self.compact_classified[slot] = None;
                }
                continue;
            }
            live_slots += 1;
            let current = words[slot];
            let classified = self.compact_classified[slot];
            if !reuse_allowed || classified != Some(current) {
                dirty[slot / 32] |= 1 << (slot % 32);
                dirty_slots += 1;
                match classified {
                    None => unclassified_slots += 1,
                    Some(previous) if previous == current => reuse_forced_slots += 1,
                    Some(previous) if previous == u32::MAX || current == u32::MAX => {
                        availability_slots += 1
                    }
                    Some(_) => generation_slots += 1,
                }
            }
        }
        let list_count = self.compact_stats.list_count;
        let full_count = self.compact_full_count;
        let grown = compact_list_needs_rebuild(list_count, full_count, capacity);
        let existing_counters = pooled("haze-resid-counters");
        let existing_args = pooled("haze-resid-args");
        let rebuild_reasons = compact_rebuild_reasons(CompactRebuildInputs {
            reuse_allowed,
            pool_stale: stale,
            counters_present: existing_counters.is_some(),
            args_present: existing_args.is_some(),
            grown,
            dirty_slots,
            live_slots,
        });
        let rebuild = rebuild_reasons != 0;
        self.compact_stats.pre_rebuild_dirty_slots = dirty_slots;
        self.compact_stats.pre_rebuild_live_slots = live_slots;
        self.compact_stats.pre_rebuild_unclassified_slots = unclassified_slots;
        self.compact_stats.pre_rebuild_generation_slots = generation_slots;
        self.compact_stats.pre_rebuild_availability_slots = availability_slots;
        self.compact_stats.pre_rebuild_reuse_forced_slots = reuse_forced_slots;
        self.compact_stats.pre_rebuild_resident_transport_slots = resident_transport_slots;
        self.compact_stats.pre_rebuild_list_count = list_count;
        self.compact_stats.pre_rebuild_full_count = full_count;
        self.compact_stats.rebuild_reasons = rebuild_reasons;
        if rebuild {
            dirty = [0; 16];
            dirty_slots = 0;
            for slot in 0..slots {
                if self.compact_dense[slot].is_some() && slot_lights[slot] != u32::MAX {
                    dirty[slot / 32] |= 1 << (slot % 32);
                    dirty_slots += 1;
                } else if slot_lights[slot] == u32::MAX {
                    // A full rebuild discards inactive records. Keep the dense
                    // id resident, but make reactivation classify it again.
                    self.compact_classified[slot] = None;
                }
            }
            self.compact_full_count = 0;
        }
        let mut dirty_lights = [0u32; 32];
        for slot in 0..slots {
            let li = slot_lights[slot];
            if li == u32::MAX {
                continue;
            }
            let li = li as usize;
            if dirty[slot / 32] & (1 << (slot % 32)) != 0 {
                self.compact_classified[slot] = Some(words[slot]);
                debug_assert!(li < 512);
                dirty_lights[li / 32] |= 1 << (li % 32);
            }
            if self.compact_dense[slot].is_some()
                && slot_lights[slot] != u32::MAX
                && self
                    .interval_cache_entries
                    .get(slot)
                    .is_some_and(|e| e[3] & 0xFF == crate::interval_cache::MODE_WRITE)
            {
                dirty_lights[16 + li / 32] |= 1 << (li % 32);
            }
        }
        let classify = dirty_slots > 0;
        let scalar = self.gpu.haze_resid_temporal != 0;
        let scalar_key = scalar.then(|| transport_key.expect("scalar transport plans carry a key"));
        let temporal = if !scalar {
            ResidualTemporalSchedule {
                period: 1,
                phase: 0,
                selected_mask: 0xF,
                full: true,
                reset_reasons: 0,
                max_age: 0.0,
            }
        } else {
            self.residual_temporal.plan(
                self.gpu.haze_resid_temporal,
                self.gpu.haze_resid_max_age,
                medium_time,
                scalar_key
                    .clone()
                    .expect("scalar transport plans carry a key"),
                if per_resident { rebuild } else { classify },
                per_resident,
            )
        };
        let transport_submission = if scalar {
            self.transport_submission = self.transport_submission.wrapping_add(1).max(1);
            self.transport_submission
        } else {
            0
        };

        if resident_topology_changed {
            self.whole_resident_epoch = self.whole_resident_epoch.wrapping_add(1).max(1);
        }
        let whole_layout = self.compact_pools.as_ref().expect("allocated above").whole;
        let whole_enabled = scalar && whole_layout.capacity > 0;
        let whole_origin_key = scalar_key.clone().map(|global| WholeOriginKey {
            global,
            resident_epoch: self.whole_resident_epoch,
        });
        if self
            .whole_saturated_key
            .as_ref()
            .is_some_and(|key| Some(key) != whole_origin_key.as_ref())
        {
            self.whole_saturated_key = None;
            self.whole_gc_pending = true;
        }
        let whole_gc = whole_enabled
            && self.whole_gc_pending
            && self.whole_saturated_key.as_ref() != whole_origin_key.as_ref();
        if whole_gc {
            self.whole_gc_pending = false;
            self.whole_temporal
                .invalidate(RESID_TEMPORAL_RESET_OVERFLOW);
        }
        let whole_temporal = if whole_enabled {
            // Residual list GC does not invalidate whole descriptors. True
            // resident changes are refreshed in the same submission through
            // dirty_dense, before either compact hot pass reads whole K.
            self.whole_temporal.plan(
                self.gpu.haze_resid_temporal,
                self.gpu.haze_resid_max_age,
                medium_time,
                scalar_key.clone().expect("whole scalar key"),
                false,
                false,
            )
        } else {
            ResidualTemporalSchedule {
                period: 1,
                phase: 0,
                selected_mask: 0xF,
                full: true,
                reset_reasons: 0,
                max_age: 0.0,
            }
        };
        let whole_submission = if whole_enabled {
            self.whole_submission = self.whole_submission.wrapping_add(1).max(1);
            self.whole_submission_keys.push_back((
                self.whole_submission,
                whole_origin_key.clone().expect("whole scalar key"),
                whole_gc,
            ));
            // Readback is intentionally asynchronous. Keep enough origins to
            // cover long profiling queues without attributing a saturation
            // or a successful post-GC collection to the wrong CPU frame.
            while self.whole_submission_keys.len() > 128 {
                self.whole_submission_keys.pop_front();
            }
            self.whole_submission
        } else {
            0
        };

        if rebuild {
            self.compact_queue_epoch = self.compact_queue_epoch.wrapping_add(1).max(1);
        }

        let counters = if let Some(existing) = existing_counters {
            if stale {
                // The descriptor and K buffers were replaced. Their allocator
                // and origin counters must restart with the new pool.
                encoder.clear_buffer(&existing, 0, Some((COMPACT_COUNTER_WORDS * 4) as u64));
            } else if rebuild {
                // Residual owns words 0..63. Preserve whole allocator/epoch
                // state at 64..79 across residual-only list GC.
                encoder.clear_buffer(&existing, 0, Some((WHOLE_COUNTER_BASE * 4) as u64));
            }
            existing
        } else {
            let mut initial = [0u32; COMPACT_COUNTER_WORDS];
            initial[54] = self.compact_queue_epoch;
            self.storage(
                encoder,
                &[initial],
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                "haze-resid-counters",
            )
        };
        if rebuild {
            // Residual GC preserves whole counters, so publish the new stable
            // queue epoch explicitly after clearing only the residual range.
            let epoch = self.storage(
                encoder,
                &[self.compact_queue_epoch],
                wgpu::BufferUsages::COPY_SRC,
                "haze-resid-queue-epoch",
            );
            encoder.copy_buffer_to_buffer(&epoch, 0, &counters, 54 * 4, 4);
        }
        let args = if stale || rebuild || existing_args.is_none() {
            self.storage(
                encoder,
                &[compact_indirect_args(blocks)],
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
                "haze-resid-args",
            )
        } else {
            existing_args.expect("checked above")
        };
        let lanes = self.gpu.haze_resid_lanes;
        let dense_stride = self
            .compact_pools
            .as_ref()
            .expect("allocated above")
            .shape
            .1;
        let mut selected_mask = temporal.selected_mask;
        if dirty_dense.iter().any(|word| *word != 0) {
            selected_mask |= 1 << 8;
        }
        let uniforms: [CompactUniform; RESID_BUCKETS] = std::array::from_fn(|bucket| {
            let segment = bucket / 4;
            CompactUniform {
                // The plane stride is the allocation, not the live count: a
                // newcomer with a higher dense id must not move every clean
                // slot's planes.
                params: [dense_stride, capacity, blocks.0, segment as u32],
                seg_lanes: [lanes[0], lanes[1], lanes[2], 0],
                target: [
                    temporal.period
                        | (temporal.phase << 8)
                        | ((u32::from(temporal.full)
                            | (u32::from(classify) << 1)
                            | (u32::from(per_resident) << 2)
                            | (u32::from(rebuild) << 3))
                            << 16),
                    transport_submission,
                    self.compact_group_columns,
                    arena_words,
                ],
                dense,
                slot_light,
                dirty,
                dirty_lights,
                dirty_dense,
                bucket: [
                    bucket as u32,
                    segment as u32,
                    (bucket % 4) as u32,
                    selected_mask,
                ],
                whole: [
                    whole_layout.capacity,
                    whole_layout.rgb_base_words,
                    whole_layout.aux_base_words,
                    whole_temporal.period
                        | (whole_temporal.phase << 8)
                        | (u32::from(whole_temporal.full) << 16)
                        | (u32::from(whole_gc) << 17)
                        | (u32::from(
                            whole_enabled
                                && self.whole_saturated_key.as_ref() != whole_origin_key.as_ref(),
                        ) << 18),
                    whole_submission,
                    u32::from(dirty_dense.iter().any(|word| *word != 0)),
                    0,
                    0,
                ],
                _pad: [0; 40],
            }
        });
        let uniform = self.storage(
            encoder,
            &uniforms,
            wgpu::BufferUsages::UNIFORM,
            "haze-compact-params",
        );
        self.compact_stats.active = true;
        self.compact_stats.classify = classify;
        self.compact_stats.dirty_slots = dirty_slots;
        self.compact_stats.rebuilt = rebuild;
        self.compact_stats.fill = stats.write > 0;
        self.compact_stats.dense_slots = count;
        self.compact_stats.capacity = capacity;
        self.compact_stats.direct_arena_capacity_words = arena_words;
        self.compact_stats.transport_submission = transport_submission;
        self.compact_stats.temporal_period = if scalar { temporal.period } else { 0 };
        self.compact_stats.temporal_phase = temporal.phase;
        self.compact_stats.temporal_selected_mask = temporal.selected_mask;
        self.compact_stats.resident_dirty_dispatch = selected_mask & 0x100 != 0;
        self.compact_stats.temporal_reset_reasons = temporal.reset_reasons;
        self.compact_stats.temporal_max_age_us =
            (temporal.max_age.max(0.0) * 1_000_000.0).round() as u32;
        self.compact_stats.whole_k_capacity = whole_layout.capacity;
        self.compact_stats.whole_k_submission = whole_submission;
        self.compact_stats.whole_k_temporal_period = if whole_enabled {
            whole_temporal.period
        } else {
            0
        };
        self.compact_stats.whole_k_temporal_phase = whole_temporal.phase;
        self.compact_stats.whole_k_temporal_reset_reasons = whole_temporal.reset_reasons;
        self.compact_stats.whole_k_temporal_max_age_us =
            (whole_temporal.max_age.max(0.0) * 1_000_000.0).round() as u32;
        self.compact_stats.frames += 1;
        Some(CompactFrame {
            uniform,
            counters,
            args,
            fill: stats.write > 0,
            classify,
            prepare_residual: scalar || classify,
            whole_enabled,
            after_integrate: self.gpu.haze_resid_after_integrate || !scene_after_fog,
            readback,
        })
    }

    /// Decide which shadow slots' cached traversal output is still exact this
    /// frame, upload the per-slot table and return the compute kernel's cache
    /// bindings. With `active` false (no native compute path, or no cached
    /// shadows) every slot is off and the bookkeeping forgets its contents.
    #[allow(clippy::too_many_arguments)]
    fn plan_interval_cache(
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
        retain_inactive: bool,
    ) -> Option<(wgpu::Buffer, wgpu::Buffer, wgpu::Buffer, wgpu::Buffer)> {
        use crate::interval_cache::{block_rect, FrameKey, SlotKey};
        if !self.gpu.interval_cache {
            self.interval_cache.forget();
            return None;
        }
        let blocks = (
            haze_size.0.div_ceil(HAZE_WORKGROUP[0]),
            haze_size.1.div_ceil(HAZE_WORKGROUP[1]),
        );
        let blocks_total = blocks.0 * blocks.1;
        let capacity = shadow_slots.len().min(512);
        let mut uniform = IntervalCacheUniform {
            params: [blocks.0, 0, 0, 0],
            flags: [
                u32::from(
                    !std::env::var_os("LUMA_INTERVAL_CACHE_PAYLOAD").is_some_and(|v| v == "0"),
                ) | u32::from(
                    std::env::var_os("LUMA_INTERVAL_CACHE_UNIFY").is_some_and(|v| v == "1"),
                ) << 1
                    | u32::from(
                        std::env::var_os("LUMA_INTERVAL_CACHE_SKIP_RESIDUAL")
                            .is_some_and(|v| v == "1"),
                    ) << 2,
                self.interval_cache.epoch(),
                0,
                0,
            ],
            slots: [[0; 4]; 512],
        };
        // Each 8×4 workgroup must lie inside one light-index tile so every
        // lane of a subgroup walks the same light list: true at native haze
        // resolution, which is the only live setting.
        let active = active
            && self.gpu.interval_cache
            && capacity > 0
            && haze_size == size
            && !std::env::var_os("LUMA_INTERVAL_CACHE_ALL_OFF").is_some_and(|v| v == "1");
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
                self.interval_pools = Some(IntervalCachePools {
                    shape: (blocks_total, capacity),
                    header: buffer("interval-cache-header", u64::from(header_words)),
                    claims: buffer("interval-cache-claims", table_entries),
                    table: buffer("interval-cache-table", table_entries * entry_words),
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
                        wash: cone.wash.to_bits(),
                        scatters: rests[index].haze_gain > 0.0,
                        rect: block_rect(tiles, scale, [blocks.0, blocks.1]),
                    })
                })
                .collect();
            let entries = if retain_inactive {
                self.interval_cache
                    .plan_retaining_inactive(frame_key, &keys)
            } else {
                self.interval_cache.plan(frame_key, &keys)
            };
            for (target, entry) in uniform.slots.iter_mut().zip(&entries) {
                *target = *entry;
            }
            self.interval_cache_entries = entries;
            let pools = self.interval_pools.as_ref().expect("allocated above");
            uniform.params = [blocks.0, pools.bucket_mask, 0, 0];
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
        Some((
            uniform_buf,
            pools.header.clone(),
            pools.claims.clone(),
            pools.table.clone(),
        ))
    }

    fn scene_bind_group(
        &self,
        globals: &wgpu::Buffer,
        instances: &wgpu::Buffer,
        point_lights: &wgpu::Buffer,
        shadows: bool,
        hard_shadows: bool,
        aerial: &crate::atmosphere::AerialTextures,
    ) -> wgpu::BindGroup {
        // The shadow map is a render target during the shadow pass, so the
        // passes that write depth bind a 1x1 placeholder instead.
        let map = if shadows {
            &self.shadow_map
        } else {
            &self.gpu.dummy_shadow
        };
        let mut entries = vec![
            binding(0, globals.as_entire_binding()),
            binding(1, instances.as_entire_binding()),
            binding(2, point_lights.as_entire_binding()),
            binding(3, wgpu::BindingResource::TextureView(map)),
            binding(
                4,
                wgpu::BindingResource::Sampler(if hard_shadows {
                    &self.gpu.hard_shadow_sampler
                } else {
                    &self.gpu.shadow_sampler
                }),
            ),
        ];
        entries.extend(aerial.entries());
        entries.extend([
            binding(
                10,
                wgpu::BindingResource::TextureView(&self.gpu.haze_field.view),
            ),
            binding(
                11,
                wgpu::BindingResource::Sampler(&self.gpu.haze_field.sampler),
            ),
        ]);
        self.gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scene"),
                layout: &self.gpu.scene_layout,
                entries: &entries,
            })
    }

    /// Upload one frame's worth of `data` under `label`, reusing the buffer
    /// the previous frame uploaded under the same label.
    ///
    /// The label is the identity: two uploads in one frame under one label
    /// would alias, so anything uploaded in a loop must carry its index in
    /// the label. Buffers only grow — a show's sizes oscillate every frame
    /// and the slack is the point. Measured effect: pooling plus the staging
    /// belt takes all per-frame upload work to ~30 µs; the frame's remaining
    /// CPU encode cost is the CPU binners, not the uploads.
    fn storage<T: Pod>(
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
    fn immutable<T: Pod>(
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

/// Upload one immutable material map with a full mip chain. Color maps use an
/// sRGB view, while normal/metallic-roughness/AO maps remain linear data.
fn upload_texture_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &Image,
    encoding: TextureEncoding,
) -> wgpu::TextureView {
    let levels = 32 - image.width.max(image.height).leading_zeros();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("material-map"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: match encoding {
            TextureEncoding::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            TextureEncoding::Linear => wgpu::TextureFormat::Rgba8Unorm,
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let mut level = (image.width, image.height, image.rgba.to_vec());
    for mip in 0..levels {
        let (w, h, ref pixels) = level;
        // Rows go up 256-byte aligned: that is the copy alignment every backend
        // agrees on, and a tightly packed odd width otherwise lands skewed.
        let row = (w * 4).div_ceil(256) * 256;
        let mut padded = vec![0u8; (row * h) as usize];
        for y in 0..h as usize {
            let src = y * (w * 4) as usize;
            let dst = y * row as usize;
            padded[dst..dst + (w * 4) as usize]
                .copy_from_slice(&pixels[src..src + (w * 4) as usize]);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        if mip + 1 < levels {
            level = downsample(w, h, pixels, encoding);
        }
    }

    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn material_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    base_color: &wgpu::TextureView,
    normal: &wgpu::TextureView,
    metallic_roughness: &wgpu::TextureView,
    occlusion: &wgpu::TextureView,
    emissive: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material"),
        layout,
        entries: &[
            binding(0, wgpu::BindingResource::TextureView(base_color)),
            binding(1, wgpu::BindingResource::TextureView(normal)),
            binding(2, wgpu::BindingResource::TextureView(metallic_roughness)),
            binding(3, wgpu::BindingResource::TextureView(occlusion)),
            binding(4, wgpu::BindingResource::TextureView(emissive)),
            binding(5, wgpu::BindingResource::Sampler(sampler)),
        ],
    })
}

/// One 2x2 box-filter step. Odd dimensions collapse to 1 and then repeat the
/// row/column, which is how GL's mip chain treats them.
fn downsample(w: u32, h: u32, pixels: &[u8], encoding: TextureEncoding) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let (x0, y0) = (x * 2, y * 2);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            for c in 0..4 {
                let at = |px: u32, py: u32| pixels[((py * w + px) * 4 + c) as usize];
                if encoding == TextureEncoding::Srgb && c < 3 {
                    let linear = |value: u8| {
                        let value = f32::from(value) / 255.0;
                        if value <= 0.04045 {
                            value / 12.92
                        } else {
                            ((value + 0.055) / 1.055).powf(2.4)
                        }
                    };
                    let mean = (linear(at(x0, y0))
                        + linear(at(x1, y0))
                        + linear(at(x0, y1))
                        + linear(at(x1, y1)))
                        * 0.25;
                    let encoded = if mean <= 0.003_130_8 {
                        mean * 12.92
                    } else {
                        1.055 * mean.powf(1.0 / 2.4) - 0.055
                    };
                    out.push((encoded * 255.0).round().clamp(0.0, 255.0) as u8);
                } else {
                    let mean = u32::from(at(x0, y0))
                        + u32::from(at(x1, y0))
                        + u32::from(at(x0, y1))
                        + u32::from(at(x1, y1));
                    out.push(((mean + 2) / 4) as u8);
                }
            }
        }
    }
    (nw, nh, out)
}

const ADD: wgpu::BlendComponent = wgpu::BlendComponent {
    src_factor: wgpu::BlendFactor::One,
    dst_factor: wgpu::BlendFactor::One,
    operation: wgpu::BlendOperation::Add,
};

/// Slot of the overlay's pipeline in [`Renderer::overlay_pipelines`]: bit 0 is
/// line topology, bit 1 is [`OverlayDepth::Free`], bit 2 is BGRA output.
fn overlay_pipeline_index(overlay: &Overlay, channels: Channels) -> usize {
    usize::from(overlay.lines)
        | (usize::from(overlay.depth == OverlayDepth::Free) << 1)
        | (channels.index() << 2)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glam::{Mat4, Vec3};

    use crate::assets::{Material, Vertex};
    use crate::coords::hex_srgb;
    use crate::frame::{
        Camera, DirectionalLight, Draw, FixtureCone, FixtureLightingDomain, Frame,
        MaterialTextures, MeshData,
    };
    use crate::light_index::cone_reaches_sphere;
    use crate::overlay::{Overlay, OverlayDepth};
    use crate::scene_desc::DebugView;

    use super::{
        bucket_kind_sums, bucket_offsets, cascade_matrices, compact_rebuild_reasons, downsample,
        normalized_interval_identity, phased_residual_groups, resident_transport_dirty,
        residual_resident_key, residual_transport_key, sanitize_fixture_cone,
        select_fixture_lighting_domain, shader, specialize_wgsl_overrides, supports_haze_subgroups,
        Channels, CompactRebuildInputs, CompositeUniform, DiagnosticLightingDomain,
        FixtureShadowMatrix, Globals, Gpu, HazeUniform, LightCore, LightRest, Renderer,
        ResidualGlobalKey, ResidualTemporalState, SurfaceClusterUniform, TextureEncoding,
        Transport, CAMERA_FAR, CAMERA_NEAR, CASCADE_COUNT, COMPACT_REBUILD_ARGS_ABSENT,
        COMPACT_REBUILD_COUNTERS_ABSENT, COMPACT_REBUILD_DIRTY_THRESHOLD, COMPACT_REBUILD_GROWN,
        COMPACT_REBUILD_POOL_STALE, COMPACT_REBUILD_REUSE_OFF, RESID_BUCKETS,
        RESID_TEMPORAL_RESET_CLASSIFY, RESID_TEMPORAL_RESET_KEY, RESID_TEMPORAL_RESET_TIME,
        SHADOW_SIZE,
    };

    fn residual_key(seed: u32) -> ResidualGlobalKey {
        ResidualGlobalKey {
            output: [seed, 2, 3, 4],
            camera: [0; 8],
            medium: [0; 16],
            fog_far: 0,
            legacy_topology: vec![seed],
            shadows: [true, true],
            casters: 0,
            depth: 0,
            shadow_samples: 2,
        }
    }

    #[test]
    fn wgsl_override_baking_preserves_defaults_and_supplied_types() {
        let source = "override A: bool = false;\noverride B: u32 = 7u;\noverride C: f32 = 2.5;\noverride D: i32 = -3;";
        let baked =
            specialize_wgsl_overrides(source, &[("A", 1.0), ("B", 11.0), ("C", 4.0), ("D", -9.0)]);
        assert_eq!(
            baked,
            "const A: bool = true;\nconst B: u32 = 11u;\nconst C: f32 = 4.0;\nconst D: i32 = -9i;"
        );
        assert_eq!(
            specialize_wgsl_overrides(source, &[]),
            "const A: bool = false;\nconst B: u32 = 7u;\nconst C: f32 = 2.5;\nconst D: i32 = -3;"
        );
    }

    #[test]
    fn residual_temporal_schedule_bounds_75_and_50_hz_age() {
        let mut state = ResidualTemporalState::default();
        let first = state.plan(4, 0.065, 0.0, residual_key(1), false, false);
        assert!(first.full);
        assert_eq!((first.period, first.selected_mask), (1, 0xF));
        for (index, time) in [1.0 / 75.0, 2.0 / 75.0, 3.0 / 75.0, 4.0 / 75.0]
            .into_iter()
            .enumerate()
        {
            let plan = state.plan(4, 0.065, time, residual_key(1), false, false);
            assert!(!plan.full);
            assert_eq!((plan.period, plan.phase), (4, index as u32));
            assert_eq!(plan.selected_mask, 1 << index);
            assert!(plan.max_age <= 0.040_001);
        }

        let mut state = ResidualTemporalState::default();
        assert!(
            state
                .plan(4, 0.065, 0.0, residual_key(1), false, false)
                .full
        );
        for (index, time) in [0.02, 0.04, 0.06, 0.08].into_iter().enumerate() {
            let plan = state.plan(4, 0.065, time, residual_key(1), false, false);
            assert!(!plan.full);
            assert_eq!(plan.phase, index as u32);
            assert_eq!(plan.selected_mask, 1 << index);
            assert!(plan.max_age <= 0.060_001);
        }
    }

    #[test]
    fn residual_temporal_period8_bounds_75_and_50_hz_age() {
        for (hz, expected_max_age) in [(75.0, 7.0 / 75.0), (50.0, 7.0 / 50.0)] {
            let mut state = ResidualTemporalState::default();
            let first = state.plan(8, 0.150, 0.0, residual_key(1), false, true);
            assert_eq!(
                (first.period, first.selected_mask, first.full),
                (1, 0xFF, true)
            );
            for frame in 1..=16 {
                let plan = state.plan(8, 0.150, frame as f32 / hz, residual_key(1), false, true);
                assert_eq!((plan.period, plan.phase), (8, (frame - 1) & 7));
                assert_eq!(plan.selected_mask, 1 << plan.phase);
                assert!(!plan.full);
                assert!(plan.max_age <= expected_max_age + 0.000_001);
            }
        }
    }

    #[test]
    fn residual_temporal_period8_catches_up_upper_phases_without_rebasing() {
        let mut state = ResidualTemporalState::default();
        assert_eq!(
            state
                .plan(8, 0.150, 0.0, residual_key(1), false, true)
                .selected_mask,
            0xFF
        );
        let phase0 = state.plan(8, 0.150, 0.100, residual_key(1), false, true);
        assert_eq!((phase0.phase, phase0.selected_mask), (0, 0x01));
        let caught_up = state.plan(8, 0.150, 0.170, residual_key(1), false, true);
        assert_eq!((caught_up.period, caught_up.phase), (8, 1));
        assert_eq!(caught_up.selected_mask, 0xFE);
        assert_eq!(state.next_phase, 2);
        assert!(caught_up.max_age <= 0.070_001);
    }

    #[test]
    fn residual_temporal_period8_time_inversion_forces_full_upper_half_reset() {
        let mut state = ResidualTemporalState::default();
        state.plan(8, 0.150, 1.0, residual_key(1), false, true);
        state.plan(8, 0.150, 1.02, residual_key(1), false, true);
        let inverted = state.plan(8, 0.150, 1.01, residual_key(1), false, true);
        assert_eq!((inverted.period, inverted.selected_mask), (1, 0xFF));
        assert!(inverted.full);
        assert_ne!(inverted.reset_reasons & RESID_TEMPORAL_RESET_TIME, 0);
    }

    #[test]
    fn residual_temporal_period8_queue_pair_and_dirty_conservation() {
        let bucket_selected = |bucket: u32, mask: u32| {
            let phase4 = bucket & 3;
            mask & ((1 << phase4) | (1 << (phase4 + 4))) != 0
        };
        let evaluates = |index: u32, lanes: u32, mask: u32, dirty: bool| {
            let member_phase = (index / lanes) & 7;
            dirty || mask & (1 << member_phase) != 0
        };
        for phase in 0..8 {
            let mask = 1 << phase;
            for bucket in 0..4 {
                assert_eq!(bucket_selected(bucket, mask), bucket == (phase & 3));
            }
            for group in 0..32 {
                assert_eq!(evaluates(group * 64, 64, mask, false), group & 7 == phase);
                assert!(evaluates(group * 64, 64, mask, true));
            }
        }
        assert!((0..4).all(|bucket| bucket_selected(bucket, 0xFF)));
    }

    #[test]
    fn residual_temporal_mapping_visits_each_logical_group_once() {
        for (count, lanes) in [(0_u32, 64_u32), (1, 64), (63, 64), (65, 64), (521, 32)] {
            let logical = count.div_ceil(lanes);
            let mut seen = vec![0u8; logical as usize];
            for phase in 0..4 {
                let physical = phased_residual_groups(count, lanes, 4, phase);
                for group in 0..physical {
                    let source = group * 4 + phase;
                    assert!(source < logical);
                    seen[source as usize] += 1;
                }
            }
            assert!(seen.into_iter().all(|visits| visits == 1));
        }
    }

    #[test]
    fn residual_temporal_schedule_catches_up_only_overage_phases() {
        let mut state = ResidualTemporalState::default();
        assert!(state.plan(4, 0.065, 0.0, residual_key(1), false, true).full);
        let phase = state.plan(4, 0.065, 0.060, residual_key(1), false, true);
        assert_eq!(
            (phase.period, phase.phase, phase.selected_mask),
            (4, 0, 0b0001)
        );
        assert!(!phase.full);
        let caught_up = state.plan(4, 0.065, 0.075, residual_key(1), false, true);
        assert!(!caught_up.full);
        assert_eq!((caught_up.period, caught_up.phase), (4, 1));
        assert_eq!(caught_up.selected_mask, 0b1110);
        assert_eq!(caught_up.reset_reasons, 0);
        assert!(caught_up.max_age <= 0.015_001);

        let next = state.plan(4, 0.065, 0.090, residual_key(1), false, true);
        assert_eq!((next.phase, next.selected_mask), (2, 0b0100));
        assert!(!next.full);
    }

    #[test]
    fn residual_temporal_unmasked_irregular_cadence_refreshes_every_expired_phase() {
        let mut state = ResidualTemporalState::default();
        assert!(
            state
                .plan(4, 0.065, 0.0, residual_key(1), false, false)
                .full
        );
        let phase = state.plan(4, 0.065, 0.060, residual_key(1), false, false);
        assert_eq!(
            (phase.phase, phase.selected_mask, phase.full),
            (0, 0b0001, false)
        );
        let expired = state.plan(4, 0.065, 0.075, residual_key(1), false, false);
        assert_eq!((expired.period, expired.selected_mask), (1, 0xF));
        assert!(expired.full);
        assert_ne!(expired.reset_reasons & RESID_TEMPORAL_RESET_TIME, 0);
    }

    #[test]
    fn residual_temporal_schedule_resets_on_key_classify_and_bad_time() {
        let mut state = ResidualTemporalState::default();
        assert!(
            state
                .plan(4, 0.065, 1.0, residual_key(1), false, false)
                .full
        );
        assert!(
            !state
                .plan(4, 0.065, 1.01, residual_key(1), false, false)
                .full
        );

        let key = state.plan(4, 0.065, 1.02, residual_key(2), false, false);
        assert!(key.full);
        assert_ne!(key.reset_reasons & RESID_TEMPORAL_RESET_KEY, 0);
        let classify = state.plan(4, 0.065, 1.03, residual_key(2), true, false);
        assert!(classify.full);
        assert_ne!(classify.reset_reasons & RESID_TEMPORAL_RESET_CLASSIFY, 0);
        let backward = state.plan(4, 0.065, 1.02, residual_key(2), false, false);
        assert!(backward.full);
        assert_ne!(backward.reset_reasons & RESID_TEMPORAL_RESET_TIME, 0);
        let nonfinite = state.plan(4, 0.065, f32::NAN, residual_key(2), false, false);
        assert!(nonfinite.full);
        assert_ne!(nonfinite.reset_reasons & RESID_TEMPORAL_RESET_TIME, 0);
    }

    #[test]
    fn haze_subgroups_require_one_group_of_32_threads() {
        let subgroup = wgpu::Features::SUBGROUP;
        assert!(!supports_haze_subgroups(
            wgpu::Features::empty(),
            64,
            false,
            false
        ));
        assert!(!supports_haze_subgroups(subgroup, 8, false, false));
        assert!(!supports_haze_subgroups(subgroup, 16, false, false));
        assert!(supports_haze_subgroups(subgroup, 32, false, false));
        assert!(supports_haze_subgroups(subgroup, 64, false, false));

        // wgpu-hal reports conservative [4, 64] limits for Metal. Apple
        // documents 32-lane SIMD-groups on native Apple GPUs; x86/Rosetta and
        // non-Metal adapters keep the exact fused/stub fallback.
        assert!(supports_haze_subgroups(subgroup, 4, true, true));
        assert!(!supports_haze_subgroups(subgroup, 4, true, false));
        assert!(!supports_haze_subgroups(subgroup, 4, false, true));
    }

    #[test]
    fn haze_jitter_advances_beyond_eight_frames() {
        // Execute the renderer's actual WGSL, not a CPU reimplementation.
        // The old toroidal tile walk repeated exactly every eight frames and
        // prevented both temporal history and offline references converging.
        let transport = include_str!("shaders/beam_transport.wgsl");
        let start = transport.find("const BLUE_NOISE_RANK =").unwrap();
        let end = transport.find("/// One fragment's camera ray").unwrap();
        let source = format!(
            "{}\n{}",
            &transport[start..end],
            r"
            @group(0) @binding(0) var<storage, read_write> samples: array<vec2<f32>>;
            @compute @workgroup_size(64)
            fn probe(@builtin(global_invocation_id) id: vec3<u32>) {
                let pixel = vec2<f32>(23.5, 45.5);
                samples[id.x] = vec2<f32>(blue_noise(pixel, id.x), blue_noise(pixel, id.x + 8u));
            }"
        );
        let gpu = super::Gpu::shared().unwrap();
        let device = &gpu.device;
        let module = super::shader(device, "haze-jitter-probe", &source);
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("haze-jitter-probe"),
            layout: None,
            module: &module,
            entry_point: Some("probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let buffer = |usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("haze-jitter-samples"),
                size: 64 * 8,
                usage,
                mapped_at_creation: false,
            })
        };
        let samples = buffer(wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
        let readback = buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("haze-jitter-probe"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[super::binding(0, samples.as_entire_binding())],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&samples, 0, &readback, 0, 64 * 8);
        gpu.queue.submit([encoder.finish()]);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let view = readback.slice(..).get_mapped_range().unwrap();
        let values: &[[f32; 2]] = bytemuck::cast_slice(&view);
        assert!(values
            .iter()
            .all(|v| v.iter().all(|x| (0.0..1.0).contains(x))));
        assert!(
            values.iter().all(|v| (v[0] - v[1]).abs() > 0.01),
            "the shadow integration repeats its samples after eight frames"
        );
    }

    #[test]
    fn profiled_capture_preserves_live_pixels_through_shadow_updates() -> anyhow::Result<()> {
        let mut profiled = Renderer::new_profiled()?;
        let mut live = Renderer::new()?;
        let mut frame = fixture_surface_frame(1);
        frame.geometry_shadows = true;
        frame.haze_density = 0.3;
        frame.haze_steps = 8;
        frame.haze_resolution = 1.0;
        let mut pixels = Vec::new();
        for (step, size) in [[96, 72], [96, 72], [103, 77], [103, 77]]
            .into_iter()
            .enumerate()
        {
            if step > 0 {
                frame.fixture_cones[0].position.x += 0.01;
            }
            let timing = profiled.profile_live_into(&frame, size[0], size[1], 2, &mut pixels)?;
            let expected = live.render_next(&frame, size[0], size[1], 2)?;
            assert_eq!(pixels, expected, "profiled pixels differ at update {step}");
            assert_eq!(pixels.len(), (size[0] * size[1] * 4) as usize);
            assert!(timing.gpu_total_ms.is_finite() && timing.gpu_total_ms >= 0.0);
            let cpu = timing.cpu;
            assert_eq!(
                cpu.prepare + cpu.clusters + cpu.upload + cpu.targets + cpu.encode,
                cpu.total,
            );
            let shadows = cpu.fixture_shadows;
            assert!(shadows.globals <= cpu.upload);
            assert!(
                shadows.cull
                    + shadows.buckets
                    + shadows.resources
                    + shadows.maps
                    + shadows.hierarchy
                    + cpu.submission.staging
                    + cpu.submission.finish
                    + cpu.submission.submit
                    <= cpu.encode,
            );
            assert!(profiled.shadow_stats().redrawn_maps > 0);
            assert_eq!(profiled.shadow_stats(), live.shadow_stats());
        }
        Ok(())
    }

    #[test]
    fn surface_profiling_keeps_live_targets_and_reports_every_frame() -> anyhow::Result<()> {
        let mut renderer = Renderer::new_profiled()?;
        let mut frame = fixture_surface_frame(1);
        frame.geometry_shadows = true;
        frame.fixture_shadows = true;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;
        for size in [[96, 72], [96, 72], [103, 77], [103, 77]] {
            let timing = renderer.profile_live_surface(&frame, size[0], size[1], 2)?;
            assert!(timing.gpu_total_ms.is_finite() && timing.gpu_total_ms > 0.0);
            let targets = renderer.targets.as_ref().expect("rendered targets");
            assert!(targets.destination == super::Destination::Compositor);
            assert_eq!([targets.width, targets.height], size);
            #[cfg(target_os = "macos")]
            assert!(matches!(
                targets.presentations[0],
                super::PresentationTarget::Shared(_)
            ));
        }
        Ok(())
    }

    #[test]
    fn volumetric_cpu_layouts_match_wgsl_storage_and_uniform_strides() {
        assert_eq!(std::mem::size_of::<Globals>(), 512);
        assert_eq!(std::mem::size_of::<HazeUniform>(), 240);
        assert_eq!(std::mem::size_of::<CompositeUniform>(), 208);
        assert_eq!(std::mem::size_of::<LightCore>(), 16);
        assert_eq!(std::mem::size_of::<LightRest>(), 64);
        assert_eq!(std::mem::size_of::<FixtureShadowMatrix>(), 80);
        assert_eq!(std::mem::size_of::<SurfaceClusterUniform>(), 32);
        assert_eq!(std::mem::size_of::<super::CompactUniform>(), 4608);
        assert_eq!(super::COMPACT_UNIFORM_STRIDE % 256, 0);
    }

    #[test]
    fn scalar_residual_values_use_one_word_per_entry() {
        for period in [1, 4] {
            assert_eq!(super::residual_value_stride(period), 1);
            assert_eq!(super::residual_value_bytes(20 << 20, period), 80 << 20);
            assert_eq!(super::residual_value_bytes(24 << 20, period), 96 << 20);
        }
        assert_eq!(super::residual_value_stride(0), 3);
        assert_eq!(super::residual_value_bytes(20 << 20, 0), 240 << 20);
        assert_eq!(super::residual_value_bytes(24 << 20, 0), 288 << 20);
    }

    #[test]
    fn whole_scalar_tail_obeys_combined_budget_and_binding_limits() {
        const MIB: u64 = 1 << 20;
        let layout = super::whole_tail_layout(128 * MIB, 256 * MIB, 24 * MIB, 38 * MIB);
        assert_eq!(layout.capacity, 3_728_270);
        assert_eq!(
            u64::from(layout.capacity) * super::WHOLE_RECORD_BYTES,
            128 * MIB - 8
        );
        assert!(layout.rgb_bytes <= 256 * MIB);
        assert!(layout.aux_bytes <= 256 * MIB);

        let disabled = super::whole_tail_layout(128 * MIB, 192 * MIB, 24 * MIB, 56 * MIB);
        assert_eq!(disabled.capacity, 0);
        assert_eq!(disabled.aux_bytes, 224 * MIB);
    }

    #[test]
    fn whole_scalar_namespaces_follow_resident_reservations() {
        assert_eq!(super::COMPACT_COUNTER_WORDS, 80);
        assert_eq!(super::WHOLE_COUNTER_BASE, 64);
        assert_eq!(super::WHOLE_ARGS_BASE, 104);
        assert_eq!(super::COMPACT_ARGS_WORDS, 110);
        assert_eq!(super::WHOLE_GC_ARGS_OFFSET, 104 * 4);
        assert_eq!(super::WHOLE_REFRESH_ARGS_OFFSET, 107 * 4);
    }

    #[test]
    fn residual_rebuild_does_not_reset_independent_whole_phase_age() {
        let mut residual = ResidualTemporalState::default();
        let mut whole = ResidualTemporalState::default();
        assert!(
            residual
                .plan(4, 0.065, 1.0, residual_key(1), false, false)
                .full
        );
        assert!(
            whole
                .plan(4, 0.065, 1.0, residual_key(1), false, false)
                .full
        );

        let residual_gc = residual.plan(4, 0.065, 1.01, residual_key(1), true, false);
        let whole_after_gc = whole.plan(4, 0.065, 1.01, residual_key(1), false, false);
        assert!(residual_gc.full);
        assert!(!whole_after_gc.full);
        assert_eq!((whole_after_gc.period, whole_after_gc.phase), (4, 0));
    }

    #[test]
    fn compact_rebuild_reasons_preserve_default_threshold_and_hard_invalidations() {
        let clean = CompactRebuildInputs {
            reuse_allowed: true,
            pool_stale: false,
            counters_present: true,
            args_present: true,
            grown: false,
            dirty_slots: 17,
            live_slots: 391,
        };
        assert_eq!(compact_rebuild_reasons(clean), 0);
        assert_eq!(
            compact_rebuild_reasons(CompactRebuildInputs {
                dirty_slots: 87,
                live_slots: 174,
                ..clean
            }),
            COMPACT_REBUILD_DIRTY_THRESHOLD
        );
        assert_eq!(
            compact_rebuild_reasons(CompactRebuildInputs {
                live_slots: 0,
                dirty_slots: 0,
                ..clean
            }),
            COMPACT_REBUILD_DIRTY_THRESHOLD
        );

        for (input, reason) in [
            (
                CompactRebuildInputs {
                    reuse_allowed: false,
                    ..clean
                },
                COMPACT_REBUILD_REUSE_OFF,
            ),
            (
                CompactRebuildInputs {
                    pool_stale: true,
                    ..clean
                },
                COMPACT_REBUILD_POOL_STALE,
            ),
            (
                CompactRebuildInputs {
                    counters_present: false,
                    ..clean
                },
                COMPACT_REBUILD_COUNTERS_ABSENT,
            ),
            (
                CompactRebuildInputs {
                    args_present: false,
                    ..clean
                },
                COMPACT_REBUILD_ARGS_ABSENT,
            ),
            (
                CompactRebuildInputs {
                    grown: true,
                    ..clean
                },
                COMPACT_REBUILD_GROWN,
            ),
        ] {
            assert_eq!(compact_rebuild_reasons(input), reason);
        }
    }

    #[test]
    fn compact_dense_capacity_clamps_hint_without_rejecting_supported_active_slots() {
        let blocks = 1_000u32;
        let bytes_per_dense = u64::from(blocks) * 8;
        assert_eq!(
            super::compact_dense_capacity(190, 470, blocks, bytes_per_dense * 512),
            Some(480)
        );
        assert_eq!(
            super::compact_dense_capacity(190, 512, blocks, bytes_per_dense * 256),
            Some(256)
        );
        assert_eq!(
            super::compact_dense_capacity(270, 512, blocks, bytes_per_dense * 256),
            None
        );
        assert_eq!(
            super::compact_dense_capacity(190, 0, blocks, bytes_per_dense * 512),
            Some(192)
        );
    }

    #[test]
    fn compact_dense_capacity_rejects_zero_or_overflowed_plane_shapes() {
        assert_eq!(super::compact_dense_capacity(1, 1, 0, u64::MAX), None);
        assert_eq!(
            super::compact_dense_capacity(u32::MAX, 512, 1, u64::MAX),
            None
        );
        assert_eq!(super::compact_dense_capacity(1, 1, u32::MAX, 7), None);
    }

    #[test]
    fn compact_list_rebuild_requires_entries_appended_since_full_build() {
        assert!(!super::compact_list_needs_rebuild(
            16_860_000,
            16_860_000,
            20 << 20
        ));
        assert!(!super::compact_list_needs_rebuild(
            16_900_000,
            16_860_000,
            24 << 20
        ));
        assert!(super::compact_list_needs_rebuild(
            16_900_000,
            16_860_000,
            20 << 20
        ));
        assert!(super::compact_list_needs_rebuild(1_501, 1_000, 20 << 20));
    }

    #[test]
    fn reverse_z_projection_is_monotonic_and_maps_bounded_planes() {
        let projection =
            Mat4::perspective_rh(48f32.to_radians(), 16.0 / 9.0, CAMERA_FAR, CAMERA_NEAR);
        let depth = |distance: f32| projection.project_point3(Vec3::new(0.0, 0.0, -distance)).z;
        assert!((depth(CAMERA_NEAR) - 1.0).abs() < 1e-5);
        assert!(depth(CAMERA_FAR).abs() < 1e-6);
        let samples = [CAMERA_NEAR, 1.0, 10.0, 100.0, CAMERA_FAR];
        assert!(samples
            .windows(2)
            .all(|pair| depth(pair[0]) > depth(pair[1])));
    }

    #[test]
    fn cascades_are_finite_bounded_and_texel_stable_under_small_translation() {
        let eye = Vec3::new(4.5, -5.0, 3.0);
        let forward = (Vec3::new(0.0, 0.8, 0.0) - eye).normalize();
        let to_light = Vec3::new(2.0, -3.0, 6.0).normalize();
        let matrices = cascade_matrices(eye, forward, 48f32.to_radians(), 16.0 / 9.0, to_light);
        assert_eq!(matrices.len(), CASCADE_COUNT);
        assert!(matrices.iter().all(Mat4::is_finite));

        // Move perpendicular to the light by much less than a near-cascade
        // texel. The snapped world-to-shadow transform must not shimmer.
        let lateral = to_light.cross(Vec3::Z).normalize_or(Vec3::X);
        let shifted = cascade_matrices(
            eye + lateral * (1.0 / SHADOW_SIZE as f32),
            forward,
            48f32.to_radians(),
            16.0 / 9.0,
            to_light,
        );
        let max_delta = matrices[0]
            .to_cols_array()
            .into_iter()
            .zip(shifted[0].to_cols_array())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_delta < 1e-5,
            "sub-texel camera motion changed the near cascade by {max_delta}"
        );
    }

    #[test]
    fn material_mips_filter_color_in_linear_light_and_data_as_bytes() {
        let pixels = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let (_, _, color) = downsample(2, 2, &pixels, TextureEncoding::Srgb);
        let (_, _, data) = downsample(2, 2, &pixels, TextureEncoding::Linear);
        assert_eq!(color, [188, 188, 188, 255]);
        assert_eq!(data, [128, 128, 128, 255]);
    }

    #[test]
    fn post_agx_overlay_keeps_authored_srgb_across_scene_lighting_and_output_formats(
    ) -> anyhow::Result<()> {
        const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
        let mut renderer = Renderer::new()?;
        let dark = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::ZERO);
        let lit = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::splat(40.0));

        let mut rgba_dark = Vec::new();
        renderer.render_into(&dark, 96, 96, 1, Channels::Rgba, &mut rgba_dark)?;
        let mut rgba_lit = Vec::new();
        renderer.render_into(&lit, 96, 96, 1, Channels::Rgba, &mut rgba_lit)?;
        let mut bgra_lit = Vec::new();
        renderer.render_into(&lit, 96, 96, 1, Channels::Bgra, &mut bgra_lit)?;

        assert_eq!(center_rgb(&rgba_dark, Channels::Rgba), AUTHORED);
        assert_eq!(center_rgb(&rgba_lit, Channels::Rgba), AUTHORED);
        assert_eq!(center_rgb(&bgra_lit, Channels::Bgra), AUTHORED);
        let bgra_offset = center_offset(96, 96);
        assert_eq!(
            &bgra_lit[bgra_offset..bgra_offset + 4],
            &[AUTHORED[2], AUTHORED[1], AUTHORED[0], 255]
        );
        Ok(())
    }

    #[test]
    fn post_agx_overlays_load_reverse_z_depth_while_free_gizmos_ignore_it() -> anyhow::Result<()> {
        const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
        let mut renderer = Renderer::new()?;
        let occluder_only = overlay_test_frame(true, OverlayDepth::Tested, 1.0, Vec3::ZERO);
        let mut without_overlay = overlay_test_frame(true, OverlayDepth::Tested, 1.0, Vec3::ZERO);
        without_overlay.overlays.clear();

        let mut background = Vec::new();
        renderer.render_into(&without_overlay, 96, 96, 1, Channels::Rgba, &mut background)?;
        let mut tested_behind = Vec::new();
        renderer.render_into(
            &occluder_only,
            96,
            96,
            1,
            Channels::Rgba,
            &mut tested_behind,
        )?;
        assert_eq!(
            center_rgb(&tested_behind, Channels::Rgba),
            center_rgb(&background, Channels::Rgba),
            "a tested cage behind opaque geometry must remain occluded"
        );

        let tested_front = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::ZERO);
        let mut front = Vec::new();
        renderer.render_into(&tested_front, 96, 96, 1, Channels::Rgba, &mut front)?;
        assert_eq!(center_rgb(&front, Channels::Rgba), AUTHORED);

        let free_behind = overlay_test_frame(true, OverlayDepth::Free, 1.0, Vec3::ZERO);
        let mut free = Vec::new();
        renderer.render_into(&free_behind, 96, 96, 1, Channels::Rgba, &mut free)?;
        assert_eq!(
            center_rgb(&free, Channels::Rgba),
            AUTHORED,
            "a free gizmo must use Always and paint over opaque geometry"
        );
        Ok(())
    }

    #[test]
    fn clustered_fixture_light_is_local_and_surface_toggle_is_independent() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(1);
        frame.fixture_cones[0].color = Vec3::new(1.0, 0.08, 0.02);
        let lit = renderer.render(&frame, 160, 120, 1)?;
        frame.fixture_surface_lighting = false;
        let dark = renderer.render(&frame, 160, 120, 1)?;

        let central = region_mean(&lit, 160, 52..108, 38..94);
        let central_dark = region_mean(&dark, 160, 52..108, 38..94);
        let corner = region_mean(&lit, 160, 0..24, 88..120);
        assert!(
            central > central_dark + 2.0,
            "fixture surface light did not contribute locally: {central:.2} vs {central_dark:.2}"
        );
        assert!(
            central > corner + 1.0,
            "finite cone illuminated the whole surface: center {central:.2}, corner {corner:.2}"
        );
        Ok(())
    }

    /// The case the relative assertions above cannot see: a stock rig's
    /// throw, at magnitude. A real moving head sits ~6 m over a dark floor
    /// with its dimmer at full — `intensity` ≈ 1, not the hot 3.0 the
    /// local-contribution case uses — and its pool must be plainly *visible*,
    /// not merely nonzero. This is the shared-radiance-scale contract: the
    /// surface pass and the haze march read the same cones, and a surface
    /// term that loses the beam gain reads as "bright shaft over a black
    /// pool" while every relative assertion still passes.
    #[test]
    fn fixture_pool_is_visible_at_a_real_rig_throw() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(1);
        frame.fixture_cones[0].position = Vec3::new(0.0, 0.0, 6.0);
        frame.fixture_cones[0].range = 10.0;
        frame.fixture_cones[0].intensity = 1.0;
        frame.draws[0].material.base_color = Vec3::splat(0.2);
        let lit = renderer.render(&frame, 160, 120, 1)?;
        frame.fixture_surface_lighting = false;
        let dark = renderer.render(&frame, 160, 120, 1)?;
        let central = region_mean(&lit, 160, 52..108, 38..94);
        let central_dark = region_mean(&dark, 160, 52..108, 38..94);
        assert!(
            central > central_dark + 25.0,
            "a full-dimmer pool at a 6 m throw must be plainly visible: \
             {central:.2} vs {central_dark:.2}"
        );
        Ok(())
    }

    #[test]
    fn clustered_surface_uses_shared_cone_and_gobo_photometry() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let open = fixture_surface_frame(1);
        let open_pixels = renderer.render(&open, 160, 120, 1)?;
        let mut spokes = fixture_surface_frame(1);
        spokes.fixture_cones[0].gobo = 1;
        spokes.fixture_cones[0].gobo_rotation = 0.31;
        let spokes_pixels = renderer.render(&spokes, 160, 120, 1)?;
        let mut outside = fixture_surface_frame(1);
        outside.fixture_cones[0].direction = Vec3::new(0.0, 1.0, -0.08).normalize();
        let outside_pixels = renderer.render(&outside, 160, 120, 1)?;

        let open_energy = region_mean(&open_pixels, 160, 42..118, 32..104);
        let spokes_energy = region_mean(&spokes_pixels, 160, 42..118, 32..104);
        let outside_energy = region_mean(&outside_pixels, 160, 42..118, 32..104);
        assert!(
            open_energy > spokes_energy + 0.5,
            "shared gobo did not remove surface energy: {open_energy:.2} vs {spokes_energy:.2}"
        );
        assert!(
            open_energy > outside_energy + 1.0,
            "shared cone cutoff did not reject an off-axis surface: {open_energy:.2} vs {outside_energy:.2}"
        );
        Ok(())
    }

    #[test]
    fn fixture_geometry_casts_a_shadow_through_the_volumetric_integral() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(1);
        frame.fixture_cones[0].position = Vec3::new(0.0, 0.0, 3.0);
        frame.draws.push(Draw {
            mesh: 0,
            model: Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
                * Mat4::from_scale(Vec3::new(0.09, 0.09, 0.09)),
            material: Material {
                base_color: Vec3::splat(0.08),
                roughness: 0.8,
                ..Material::default()
            },
            textures: MaterialTextures::default(),
            editor_object: None,
        });
        frame.camera = Camera {
            eye: Vec3::new(3.8, -6.0, 3.2),
            target: Vec3::new(0.0, 0.0, 1.35),
            fov_y_deg: 45.0,
        };
        frame.ambient = Vec3::ZERO;
        frame.haze_steps = 8;
        frame.haze_resolution = 1.0;
        let occluder_ndc = super::fixture_shadow_matrix(&frame.fixture_cones[0])
            .project_point3(Vec3::new(0.0, 0.0, 2.0));
        assert!(
            occluder_ndc.x.abs() < 1.0
                && occluder_ndc.y.abs() < 1.0
                && (0.0..=1.0).contains(&occluder_ndc.z),
            "the shadow contract's occluder must remain inside the fixture projection: {occluder_ndc:?}"
        );

        frame.haze_density = 0.0;
        frame.fixture_surface_lighting = true;
        frame.fixture_shadows = false;
        let surface_open = renderer.render(&frame, 320, 240, 1)?;
        frame.fixture_shadows = true;
        let surface_shadowed = renderer.render(&frame, 320, 240, 1)?;
        let surface_changed = surface_open
            .chunks_exact(4)
            .zip(surface_shadowed.chunks_exact(4))
            .filter(|(left, right)| left[..3] != right[..3])
            .count();
        assert!(
            surface_changed > 300,
            "the fixture shadow map did not alter a lit surface: {surface_changed} pixels"
        );

        frame.haze_density = 0.8;
        frame.fixture_surface_lighting = false;
        frame.fixture_shadows = false;
        let open = renderer.render(&frame, 320, 240, 2)?;
        frame.fixture_shadows = true;
        let shadowed = renderer.render(&frame, 320, 240, 2)?;

        let changed = open
            .chunks_exact(4)
            .zip(shadowed.chunks_exact(4))
            .filter(|(left, right)| left[..3] != right[..3])
            .count();
        let energy = |pixels: &[u8]| {
            pixels
                .chunks_exact(4)
                .map(|pixel| u64::from(pixel[0]) + u64::from(pixel[1]) + u64::from(pixel[2]))
                .sum::<u64>()
        };
        assert!(
            changed > 300,
            "the fixture shadow map did not alter a visible volume: {changed} pixels"
        );
        assert!(
            energy(&shadowed) < energy(&open),
            "shadow visibility added energy instead of removing in-scatter"
        );
        Ok(())
    }

    #[test]
    fn shared_fog_visibility_updates_after_a_caster_moves() -> anyhow::Result<()> {
        let mut frame = fixture_surface_frame(1);
        let mut light = frame.fixture_cones[0];
        light.position = Vec3::new(0.0, 0.0, 8.0);
        light.range = 24.0;
        light.wash = 0.9;
        light.cos_field = 0.6;
        light.cos_beam = 0.85;
        light.intensity = 1.0;
        let mut decoy = light;
        decoy.direction = Vec3::Z;
        frame.fixture_cones = vec![decoy; 300];
        frame.fixture_cones.push(light);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.8;
        frame.haze_steps = 8;
        frame.camera.eye = Vec3::new(0.0, -12.0, 6.0);
        frame.draws.push(Draw {
            mesh: 0,
            model: Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0))
                * Mat4::from_scale(Vec3::splat(0.25)),
            material: Material::default(),
            textures: MaterialTextures::default(),
            editor_object: None,
        });
        let mut renderer = Renderer::new()?;
        let blocked = renderer.render(&frame, 160, 120, 1)?;
        assert_eq!(
            renderer
                .targets
                .as_ref()
                .unwrap()
                .fog
                .integral
                .texture()
                .depth_or_array_layers(),
            crate::fog_grid::SLICES + 1
        );
        frame.draws[1].model = Mat4::from_translation(Vec3::new(30.0, 0.0, 5.0));
        let open = renderer.render(&frame, 160, 120, 1)?;
        let energy = |p: &[u8]| {
            p.chunks_exact(4)
                .map(|p| p[..3].iter().map(|v| u64::from(*v)).sum::<u64>())
                .sum::<u64>()
        };
        assert!(
            energy(&blocked) < energy(&open),
            "the shared grid retained an obsolete caster shadow"
        );
        Ok(())
    }

    #[test]
    #[ignore = "serial Metal assertion test; mutates fog-cache environment"]
    fn fog_visibility_cache_asserts_lifecycle_and_detects_poison() -> anyhow::Result<()> {
        unsafe {
            std::env::set_var("LUMA_FOG_VISIBILITY_CACHE", "1");
            std::env::set_var("LUMA_FOG_VISIBILITY_ASSERT", "1");
            std::env::set_var("LUMA_HAZE_VENUE_DOMAIN", "1");
        }
        let mut frame = fixture_surface_frame(1);
        let light = &mut frame.fixture_cones[0];
        light.position = Vec3::new(0.0, 0.0, 8.0);
        light.range = 24.0;
        light.wash = 0.9;
        light.cos_field = 0.6;
        light.cos_beam = 0.85;
        light.intensity = 1.0;
        frame.fixture_surface_lighting = false;
        frame.geometry_shadows = true;
        frame.haze_density = 0.8;
        frame.haze_steps = 8;
        frame.camera.eye = Vec3::new(0.0, -12.0, 6.0);
        frame.fixture_lighting_domain = Some(FixtureLightingDomain {
            bounds: luma_scene::Aabb::new(Vec3::splat(-100.0), Vec3::splat(100.0)),
            fog_far: 120.0,
            source_camera_eye: frame.camera.eye.to_array(),
        });
        frame.draws.push(Draw {
            mesh: 0,
            model: Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0))
                * Mat4::from_scale(Vec3::splat(0.25)),
            material: Material::default(),
            textures: MaterialTextures::default(),
            editor_object: None,
        });

        let mut renderer = Renderer::new()?;
        renderer.render(&frame, 160, 120, 1)?;
        let initial = renderer.fog_visibility_cache_stats()?;
        assert!(initial.active);
        assert!(initial.payload_used > 0, "{initial:?}");
        assert!(initial.cache_hits > 0, "{initial:?}");
        assert!(initial.cold_shadow_calls > 0, "{initial:?}");
        assert_eq!(initial.raw_mismatches, 0, "{initial:?}");

        renderer.render(&frame, 160, 120, 1)?;
        let stable = renderer.fog_visibility_cache_stats()?;
        assert!(stable.cache_hits > initial.cache_hits, "{stable:?}");
        assert_eq!(stable.raw_mismatches, 0, "{stable:?}");

        assert!(renderer
            .fog_visibility
            .poison_payload_prefix(&renderer.gpu.queue, stable.payload_used));
        renderer.render(&frame, 160, 120, 1)?;
        let poisoned = renderer.fog_visibility_cache_stats()?;
        assert!(poisoned.cache_hits > stable.cache_hits);
        assert!(poisoned.raw_mismatches > 0);

        frame.camera.eye.x += 0.25;
        renderer.render(&frame, 160, 120, 1)?;
        let moved_camera = renderer.fog_visibility_cache_stats()?;
        assert!(moved_camera.global_resets > poisoned.global_resets);
        assert!(moved_camera.cache_hits > 0);
        assert_eq!(moved_camera.raw_mismatches, 0);

        frame.camera.eye.x -= 0.25;
        renderer.render(&frame, 160, 120, 1)?;
        let selected_again = renderer.fog_visibility_cache_stats()?;
        assert!(selected_again.global_resets > moved_camera.global_resets);
        assert!(selected_again.cache_hits > 0);
        assert_eq!(selected_again.raw_mismatches, 0);

        frame.fixture_cones[0].range += 1.0;
        renderer.render(&frame, 160, 120, 1)?;
        let projection = renderer.fog_visibility_cache_stats()?;
        assert_eq!(projection.global_resets, selected_again.global_resets);
        assert!(projection.slot_clears > selected_again.slot_clears);
        assert!(projection.cache_hits > selected_again.cache_hits);
        assert_eq!(projection.raw_mismatches, 0);

        frame.draws[1].model =
            Mat4::from_translation(Vec3::new(2.0, 0.0, 1.5)) * Mat4::from_scale(Vec3::splat(0.4));
        renderer.render(&frame, 160, 120, 1)?;
        let caster = renderer.fog_visibility_cache_stats()?;
        assert!(caster.slot_clears > projection.slot_clears);
        assert!(caster.cache_hits > projection.cache_hits);
        assert_eq!(caster.raw_mismatches, 0);

        let intensity = frame.fixture_cones[0].intensity;
        frame.fixture_cones[0].intensity = 0.0;
        renderer.render(&frame, 160, 120, 1)?;
        let inactive = renderer.fog_visibility_cache_stats()?;
        assert!(!inactive.active);
        assert_eq!(inactive.raw_mismatches, 0);
        frame.fixture_cones[0].intensity = intensity;
        renderer.render(&frame, 160, 120, 1)?;
        let reentry = renderer.fog_visibility_cache_stats()?;
        assert!(reentry.active);
        assert!(reentry.cache_hits > caster.cache_hits);
        assert_eq!(reentry.raw_mismatches, 0);
        Ok(())
    }

    #[test]
    #[ignore = "serial Metal assertion test; allocates both fixture-shadow banks"]
    fn fog_visibility_cache_copies_a_live_slot_across_shadow_banks() -> anyhow::Result<()> {
        unsafe {
            std::env::set_var("LUMA_FOG_VISIBILITY_CACHE", "1");
            std::env::set_var("LUMA_FOG_VISIBILITY_ASSERT", "1");
            std::env::set_var("LUMA_HAZE_VENUE_DOMAIN", "1");
        }
        let mut frame = fixture_surface_frame(512);
        frame.fixture_surface_lighting = false;
        frame.geometry_shadows = true;
        frame.haze_density = 0.8;
        frame.haze_steps = 8;
        frame.fixture_shadow_capacity_hint = 512;
        for (index, light) in frame.fixture_cones.iter_mut().enumerate() {
            if index == 256 {
                continue;
            }
            light.position = Vec3::new(
                50.0 + (index % 16) as f32 * 0.01,
                50.0 + (index / 16) as f32 * 0.01,
                3.0,
            );
            light.range = 0.5;
            light.direction = Vec3::X;
        }
        let light = &mut frame.fixture_cones[256];
        light.position = Vec3::new(0.0, 0.0, 8.0);
        light.range = 24.0;
        light.wash = 0.9;
        light.cos_field = 0.6;
        light.cos_beam = 0.85;
        light.intensity = 1.0;
        frame.camera.eye = Vec3::new(0.0, -12.0, 6.0);
        frame.fixture_lighting_domain = Some(FixtureLightingDomain {
            bounds: luma_scene::Aabb::new(Vec3::splat(-100.0), Vec3::splat(100.0)),
            fog_far: 120.0,
            source_camera_eye: frame.camera.eye.to_array(),
        });
        frame.draws.push(Draw {
            mesh: 0,
            model: Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0))
                * Mat4::from_scale(Vec3::splat(0.25)),
            material: Material::default(),
            textures: MaterialTextures::default(),
            editor_object: None,
        });

        let mut renderer = Renderer::new()?;
        let mut populated = None;
        for _ in 0..16 {
            renderer.render(&frame, 64, 48, 1)?;
            let stats = renderer.fog_visibility_cache_stats()?;
            let source = renderer.fog_visibility.read_slot_words(
                &renderer.gpu.device,
                &renderer.gpu.queue,
                256,
            )?;
            if source.iter().any(|word| *word != 0) {
                populated = Some((stats, source));
                break;
            }
        }
        let (settled, source) =
            populated.expect("second-bank fog visibility slot was not populated in 16 frames");
        assert_eq!(settled.raw_mismatches, 0, "{settled:?}");

        // Light 256's projection is resident in the second texture bank.
        // Giving light zero the same projection makes cached slot assignment
        // place the displaced identical projection in slot zero. The fog cache
        // must copy slot 256's complete directory before the grid reads slot 0.
        frame.fixture_cones[0] = frame.fixture_cones[256].clone();
        renderer.render(&frame, 64, 48, 1)?;
        assert_eq!(renderer.fixture_shadow_slots[0], Some(256));
        assert_eq!(renderer.fixture_shadow_slots[256], Some(0));
        let migrated = renderer.fog_visibility_cache_stats()?;
        assert!(migrated.slot_copies > settled.slot_copies, "{migrated:?}");
        assert!(migrated.cache_hits > settled.cache_hits, "{migrated:?}");
        assert_eq!(migrated.raw_mismatches, 0, "{migrated:?}");
        let destination = renderer.fog_visibility.read_slot_words(
            &renderer.gpu.device,
            &renderer.gpu.queue,
            0,
        )?;
        let source_after = renderer.fog_visibility.read_slot_words(
            &renderer.gpu.device,
            &renderer.gpu.queue,
            256,
        )?;
        for (index, handle) in source.iter().copied().enumerate() {
            if handle != 0 {
                assert_eq!(destination[index], handle);
                assert_eq!(source_after[index], handle);
            }
        }
        Ok(())
    }

    #[test]
    fn sampled_washes_preserve_colour_energy_and_reset_on_cues() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        // Non-multiple of eight exercises the partial reservoir; one much
        // brighter emitter and mixed colours challenge uniform sampling.
        let mut frame = fixture_surface_frame(137);
        frame.geometry_shadows = true;
        frame.haze_density = 0.7;
        frame.haze_steps = 8;
        frame.fixture_surface_lighting = false;
        for (i, light) in frame.fixture_cones.iter_mut().enumerate() {
            light.wash = 0.8;
            light.cos_field = 0.6;
            light.cos_beam = 0.85;
            light.color = if i % 3 == 0 {
                Vec3::X
            } else {
                Vec3::new(0.02, 0.3, 1.0)
            };
        }
        frame.fixture_cones[136].intensity *= 80.0;
        renderer.wide_light_group = 1;
        let reference = renderer.render(&frame, 160, 120, 32)?;
        renderer.wide_light_group = 8;
        let sampled = renderer.render(&frame, 160, 120, 32)?;
        let energy = |p: &[u8], channel: usize| {
            p.chunks_exact(4)
                .map(|p| f64::from(p[channel]))
                .sum::<f64>()
        };
        for channel in 0..3 {
            let expected = energy(&reference, channel);
            let actual = energy(&sampled, channel);
            assert!(expected > 1000.0, "test must contain visible fog");
            assert!(
                (actual - expected).abs() / expected < 0.05,
                "sampling changed channel {channel} energy: {actual} vs {expected}"
            );
        }
        for i in 0..8 {
            frame.time = i as f32 / 60.0;
            renderer.render_next(&frame, 160, 120, 1)?;
        }
        for light in &mut frame.fixture_cones {
            light.color = Vec3::Y;
            light.intensity *= 0.05;
        }
        frame.time += 1.0 / 60.0;
        let cue = renderer.render_next(&frame, 160, 120, 1)?;
        let fresh = renderer.render(&frame, 160, 120, 1)?;
        assert_eq!(cue, fresh, "a colour/dimmer cue retained stale fog history");
        Ok(())
    }

    #[test]
    fn cached_shadows_survive_blackout_reordering_and_colour_changes() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(300);
        frame.geometry_shadows = true;
        frame.haze_density = 0.0;
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(renderer.shadowed_fixture_count(), 300);
        assert_eq!(renderer.shadow_stats().redrawn_maps, 300);
        assert_eq!(renderer.shadow_stats().hierarchy_layers, 300);
        let mut lights = std::mem::take(&mut frame.fixture_cones);
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(renderer.shadow_stats().hierarchy_layers, 0);
        lights.reverse();
        for light in &mut lights {
            light.color = Vec3::X;
            light.intensity *= 0.5;
        }
        frame.fixture_cones = lights;
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(
            renderer.shadow_stats().redrawn_maps,
            0,
            "blackout and colour are not geometry changes"
        );
        assert_eq!(renderer.shadow_stats().hierarchy_layers, 0);
        frame.fixture_cones[0].direction = Vec3::new(0.1, 0.0, -1.0).normalize();
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(
            renderer.shadow_stats().redrawn_maps,
            1,
            "only the moved light needs a new map"
        );
        assert_eq!(renderer.shadow_stats().hierarchy_layers, 1);
        frame.draws[0].model = Mat4::from_translation(Vec3::new(0.0, 0.0, -0.1));
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(
            renderer.shadow_stats().redrawn_maps,
            300,
            "stage edits invalidate old depth"
        );
        assert_eq!(renderer.shadow_stats().hierarchy_layers, 300);
        Ok(())
    }

    #[test]
    fn shadow_capacity_hint_avoids_atlas_growth_during_cue_ramp() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut full = fixture_surface_frame(340);
        full.geometry_shadows = true;
        full.haze_density = 0.0;
        let lights = std::mem::take(&mut full.fixture_cones);
        // Public callers can construct a frame without the scene builder's
        // clamp. The allocator owns its shader/resource bound as well.
        full.fixture_shadow_capacity_hint = usize::MAX;

        renderer.render(&full, 80, 60, 1)?;
        assert_eq!(renderer.fixture_shadow_cache.len(), 512);
        assert_eq!(renderer.shadow_stats().redrawn_maps, 0);
        full.fixture_shadow_capacity_hint = 470;
        let cache_address = renderer.fixture_shadow_cache.as_ptr();
        let mut previous = Vec::new();
        let mut hinted = Vec::new();
        for (count, expected_redraws) in [(34, 34), (210, 176), (340, 130)] {
            full.fixture_cones = lights[..count].to_vec();
            hinted = renderer.render(&full, 80, 60, 1)?;
            assert_eq!(renderer.fixture_shadow_cache.len(), 512);
            assert_eq!(renderer.fixture_shadow_cache.as_ptr(), cache_address);
            assert_eq!(renderer.shadow_stats().redrawn_maps, expected_redraws);
            assert_eq!(&renderer.fixture_shadow_cache[..previous.len()], &previous);
            previous = renderer.fixture_shadow_cache[..count].to_vec();
        }

        let mut fresh = fixture_surface_frame(340);
        fresh.geometry_shadows = true;
        fresh.haze_density = 0.0;
        fresh.fixture_shadow_capacity_hint = 0;
        let reference = Renderer::new()?.render(&fresh, 80, 60, 1)?;
        let max_delta = hinted
            .iter()
            .zip(&reference)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 1,
            "capacity hint changed the final image (max {max_delta})"
        );
        Ok(())
    }

    #[test]
    fn interval_cache_and_compact_list_resume_after_empty_blackout() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        if !renderer.gpu.interval_cache || renderer.gpu.haze_compact_pipelines.is_none() {
            return Ok(());
        }
        let mut frame = fixture_surface_frame(300);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;

        for _ in 0..3 {
            renderer.render(&frame, 80, 60, 1)?;
        }
        assert_eq!(renderer.interval_cache_stats().read, 300);
        assert!(renderer.compact_stats().active);
        assert_eq!(renderer.compact_stats().dirty_slots, 0);

        let lights = std::mem::take(&mut frame.fixture_cones);
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(
            renderer.interval_cache_stats(),
            crate::interval_cache::IntervalCacheStats::default()
        );
        assert!(!renderer.compact_stats().active);

        frame.fixture_cones = lights;
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(renderer.shadow_stats().redrawn_maps, 0);
        assert_eq!(renderer.interval_cache_stats().read, 300);
        assert!(renderer.compact_stats().active);
        assert_eq!(renderer.compact_stats().dirty_slots, 0);
        Ok(())
    }

    #[test]
    fn partial_blackout_retains_only_resident_interval_slots() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        if !renderer.gpu.interval_cache || renderer.gpu.haze_compact_pipelines.is_none() {
            return Ok(());
        }
        let mut frame = fixture_surface_frame(300);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;
        for _ in 0..3 {
            renderer.render(&frame, 80, 60, 1)?;
        }

        let lights = frame.fixture_cones.clone();
        frame.fixture_cones.truncate(3);
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(renderer.interval_cache_stats().read, 3);

        frame.fixture_cones.clear();
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(
            renderer.interval_cache_stats(),
            crate::interval_cache::IntervalCacheStats::default()
        );

        frame.fixture_cones = lights;
        renderer.render(&frame, 80, 60, 1)?;
        assert_eq!(renderer.shadow_stats().redrawn_maps, 0);
        assert_eq!(renderer.interval_cache_stats().read, 3);
        assert_eq!(renderer.interval_cache_stats().write, 297);
        assert!(renderer.compact_stats().active);
        assert_eq!(renderer.compact_stats().dirty_slots, 300);
        Ok(())
    }

    #[test]
    #[ignore = "serial Metal test; mutates retained-cache environment"]
    fn retained_hash_payload_reclaim_falls_back_to_exact_traversal() -> anyhow::Result<()> {
        unsafe {
            std::env::set_var("LUMA_INTERVAL_RETAIN_INACTIVE", "1");
            std::env::set_var("LUMA_HAZE_RESID_TEMPORAL", "1");
        }
        let mut gpu = Gpu::build()?;
        if gpu.haze_compact_pipelines.is_none() || !gpu.interval_cache {
            return Ok(());
        }
        // The direct arena is disabled so payload entries carry raw shared
        // hash-table indices. A small table keeps the deterministic overwrite
        // below bounded.
        gpu.interval_cache_table_bits = 10;
        gpu.haze_direct_arena_words = 0;
        let mut renderer = Renderer::on(Arc::new(gpu));
        let mut original = fixture_surface_frame(300);
        original.geometry_shadows = true;
        original.fixture_surface_lighting = false;
        original.haze_density = 0.3;
        original.haze_resolution = 1.0;
        for _ in 0..4 {
            renderer.render(&original, 80, 60, 1)?;
        }
        assert!(
            renderer.compact_stats().segments[1] > 0,
            "fixture has no hash payload records"
        );

        // Retain every slot through a real all-off frame, then model another
        // light reclaiming the old ways by clearing both the published claims
        // and their bodies. CPU classification deliberately remains intact.
        let lights = std::mem::take(&mut original.fixture_cones);
        renderer.render(&original, 80, 60, 1)?;
        let pools = renderer.interval_pools.as_ref().expect("cache pools");
        let layout =
            renderer
                .gpu
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("force retained hash payload reclaim"),
                    entries: &[0, 1].map(|binding| wgpu::BindGroupLayoutEntry {
                        binding,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }),
                });
        let module = shader(
            &renderer.gpu.device,
            "force retained hash payload reclaim",
            r#"
                @group(0) @binding(0) var<storage, read_write> claims: array<atomic<u32>>;
                @group(0) @binding(1) var<storage, read_write> table: array<u32>;
                @compute @workgroup_size(256)
                fn clear(@builtin(global_invocation_id) id: vec3<u32>) {
                    let i = id.x;
                    if i < arrayLength(&claims) { atomicStore(&claims[i], 0u); }
                    if i < arrayLength(&table) { table[i] = 0u; }
                }
            "#,
        );
        let pipeline_layout =
            renderer
                .gpu
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("force retained hash payload reclaim"),
                    bind_group_layouts: &[Some(&layout)],
                    immediate_size: 0,
                });
        let pipeline =
            renderer
                .gpu
                .device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("force retained hash payload reclaim"),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some("clear"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                });
        let bind_group = renderer
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("force retained hash payload reclaim"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: pools.claims.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: pools.table.as_entire_binding(),
                    },
                ],
            });
        let mut encoder =
            renderer
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("force retained hash payload reclaim"),
                });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("force retained hash payload reclaim"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let words = (pools.claims.size().max(pools.table.size()) / 4) as u32;
            pass.dispatch_workgroups(words.div_ceil(256), 1, 1);
        }
        renderer.gpu.queue.submit([encoder.finish()]);
        original.fixture_cones = lights;

        let compact = renderer.render(&original, 80, 60, 1)?;
        assert_eq!(renderer.interval_cache_stats().read, 300);
        assert_eq!(renderer.compact_stats().dirty_slots, 0);

        // The fused cached path validates every table lookup. It is the exact
        // oracle for a compact retained descriptor whose old way was stolen.
        renderer.compact_stats.suspended = 6;
        let fused = renderer.render(&original, 80, 60, 1)?;
        let max_delta = compact
            .iter()
            .zip(&fused)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 1,
            "retained reclaimed payload did not traverse exactly (max {max_delta})"
        );
        Ok(())
    }

    #[test]
    fn residual_compaction_encodes_more_than_256_resident_lights() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        if renderer.gpu.haze_compact_pipelines.is_none() {
            return Ok(());
        }
        renderer.compact_group_columns = 2;
        let light_count = 300;
        let high_start = 256;
        let mut frame = fixture_surface_frame(light_count);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;
        for light in &mut frame.fixture_cones {
            light.intensity = 0.001;
        }

        // Warm the interval cache, then keep settled frames on the fused
        // path. The following frame resumes compaction in the same renderer,
        // so the captures differ only by that path and the repeated fused
        // frame proves that the test scene itself is deterministic.
        renderer.compact_stats.suspended = 5;
        renderer.render(&frame, 96, 72, 1)?;
        assert_eq!(renderer.shadowed_fixture_count(), light_count);
        let high_lights: Vec<_> = renderer.fixture_shadow_slots[high_start..light_count]
            .iter()
            .map(|light| light.expect("resident high slot"))
            .collect();
        for (index, light) in high_lights.iter().copied().enumerate() {
            frame.fixture_cones[light].color = [Vec3::X, Vec3::Y, Vec3::Z][index % 3];
            frame.fixture_cones[light].intensity = 0.01;
        }
        renderer.render(&frame, 96, 72, 1)?;
        assert!(!renderer.compact_stats().active);
        renderer.render(&frame, 96, 72, 1)?;
        assert!(!renderer.compact_stats().active);
        let fused = renderer.render(&frame, 96, 72, 1)?;
        assert!(!renderer.compact_stats().active);
        let fused_repeat = renderer.render(&frame, 96, 72, 1)?;
        assert!(!renderer.compact_stats().active);
        let fused_floor = fused
            .iter()
            .zip(&fused_repeat)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert_eq!(fused_floor, 0, "settled fused frame was not stable");
        for (slot, light) in high_lights.iter().copied().enumerate() {
            assert_eq!(
                renderer.fixture_shadow_slots[high_start + slot],
                Some(light)
            );
        }

        let compact = renderer.render(&frame, 96, 72, 1)?;
        let stats = renderer.compact_stats();
        assert!(stats.active);
        assert_eq!(stats.dense_slots, light_count as u32);
        assert_eq!(renderer.compact_dense[high_start], Some(high_start as u32));
        let max_delta = fused
            .iter()
            .zip(&compact)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let mut changed_pixels = 0usize;
        let mut sum_squared = 0u64;
        let mut max_location = 0usize;
        for (pixel, (a, b)) in fused
            .chunks_exact(4)
            .zip(compact.chunks_exact(4))
            .enumerate()
        {
            if a != b {
                changed_pixels += 1;
            }
            for channel in 0..4 {
                let delta = a[channel].abs_diff(b[channel]);
                sum_squared += u64::from(delta) * u64::from(delta);
                if delta == max_delta {
                    max_location = pixel * 4 + channel;
                }
            }
        }
        let rmse = (sum_squared as f64 / fused.len() as f64).sqrt();
        assert!(
            max_delta <= 1,
            "compaction changed {changed_pixels} pixels, max {max_delta} at byte {max_location}, RMSE {rmse:.4}; settled fused repeat floor max {fused_floor}"
        );
        renderer.render(&frame, 96, 72, 1)?;
        let completed = renderer.compact_stats();
        assert_eq!(completed.segments.iter().sum::<u32>(), completed.list_count);
        assert_eq!(
            completed.queue_offsets,
            bucket_offsets(completed.queue_cursors)
        );
        assert_eq!(
            bucket_kind_sums(completed.queue_cursors),
            completed.segments
        );
        if renderer.gpu.haze_resid_temporal == 4 {
            assert!(completed.readback_transport_submission > 0);
            assert!(
                completed.readback_transport_submission <= completed.transport_submission,
                "readback origin is ahead of the current CPU plan: {completed:?}"
            );
            assert!(matches!(completed.readback_temporal_period, 1 | 4));
            assert!(completed.readback_temporal_phase < completed.readback_temporal_period);
            for segment in 0..3 {
                let logical =
                    completed.segments[segment].div_ceil(renderer.gpu.haze_resid_lanes[segment]);
                assert_eq!(completed.temporal_logical_groups[segment], logical);
                assert_eq!(
                    completed.temporal_physical_groups[segment],
                    phased_residual_groups(
                        completed.segments[segment],
                        renderer.gpu.haze_resid_lanes[segment],
                        completed.readback_temporal_period,
                        completed.readback_temporal_phase,
                    )
                );
            }
        }
        assert!(completed.segments[0] > 0);
        assert!(completed.segments[1] > 0);
        assert!(
            completed.segments[2] > 0 || completed.direct_arena_stored_records > 0,
            "neither exact traversal nor direct-arena replay was exercised: {completed:?}"
        );
        if completed.direct_arena_capacity_words > 0 {
            assert_eq!(
                completed.direct_arena_stored_records + completed.direct_arena_failed_records,
                completed.direct_arena_requested_records
            );
            assert_eq!(
                completed.direct_arena_stored_words + completed.direct_arena_failed_words,
                completed.direct_arena_requested_words
            );
            assert!(
                completed.direct_arena_requested_nonempty
                    <= completed.direct_arena_requested_intervals
            );
        }
        assert!(
            completed
                .segments
                .into_iter()
                .zip(renderer.gpu.haze_resid_lanes)
                .any(|(count, lanes)| count > lanes * renderer.compact_group_columns),
            "no segment crossed an artificial dispatch row: {completed:?}"
        );
        Ok(())
    }

    fn whole_scalar_test_renderer_period(
        bytes: u64,
        expected_period: u32,
    ) -> anyhow::Result<Option<Renderer>> {
        let mut gpu = Gpu::build()?;
        assert_eq!(
            gpu.haze_resid_temporal, expected_period,
            "whole scalar GPU test temporal specialization"
        );
        if gpu.haze_compact_pipelines.is_none() {
            return Ok(None);
        }
        gpu.haze_whole_k_bytes = bytes;
        gpu.haze_compact = super::HazeCompact::Always;
        gpu.haze_resid_capacity = 4 << 20;
        Ok(Some(Renderer::on(Arc::new(gpu))))
    }

    fn whole_scalar_test_renderer(bytes: u64) -> anyhow::Result<Option<Renderer>> {
        whole_scalar_test_renderer_period(bytes, 1)
    }

    fn whole_scalar_frame(count: usize) -> Frame {
        let mut frame = fixture_surface_frame(count);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;
        for (index, light) in frame.fixture_cones.iter_mut().enumerate() {
            light.color = [
                Vec3::new(0.9, 0.2, 0.1),
                Vec3::new(0.1, 0.8, 0.3),
                Vec3::new(0.2, 0.3, 1.0),
            ][index % 3];
            light.intensity = 0.4;
        }
        frame
    }

    #[test]
    #[ignore = "serial Metal test; run explicitly with scalar transport enabled"]
    fn whole_scalar_period1_initializes_all_whole_zero_residual_scene() -> anyhow::Result<()> {
        let Some(mut renderer) = whole_scalar_test_renderer(128 << 20)? else {
            return Ok(());
        };
        let mut frame = whole_scalar_frame(1);
        frame.camera.fov_y_deg = 1.0;
        frame.fixture_cones[0].position = Vec3::new(0.0, 0.0, 3.0);
        for _ in 0..12 {
            renderer.render(&frame, 8, 4, 1)?;
        }
        let stats = renderer.compact_stats();
        assert!(stats.active, "compact path did not activate: {stats:?}");
        assert_eq!(
            stats.segments.iter().sum::<u32>(),
            0,
            "scene was not all-whole: {stats:?}"
        );
        assert!(
            stats.whole_k_capacity > 0 && stats.whole_k_count > 0,
            "whole pool did not initialize: {stats:?}"
        );
        assert!(
            stats.whole_k_logical_groups > 0,
            "pre-hot descriptor prefix was never revisited: {stats:?}"
        );
        assert_ne!(
            stats.whole_k_readback_temporal & (1 << 16),
            0,
            "period-1 origin was not full: {stats:?}"
        );
        assert!(stats.whole_k_refreshed + stats.whole_k_stale <= stats.whole_k_logical_groups);
        Ok(())
    }

    #[test]
    #[ignore = "serial Metal test; run explicitly with scalar transport enabled"]
    fn whole_scalar_tiny_capacity_falls_back_exactly_and_does_not_gc_loop() -> anyhow::Result<()> {
        let Some(mut control) = whole_scalar_test_renderer(0)? else {
            return Ok(());
        };
        let Some(mut candidate) = whole_scalar_test_renderer(2 * super::WHOLE_RECORD_BYTES)? else {
            return Ok(());
        };
        let frame = whole_scalar_frame(8);
        let mut last_gc = None;
        for _ in 0..32 {
            let exact = control.render(&frame, 96, 72, 1)?;
            let bounded = candidate.render(&frame, 96, 72, 1)?;
            let max_delta = exact
                .iter()
                .zip(&bounded)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            assert!(
                max_delta <= 1,
                "tiny whole pool changed exact fallback (max {max_delta})"
            );
            let stats = candidate.compact_stats();
            if stats.whole_k_gc_count > 0 && stats.whole_k_saturated {
                last_gc = Some(stats.whole_k_gc_count);
                break;
            }
        }
        let gc = last_gc.expect("tiny pool never reached post-GC live-over-cap saturation");
        let stats = candidate.compact_stats();
        assert_eq!(stats.whole_k_capacity, 2);
        assert!(stats.whole_k_failed > 0);
        for _ in 0..6 {
            candidate.render(&frame, 96, 72, 1)?;
        }
        assert_eq!(
            candidate.compact_stats().whole_k_gc_count,
            gc,
            "live-over-cap epoch repeated whole GC"
        );
        Ok(())
    }

    #[test]
    #[ignore = "serial Metal regression; requires period1, per-resident K and interval retention"]
    fn whole_gc_clears_inactive_owner_before_descriptor_reuse_and_reentry() -> anyhow::Result<()> {
        let Some(mut control) = whole_scalar_test_renderer(0)? else {
            return Ok(());
        };
        let Some(mut candidate) = whole_scalar_test_renderer(super::WHOLE_RECORD_BYTES)? else {
            return Ok(());
        };
        assert!(
            candidate.gpu.haze_resid_per_resident,
            "run with LUMA_HAZE_RESID_PER_RESIDENT=1"
        );

        let frame = |replacement: bool| {
            let mut frame = whole_scalar_frame(1);
            frame.camera.fov_y_deg = 1.0;
            frame.fixture_cones[0].position = if replacement {
                Vec3::new(0.0, 0.0, 5.0)
            } else {
                Vec3::new(0.0, 0.0, 3.0)
            };
            frame.fixture_cones[0].range = if replacement { 8.0 } else { 3.0 };
            frame.fixture_cones[0].cos_beam = if replacement { 0.72 } else { 0.99 };
            frame
        };
        let old = frame(false);
        for _ in 0..12 {
            control.render(&old, 8, 4, 1)?;
            candidate.render(&old, 8, 4, 1)?;
        }
        assert_eq!(candidate.compact_stats().whole_k_count, 1);

        let mut inactive = frame(false);
        inactive.fixture_cones.clear();
        // Force the same ordered validation→clear→reset path that bounded
        // saturation schedules. Retention keeps the inactive plane available
        // so this specifically exercises clearing an unmapped owner.
        let gc_before = candidate.compact_stats().whole_k_gc_count;
        candidate.whole_gc_pending = true;
        candidate.whole_saturated_key = None;
        control.render(&inactive, 8, 4, 1)?;
        candidate.render(&inactive, 8, 4, 1)?;
        for _ in 0..4 {
            control.render(&inactive, 8, 4, 1)?;
            candidate.render(&inactive, 8, 4, 1)?;
        }
        assert!(
            candidate.compact_stats().whole_k_gc_count > gc_before,
            "forced whole GC origin did not complete"
        );

        let replacement = frame(true);
        let replacement_exact = control.render(&replacement, 8, 4, 1)?;
        candidate.render(&replacement, 8, 4, 1)?;
        for _ in 0..4 {
            control.render(&replacement, 8, 4, 1)?;
            candidate.render(&replacement, 8, 4, 1)?;
        }
        assert_eq!(candidate.compact_stats().whole_k_count, 1);

        // Descriptor zero now belongs to the replacement plane. If GC left
        // the inactive old plane's backpointer nonzero, this first re-entry
        // reads replacement K. A correctly cleared plane lazily computes exact
        // old K and publishes a fresh descriptor instead.
        let exact = control.render(&old, 8, 4, 1)?;
        let oracle_separation = exact
            .iter()
            .zip(&replacement_exact)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            oracle_separation > 8,
            "old and replacement K stimulus is not discriminating: {oracle_separation}"
        );
        let reused = candidate.render(&old, 8, 4, 1)?;
        let max_delta = exact
            .iter()
            .zip(&reused)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 1,
            "inactive plane read reused descriptor K: {max_delta}"
        );
        Ok(())
    }

    #[test]
    #[ignore = "serial Metal test; mutates the process temporal specialization"]
    fn whole_scalar_period4_keeps_mapped_zero_gain_fixture_fresh() -> anyhow::Result<()> {
        let Some(mut period1) = whole_scalar_test_renderer_period(128 << 20, 1)? else {
            return Ok(());
        };
        // This exact test is the only test in its process. Each Gpu owns the
        // shader module specialized while it was built, so the two renderers
        // remain a same-source period-1 oracle and period-4 candidate.
        unsafe { std::env::set_var("LUMA_HAZE_RESID_TEMPORAL", "4") };
        let period4 = whole_scalar_test_renderer_period(128 << 20, 4);
        unsafe { std::env::set_var("LUMA_HAZE_RESID_TEMPORAL", "1") };
        let Some(mut period4) = period4? else {
            return Ok(());
        };

        let mut frame = whole_scalar_frame(1);
        for _ in 0..12 {
            frame.time += 1.0 / 75.0;
            period1.render(&frame, 96, 72, 1)?;
            period4.render(&frame, 96, 72, 1)?;
        }
        assert!(period4.compact_stats().whole_k_count > 0);

        frame.fixture_cones[0].haze_gain = 0.0;
        let dark_start_submission = period4.compact_stats().whole_k_submission;
        let mut joined_dark_refresh = false;
        let mut last_origin = 0;
        for _ in 0..8 {
            frame.time += 1.0 / 75.0;
            period1.render(&frame, 96, 72, 1)?;
            period4.render(&frame, 96, 72, 1)?;
            let stats = period4.compact_stats();
            if stats.whole_k_readback_submission != last_origin {
                last_origin = stats.whole_k_readback_submission;
                joined_dark_refresh |= stats.whole_k_readback_submission > dark_start_submission
                    && stats.whole_k_refreshed > 0
                    && stats.whole_k_readback_temporal & 0xff == 4;
            }
        }
        assert_eq!(period4.compact_stats().whole_k_temporal_period, 4);
        assert!(
            joined_dark_refresh,
            "no origin-joined whole refresh ran while haze gain was zero"
        );

        frame.fixture_cones[0].haze_gain = 1.0;
        frame.fixture_cones[0].color = Vec3::new(0.12, 0.31, 0.93);
        frame.time += 1.0 / 75.0;
        let exact = period1.render(&frame, 96, 72, 1)?;
        let phased = period4.render(&frame, 96, 72, 1)?;
        let stats = period4.compact_stats();
        assert_eq!(stats.whole_k_temporal_period, 4);
        assert!(
            stats.whole_k_physical_groups < stats.whole_k_logical_groups,
            "reentry refreshed the entire cache and hid a stale phase: {stats:?}"
        );
        let max_delta = exact
            .iter()
            .zip(&phased)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 2,
            "mapped haze-gain reentry used stale whole K (max {max_delta})"
        );
        Ok(())
    }

    #[test]
    fn residual_overflow_falls_back_on_its_first_frame() -> anyhow::Result<()> {
        let mut gpu = Gpu::build()?;
        if gpu.haze_compact_pipelines.is_none() {
            return Ok(());
        }
        gpu.haze_direct_arena_words = 0;
        gpu.haze_resid_capacity = 64;
        let mut renderer = Renderer::on(Arc::new(gpu));
        let mut frame = fixture_surface_frame(300);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;

        // Warm a stable fused oracle in this renderer without allocating
        // compact pools. The odd dimensions exercise partial edge groups.
        renderer.compact_stats.suspended = 6;
        for _ in 0..4 {
            renderer.render(&frame, 97, 73, 1)?;
        }
        let fused = renderer.render(&frame, 97, 73, 1)?;
        renderer.compact_stats.suspended = 0;

        let first = renderer.render(&frame, 97, 73, 1)?;
        assert!(renderer.compact_stats().active);
        let max_delta = fused
            .iter()
            .zip(&first)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 1,
            "first overflow frame did not use the fused oracle (max delta {max_delta})"
        );

        for _ in 0..3 {
            renderer.render(&frame, 97, 73, 1)?;
            if renderer.compact_stats().overflow {
                break;
            }
        }
        let completed = renderer.compact_stats();
        assert!(
            completed.overflow,
            "overflow readback did not land: {completed:?}"
        );
        assert!(
            completed.list_count > renderer.gpu.haze_resid_capacity,
            "total overflow was not exercised"
        );
        assert_eq!(completed.queue_cursors, [0; RESID_BUCKETS]);
        Ok(())
    }

    #[test]
    fn shared_residual_queue_repacks_incremental_and_gc_lists() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        if renderer.gpu.haze_compact_pipelines.is_none() {
            return Ok(());
        }
        let mut frame = fixture_surface_frame(300);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;
        for _ in 0..5 {
            renderer.render(&frame, 80, 60, 1)?;
        }
        let baseline = renderer.compact_stats();
        assert!(baseline.list_count > 0);
        assert_eq!(bucket_kind_sums(baseline.queue_cursors), baseline.segments);

        frame.fixture_cones[0].position.x += 0.01;
        renderer.render(&frame, 80, 60, 1)?;
        let planned = renderer.compact_stats();
        assert!(planned.classify);
        assert!(!planned.rebuilt);
        assert_eq!(planned.dirty_slots, 1);
        let readbacks = planned.readbacks;
        for _ in 0..4 {
            renderer.render(&frame, 80, 60, 1)?;
            if renderer.compact_stats().readbacks > readbacks {
                break;
            }
        }
        let appended = renderer.compact_stats();
        assert!(appended.list_count > baseline.list_count);
        assert_eq!(appended.segments.iter().sum::<u32>(), appended.list_count);
        assert_eq!(
            appended.queue_offsets,
            bucket_offsets(appended.queue_cursors)
        );
        assert_eq!(bucket_kind_sums(appended.queue_cursors), appended.segments);

        renderer.compact_full_count = 1;
        renderer.render(&frame, 80, 60, 1)?;
        let planned = renderer.compact_stats();
        assert!(planned.classify);
        assert!(planned.rebuilt);
        let readbacks = planned.readbacks;
        for _ in 0..4 {
            renderer.render(&frame, 80, 60, 1)?;
            if renderer.compact_stats().readbacks > readbacks {
                break;
            }
        }
        let rebuilt = renderer.compact_stats();
        assert!(rebuilt.list_count < appended.list_count);
        assert_eq!(rebuilt.segments.iter().sum::<u32>(), rebuilt.list_count);
        assert_eq!(bucket_kind_sums(rebuilt.queue_cursors), rebuilt.segments);
        Ok(())
    }

    #[test]
    fn compact_output_dispatch_tracks_equal_area_reshape() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        if renderer.gpu.haze_compact_pipelines.is_none() {
            return Ok(());
        }
        let mut frame = fixture_surface_frame(300);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;
        for _ in 0..3 {
            renderer.render(&frame, 96, 72, 1)?;
        }
        assert!(renderer.compact_stats().active);

        // Both sizes have 216 haze workgroups. Let the reshaped frame key
        // settle, then compare its compact READ frame with a forced fused
        // frame at the same size. The rebuild must refresh the indirect x/y
        // dimensions even though the total workgroup count did not change.
        renderer.render(&frame, 72, 96, 1)?;
        assert!(!renderer.compact_stats().active);
        renderer.render(&frame, 72, 96, 1)?;
        assert!(renderer.compact_stats().active);
        assert!(renderer.compact_stats().rebuilt);
        let compact = renderer.render(&frame, 72, 96, 1)?;
        assert!(renderer.compact_stats().active);
        renderer.compact_stats.suspended = 5;
        for _ in 0..3 {
            renderer.render(&frame, 72, 96, 1)?;
        }
        let fused = renderer.render(&frame, 72, 96, 1)?;
        assert!(!renderer.compact_stats().active);
        let fused_repeat = renderer.render(&frame, 72, 96, 1)?;
        assert!(!renderer.compact_stats().active);
        let fused_floor = fused
            .iter()
            .zip(&fused_repeat)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert_eq!(fused_floor, 0, "settled fused reshape frame was not stable");
        let max_delta = fused
            .iter()
            .zip(&compact)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            max_delta <= 1,
            "reshape left stale output groups (max delta {max_delta})"
        );
        Ok(())
    }

    #[test]
    fn cached_shadows_clear_when_the_last_caster_is_removed() -> anyhow::Result<()> {
        let mut frame = fixture_surface_frame(1);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.6;
        frame.haze_steps = 8;
        frame.draws[0].model = Mat4::from_translation(Vec3::new(0.0, 0.0, 1.5));
        let mut renderer = Renderer::new()?;
        renderer.render(&frame, 160, 120, 1)?;
        frame.draws.clear();
        let cleared = renderer.render(&frame, 160, 120, 1)?;
        assert_eq!(renderer.shadow_stats().redrawn_maps, 1);
        assert_eq!(renderer.shadow_stats().hierarchy_layers, 1);
        let fresh = Renderer::new()?.render(&frame, 160, 120, 1)?;
        assert!(fresh.chunks_exact(4).any(|pixel| pixel[..3] != [0; 3]));
        assert_eq!(cleared, fresh, "removed geometry left a stale haze shadow");
        assert_eq!(renderer.render(&frame, 160, 120, 1)?, fresh);
        assert_eq!(renderer.shadow_stats().hierarchy_layers, 0);
        Ok(())
    }

    #[test]
    fn geometry_visibility_shadows_a_fixture_beyond_the_atlas_budget() -> anyhow::Result<()> {
        const DECOYS: usize = 300;
        let mut renderer = Renderer::new()?;
        renderer.set_geometry_shadows(false);
        let mut frame = fixture_surface_frame(1);
        let mut light = frame.fixture_cones[0];
        light.position = Vec3::new(0.0, 0.0, 3.0);
        light.intensity = 1.0;
        let mut decoy = light;
        decoy.direction = Vec3::Z;
        decoy.intensity = 10.0;
        frame.fixture_cones = vec![decoy; DECOYS];
        frame.fixture_cones.push(light);
        frame.fixture_shadows = true;
        frame.ambient = Vec3::ZERO;
        frame.draws.push(Draw {
            mesh: 0,
            model: Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
                * Mat4::from_scale(Vec3::splat(0.09)),
            material: Material::default(),
            textures: MaterialTextures::default(),
            editor_object: None,
        });
        for haze in [false, true] {
            frame.fixture_surface_lighting = !haze;
            frame.haze_density = if haze { 0.8 } else { 0.0 };
            frame.geometry_shadows = false;
            let open = renderer.render(&frame, 160, 120, 2)?;
            assert!(!renderer.fixture_shadow_slots.contains(&Some(DECOYS)));
            frame.geometry_shadows = true;
            let blocked = renderer.render(&frame, 160, 120, 2)?;
            let energy = |p: &[u8]| {
                p.chunks_exact(4)
                    .map(|p| p[..3].iter().map(|v| *v as u64).sum::<u64>())
                    .sum::<u64>()
            };
            assert!(
                energy(&blocked) < energy(&open),
                "second shadow bank must remove occluded energy, haze={haze}"
            );
            // Moving the stage caster invalidates the cached acceleration structure.
            frame.draws[1].model = Mat4::from_translation(Vec3::new(20.0, 0.0, 2.0));
            let moved = renderer.render(&frame, 160, 120, 2)?;
            assert!(
                energy(&moved) > energy(&blocked),
                "moved caster left a stale shadow"
            );
            frame.draws[1].model = Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
                * Mat4::from_scale(Vec3::splat(0.09));
        }
        Ok(())
    }

    /// The fixed-width mask has no growth path, so "bounded" is structural
    /// now; this keeps the three cone counts rendering, which is what used to
    /// overflow before the CSR era's `max_lights_per_cluster` cap was removed.
    #[test]
    fn clustered_surface_renders_at_32_128_and_512_cones() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        for count in [32, 128, 512] {
            let frame = fixture_surface_frame(count);
            renderer.render(&frame, 320, 180, 1)?;
        }
        Ok(())
    }

    /// The caster cull must never drop something the cone reaches — a missing
    /// caster is a silently missing shadow, which no golden here is dense
    /// enough to catch.
    #[test]
    fn cone_caster_cull_keeps_everything_the_beam_touches() {
        let apex = Vec3::ZERO;
        let direction = Vec3::Z;
        let range = 8.0;
        // cos of a 20-degree half-angle.
        let cos_field = 0.9397;
        let reaches = |centre: Vec3, radius: f32| {
            cone_reaches_sphere(apex, direction, range, cos_field, centre, radius)
        };

        // On the axis, inside the throw.
        assert!(reaches(Vec3::new(0.0, 0.0, 4.0), 0.1));
        // At the apex.
        assert!(reaches(apex, 0.1));
        // Just outside the opening angle, but a large enough sphere still
        // straddles the surface.
        let off_axis = Vec3::new(4.0 * 0.45, 0.0, 4.0);
        assert!(!reaches(off_axis, 0.05), "well outside the beam");
        assert!(reaches(off_axis, 1.5), "a sphere overlapping the beam edge");
        // Past the end of the throw, and behind the apex.
        assert!(!reaches(Vec3::new(0.0, 0.0, range + 2.0), 0.5));
        assert!(!reaches(Vec3::new(0.0, 0.0, -2.0), 0.5));
        // But a sphere big enough to reach back over the apex counts.
        assert!(reaches(Vec3::new(0.0, 0.0, -2.0), 3.0));
        // Degenerate radius must not panic or reject an axis hit.
        assert!(reaches(Vec3::new(0.0, 0.0, 1.0), 0.0));
    }

    #[test]
    fn cluster_occupancy_debug_is_topology_only() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        let mut frame = fixture_surface_frame(32);
        frame.cluster_debug = true;
        frame.fixture_surface_lighting = false;
        let first = renderer.render(&frame, 160, 120, 1)?;
        for light in &mut frame.fixture_cones {
            light.color = Vec3::ZERO;
            light.intensity = 0.0;
            light.gobo = 2;
        }
        let shading_only = renderer.render(&frame, 160, 120, 1)?;
        assert_eq!(first, shading_only);
        assert!(first.chunks_exact(4).any(|pixel| pixel[2] > 32));
        Ok(())
    }

    #[test]
    fn compute_haze_preserves_fragment_output_through_frame_transitions() -> anyhow::Result<()> {
        let mut frame = fixture_surface_frame(3);
        frame.geometry_shadows = true;
        frame.haze_density = 0.6;
        frame.haze_resolution = 1.0;
        frame.haze_steps = 8;
        frame.fixture_cones[0].wash = 1.0;
        frame.draws.push(Draw {
            mesh: 0,
            model: Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
                * Mat4::from_scale(Vec3::splat(0.07)),
            material: Material::default(),
            textures: MaterialTextures::default(),
            editor_object: None,
        });
        let mut compute = Renderer::new()?;
        let mut fragment = Renderer::new()?;
        compute.grid_fog = true;
        fragment.grid_fog = true;
        compute.haze_compute = true;
        fragment.haze_compute = false;
        let mut first = Vec::new();
        let original_intensity = frame.fixture_cones[1].intensity;
        for (probe, (width, height)) in [
            (127, 97),
            (127, 97),
            (127, 97),
            (65, 49),
            (127, 97),
            (127, 97),
            (127, 97),
            (127, 97),
            (127, 97),
            (127, 97),
            (127, 97),
            (127, 97),
        ]
        .into_iter()
        .enumerate()
        {
            match probe {
                2 => frame.draws[1].model = Mat4::from_translation(Vec3::new(20.0, 0.0, 2.0)),
                4 => frame.haze_density = 0.0,
                5 => {
                    frame.haze_density = 0.6;
                    frame.camera.eye.x += 0.2;
                }
                6 => frame.fixture_cones[1].gobo = 1,
                7 => frame.fixture_cones[1].gobo = 0,
                8 => frame.fixture_cones[1].intensity = 0.0,
                9 => frame.fixture_shadows = false,
                10 => {
                    frame.fixture_shadows = true;
                    frame.fixture_cones[1].intensity = original_intensity;
                }
                11 => {
                    compute.visibility_reference = true;
                    fragment.visibility_reference = true;
                }
                _ => {}
            }
            let actual = compute.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
            let expected = fragment.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
            assert_eq!(actual, expected, "compute haze differs at probe {probe}");
            if probe == 0 {
                first = actual;
            } else if probe == 4 {
                assert_ne!(
                    actual, first,
                    "haze-off transition did not change the image"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn fog_block_stats_follow_frames_and_resizes() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        renderer.grid_fog = true;
        assert!(renderer.fog_block_stats()?.is_none());
        let mut frame = fixture_surface_frame(3);
        frame.geometry_shadows = true;
        frame.haze_density = 0.6;
        for light in &mut frame.fixture_cones {
            light.wash = 1.0;
            light.range = 15.0;
            light.cos_field = 0.0;
            light.cos_beam = 0.1;
        }
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        if !crate::fog_grid::block_visibility() {
            assert!(renderer.fog_block_stats()?.is_none());
            return Ok(());
        }
        let first = renderer.fog_block_stats()?.expect("classified frame");
        assert_eq!(first.image_size, [127, 97]);
        let tile = crate::fog_grid::tile_size();
        let first_grid = [127_u32.div_ceil(tile), 97_u32.div_ceil(tile), 128];
        let first_blocks = first_grid.map(|n| n.div_ceil(4));
        assert_eq!(first.grid_size, first_grid);
        assert_eq!(first.blocks, first_blocks);
        assert_eq!(
            first.counts.len(),
            first_blocks.into_iter().product::<u32>() as usize
        );
        assert!(first.counts.iter().any(|counts| counts[0] > 0));
        assert!(first
            .counts
            .iter()
            .all(|counts| counts[1] <= counts[0] && counts[0] <= 3));
        assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
        // The lighting shader trusts these source eligibility filters. A cue
        // must clear candidates that become ineligible, then restore them.
        frame.fixture_cones[0].wash = crate::fog_grid::BROAD_WASH - 0.01;
        frame.fixture_cones[1].gobo = 1;
        frame.fixture_cones[2].haze_gain = 0.0;
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        assert!(renderer
            .fog_block_stats()?
            .unwrap()
            .counts
            .iter()
            .all(|counts| *counts == [0, 0]));
        frame.fixture_cones[0].wash = 1.0;
        frame.fixture_cones[1].gobo = 0;
        frame.fixture_cones[2].haze_gain = 1.0;
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        let resized = renderer.fog_block_stats()?.expect("resized frame");
        let resized_grid = [65_u32.div_ceil(tile), 49_u32.div_ceil(tile), 128];
        let resized_blocks = resized_grid.map(|n| n.div_ceil(4));
        assert_eq!(resized.grid_size, resized_grid);
        assert_eq!(resized.blocks, resized_blocks);
        assert_eq!(
            resized.counts.len(),
            resized_blocks.into_iter().product::<u32>() as usize
        );
        frame.haze_density = 0.0;
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        assert!(renderer.fog_block_stats()?.is_none());
        frame.haze_density = 0.6;
        renderer.grid_fog = false;
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        assert!(renderer.fog_block_stats()?.is_none());
        Ok(())
    }

    #[test]
    fn haze_work_counts_follow_frames_and_resizes() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        renderer.grid_fog = true;
        renderer.haze_compute = true;
        assert!(renderer.haze_work_stats()?.is_none());
        let mut frame = fixture_surface_frame(3);
        frame.geometry_shadows = true;
        frame.haze_density = 0.6;
        frame.haze_resolution = 1.0;
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        // The ordinary suite checks the disabled contract. Run this filter in
        // its own process with LUMA_HAZE_WORK_COUNTS=1 for the counter checks.
        if !renderer.gpu.haze_work_counts {
            assert!(renderer.haze_work_stats()?.is_none());
            assert_eq!(
                renderer.targets.as_ref().unwrap().haze_work_counts.size(),
                4
            );
            return Ok(());
        }
        let first = renderer.haze_work_stats()?.expect("counted frame");
        assert_eq!(first.image_size, [127, 97]);
        assert_eq!(first.workgroups, [16, 25]);
        assert_eq!(first.groups.len(), 16 * 25);
        for (index, group) in first.groups.iter().enumerate() {
            let x = index as u32 % first.workgroups[0] * first.workgroup_size[0];
            let y = index as u32 / first.workgroups[0] * first.workgroup_size[1];
            let valid =
                (127 - x).min(first.workgroup_size[0]) * (97 - y).min(first.workgroup_size[1]);
            for counter in 0..8 {
                assert!(group[counter] >= group[8 + counter]);
                assert!(group[counter] <= valid * group[8 + counter]);
            }
        }
        assert!(first.groups.iter().any(|group| group[5] > 0));
        assert_eq!(renderer.haze_work_stats()?.unwrap().groups, first.groups);
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        assert_eq!(renderer.haze_work_stats()?.unwrap().groups, first.groups);
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        let resized = renderer.haze_work_stats()?.expect("resized frame");
        assert_eq!(resized.image_size, [65, 49]);
        assert_eq!(resized.workgroups, [9, 13]);
        assert_eq!(resized.groups.len(), 9 * 13);
        let intensity = frame.fixture_cones[0].intensity;
        frame.fixture_cones[0].intensity = 0.0;
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        assert!(
            renderer.haze_work_stats()?.is_none(),
            "an unmapped source needs generic transport"
        );
        frame.fixture_cones[0].intensity = intensity;
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        assert!(
            renderer.haze_work_stats()?.is_some(),
            "mapped sources restore native transport"
        );
        frame.haze_density = 0.0;
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        assert!(renderer.haze_work_stats()?.is_none());
        frame.haze_density = 0.6;
        renderer.haze_compute = false;
        renderer.render_next(&frame, 65, 49, crate::LIVE_SUBFRAMES)?;
        assert!(renderer.haze_work_stats()?.is_none());
        Ok(())
    }

    #[test]
    fn fragment_counts_follow_unprofiled_frames_and_resizes() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        assert_eq!(renderer.fragment_stats()?, None);
        let frame = fixture_surface_frame(64);
        renderer.render(&frame, 127, 97, 1)?;
        let first = renderer.fragment_stats()?.expect("visible receiver");
        assert!(first.0 > 0 && first.0 <= 127 * 97 && first.1 > first.0);
        assert_eq!(
            renderer.fragment_stats()?,
            Some(first),
            "counter accumulated across requests"
        );
        renderer.render(&frame, 64, 48, 1)?;
        let resized = renderer.fragment_stats()?.expect("resized receiver");
        assert!(resized.0 > 0 && resized.0 <= 64 * 48 && resized.0 < first.0);
        assert!(resized.1 > resized.0 && resized.1 < first.1);
        Ok(())
    }

    #[test]
    fn surface_depth_culling_preserves_subpixel_and_coplanar_receivers() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        renderer.render(&fixture_surface_frame(16), 127, 97, 1)?;
        assert!(renderer
            .targets
            .as_ref()
            .unwrap()
            .msaa_surface_depth
            .is_none());
        let mut frame = fixture_surface_frame(64);
        let receiver = || Draw {
            mesh: 0,
            model: Mat4::IDENTITY,
            material: Material::default(),
            textures: MaterialTextures::default(),
            editor_object: None,
        };
        for (width, x, tilt) in [(0.002, 0.41, 0.4), (0.0006, -0.27, -0.2)] {
            let mut strip = receiver();
            strip.model = Mat4::from_scale_rotation_translation(
                Vec3::new(width, 0.35, 1.0),
                glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)
                    * glam::Quat::from_rotation_y(tilt),
                Vec3::new(x, -0.8, 1.2),
            );
            strip.material.base_color = Vec3::new(0.05, 0.45, 0.8);
            frame.draws.push(strip);
        }
        let mut panel = receiver();
        panel.model = Mat4::from_scale(Vec3::new(0.2, 0.2, 1.0));
        panel.material.base_color = Vec3::new(0.8, 0.1, 0.02);
        panel.material.metallic = 0.7;
        frame.draws.push(panel);
        // A cable over the ground: a thin strip halfway along the first
        // camera's view ray, so its tiles hold depths ~3.7 m and ~7.5 m and
        // the surface refinement splits them into two buckets.
        let mut cable = receiver();
        cable.model = Mat4::from_scale_rotation_translation(
            Vec3::new(0.004, 0.6, 1.0),
            glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)
                * glam::Quat::from_rotation_y(0.3),
            Vec3::new(0.05, -3.0, 2.4),
        );
        cable.material.base_color = Vec3::new(0.9, 0.8, 0.2);
        frame.draws.push(cable);
        for (eye, target) in [
            (Vec3::new(0.0, -6.0, 4.5), Vec3::ZERO),
            (Vec3::new(4.0, -4.0, 0.12), Vec3::new(0.0, 0.0, 0.001)),
            (Vec3::new(-2.0, -1.1, 1.3), Vec3::new(0.0, 0.0, 1.0)),
        ] {
            for shift in [0.0, 0.013, 0.029] {
                frame.camera.eye = eye + Vec3::X * shift;
                frame.camera.target = target + Vec3::X * shift;
                renderer.surface_depth_cull = false;
                let reference = renderer.render(&frame, 127, 97, 1)?;
                renderer.surface_depth_cull = true;
                let culled = renderer.render(&frame, 127, 97, 1)?;
                assert_eq!(
                    reference, culled,
                    "changed subpixel/coplanar receiver at {eye:?}, shift {shift}"
                );
            }
        }
        for (width, height) in [(129, 99), (64, 48)] {
            renderer.surface_depth_cull = false;
            let reference = renderer.render(&frame, width, height, 1)?;
            renderer.surface_depth_cull = true;
            assert_eq!(
                reference,
                renderer.render(&frame, width, height, 1)?,
                "resize {width}x{height}"
            );
        }
        // Verify this exercised real pruning, not an empty or unused third plane.
        let masks = renderer.light_index.bindings().tile_masks;
        let readback = renderer.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("surface-mask-test"),
            size: masks.size(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = renderer
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&masks, 0, &readback, 0, masks.size());
        renderer.gpu.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let mapped = readback.slice(..).get_mapped_range()?;
        let values: &[u32] = bytemuck::cast_slice(&mapped);
        let plane = values.len() / crate::light_index::MASK_PLANES;
        let full = &values[..plane];
        let surface = &values[2 * plane..3 * plane];
        let surface_b = &values[3 * plane..];
        assert!(full.iter().zip(surface).all(|(a, b)| b & !a == 0));
        assert!(full.iter().zip(surface_b).all(|(a, b)| b & !a == 0));
        let original: u32 = full.iter().map(|b| b.count_ones()).sum();
        let refined: u32 = surface.iter().map(|b| b.count_ones()).sum();
        assert!(
            refined > 0 && refined < original,
            "surface plane did not prune: {original} -> {refined}"
        );
        // The straddling tiles (cable over ground) must have been split into
        // two buckets when the split build is active.
        if renderer.gpu.light_index_pipelines.surface_split() >= 2 {
            let splits = renderer.light_index.bindings().surface_splits;
            let readback = renderer.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("surface-split-test"),
                size: splits.size(),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = renderer
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            encoder.copy_buffer_to_buffer(&splits, 0, &readback, 0, splits.size());
            renderer.gpu.queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            renderer
                .gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())?;
            rx.recv()??;
            let split_mapped = readback.slice(..).get_mapped_range()?;
            let split_depths: &[f32] = bytemuck::cast_slice(&split_mapped);
            let split_tiles = split_depths.iter().filter(|s| **s < 1e29).count();
            let refined_b: u32 = surface_b.iter().map(|b| b.count_ones()).sum();
            assert!(
                split_tiles > 0 && refined_b > 0,
                "no straddling tile was split: {split_tiles} tiles, {refined_b} bucket-B bits"
            );
            assert!(
                refined_b < refined,
                "bucket B did not prune below bucket A: {refined} vs {refined_b}"
            );
        }
        Ok(())
    }

    fn fixture_surface_frame(count: usize) -> Frame {
        let vertex = |position: [f32; 3]| Vertex {
            position,
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let mesh = MeshData {
            key: "::clustered-surface-floor".into(),
            vertices: vec![
                vertex([-5.0, -5.0, 0.0]),
                vertex([5.0, -5.0, 0.0]),
                vertex([5.0, 5.0, 0.0]),
                vertex([-5.0, 5.0, 0.0]),
            ]
            .into(),
            indices: Arc::from([0, 1, 2, 0, 2, 3]),
        };
        let fixture_cones = (0..count)
            .map(|index| {
                let column = (index % 32) as f32;
                let row = (index / 32) as f32;
                FixtureCone {
                    position: Vec3::new((column - 15.5) * 0.08, (row - 7.5) * 0.08, 3.0),
                    range: 5.0,
                    direction: Vec3::NEG_Z,
                    cos_beam: 0.96,
                    color: Vec3::ONE,
                    intensity: 3.0 / count.max(1) as f32,
                    cos_field: 0.88,
                    wash: 0.0,
                    gobo: 0,
                    gobo_rotation: 0.0,
                    haze_gain: 1.0,
                }
            })
            .collect();
        Frame {
            meshes: vec![mesh],
            images: Vec::new(),
            draws: vec![Draw {
                mesh: 0,
                model: Mat4::IDENTITY,
                material: Material {
                    base_color: Vec3::splat(0.65),
                    roughness: 0.55,
                    ..Material::default()
                },
                textures: MaterialTextures::default(),
                editor_object: None,
            }],
            transparent: Vec::new(),
            gizmo_pivot: None,
            overlays: Vec::new(),
            point_lights: Vec::new(),
            fixture_cones,
            fixture_shadow_capacity_hint: 0,
            fixture_lighting_domain: None,
            fixture_surface_lighting: true,
            beam_proxy: false,
            fixture_shadows: true,
            geometry_shadows: false,
            cluster_debug: false,
            clear_color: Vec3::ZERO,
            room: None,
            ambient: Vec3::splat(0.002),
            environment: None,
            directional: None,
            sky: None,
            haze_density: 0.0,
            haze_appearance: Default::default(),
            haze_bounds: luma_scene::Aabb::new(Vec3::splat(-32.0), Vec3::splat(32.0)),
            haze_steps: 1,
            haze_resolution: 0.5,
            time: 0.0,
            debug_view: DebugView::Pbr,
            camera: Camera {
                eye: Vec3::new(0.0, -6.0, 4.5),
                target: Vec3::ZERO,
                fov_y_deg: 48.0,
            },
        }
    }

    fn residual_key_for_with_medium(
        frame: &Frame,
        keyed_medium: Option<(crate::medium::Uniform, f32)>,
    ) -> ResidualGlobalKey {
        let cones: Vec<_> = frame
            .fixture_cones
            .iter()
            .map(sanitize_fixture_cone)
            .collect();
        let rests: Vec<_> = cones
            .iter()
            .map(|light| {
                let direction = light.direction.try_normalize().unwrap_or(Vec3::NEG_Y);
                let helper = if direction.z.abs() > 0.98 {
                    Vec3::Y
                } else {
                    Vec3::Z
                };
                let field = light.cos_field.clamp(-1.0, 1.0);
                LightRest {
                    direction: direction.to_array(),
                    cos_beam: light.cos_beam.clamp(-1.0, 1.0),
                    color: light.color.to_array(),
                    intensity: light.intensity.clamp(0.0, 100.0),
                    cos_field: field,
                    wash: light.wash.clamp(0.0, 1.0),
                    gobo: light.gobo.min(2) as f32,
                    gobo_rotation: light.gobo_rotation.rem_euclid(std::f32::consts::TAU),
                    shadow_slot: 0.0,
                    haze_gain: light.haze_gain.clamp(0.0, 1.0),
                    inverse_right_length: direction.cross(helper).length().recip(),
                    field_tangent: (1.0 - field * field).max(0.0).sqrt() / field.max(0.05),
                }
            })
            .collect();
        let (medium, fog_far) = keyed_medium.unwrap_or_else(|| {
            let medium =
                crate::medium::Uniform::new(frame, 0.3 * Transport::EXTINCTION, frame.time);
            let fog_far = cones
                .iter()
                .map(|cone| frame.camera.eye.distance(cone.position) + cone.range)
                .fold(1.0_f32, f32::max)
                .min(CAMERA_FAR);
            (medium, fog_far)
        });
        residual_transport_key(
            frame,
            96,
            72,
            (96, 72),
            medium,
            fog_far,
            CAMERA_FAR,
            7,
            11,
            true,
            2,
            false,
            &cones,
            &rests,
        )
    }

    fn residual_key_for(frame: &Frame) -> ResidualGlobalKey {
        residual_key_for_with_medium(frame, None)
    }

    #[test]
    fn production_lighting_domain_is_active_only_enclosing_and_indoor_safe() {
        let mut frame = fixture_surface_frame(1);
        frame.sky = Some(crate::atmosphere::SkyFrame {
            sun_direction: Vec3::Z,
            sun_radiance: Vec3::ONE,
            exposure: 1.0,
            ground_albedo: 0.1,
        });
        let active = crate::medium::Uniform::new(&frame, 0.3, 17.0);
        let active_far = frame.camera.eye.distance(frame.fixture_cones[0].position)
            + frame.fixture_cones[0].range;
        frame.fixture_lighting_domain = Some(FixtureLightingDomain {
            bounds: luma_scene::Aabb::new(Vec3::splat(-100.0), Vec3::splat(100.0)),
            fog_far: active_far + 10.0,
            source_camera_eye: frame.camera.eye.to_array(),
        });
        let (selected, selected_far, selected_domain) =
            select_fixture_lighting_domain(true, &frame, false, active, active_far, CAMERA_FAR);
        assert_eq!(&selected.min[..3], &[-100.0; 3]);
        assert_eq!(&selected.max[..3], &[100.0; 3]);
        assert_eq!(selected_far, active_far + 10.0);
        assert!(selected_domain);
        let (empty_medium, empty_far, empty_selected) =
            select_fixture_lighting_domain(true, &frame, true, active, active_far, CAMERA_FAR);
        assert_eq!(empty_medium.shape, active.shape);
        assert_eq!(empty_medium.wind, active.wind);
        assert_eq!(empty_medium.min, active.min);
        assert_eq!(empty_medium.max, active.max);
        assert_eq!((empty_far, empty_selected), (active_far, false));

        frame.sky = None;
        let indoor = crate::medium::Uniform::new(&frame, 0.3, 17.0);
        let (selected, _, selected_domain) =
            select_fixture_lighting_domain(true, &frame, false, indoor, active_far, CAMERA_FAR);
        assert_eq!(selected.min, indoor.min);
        assert_eq!(selected.max, indoor.max);
        assert!(selected_domain);

        frame.sky = Some(crate::atmosphere::SkyFrame {
            sun_direction: Vec3::Z,
            sun_radiance: Vec3::ONE,
            exposure: 1.0,
            ground_albedo: 0.1,
        });
        frame.fixture_lighting_domain = Some(FixtureLightingDomain {
            bounds: luma_scene::Aabb::new(Vec3::splat(-0.01), Vec3::splat(0.01)),
            fog_far: active_far + 10.0,
            source_camera_eye: frame.camera.eye.to_array(),
        });
        for enabled in [true, false] {
            let (fallback, fallback_far, fallback_selected) = select_fixture_lighting_domain(
                enabled, &frame, false, active, active_far, CAMERA_FAR,
            );
            assert_eq!(fallback.shape, active.shape);
            assert_eq!(fallback.wind, active.wind);
            assert_eq!(fallback.min, active.min);
            assert_eq!(fallback.max, active.max);
            assert_eq!((fallback_far, fallback_selected), (active_far, false));
        }
    }

    #[test]
    fn production_lighting_domain_requires_the_source_camera_eye() {
        let mut frame = fixture_surface_frame(1);
        frame.sky = Some(crate::atmosphere::SkyFrame {
            sun_direction: Vec3::Z,
            sun_radiance: Vec3::ONE,
            exposure: 1.0,
            ground_albedo: 0.1,
        });
        let active = crate::medium::Uniform::new(&frame, 0.3, 17.0);
        let active_far = frame.camera.eye.distance(frame.fixture_cones[0].position)
            + frame.fixture_cones[0].range;
        let source_eye = frame.camera.eye;
        frame.fixture_lighting_domain = Some(FixtureLightingDomain {
            bounds: luma_scene::Aabb::new(Vec3::splat(-100.0), Vec3::splat(100.0)),
            fog_far: active_far + 10.0,
            source_camera_eye: source_eye.to_array(),
        });

        frame.camera.target += Vec3::new(3.0, -2.0, 1.0);
        frame.camera.fov_y_deg += 7.0;
        let (_, selected_far, selected) =
            select_fixture_lighting_domain(true, &frame, false, active, active_far, CAMERA_FAR);
        assert!(
            selected,
            "target and FOV do not affect the radial fog bound"
        );
        assert_eq!(selected_far, active_far + 10.0);

        for stale_eye in [
            source_eye + Vec3::X,
            Vec3::new(f32::NAN, source_eye.y, source_eye.z),
        ] {
            frame.camera.eye = stale_eye;
            let (fallback, fallback_far, selected) =
                select_fixture_lighting_domain(true, &frame, false, active, active_far, CAMERA_FAR);
            assert!(!selected);
            assert_eq!(fallback.min, active.min);
            assert_eq!(fallback.max, active.max);
            assert_eq!(fallback_far, active_far);
        }

        frame.camera.eye = Vec3::new(
            f32::from_bits(source_eye.x.to_bits() ^ 1),
            source_eye.y,
            source_eye.z,
        );
        let (_, _, selected) =
            select_fixture_lighting_domain(true, &frame, false, active, active_far, CAMERA_FAR);
        assert!(
            !selected,
            "camera-eye identity is exact at the float-bit boundary"
        );

        frame.camera.eye = source_eye;
        frame
            .fixture_lighting_domain
            .as_mut()
            .unwrap()
            .source_camera_eye[0] = f32::INFINITY;
        let (_, _, selected) =
            select_fixture_lighting_domain(true, &frame, false, active, active_far, CAMERA_FAR);
        assert!(!selected, "invalid stored provenance must also fall back");
    }

    #[test]
    fn selected_domain_key_ignores_only_medium_time() {
        let frame = fixture_surface_frame(1);
        let mut medium = crate::medium::Uniform::new(&frame, 0.3, 1.0);
        let first = residual_key_for_with_medium(&frame, Some((medium, 50.0)));
        medium.wind[3] = 2.0;
        assert_eq!(
            residual_key_for_with_medium(&frame, Some((medium, 50.0))),
            first
        );
        medium.max[0] += 1.0;
        assert_ne!(
            residual_key_for_with_medium(&frame, Some((medium, 50.0))),
            first
        );
        medium.max[0] -= 1.0;
        assert_ne!(
            residual_key_for_with_medium(&frame, Some((medium, 51.0))),
            first
        );
    }

    #[test]
    fn residual_transport_key_excludes_only_current_radiance() {
        let mut frame = fixture_surface_frame(1);
        let baseline = residual_key_for(&frame);
        frame.fixture_cones[0].color = Vec3::new(0.2, 0.6, 0.9);
        frame.fixture_cones[0].intensity = 0.25;
        frame.fixture_cones[0].haze_gain = 0.4;
        frame.time = 123.0;
        assert_eq!(residual_key_for(&frame), baseline);

        frame.fixture_cones[0].position.x += 0.01;
        assert_ne!(residual_key_for(&frame), baseline);
        frame.fixture_cones[0].position.x -= 0.01;
        frame.camera.eye.x += 0.01;
        assert_ne!(residual_key_for(&frame), baseline);
        frame.camera.eye.x -= 0.01;
        frame.haze_appearance.cloudiness += 0.01;
        assert_ne!(residual_key_for(&frame), baseline);

        let mut fog_extent = baseline.clone();
        fog_extent.fog_far ^= 1;
        assert_ne!(fog_extent, baseline);
    }

    #[test]
    fn resident_interval_identity_normalizes_write_to_read() {
        use crate::interval_cache::{MODE_OFF, MODE_READ, MODE_WRITE};
        let base = [19, 2 | 3 << 16, 7 | 9 << 16, MODE_WRITE | 41 << 8];
        let mut read = base;
        read[3] = MODE_READ | 41 << 8;
        assert_eq!(
            normalized_interval_identity(base),
            normalized_interval_identity(read)
        );
        read[3] = MODE_WRITE | 42 << 8;
        assert_ne!(
            normalized_interval_identity(base),
            normalized_interval_identity(read)
        );
        read[3] = MODE_OFF;
        assert_ne!(
            normalized_interval_identity(base),
            normalized_interval_identity(read)
        );
    }

    #[test]
    fn resident_key_excludes_radiance_but_tracks_transport() {
        use crate::interval_cache::MODE_READ;
        let cone = sanitize_fixture_cone(&fixture_surface_frame(1).fixture_cones[0]);
        let mut rest = LightRest {
            direction: cone.direction.to_array(),
            cos_beam: cone.cos_beam,
            color: cone.color.to_array(),
            intensity: cone.intensity,
            cos_field: cone.cos_field,
            wash: cone.wash,
            gobo: cone.gobo as f32,
            gobo_rotation: cone.gobo_rotation,
            shadow_slot: 0.0,
            haze_gain: 1.0,
            inverse_right_length: 1.0,
            field_tangent: 1.0,
        };
        let interval = [5, 1 | 2 << 16, 3 | 4 << 16, MODE_READ | 7 << 8];
        let baseline = residual_resident_key(&cone, &rest, None, interval);
        rest.color = [0.1, 0.7, 0.2];
        rest.intensity = 0.25;
        rest.haze_gain = 0.5;
        assert_eq!(
            residual_resident_key(&cone, &rest, None, interval),
            baseline
        );
        rest.direction[0] += 0.01;
        assert_ne!(
            residual_resident_key(&cone, &rest, None, interval),
            baseline
        );
    }

    #[test]
    fn resident_mapping_reentry_forces_same_frame_refresh() {
        use crate::interval_cache::MODE_READ;
        let cone = sanitize_fixture_cone(&fixture_surface_frame(1).fixture_cones[0]);
        let rest = LightRest {
            direction: cone.direction.to_array(),
            cos_beam: cone.cos_beam,
            color: cone.color.to_array(),
            intensity: cone.intensity,
            cos_field: cone.cos_field,
            wash: cone.wash,
            gobo: cone.gobo as f32,
            gobo_rotation: cone.gobo_rotation,
            shadow_slot: 0.0,
            haze_gain: 1.0,
            inverse_right_length: 1.0,
            field_tangent: 1.0,
        };
        let key = Some(residual_resident_key(
            &cone,
            &rest,
            None,
            [1, 2, 3, MODE_READ | 9 << 8],
        ));
        assert!(!resident_transport_dirty(&key, true, &key, true));
        assert!(!resident_transport_dirty(&key, true, &key, false));
        assert!(resident_transport_dirty(&key, false, &key, true));
        assert!(resident_transport_dirty(&None, false, &key, true));
    }

    #[test]
    fn diagnostic_lighting_domain_selects_one_uploaded_and_keyed_domain() {
        let mut frame = fixture_surface_frame(1);
        frame.sky = Some(crate::atmosphere::SkyFrame {
            sun_direction: Vec3::Z,
            sun_radiance: Vec3::ONE,
            exposure: 1.0,
            ground_albedo: 0.1,
        });
        frame.fixture_cones[0].position = Vec3::new(2.0, 3.0, 4.0);
        frame.fixture_cones[0].range = 5.0;
        let medium = crate::medium::Uniform::new(&frame, 0.3, 17.0);
        let active_fog_far = frame.camera.eye.distance(frame.fixture_cones[0].position) + 5.0;
        let domain = DiagnosticLightingDomain {
            min: Vec3::splat(-10.0),
            max: Vec3::splat(10.0),
            fog_far: active_fog_far + 2.0,
        };
        let (selected, fog_far) = domain.select(&frame, medium, active_fog_far, 100.0);
        assert_eq!(&selected.min[..3], &domain.min.to_array());
        assert_eq!(&selected.max[..3], &domain.max.to_array());
        assert_eq!(fog_far, domain.fog_far);

        let mut later = selected;
        later.wind[3] = 99.0;
        let cones: Vec<_> = frame
            .fixture_cones
            .iter()
            .map(sanitize_fixture_cone)
            .collect();
        let rests: Vec<_> = cones
            .iter()
            .map(|cone| LightRest {
                direction: cone.direction.to_array(),
                cos_beam: cone.cos_beam,
                color: cone.color.to_array(),
                intensity: cone.intensity,
                cos_field: cone.cos_field,
                wash: cone.wash,
                gobo: cone.gobo as f32,
                gobo_rotation: cone.gobo_rotation,
                shadow_slot: 0.0,
                haze_gain: cone.haze_gain,
                inverse_right_length: 1.0,
                field_tangent: 1.0,
            })
            .collect();
        let key = |medium| {
            residual_transport_key(
                &frame,
                96,
                72,
                (96, 72),
                medium,
                fog_far,
                100.0,
                7,
                11,
                true,
                2,
                false,
                &cones,
                &rests,
            )
        };
        assert_eq!(key(selected), key(later), "only medium time is approximate");
        let mut changed = selected;
        changed.max[2] += 1.0;
        assert_ne!(key(selected), key(changed));
        let changed_far = residual_transport_key(
            &frame,
            96,
            72,
            (96, 72),
            selected,
            fog_far + 1.0,
            100.0,
            7,
            11,
            true,
            2,
            false,
            &cones,
            &rests,
        );
        assert_ne!(key(selected), changed_far);
    }

    #[test]
    #[should_panic(expected = "must enclose the current active lighting bounds")]
    fn diagnostic_lighting_domain_rejects_non_enclosing_bounds() {
        let mut frame = fixture_surface_frame(1);
        frame.sky = Some(crate::atmosphere::SkyFrame {
            sun_direction: Vec3::Z,
            sun_radiance: Vec3::ONE,
            exposure: 1.0,
            ground_albedo: 0.1,
        });
        let medium = crate::medium::Uniform::new(&frame, 0.3, 0.0);
        DiagnosticLightingDomain {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
            fog_far: 100.0,
        }
        .select(&frame, medium, 1.0, 100.0);
    }

    fn region_mean(
        pixels: &[u8],
        width: usize,
        xs: std::ops::Range<usize>,
        ys: std::ops::Range<usize>,
    ) -> f64 {
        let mut sum = 0_u64;
        let mut samples = 0_u64;
        for y in ys {
            for x in xs.clone() {
                let offset = (y * width + x) * 4;
                sum += u64::from(pixels[offset])
                    + u64::from(pixels[offset + 1])
                    + u64::from(pixels[offset + 2]);
                samples += 3;
            }
        }
        sum as f64 / samples as f64
    }

    fn overlay_test_frame(
        occluder: bool,
        depth: OverlayDepth,
        overlay_y: f32,
        scene_radiance: Vec3,
    ) -> Frame {
        let vertex = |position: [f32; 3]| Vertex {
            position,
            normal: [0.0, -1.0, 0.0],
            uv: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let mesh = MeshData {
            key: "::post-agx-overlay-test".into(),
            vertices: vec![
                vertex([-2.0, 0.0, -2.0]),
                vertex([2.0, 0.0, -2.0]),
                vertex([0.0, 0.0, 2.0]),
            ]
            .into(),
            indices: Arc::from([0, 1, 2]),
        };
        let draws = occluder
            .then(|| Draw {
                mesh: 0,
                model: Mat4::IDENTITY,
                material: Material {
                    base_color: Vec3::splat(0.35),
                    roughness: 1.0,
                    ..Material::default()
                },
                textures: MaterialTextures::default(),
                editor_object: None,
            })
            .into_iter()
            .collect();
        Frame {
            meshes: vec![mesh],
            images: Vec::new(),
            draws,
            transparent: Vec::new(),
            gizmo_pivot: None,
            overlays: vec![Overlay {
                mesh: 0,
                model: Mat4::from_translation(Vec3::Y * overlay_y),
                lines: false,
                color: hex_srgb(0x33_99_e6),
                opacity: 1.0,
                depth,
            }],
            point_lights: Vec::new(),
            fixture_cones: Vec::new(),
            fixture_shadow_capacity_hint: 0,
            fixture_lighting_domain: None,
            fixture_surface_lighting: false,
            beam_proxy: false,
            fixture_shadows: true,
            geometry_shadows: false,
            cluster_debug: false,
            environment: None,
            clear_color: scene_radiance,
            room: None,
            ambient: scene_radiance,
            directional: Some(DirectionalLight {
                direction: Vec3::new(0.0, -1.0, 1.0).normalize(),
                radiance: scene_radiance,
                shadow_eye: Vec3::new(0.0, -4.0, 5.0),
                shadows: false,
                shadow_softness: 1.0,
            }),
            sky: None,
            haze_density: 0.0,
            haze_appearance: Default::default(),
            haze_bounds: luma_scene::Aabb::new(Vec3::splat(-32.0), Vec3::splat(32.0)),
            haze_steps: 1,
            haze_resolution: 1.0,
            time: 0.0,
            debug_view: DebugView::Pbr,
            camera: Camera {
                eye: Vec3::new(0.0, -5.0, 0.0),
                target: Vec3::ZERO,
                fov_y_deg: 48.0,
            },
        }
    }

    fn center_offset(width: usize, height: usize) -> usize {
        ((height / 2) * width + width / 2) * 4
    }

    fn center_rgb(pixels: &[u8], channels: Channels) -> [u8; 3] {
        let offset = center_offset(96, 96);
        match channels {
            Channels::Rgba => [pixels[offset], pixels[offset + 1], pixels[offset + 2]],
            Channels::Bgra => [pixels[offset + 2], pixels[offset + 1], pixels[offset]],
        }
    }
}

impl Completion {
    /// Whether the GPU is finished with this frame, and when it said so.
    ///
    /// `signalled` is the instant the driver's callback fired, not the instant
    /// this poll noticed it. The difference between the two is the only thing
    /// that separates "the GPU took that long" from "nobody was looking".
    fn ready(&mut self, signalled: &mut Option<Instant>) -> anyhow::Result<bool> {
        match self {
            Self::Staged {
                mapped,
                mapped_result,
                ..
            } => {
                if mapped_result.is_none() {
                    *mapped_result = match mapped.try_recv() {
                        Ok((at, result)) => {
                            *signalled = Some(at);
                            Some(result)
                        }
                        Err(mpsc::TryRecvError::Empty) => None,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            return Err(anyhow::anyhow!("GPU map callback disconnected"));
                        }
                    };
                }
                Ok(mapped_result.is_some())
            }
            Self::Shared { done, finished, .. } => {
                if !*finished {
                    *finished = match done.try_recv() {
                        Ok(at) => {
                            *signalled = Some(at);
                            true
                        }
                        Err(mpsc::TryRecvError::Empty) => false,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            return Err(anyhow::anyhow!("GPU completion callback disconnected"));
                        }
                    };
                }
                Ok(*finished)
            }
        }
    }

    /// The finished pixels.
    ///
    /// # Panics
    /// Panics unless [`Self::ready`] has returned `true`.
    fn image(&mut self, width: u32, height: u32) -> anyhow::Result<Presented> {
        match self {
            Self::Staged {
                readback,
                mapped_result,
                bytes_per_row,
                ..
            } => {
                mapped_result
                    .take()
                    .expect("mapped result checked above")
                    .map_err(anyhow::Error::msg)?;
                let view = readback
                    .slice(..)
                    .get_mapped_range()
                    .map_err(anyhow::Error::msg)?;
                let row_bytes = (width * 4) as usize;
                let mut pixels = Vec::with_capacity(row_bytes * height as usize);
                if *bytes_per_row as usize == row_bytes {
                    pixels.extend_from_slice(&view);
                } else {
                    for row in 0..height {
                        let start = (row * *bytes_per_row) as usize;
                        pixels.extend_from_slice(&view[start..start + row_bytes]);
                    }
                }
                drop(view);
                readback.unmap();
                Ok(Presented::Pixels(pixels))
            }
            // Already where it belongs; the handle is all that moves.
            Self::Shared { surface, .. } => Ok(Presented::Shared(surface.clone())),
        }
    }
}

impl PendingFrame {
    /// Wait for this frame and hand back its pixels.
    ///
    /// # Why this polls rather than blocking
    ///
    /// One `PollType::Wait` was enough while every renderer owned a private
    /// device: the device went idle exactly when this frame finished, and the
    /// map callback had been serviced by the time the poll returned. Neither
    /// half survives sharing the device. The wait is satisfied by "no
    /// submissions in flight as of the beginning of the call", which another
    /// thread's traffic can satisfy while this buffer's mapping is left to a
    /// maintain pass we then skip — so the wait returns with no pixels.
    ///
    /// Worse, a *blocking* wait here holds the device against everyone else.
    /// A caller rendering a few hundred offline frames in a loop, each with
    /// its own blocking wait, starves the live workers sharing the device:
    /// their map callbacks stop being serviced and their frames never arrive.
    /// That reads as a hang, not as the slowdown it sounds like.
    ///
    /// So this waits on the only thing that actually answers the question —
    /// the readback itself — using the same non-blocking maintain and 1 ms
    /// cadence the live worker uses, and blocks nobody.
    ///
    /// # Errors
    /// Propagates a device poll or mapping failure, and gives up rather than
    /// spinning for ever on a device that has stopped returning frames.
    pub(crate) fn complete_blocking(
        &mut self,
        device: &wgpu::Device,
    ) -> anyhow::Result<CompletedFrame> {
        /// A frame that has not landed in this long is a dead device, not a
        /// slow one. Generous because a debug build under a loaded machine is
        /// genuinely slow, and a false positive here would look like a driver
        /// bug.
        const DEADLINE: Duration = Duration::from_secs(30);
        // Exhaustive offline references can exceed the live watchdog on a
        // laptop. Explicit diagnostic override; live asynchronous rendering
        // does not use this blocking readback path.
        let timeout = std::env::var("LUMA_READBACK_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(DEADLINE, |v| Duration::from_secs(v.clamp(30, 600)));
        let deadline = Instant::now() + timeout;
        loop {
            device.poll(wgpu::PollType::Poll)?;
            if let Some(frame) = self.try_complete()? {
                return Ok(frame);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "the GPU did not return a frame within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(crate) fn try_complete(&mut self) -> anyhow::Result<Option<CompletedFrame>> {
        let ready = self.completion.ready(&mut self.signalled)?;
        if let Some(profile) = &mut self.profile {
            if ready {
                profile.resolve_after_frame();
            }
            if let (None, Some(mapped)) = (&profile.mapped_result, &profile.mapped) {
                profile.mapped_result = match mapped.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(anyhow::anyhow!("GPU timestamp map callback disconnected"));
                    }
                };
            }
        }
        if !ready
            || self
                .profile
                .as_ref()
                .is_some_and(|profile| profile.mapped_result.is_none())
        {
            return Ok(None);
        }
        let profile = if let Some(profile) = &mut self.profile {
            profile
                .mapped_result
                .take()
                .expect("profile map result checked above")
                .map_err(anyhow::Error::msg)?;
            let view = profile
                .readback
                .slice(..)
                .get_mapped_range()
                .map_err(anyhow::Error::msg)?;
            let mut timestamps = vec![0_u64; profile.query_count as usize];
            for (timestamp, bytes) in timestamps.iter_mut().zip(view.chunks_exact(8)) {
                *timestamp =
                    u64::from_ne_bytes(bytes.try_into().expect("timestamp is eight bytes"));
            }
            drop(view);
            profile.readback.unmap();
            // Consecutive end samples partition the frame, per `QUERY_COUNT`.
            // A zero haze end means haze did not run; a whole-frame zero means
            // the driver dropped the samples.
            let [start, scene_end, haze_end, composite_end, index0, index1, grid0, grid1, prepared, lit] =
                <[u64; 10]>::try_from(&timestamps[..10]).expect("base queries");
            let haze_ran = haze_end > 0;
            // The composite pass samples both of its predecessors' outputs, so
            // it is the frame's sink and its completion is the frame's end.
            // The scene and haze passes have no such dependency on each other
            // and do overlap, either order — hence saturating cuts below.
            let timestamps_valid = start > 0
                && scene_end >= start
                && composite_end >= scene_end
                && (!haze_ran || (haze_end >= start && composite_end >= haze_end))
                && index0 > 0
                && index1 >= index0
                && (!profile.grid_fog
                    || (grid0 >= start && grid1 >= grid0 && composite_end >= grid1));
            if !timestamps_valid {
                anyhow::ensure!(
                    !profile.strict_timestamps,
                    "inconsistent GPU pass timestamps: {timestamps:?}"
                );
                None
            } else {
                let milliseconds = f64::from(profile.timestamp_period_ns) / 1_000_000.0;
                let span = |begin: u64, end: u64| end.saturating_sub(begin) as f64 * milliseconds;
                let origin = timestamps
                    .iter()
                    .copied()
                    .filter(|t| *t > 0)
                    .min()
                    .unwrap_or(start);
                let passes = profile
                    .passes
                    .iter()
                    .map(|&(name, begin, end)| {
                        let (begin, end) = (timestamps[begin as usize], timestamps[end as usize]);
                        anyhow::ensure!(
                            begin > 0 && end >= begin,
                            "invalid {name} timestamps: {begin}..{end}"
                        );
                        Ok(crate::pass_profile::GpuPassTiming {
                            name,
                            start_ms: span(origin, begin),
                            end_ms: span(origin, end),
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Some(FrameTimings {
                    passes,
                    cpu: self.cpu,
                    gpu_total_ms: span(start, composite_end),
                    gpu_volumetric_ms: if haze_ran {
                        span(scene_end, haze_end)
                    } else {
                        0.0
                    },
                    gpu_scene_ms: span(start, scene_end),
                    gpu_composite_ms: span(scene_end.max(haze_end), composite_end),
                    gpu_index_ms: span(index0, index1),
                    gpu_fog_grid_ms: if profile.grid_fog {
                        span(grid0, grid1)
                    } else {
                        0.0
                    },
                    gpu_fog_prepare_ms: if profile.grid_fog {
                        span(grid0, prepared)
                    } else {
                        0.0
                    },
                    gpu_fog_light_ms: if profile.grid_fog {
                        span(prepared, lit)
                    } else {
                        0.0
                    },
                    gpu_fog_integrate_ms: if profile.grid_fog {
                        span(lit, grid1)
                    } else {
                        0.0
                    },
                    cpu_encode_submit_ms: profile.cpu_encode_submit.as_secs_f64() * 1000.0,
                    cpu_cluster_ms: profile.cpu_cluster.as_secs_f64() * 1000.0,
                })
            }
        } else {
            None
        };
        let image = self.completion.image(self.width, self.height)?;
        Ok(Some(CompletedFrame {
            width: self.width,
            height: self.height,
            image,
            draw_time: self.started.elapsed(),
            profile,
            shadows: self.shadows,
            clusters: self.clusters,
            queued: self.queued,
            cpu: self.cpu,
            until_signalled: self
                .signalled
                .map(|at| at.saturating_duration_since(self.started)),
            until_noticed: self.signalled.map(|at| at.elapsed()),
        }))
    }
}

fn instance_of(draw: &Draw) -> Instance {
    Instance {
        model: draw.model.to_cols_array_2d(),
        normal_matrix: draw.model.inverse().transpose().to_cols_array_2d(),
        base_color: draw
            .material
            .base_color
            .extend(draw.material.metallic)
            .to_array(),
        emissive: draw
            .material
            .emissive
            .extend(draw.material.roughness)
            .to_array(),
        flags: [
            f32::from(u8::from(draw.material.flat_shading)),
            if draw.textures.normal.is_some() {
                draw.material.normal_scale
            } else {
                0.0
            },
            draw.material.occlusion_strength,
            0.0,
        ],
    }
}

/// Centre and radius of a bounding sphere around a mesh's vertices.
///
/// Centred on the midpoint of the extent rather than the centroid: a mesh with
/// most of its vertices clustered at one end would otherwise get a sphere that
/// has to reach much further to cover the rest.
fn local_bounding_sphere(vertices: &[crate::assets::Vertex]) -> (Vec3, f32) {
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

/// Claim the frame's start timestamp for the pass being encoded.
///
/// The first pass to ask gets it and later ones get `None`, so passes that skip
/// themselves cannot leave the frame without a start marker. Conditioning each
/// pass on whether the earlier ones ran is what made this fragile.
fn claim_start_timestamp<'a>(
    pending: &mut Option<&'a wgpu::QuerySet>,
) -> Option<wgpu::RenderPassTimestampWrites<'a>> {
    pending
        .take()
        .map(|query_set| wgpu::RenderPassTimestampWrites {
            query_set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: None,
        })
}

/// wgpu rejects a zero-sized storage buffer; an empty light list still needs a
/// binding.
fn pad_at_least_one<T: Pod + Zeroable>(mut v: Vec<T>) -> Vec<T> {
    if v.is_empty() {
        v.push(T::zeroed());
    }
    v
}

/// Select cue-independent fixture support only while fixture transport exists.
/// Invalid or non-enclosing metadata falls back to the current active domain.
fn select_fixture_lighting_domain(
    enabled: bool,
    frame: &Frame,
    active_empty: bool,
    mut medium: crate::medium::Uniform,
    active_fog_far: f32,
    camera_far: f32,
) -> (crate::medium::Uniform, f32, bool) {
    let Some(domain) = (enabled && !active_empty)
        .then_some(frame.fixture_lighting_domain)
        .flatten()
    else {
        return (medium, active_fog_far, false);
    };
    let current_eye = frame.camera.eye.to_array();
    let source_eye = domain.source_camera_eye;
    let matching_camera_eye = current_eye.iter().zip(source_eye).all(|(current, source)| {
        current.is_finite() && source.is_finite() && current.to_bits() == source.to_bits()
    });
    if !matching_camera_eye {
        return (medium, active_fog_far, false);
    }
    let selected_far = domain.fog_far.min(camera_far);
    let valid_far = domain.fog_far.is_finite()
        && domain.fog_far > 0.0
        && selected_far >= active_fog_far
        && selected_far <= camera_far;
    if !valid_far {
        return (medium, active_fog_far, false);
    }
    if frame.sky.is_some() {
        let active_min = Vec3::from_array(medium.min[..3].try_into().unwrap());
        let active_max = Vec3::from_array(medium.max[..3].try_into().unwrap());
        let valid_bounds = !domain.bounds.is_empty()
            && domain.bounds.min.is_finite()
            && domain.bounds.max.is_finite()
            && domain.bounds.min.cmple(active_min).all()
            && domain.bounds.max.cmpge(active_max).all();
        if !valid_bounds {
            return (medium, active_fog_far, false);
        }
        medium.min[..3].copy_from_slice(&domain.bounds.min.to_array());
        medium.max[..3].copy_from_slice(&domain.bounds.max.to_array());
    }
    // Indoors retain authored room bounds and the indoor/outdoor tag in
    // `medium.max.w`; only the cue-independent radial fog limit is selected.
    (medium, selected_far, true)
}

fn sanitize_fixture_cone(light: &crate::frame::FixtureCone) -> crate::frame::FixtureCone {
    let finite = |value: f32, fallback: f32| value.is_finite().then_some(value).unwrap_or(fallback);
    let finite_vec =
        |value: Vec3, fallback: Vec3| value.is_finite().then_some(value).unwrap_or(fallback);
    let cos_beam = finite(light.cos_beam, 0.95).clamp(0.01, 1.0);
    let cos_field = finite(light.cos_field, cos_beam)
        .clamp(0.01, 1.0)
        .min(cos_beam);
    crate::frame::FixtureCone {
        position: finite_vec(light.position, Vec3::ZERO)
            .clamp(Vec3::splat(-10_000.0), Vec3::splat(10_000.0)),
        range: finite(light.range, 0.05).clamp(0.05, 100.0),
        direction: finite_vec(light.direction, Vec3::NEG_Y)
            .try_normalize()
            .unwrap_or(Vec3::NEG_Y),
        cos_beam,
        color: finite_vec(light.color, Vec3::ZERO).clamp(Vec3::ZERO, Vec3::splat(100.0)),
        intensity: finite(light.intensity, 0.0).clamp(0.0, 100.0),
        cos_field,
        wash: finite(light.wash, 0.0).clamp(0.0, 1.0),
        gobo: light.gobo.min(2),
        gobo_rotation: finite(light.gobo_rotation, 0.0).rem_euclid(std::f32::consts::TAU),
        haze_gain: finite(light.haze_gain, 1.0).clamp(0.0, 1.0),
    }
}

/// History is valid only while projection-affecting state and the physical
/// light volumes are identical. Color/intensity may animate through history;
/// moving a cone, changing a gobo, resizing, orbiting, or changing density
/// resets it. Time continuity is checked separately at submission.
fn haze_history_key(
    frame: &Frame,
    width: u32,
    height: u32,
    haze: (u32, u32),
    density: f32,
    casters: u64,
) -> HazeHistoryKey {
    let mut topology = 0xcbf2_9ce4_8422_2325u64;
    let mut push = |value: u32| {
        topology = (topology ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    };
    let medium = crate::medium::Uniform::new(frame, density, 0.0);
    for value in medium
        .shape
        .into_iter()
        .chain(medium.wind)
        .chain(medium.min)
        .chain(medium.max)
    {
        push(value.to_bits());
    }
    push(frame.fixture_cones.len() as u32);
    for light in &frame.fixture_cones {
        for value in light.position.to_array() {
            push(value.to_bits());
        }
        push(light.range.to_bits());
        for value in light.direction.to_array() {
            push(value.to_bits());
        }
        push(light.cos_beam.to_bits());
        push(light.cos_field.to_bits());
        push(light.wash.to_bits());
        push(light.gobo);
        push(light.gobo_rotation.to_bits());
        push(light.intensity.to_bits());
        push(light.haze_gain.to_bits());
        for value in light.color.to_array() {
            push(value.to_bits());
        }
    }
    HazeHistoryKey {
        output: [width, height, haze.0, haze.1],
        camera: [
            frame.camera.eye.x.to_bits(),
            frame.camera.eye.y.to_bits(),
            frame.camera.eye.z.to_bits(),
            frame.camera.target.x.to_bits(),
            frame.camera.target.y.to_bits(),
            frame.camera.target.z.to_bits(),
        ],
        fov: frame.camera.fov_y_deg.to_bits(),
        density: density.to_bits(),
        topology,
        shadows: [frame.fixture_shadows, frame.geometry_shadows],
        casters,
    }
}

/// Exact key for a scalar residual's geometry, medium and transmittance.
/// Time is intentionally zero: phased refresh is the bounded approximation
/// for animated medium state. Current radiance controls are intentionally
/// absent because `beam_scatter_hot` applies them after loading the scalar.
#[allow(clippy::too_many_arguments)]
fn residual_transport_key(
    frame: &Frame,
    width: u32,
    height: u32,
    haze: (u32, u32),
    mut medium: crate::medium::Uniform,
    fog_far: f32,
    camera_far: f32,
    casters: u64,
    depth: u64,
    cached_shadows: bool,
    shadow_samples: u32,
    per_resident: bool,
    cones: &[crate::frame::FixtureCone],
    rests: &[LightRest],
) -> ResidualGlobalKey {
    medium.wind[3] = 0.0;
    let medium = medium
        .shape
        .into_iter()
        .chain(medium.wind)
        .chain(medium.min)
        .chain(medium.max)
        .map(f32::to_bits)
        .collect::<Vec<_>>()
        .try_into()
        .expect("medium uniform has sixteen words");
    let mut legacy_topology = Vec::new();
    if !per_resident {
        legacy_topology.reserve(1 + cones.len() * 14);
        legacy_topology.push(cones.len() as u32);
        for (cone, rest) in cones.iter().zip(rests) {
            legacy_topology.extend(cone.position.to_array().map(f32::to_bits));
            legacy_topology.push(cone.range.to_bits());
            legacy_topology.extend(rest.direction.map(f32::to_bits));
            legacy_topology.extend([
                rest.cos_beam.to_bits(),
                rest.cos_field.to_bits(),
                rest.wash.to_bits(),
                rest.gobo.to_bits(),
                rest.gobo_rotation.to_bits(),
                rest.shadow_slot.to_bits(),
                rest.inverse_right_length.to_bits(),
                rest.field_tangent.to_bits(),
            ]);
        }
    }
    ResidualGlobalKey {
        output: [width, height, haze.0, haze.1],
        camera: [
            frame.camera.eye.x.to_bits(),
            frame.camera.eye.y.to_bits(),
            frame.camera.eye.z.to_bits(),
            frame.camera.target.x.to_bits(),
            frame.camera.target.y.to_bits(),
            frame.camera.target.z.to_bits(),
            frame.camera.fov_y_deg.to_bits(),
            camera_far.to_bits(),
        ],
        medium,
        fog_far: fog_far.to_bits(),
        legacy_topology,
        shadows: [frame.fixture_shadows, cached_shadows],
        casters,
        depth,
        shadow_samples,
    }
}

fn shader(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
    })
}

fn specialize_wgsl_overrides(source: &str, values: &[(&str, f64)]) -> String {
    source
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let Some(declaration) = trimmed.strip_prefix("override ") else {
                return line.to_owned();
            };
            let (name, remainder) = declaration.split_once(':').expect("WGSL override type");
            let Some(value) = values
                .iter()
                .find_map(|(key, value)| (*key == name).then_some(*value))
            else {
                return line.replacen("override", "const", 1);
            };
            let (kind, _) = remainder
                .split_once('=')
                .expect("WGSL override initializer");
            let literal = match kind.trim() {
                "bool" => match value {
                    0.0 => "false".to_owned(),
                    1.0 => "true".to_owned(),
                    _ => panic!("boolean WGSL override {name} must be zero or one"),
                },
                "u32" => format!("{}u", value as u32),
                "i32" => format!("{}i", value as i32),
                "f32" => {
                    assert!(value.is_finite(), "WGSL override {name} must be finite");
                    let mut literal = value.to_string();
                    if !literal.contains(['.', 'e', 'E']) {
                        literal.push_str(".0");
                    }
                    literal
                }
                other => panic!("unsupported WGSL override type {other}"),
            };
            let indent = &line[..line.len() - trimmed.len()];
            format!("{indent}const {name}: {} = {literal};", kind.trim())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fit three stable orthographic sun cameras to bounded view-frustum slices.
///
/// Each slice uses a quantized bounding sphere instead of a tight AABB. Camera
/// translation then changes only the snapped light-space centre; small camera
/// rotations cannot resize the projection and make every shadow texel swim.
fn cascade_matrices(
    eye: Vec3,
    forward: Vec3,
    fov_y: f32,
    aspect: f32,
    to_light: Vec3,
) -> [Mat4; CASCADE_COUNT] {
    let forward = forward.normalize_or(Vec3::Y);
    let world_up = if forward.z.abs() > 0.99 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let right = forward.cross(world_up).normalize_or(Vec3::X);
    let up = right.cross(forward).normalize_or(Vec3::Z);
    let light_dir = to_light.normalize_or(Vec3::new(0.4, -0.6, 0.7));
    let light_up = if light_dir.z.abs() > 0.99 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let tan_half = (fov_y * 0.5).tan();

    std::array::from_fn(|cascade| {
        let near = if cascade == 0 {
            CAMERA_NEAR
        } else {
            CASCADE_SPLITS[cascade - 1]
        };
        let far = CASCADE_SPLITS[cascade];
        let corners = [near, far].map(|distance| {
            let center = eye + forward * distance;
            let half_y = tan_half * distance;
            let half_x = half_y * aspect;
            [
                center - right * half_x - up * half_y,
                center + right * half_x - up * half_y,
                center - right * half_x + up * half_y,
                center + right * half_x + up * half_y,
            ]
        });
        let corners = [
            corners[0][0],
            corners[0][1],
            corners[0][2],
            corners[0][3],
            corners[1][0],
            corners[1][1],
            corners[1][2],
            corners[1][3],
        ];
        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        // Quantization prevents floating-point radius noise from changing the
        // projection scale. 1/16 m is well below one far-cascade texel.
        let radius = corners
            .iter()
            .map(|corner| corner.distance(center))
            .fold(0.0_f32, f32::max);
        let radius = (radius * 16.0).ceil() / 16.0;
        // Keep light orientation and origin independent of the camera. The
        // orthographic bounds carry the moving slice; this is what lets their
        // centre snap in world-shadow space instead of remaining perpetually
        // zero in a camera-following look-at matrix.
        let light_view = Mat4::look_at_rh(light_dir, Vec3::ZERO, light_up);
        let center_light = light_view.transform_point3(center);
        let texel = (2.0 * radius) / SHADOW_SIZE as f32;
        let snapped_x = (center_light.x / texel).round() * texel;
        let snapped_y = (center_light.y / texel).round() * texel;
        let (min_depth, max_depth) = corners
            .iter()
            .map(|corner| -light_view.transform_point3(*corner).z)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), depth| {
                (min.min(depth), max.max(depth))
            });
        // Swap near/far to map the nearest receiver to one and the furthest to
        // zero, matching the scene's GreaterEqual reverse-Z convention. The
        // signed distances are valid for an orthographic projection and let
        // this fixed light frame cover venues on either side of its origin.
        Mat4::orthographic_rh(
            snapped_x - radius,
            snapped_x + radius,
            snapped_y - radius,
            snapped_y + radius,
            max_depth + 25.0,
            min_depth - 25.0,
        ) * light_view
    })
}

fn depth_state(write: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(write),
        // Equal preserves the intentional coplanar photo panels in stage GLBs.
        // Greater is the single convention for camera and sun reverse-Z.
        depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn depth_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    samples: u32,
    label: &str,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn depth_attachment(view: &wgpu::TextureView) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

/// Depth that lives and dies inside its own pass. `Discard` maps to Metal's
/// `DontCare`, so on a tile architecture the buffer never leaves tile memory —
/// a `Store` here writes the full MSAA depth surface to DRAM every frame for
/// nobody, since nothing samples it and the resolve is colour-only.
/// Pins the scene-encoding closure's signature so its query-set argument and
/// the pass profiler share one lifetime; closure parameters cannot name one.
fn scene_encoder<'q, F>(encode: F) -> F
where
    F: Fn(
        &mut wgpu::CommandEncoder,
        &mut crate::pass_profile::PassQueries<'q>,
        &Gpu,
        Option<&'q wgpu::QuerySet>,
    ),
{
    encode
}

fn depth_attachment_transient(
    view: &wgpu::TextureView,
) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0.0),
            store: wgpu::StoreOp::Discard,
        }),
        stencil_ops: None,
    }
}

/// Load the full-resolution prepass depth for post-composite editor cages.
fn depth_attachment_load(view: &wgpu::TextureView) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    }
}

fn shadow_texture_array(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    layers: u32,
    label: &str,
) -> (wgpu::TextureView, [wgpu::TextureView; CASCADE_COUNT]) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let array = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(label),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        base_array_layer: 0,
        array_layer_count: Some(layers),
        ..Default::default()
    });
    let render_layers = std::array::from_fn(|index| {
        let layer = (index as u32).min(layers - 1);
        texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("shadow-cascade-layer"),
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        })
    });
    (array, render_layers)
}

fn binding(index: u32, resource: wgpu::BindingResource) -> wgpu::BindGroupEntry {
    wgpu::BindGroupEntry {
        binding: index,
        resource,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn rw_storage_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn depth_array_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
