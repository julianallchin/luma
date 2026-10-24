//! The wgpu device and the frame's pass chain.
//!
//! Offscreen only: nothing here knows about a window or a swapchain. A frame
//! goes shadow → depth → scene (MSAA) → haze (accumulated) → composite+AgX →
//! editor overlays, and comes out sRGB-encoded 8-bit — either read back as
//! bytes in the caller's [`Channels`] order, or left in memory a compositor
//! addresses directly. That choice is the frame's [`Destination`], and it is
//! the only thing about a frame this module lets a caller vary that is not
//! about the picture. The one exception is a compositor that presents HDR:
//! its frame comes out half-float linear, with highlights above SDR white
//! (see [`DisplayRange`]).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::viewport::DisplayRange;

use crate::assets::Image;
use crate::atmosphere::{AtmosphereCache, AtmospherePipelines};
use crate::environment::{EnvironmentCache, EnvironmentPipelines};
use crate::frame::{Draw, Frame};
use crate::haze_compact::{CompactStats, HAZE_WORKGROUP};
use crate::haze_field::HazeField;
use crate::light_index::{
    LightCore, LightIndex, LightIndexInput, LightIndexPipelines, LightIndexStats, LightRest,
};
use crate::overlay::{Overlay, OverlayDepth};
use crate::shadow::{
    FIXTURE_SHADOW_SIZE, MAX_FIXTURE_SHADOWS, ShadowCacheKey, assign_shadow_slots,
    fixture_shadow_caster_hash, fixture_shadow_matrix, fixture_shadow_planes,
    fixture_shadow_texture_array, shadow_matrix_bits,
};
use crate::viewport::{PRESENTATION_SLOTS, Presented};

/// Three bounded layers cover the part of a venue in which directional
/// shadows remain useful. 2048² per layer costs 48 MiB in `Depth32Float`, versus
/// an unbounded camera-sized allocation or 192 MiB for three legacy 4096 maps.
pub(crate) const SHADOW_SIZE: u32 = 2048;
pub(crate) const CASCADE_COUNT: usize = 3;
pub(crate) const CASCADE_SPLITS: [f32; CASCADE_COUNT] = [12.0, 45.0, 180.0];
pub(crate) const CASCADE_BLEND: f32 = 0.1;

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
pub(crate) const CAMERA_NEAR: f32 = 0.1;
const CAMERA_FAR: f32 = 2000.0;
/// Emitted intensity times the brightest colour channel below which a cone
/// is dark. The frame builder already drops dimmers under 1%; this catches a
/// lit dimmer on a black colour.
const DARK_CONE: f32 = 1e-4;

/// The per-frame work a [`Quality`](crate::scene_desc::Quality) level buys.
/// `High` is the reference picture: its shadow walk is the one the renderer
/// had before the levels existed. Its plain-beam piece cap and shadow redraw
/// budget are new, and measured to leave the picture unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
struct QualityBudget {
    /// Gauss-Legendre piece cap for a plain beam (`quadrature_pieces`).
    plain_pieces: f32,
    /// Scale on the haze shadow walk's pixel-footprint allowance.
    shadow_footprint: f32,
    /// Scale on the haze shadow walk's soft and hard step counts.
    shadow_steps: f32,
    /// Most moved fixture shadow maps redrawn in one frame.
    shadow_redraws: usize,
    /// Far-field fog grid cell size in pixels.
    fog_tile: u32,
    /// Scene-pass samples per pixel.
    msaa: u32,
}

impl QualityBudget {
    fn of(quality: crate::scene_desc::Quality, camera_moving: bool) -> Self {
        use crate::scene_desc::Quality;
        match (quality, camera_moving) {
            (Quality::High, _) => Self {
                plain_pieces: 8.0,
                shadow_footprint: 1.0,
                shadow_steps: 1.0,
                shadow_redraws: 64,
                fog_tile: crate::fog_grid::TILE_SIZE,
                msaa: MSAA_SAMPLES,
            },
            // A laptop budget: twice the shadow walk's footprint allowance
            // and half its steps, and twice again while the camera moves,
            // when nothing settles anyway.
            (Quality::Low, moving) => Self {
                plain_pieces: 4.0,
                shadow_footprint: if moving { 4.0 } else { 2.0 },
                shadow_steps: if moving { 0.25 } else { 0.5 },
                shadow_redraws: 16,
                fog_tile: crate::fog_grid::LOW_TILE_SIZE,
                msaa: 1,
            },
        }
    }

    /// `Haze::quality`.
    fn haze(self) -> [f32; 4] {
        [
            self.plain_pieces,
            self.shadow_footprint,
            self.shadow_steps,
            0.0,
        ]
    }
}

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
    /// x: fog-grid radial extent, y: 1 when the surface takes outdoor camera
    /// transmittance from the far-field fog grid, 0 when it marches the
    /// medium. See `scene_bindings.wgsl`.
    surface_fog: [f32; 4],
    /// xy: output size in pixels, zw: reciprocal.
    viewport: [f32; 4],
    /// The ground's floor material; see `floor.wgsl`.
    floor: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    model: [[f32; 4]; 4],
    normal_matrix: [[f32; 4]; 4],
    base_color: [f32; 4],
    emissive: [f32; 4],
    /// x: 1 for the ground, which casts no sun shadow; y: normal-map scale;
    /// z: AO strength.
    flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct OverlayInstance {
    model: [[f32; 4]; 4],
    color: [f32; 4],
}

/// Mirrors `IntervalCacheParams` in `haze_cache.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct IntervalCacheUniform {
    /// x: cache blocks per row, y: hash bucket count minus one, z: nonzero at
    /// native haze resolution, where 2×2 quads may share quadrature.
    params: [u32; 4],
    /// y: frame epoch.
    flags: [u32; 4],
    slots: [[u32; 4]; 512],
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
    /// [`QualityBudget::haze`].
    quality: [f32; 4],
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
    /// x: display headroom, the brightest output as a multiple of SDR white.
    /// Read only by the [`Channels::Hdr`] pipeline. y: 1 when the sun
    /// shaft texture (`sun_shafts.rs`) holds this frame. zw unused.
    display: [f32; 4],
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

/// The output texture's format: the byte order a readback is written in, or
/// half-float for a compositor that presents HDR.
///
/// This is the *output texture's* format, not a post-processing step: the
/// composite pass writes straight into the order the caller asked for, so the
/// readback is a row memcpy either way. Swizzling on the CPU instead cost about
/// a millisecond per megapixel, for a choice the sampler makes for free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Channels {
    /// What a PNG wants.
    Rgba,
    /// What `gpui::RenderImage` wants.
    Bgra,
    /// The [`Self::Rgba`] picture at ten bits a channel, for a video encoder:
    /// 8-bit darks band, and their dither is noise it smears into blocks.
    /// `Rgb10a2Unorm`, so four bytes a pixel like the others. It has no sRGB
    /// variant, so the shaders that write it encode sRGB themselves.
    Rgb10,
    /// Linear light for an HDR compositor: 1.0 is SDR white and highlights
    /// go above it. Only ever a shared target, never read back as an image.
    Hdr,
}

/// Who reads a finished frame, which is what decides where it is written.
///
/// This is not a quality or a format switch — both destinations run the same
/// passes and produce the same picture. It decides only whether the frame is
/// staged for the CPU or left in memory the window compositor can address, and
/// therefore whether a copy is encoded at all. The exception is a compositor
/// that presents HDR, which receives the same picture with its highlights
/// kept above SDR white.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Destination {
    /// The caller, as bytes in this order. An export, a golden, a video frame.
    Bytes(Channels),
    /// The window compositor, in place, for a display in this range. Falls
    /// back to `Bytes(Bgra)` (SDR) on a platform or adapter with no shareable
    /// memory, so a caller asking for this never has to know whether it got
    /// it.
    Compositor(DisplayRange),
}

impl Destination {
    /// The format a shared target is allocated in.
    fn channels(self) -> Channels {
        match self {
            Self::Bytes(channels) => channels,
            Self::Compositor(DisplayRange::Sdr) => Channels::Bgra,
            Self::Compositor(DisplayRange::Hdr { .. }) => Channels::Hdr,
        }
    }

    /// The format a staged target is allocated in: a readback is always
    /// SDR, so an HDR frame that could not be shared falls back to it.
    fn staged_channels(self) -> Channels {
        match self.channels() {
            Channels::Hdr => Channels::Bgra,
            channels => channels,
        }
    }

    /// Whether `other` can reuse this destination's targets. The headroom is
    /// a uniform, not a target property, so a new one reallocates nothing.
    fn same_targets(self, other: Self) -> bool {
        matches!(self, Self::Compositor(_)) == matches!(other, Self::Compositor(_))
            && self.channels() == other.channels()
    }

    /// The headroom the composite pass maps highlights into; 1 in SDR.
    fn headroom(self) -> f32 {
        match self {
            Self::Compositor(DisplayRange::Hdr { headroom }) => headroom.max(1.0),
            _ => 1.0,
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
    /// Scene completion through the last haze pass's completion. Zero when haze did not run — and also when it
    /// ran entirely alongside the scene pass and finished first, in which case
    /// its cost is inside [`Self::gpu_scene_ms`].
    pub gpu_volumetric_ms: f64,
    /// Frame start through scene completion: the light-index build, the
    /// shadow and depth passes, the scene pass, and whatever haze work overlapped
    /// them. Zero redrawn maps (`ShadowStats::redrawn_maps`) makes this the
    /// depth prepass and the scene pass alone.
    pub gpu_scene_ms: f64,
    /// The composite pass, and the post chain (`post.rs`) when the frame's
    /// look runs one: its two inputs' completion through the output's last
    /// write. Editor overlays run after the last sample, so they are outside
    /// every span here.
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
    const ALL: [Self; 4] = [Self::Rgba, Self::Bgra, Self::Hdr, Self::Rgb10];

    pub(crate) fn format(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Bgra => wgpu::TextureFormat::Bgra8UnormSrgb,
            Self::Hdr => wgpu::TextureFormat::Rgba16Float,
            Self::Rgb10 => wgpu::TextureFormat::Rgb10a2Unorm,
        }
    }

    /// The overrides the composite and tonemap passes take for this target.
    pub(crate) fn display_constants(self) -> &'static [(&'static str, f64)] {
        match self {
            Self::Hdr => &[("HDR_OUTPUT", 1.0)],
            Self::Rgb10 => &[("TEN_BIT_OUTPUT", 1.0)],
            Self::Rgba | Self::Bgra => &[],
        }
    }

    /// The channels an output texture of `format` was allocated for.
    fn of(format: wgpu::TextureFormat) -> Self {
        Self::ALL
            .into_iter()
            .find(|channels| channels.format() == format)
            .expect("every output texture is allocated from a `Channels`")
    }

    /// Slot in [`Renderer::composite_pipelines`].
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Rgba => 0,
            Self::Bgra => 1,
            Self::Hdr => 2,
            Self::Rgb10 => 3,
        }
    }
}

/// One scene's worth of GPU state: everything a frame writes, plus the caches
/// that keep it from being rebuilt every frame.
///
/// The device and the pipelines are *not* here — they are [`Gpu`], shared by
/// every renderer in the process. What is left is exactly the state two
/// renderers could not share without drawing into each other's picture: the
/// shadow atlases, the sized render targets, the haze caches and the
/// resident-asset caches. One instance renders any number of frames at any
/// number of sizes; render targets are reallocated on size change.
pub struct Renderer {
    gpu: Arc<Gpu>,
    /// Which environment probe this renderer has resident. The pipelines that
    /// produce it are shared; the probe is not, because it is uploaded from
    /// whatever scene *this* renderer was last asked to draw.
    environment: EnvironmentCache,
    atmosphere: AtmosphereCache,
    sun_shafts: crate::sun_shafts::Cache,
    /// The local reflection probes over this renderer's stage.
    probes: crate::probes::Probes,
    sky_visibility: crate::sky_visibility::SkyVisibility,
    /// Exposure state and post-chain targets (`post.rs`).
    post: crate::post::Post,
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
    /// Per slot, the position a map was last drawn from and the projection it
    /// was drawn with; a deferred redraw keeps sampling it.
    fixture_shadow_drawn: Vec<Option<(Vec3, FixtureShadowMatrix)>>,
    /// Per slot, frames a moved map has waited for its redraw.
    fixture_shadow_age: Vec<u32>,
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
    /// Draw haze with the fragment pass's per-ray MIS estimator
    /// ([`Self::render_reference`]).
    reference: bool,
    fog_blocks_valid: bool,
    surface_depth_cull: bool,
    upload_stats: UploadStats,
    targets: Option<Targets>,
    medium_cache: crate::medium::Cache,
    shadow_hierarchy: crate::shadow_hierarchy::Targets,
    /// The previous live frame's eye, target and field of view, to tell a
    /// moving camera from a still one.
    last_live_camera: Option<[u32; 7]>,
    profiler: Option<ProfilerResources>,
    light_index: LightIndex,
    shadow_stats: ShadowStats,
    /// Lit-interval cache bookkeeping and pools; see `interval_cache.rs`.
    interval_cache: crate::interval_cache::IntervalCacheState,
    interval_pools: Option<IntervalCachePools>,
    /// Residual compaction of the cached kernel; see `haze_compact.rs`.
    compaction: crate::haze_compact::Compaction,
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
    /// The ground's floor material, which samples anisotropically: it is
    /// seen at grazing angles out to the horizon.
    floor: bool,
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
            floor: frame.floor.is_some() && crate::frame::is_ground(&frame.meshes[draw.mesh].key),
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

/// Cache identity of each sun cascade. The cascades rasterise the whole opaque
/// list — moving heads cast sun shadows — so their caster identity is
/// [`opaque_depth_hash`], not the fixture maps' caster hash, which leaves
/// fixture bodies out and would keep a moved head's old shadow.
fn cascade_cache_keys(
    light_view_proj: &[Mat4; CASCADE_COUNT],
    depth_hash: u64,
) -> [ShadowCacheKey; CASCADE_COUNT] {
    light_view_proj.map(|matrix| ShadowCacheKey {
        matrix_bits: shadow_matrix_bits(&matrix.to_cols_array_2d()),
        caster_hash: depth_hash,
    })
}

struct Targets {
    width: u32,
    height: u32,
    /// Size of the haze target, which runs at `haze_resolution` of the output.
    haze_width: u32,
    haze_height: u32,
    destination: Destination,
    /// The multisampled scene colour; `None` at one sample, where the scene
    /// pass draws straight into `scene`.
    msaa_color: Option<wgpu::TextureView>,
    /// Scene-pass samples per pixel.
    samples: u32,
    msaa_surface_depth: Option<wgpu::TextureView>,
    msaa_depth: wgpu::TextureView,
    scene: wgpu::TextureView,
    depth: wgpu::TextureView,
    haze: wgpu::TextureView,
    fog: crate::fog_grid::Targets,
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

    /// The format this target was allocated in, which is what the passes
    /// that write it must target. It can differ from what was asked for: an
    /// HDR frame with no shareable memory is staged as SDR.
    fn channels(&self) -> Channels {
        Channels::of(self.view().texture().format())
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
/// haze caches and every other cache are [`Renderer`] state, because two
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
/// The process-wide wgpu device and queue, with every pipeline and layout the
/// renderers share. Built once and borrowed by each [`Renderer`].
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_profile: RendererProfile,
    environment: EnvironmentPipelines,
    atmosphere: AtmospherePipelines,
    sun_shafts: crate::sun_shafts::Pipelines,
    /// The local reflection probes' capture, relight and prefilter.
    probes: crate::probes::Pipelines,
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
    composite_layout: wgpu::BindGroupLayout,
    overlay_layout: wgpu::BindGroupLayout,
    /// Scene-pass pipelines at [`MSAA_SAMPLES`].
    scene_pipelines: ScenePipelines,
    /// What the single-sample set is built from, on first low-quality frame.
    scene_source: ScenePipelineSource,
    scene_pipelines_single: std::sync::OnceLock<ScenePipelines>,
    depth_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    fixture_shadow_layout: wgpu::BindGroupLayout,
    fixture_shadow_pipeline: wgpu::RenderPipeline,
    /// The per-ray MIS haze pass: gobos, multi-subframe captures and the
    /// exhaustive reference.
    haze_pipeline: wgpu::RenderPipeline,
    /// The fused compute haze kernel, which caches its shadow-traversal
    /// output (`interval_cache.rs`).
    haze_compute_pipeline: wgpu::ComputePipeline,
    haze_compute_layout: wgpu::BindGroupLayout,
    /// log2 of the payload hash table's entry count.
    interval_cache_table_bits: u32,
    /// Residual compaction of the cached kernel (`haze_compact.rs`).
    haze_compact_pipelines: crate::haze_compact::Pipelines,
    profile_repeat: Option<String>,
    fog_grid_pipeline: wgpu::ComputePipeline,
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
    /// Indexed by [`Channels::index`]: the same pass, targeting each output
    /// format. The [`Channels::Hdr`] one keeps highlights above SDR white.
    composite_pipelines: [wgpu::RenderPipeline; 4],
    /// The composite pass writing scene-linear light for the post chain.
    composite_linear_pipeline: wgpu::RenderPipeline,
    /// Exposure, lens glow, tone curve and glare (`post.rs`).
    post: crate::post::Pipelines,
    /// Indexed by [`overlay_pipeline_index`]: the four output formats
    /// crossed with two topologies and two depth behaviours.
    overlay_pipelines: [wgpu::RenderPipeline; 16],
    hard_shadow_sampler: wgpu::Sampler,
    shadow_sampler: wgpu::Sampler,
    /// A 1x1 depth array bound where a pass has no real shadow map to offer.
    dummy_shadow: wgpu::TextureView,
    linear_sampler: wgpu::Sampler,
    texture_sampler: wgpu::Sampler,
    /// The floor material's sampler: [`Self::texture_sampler`] with 16x
    /// anisotropy.
    floor_sampler: wgpu::Sampler,
    /// Neutral glTF maps, bound by procedural/depth-only draws.
    white_material: wgpu::BindGroup,
    material_defaults: MaterialDefaults,
    sky_visibility: crate::sky_visibility::Pipelines,
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
        // An adopted compositor device was made without asking us.
        crate::device::require_subgroups(context.device.features(), &context.adapter.get_info())?;
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
        // The probe relight (`probes.rs`) lights its texels with the scene
        // pass's own functions, in compute, so groups 0 and 3 are visible there.
        for entry in &mut scene_entries {
            entry.visibility |= wgpu::ShaderStages::COMPUTE;
        }
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
        let mut cluster_entries = vec![
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
            // Per-pixel ambient visibility (`sky_visibility.rs`).
            wgpu::BindGroupLayoutEntry {
                binding: 15,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            // The sun-shaft fraction (`sun_shafts.rs`).
            wgpu::BindGroupLayoutEntry {
                binding: 16,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ];
        for entry in &mut cluster_entries {
            entry.visibility |= wgpu::ShaderStages::COMPUTE;
        }
        let cluster_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("surface-clusters"),
            entries: &cluster_entries,
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
                // The sun-shaft fraction (`sun_shafts.rs`).
                texture_entry(7),
            ],
        });

        // Both scene-geometry shaders open with the shared bind-group
        // declarations; see `scene_bindings.wgsl`.
        let bindings = scene_bindings_wgsl();
        let sun_shafts = crate::sun_shafts::Pipelines::new(
            &device,
            &queue,
            &format!("{bindings}{}", include_str!("shaders/sun_shafts.wgsl")),
        );
        let fixture_light = include_str!("shaders/fixture_light.wgsl");
        let visibility = include_str!("shaders/visibility.wgsl");
        let haze_visibility = visibility.replace("@group(3) @binding(11)", "@group(0) @binding(8)");
        // The light-index prelude is authored against group 1 (the haze
        // pass's slot); the surface pass carries the same bindings inside its
        // group 3, so its copy is rebound by this one documented replace.
        let light_index_prelude = include_str!("shaders/light_index.wgsl");
        let scene_wgsl = scene_wgsl();
        let scene_module = shader(&device, "scene", &scene_wgsl);
        // The transport (ray reconstruction + per-light integral + group-0
        // layout) is one file both volumetric passes prepend, so they cannot
        // draw two different beams.
        let beam_transport = beam_transport_wgsl();
        // The density field's dimensions are compile-time properties of
        // `haze_field`, so they arrive as injected constants rather than as
        // uniform members nobody could see drift.
        let haze_field_prelude = crate::haze_field::prelude();
        let haze_base_text = format!(
            "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}{}",
            include_str!("shaders/haze.wgsl"),
            include_str!("shaders/haze_cache.wgsl"),
        );
        let haze_module = shader(&device, "haze", &haze_base_text);
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
        let composite_module = shader(&device, "composite", &composite_wgsl());
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

        let vertex_layout = mesh_vertex_layout();

        let probes = crate::probes::Pipelines::new(
            &device,
            &crate::probes::PipelineSources {
                material_layout: &material_layout,
                scene_layout: &scene_layout,
                environment_layout: environment.scene_layout(),
                cluster_layout: &cluster_layout,
                vertex_layout: mesh_vertex_layout(),
                scene_source: &scene_wgsl,
            },
        );

        let scene_source = ScenePipelineSource {
            layout: scene_pipeline_layout.clone(),
            scene: scene_module.clone(),
            grid: grid_module.clone(),
            cables: cable_module.clone(),
            skips: [
                omitted("surface-clouds"),
                omitted("surface-lighting"),
                omitted("surface-shadows"),
            ],
        };
        let scene_pipelines = scene_source.build(&device, MSAA_SAMPLES);

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
        let fog_grid_write_entries = [
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
        let fog_grid_write_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("fog-grid-write"),
                entries: &fog_grid_write_entries,
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
        let fog_grid_module = shader(
            &device,
            "fog-grid",
            &format!(
                "{haze_field_prelude}{fixture_light}{light_index_prelude}{haze_visibility}{beam_transport}{}",
                include_str!("shaders/haze_grid.wgsl")
            ),
        );
        let fog_grid_constants = [(
            "PROFILE_SKIP_GRID_SHADOW_TESTS",
            omitted("grid-shadow-tests"),
        )];
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

        let haze_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("haze"),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("haze"),
                        // The transport declares the compute kernels' grid
                        // groups too; the reference reads neither.
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
                    targets: &[Some(wgpu::ColorTargetState {
                        format: SCENE_FORMAT,
                        blend: Some(wgpu::BlendState {
                            color: ADD,
                            alpha: ADD,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &[
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
            });

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
        // Lit-interval cache: slot table, header words, payload claims and
        // payload entries. Writable bindings, but this layout belongs to the
        // compute kernels alone, so no other pass is serialized against them.
        let haze_compute_entries = [
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
            uniform_entry(4, wgpu::ShaderStages::COMPUTE),
            rw_storage_entry(5, wgpu::ShaderStages::COMPUTE),
            rw_storage_entry(6, wgpu::ShaderStages::COMPUTE),
            rw_storage_entry(7, wgpu::ShaderStages::COMPUTE),
        ];
        let haze_compute_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("haze-compute"),
                entries: &haze_compute_entries,
            });
        let haze_constants = [
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
        let haze_compact_pipelines = crate::haze_compact::Pipelines::new(
            &device,
            [
                &haze_layout,
                light_index_pipelines.layout(),
                &fog_grid_read_layout,
            ],
            &haze_compute_entries,
            &format!(
                "{haze_base_text}{}",
                include_str!("shaders/haze_compact.wgsl")
            ),
            &haze_constants,
        );
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
                entry_point: Some("compute_haze"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &haze_constants,
                    ..Default::default()
                },
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
        let composite_pipelines = Channels::ALL.map(|channels| {
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
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: channels.display_constants(),
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

        let composite_linear_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("composite-linear"),
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
                    targets: &[Some(crate::post::SCENE_FORMAT.into())],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &[("LINEAR_OUTPUT", 1.0)],
                        ..Default::default()
                    },
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
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
        let overlay_module = shader(
            &device,
            "overlay",
            &format!(
                "{}{}",
                include_str!("shaders/tone.wgsl"),
                include_str!("shaders/overlay.wgsl")
            ),
        );
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
            let channels = Channels::ALL[i / 4];
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
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: if channels == Channels::Rgb10 {
                            &[("TEN_BIT_OUTPUT", 1.0)]
                        } else {
                            &[]
                        },
                        ..Default::default()
                    },
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
        let floor_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("floor"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
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
        let sky_visibility = crate::sky_visibility::Pipelines::new(&device, &queue);
        let post = crate::post::Pipelines::new(
            &device,
            &queue,
            &Channels::ALL.map(|channels| (channels.format(), channels.display_constants())),
        );
        let interval_cache_table_bits = crate::interval_cache::table_bits(&device.limits());
        Ok(Self {
            sky_visibility,
            post,
            composite_linear_pipeline,
            device,
            queue,
            adapter_profile,
            environment,
            atmosphere,
            sun_shafts,
            probes,
            scene_layout,
            material_layout,
            cluster_layout,
            haze_layout,
            medium_cache_layout,
            medium_cache_pipeline,
            shadow_hierarchy_pipelines,
            haze_field,
            light_index_pipelines,
            composite_layout,
            overlay_layout,
            scene_pipelines,
            scene_source,
            scene_pipelines_single: std::sync::OnceLock::new(),
            depth_pipeline,
            shadow_pipeline,
            fixture_shadow_layout,
            fixture_shadow_pipeline,
            haze_pipeline,
            haze_compute_pipeline,
            haze_compute_layout,
            interval_cache_table_bits,
            haze_compact_pipelines,
            profile_repeat,
            fog_grid_pipeline,
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
            composite_pipelines,
            overlay_pipelines,
            hard_shadow_sampler,
            shadow_sampler,
            dummy_shadow,
            linear_sampler,
            texture_sampler,
            floor_sampler,
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
        let post = crate::post::Post::new(&gpu.device);
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
        let compaction = crate::haze_compact::Compaction::new(&gpu.device);
        let probes = crate::probes::Probes::new(&gpu.device);
        Self {
            gpu,
            fog_blocks_valid: false,
            environment: EnvironmentCache::default(),
            atmosphere: AtmosphereCache::default(),
            sun_shafts: crate::sun_shafts::Cache::default(),
            probes,
            sky_visibility: crate::sky_visibility::SkyVisibility::default(),
            post,
            shadow_map,
            shadow_layers,
            fixture_shadow_map,
            fixture_shadow_map_extra,
            fixture_shadow_layers,
            fixture_shadow_cache: vec![None; MAX_FIXTURE_SHADOWS],
            cascade_shadow_cache: [None; CASCADE_COUNT],
            fixture_shadow_slots: vec![None; MAX_FIXTURE_SHADOWS],
            fixture_shadow_drawn: vec![None; MAX_FIXTURE_SHADOWS],
            fixture_shadow_age: vec![0; MAX_FIXTURE_SHADOWS],
            texture_views: HashMap::new(),
            materials: HashMap::new(),
            geometry: None,
            visibility,
            geometry_shadows: false,
            visibility_reference: std::env::var_os("LUMA_VISIBILITY_REFERENCE")
                .is_some_and(|v| v == "1"),
            surface_depth_cull: !std::env::var_os("LUMA_SURFACE_DEPTH_CULL")
                .is_some_and(|v| v == "0"),
            reference: false,
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
            medium_cache,
            shadow_hierarchy,
            last_live_camera: None,
            profiler,
            light_index,
            shadow_stats: ShadowStats::default(),
            interval_cache: crate::interval_cache::IntervalCacheState::new(0),
            interval_pools: None,
            compaction,
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
        fog_tile: u32,
        samples: u32,
    ) -> &Targets {
        let stale = self.targets.as_ref().is_none_or(|t| {
            t.width != width
                || t.height != height
                || !t.destination.same_targets(destination)
                || (t.haze_width, t.haze_height) != haze
                || t.samples != samples
        });
        let channels = destination.staged_channels();
        if stale {
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
                if matches!(destination, Destination::Compositor(_)) {
                    if let Some(shared) =
                        crate::share::Shared::new(&self.gpu, width, height, destination.channels())
                    {
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
                msaa_color: (samples > 1).then(|| {
                    color(
                        width,
                        height,
                        samples,
                        wgpu::TextureUsages::RENDER_ATTACHMENT,
                        "scene-msaa",
                    )
                }),
                samples,
                msaa_surface_depth: None,
                msaa_depth: depth_texture(&self.gpu.device, width, height, samples, "depth-msaa"),
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
                fog: crate::fog_grid::Targets::new(&self.gpu.device, None, fog_tile),
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
                            sample_count: samples,
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
                .ensure(&self.gpu.device, [width, height], fog_tile);
        }
        self.targets.as_ref().expect("just populated")
    }

    /// Render one frame and read it back as sRGB-encoded RGBA8, row-major, no
    /// padding.
    ///
    /// The haze is deterministic, so `subframes` matters only to
    /// [`Self::render_reference`]. The image is a function of this `frame`
    /// alone; sampling a sequence of *consecutive* moments is
    /// [`Renderer::render_next`].
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

    /// Exhaustive per-ray reference capture: the MIS estimator over every
    /// light, with no shared volume, cache or quad sharing, accumulating
    /// `subframes` jittered passes. The caller controls pixel resolution and
    /// sample count through `frame` and `subframes`. This converges the
    /// renderer's transport model, not an independent tracer.
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
        self.reference = true;
        let result = self.render(frame, width, height, subframes);
        self.reference = false;
        result
    }

    /// Drop every piece of haze state carried between frames, as a new
    /// renderer would start; the reference for transition tests.
    #[cfg(test)]
    fn forget_haze_history(&mut self) {
        self.interval_cache = crate::interval_cache::IntervalCacheState::new(0);
        self.interval_pools = None;
        self.interval_cache_entries.clear();
        self.compaction.forget();
    }

    /// Render the *next* frame of a sequence and read it back as
    /// sRGB-encoded RGBA8, row-major, no padding.
    ///
    /// Lighting is evaluated once at the current scene time; a sequence
    /// differs from [`Self::render`] only in the low-quality budget it takes
    /// while the camera moves.
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

    /// Measure one frame through the production live pass chain.
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
    /// capture reallocates targets.
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
            Destination::Compositor(DisplayRange::Sdr),
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

    /// The exposure the last post-chain frame used and the one it was
    /// adapting toward, in stops (`post.rs`). Blocks until the GPU is idle.
    ///
    /// # Errors
    /// Fails if the readback cannot be mapped.
    pub fn metered_exposure(&self) -> anyhow::Result<[f32; 2]> {
        self.post.read_exposure(&self.gpu.device, &self.gpu.queue)
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
        self.geometry_shadows = enabled;
    }

    /// Faces of the reflection probes still to capture since the stage last
    /// changed: they are captured a probe a frame. Zero when every probe
    /// holds the current stage, or when they are off.
    #[doc(hidden)]
    #[must_use]
    pub fn reflection_probes_pending(&self) -> u32 {
        self.probes.pending()
    }

    /// One reflection probe's six faces, side by side, as linear RGBA: `what`
    /// 0 its relit change to the sky probe at `mip` above its base, 1 its
    /// captured albedo, 2 its captured normal, metal and distance. For
    /// diagnosis.
    ///
    /// # Errors
    /// Fails when no probes are placed or the readback cannot be mapped.
    #[doc(hidden)]
    pub fn read_reflection_probe(
        &self,
        probe: u32,
        what: u32,
        mip: u32,
    ) -> anyhow::Result<(u32, u32, Vec<[f32; 4]>)> {
        self.probes
            .read_back(&self.gpu.device, &self.gpu.queue, probe, what, mip)
    }

    /// Where the reflection probes stand, world metres.
    #[doc(hidden)]
    #[must_use]
    pub fn reflection_probe_positions(&self) -> Vec<Vec3> {
        self.probes.positions()
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
        self.compaction.stats()
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
        let mut pending = self.submit_live_bytes(frame, width, height, subframes, channels, 0);
        let completed = pending.complete_blocking(&self.gpu.device)?;
        *out = completed
            .image
            .into_pixels()
            .expect("a Bytes destination reads its pixels back");
        Ok(())
    }

    /// [`Self::render_live_into`] without the wait: the frame is drawn into
    /// presentation `slot` and read back through the returned handle, so
    /// frames in other slots can be drawn while this one is copied out.
    pub(crate) fn submit_live_bytes(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        channels: Channels,
        slot: usize,
    ) -> PendingFrame {
        self.submit_readback(
            frame,
            width,
            height,
            subframes,
            Destination::Bytes(channels),
            slot,
            true,
            false,
        )
    }

    pub(crate) fn submit_live(
        &mut self,
        frame: &Frame,
        width: u32,
        height: u32,
        subframes: u32,
        range: DisplayRange,
        slot: usize,
        measure: bool,
        queued: Duration,
    ) -> PendingFrame {
        let mut pending = self.submit_readback(
            frame,
            width,
            height,
            subframes,
            Destination::Compositor(range),
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
        self.fog_blocks_valid = false;
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

        let aspect = width as f32 / height as f32;
        let CameraMatrices {
            far: camera_far,
            view,
            proj,
        } = camera_matrices(frame, aspect);
        let view_proj = proj * view;

        let camera_forward = (frame.camera.target - frame.camera.eye).normalize_or(Vec3::Y);
        let opaque = frame.draws.len() - frame.transparent.len();
        let light_view_proj = frame
            .directional
            .map_or([Mat4::IDENTITY; CASCADE_COUNT], |light| {
                let bounds = &self
                    .geometry
                    .as_ref()
                    .expect("frame geometry uploaded")
                    .bounds;
                let casters: Vec<_> = sun_casters(frame, bounds)
                    .map(|(_, sphere)| sphere)
                    .collect();
                cascade_matrices(
                    frame.camera.eye,
                    camera_forward,
                    frame.camera.fov_y_deg.to_radians(),
                    aspect,
                    light.direction,
                    &casters,
                )
            });

        // A cone that emits nothing visible (a black colour at any dimmer)
        // gets no shadow map, no light-index entry and no beam work.
        let fixture_cones: Vec<_> = frame
            .fixture_cones
            .iter()
            .map(sanitize_fixture_cone)
            .filter(|cone| cone.intensity * cone.color.max_element() > DARK_CONE)
            .take(crate::frame::MAX_FIXTURE_CONES)
            .collect();
        let camera_bits = [
            frame.camera.eye.x,
            frame.camera.eye.y,
            frame.camera.eye.z,
            frame.camera.target.x,
            frame.camera.target.y,
            frame.camera.target.z,
            frame.camera.fov_y_deg,
        ]
        .map(f32::to_bits);
        let camera_moving = temporal
            && self
                .last_live_camera
                .is_some_and(|last| last != camera_bits);
        self.last_live_camera = temporal.then_some(camera_bits);
        let budget = QualityBudget::of(frame.quality, camera_moving);
        let fog_tile = budget.fog_tile;
        let caster_hash = fixture_shadow_caster_hash(frame, opaque);
        // Everything `draw_depth` rasterises, fixture bodies included: the
        // camera depth and the sun cascades draw the same opaque list.
        let depth_hash = opaque_depth_hash(frame, opaque);
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
            self.fixture_shadow_drawn = vec![None; shadow_capacity];
            self.fixture_shadow_age = vec![0; shadow_capacity];
        }
        // Full mode retains every projection; the legacy comparison keeps
        // its priority-based 16-map selection.
        let shadow_slots = if frame.fixture_shadows {
            if cached_shadows {
                let origins: Vec<_> = self
                    .fixture_shadow_drawn
                    .iter()
                    .map(|drawn| drawn.map(|(origin, _)| origin))
                    .collect();
                crate::shadow::assign_cached_slots(
                    &fixture_cones,
                    &self.fixture_shadow_cache,
                    &origins,
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
                    lens_distance: light.lens_distance(),
                    lens_reserved: [0.0; 3],
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
        // Bound the maps redrawn per frame. A moved head over the budget keeps
        // sampling the map it has, with the projection that map was drawn
        // with, until its turn comes.
        let mut fixture_shadow_matrices = fixture_shadow_matrices;
        if cached_shadows && frame.fixture_shadows {
            let dirty: Vec<bool> = fixture_shadow_matrices
                .iter()
                .zip(&shadow_slots)
                .zip(&self.fixture_shadow_cache)
                .map(|((matrix, resident), cached)| {
                    resident.is_some()
                        && *cached
                            != Some(ShadowCacheKey {
                                matrix_bits: shadow_matrix_bits(&matrix.view_proj),
                                caster_hash,
                            })
                })
                .collect();
            let moved: Vec<bool> = shadow_slots
                .iter()
                .zip(&self.fixture_shadow_drawn)
                .zip(&self.fixture_shadow_cache)
                .map(|((resident, drawn), cached)| {
                    // Same head and the same casters: only the projection moved.
                    resident
                        .zip(*drawn)
                        .is_some_and(|(index, (origin, _))| origin == fixture_cones[index].position)
                        && cached.is_some_and(|key| key.caster_hash == caster_hash)
                })
                .collect();
            let priority: Vec<f32> = shadow_slots
                .iter()
                .map(|resident| {
                    resident.map_or(0.0, |index| {
                        crate::shadow::shadow_priority(&fixture_cones[index], frame.camera.eye)
                    })
                })
                .collect();
            let redraw = crate::shadow::budget_redraws(
                &dirty,
                &moved,
                &priority,
                &self.fixture_shadow_age,
                budget.shadow_redraws,
            );
            for slot in 0..dirty.len() {
                if dirty[slot] && !redraw[slot] {
                    let (_, drawn) =
                        self.fixture_shadow_drawn[slot].expect("a moved map was drawn");
                    fixture_shadow_matrices[slot] = drawn;
                    self.fixture_shadow_age[slot] += 1;
                } else {
                    self.fixture_shadow_age[slot] = 0;
                }
            }
        }
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
        // The compute kernels draw the haze, except for the exhaustive
        // reference's per-ray fragment pass.
        let per_ray = self.reference;
        // The far-field grid chain runs under the compute kernels whenever
        // there is haze to light.
        let grid_fog = !per_ray && haze_density >= 0.001;
        // The far-field grid's radial extent: the haze uniform's `shadow.w`,
        // and the surface pass's `surface_fog.x` when it reads that grid.
        let fog_far = fixture_cones
            .iter()
            .map(|c| frame.camera.eye.distance(c.position) + c.range)
            .fold(1.0_f32, f32::max)
            .min(camera_far);
        // The surface takes outdoor camera transmittance from the far-field
        // fog grid on frames that run the fog chain, so the scene pass is
        // encoded after `fog-transmittance` then; otherwise it marches.
        let scene_after_fog = grid_fog;
        let mut globals = Globals {
            surface_fog: [fog_far, f32::from(u8::from(grid_fog)), 0.0, 0.0],
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
                0.0,
                1.0 / SHADOW_SIZE as f32,
                f32::from(u8::from(
                    frame.directional.is_some_and(|light| light.shadows),
                )),
                frame.debug_view.shader_code() as f32,
            ],
            floor: frame.floor.map_or([[0.0; 4]; 4], |floor| floor.uniform()),
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
        let (probe_cubes, probe_grid, probe_ambient) = self.probes.bindings();
        let local_probes = crate::environment::LocalProbes {
            cubes: probe_cubes,
            dummy: self.gpu.probes.dummy_cubes(),
            grid: probe_grid,
            ambient: probe_ambient,
        };
        let (_environment_uniform, environment_groups) = self.environment.bind_group(
            &self.gpu.environment,
            &self.gpu.device,
            frame.environment.as_ref(),
            &ambient_buf,
            &local_probes,
        );
        let mut environment_bg = environment_groups.scene;
        let mut probe_environment_bg = environment_groups.relight;
        // The sky, when there is one, is both the background the composite
        // resolves and the probe the scene pass is lit by. Its tables are
        // rebuilt only when the sun moves, so this is a cache hit for every
        // frame of a fixed hour.
        // The clouds are traced here too, from the unjittered camera: they
        // are the background, and the composite reads them.
        let (sky_bg, sky_probe) = self.atmosphere.prepare(
            &self.gpu.atmosphere,
            &self.gpu.environment,
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            frame.sky.as_ref(),
            &crate::atmosphere::CloudFrame {
                view_proj,
                eye: frame.camera.eye,
                width,
                height,
                time: frame.time,
                temporal,
                quality: frame.quality,
            },
            &mut pass_queries,
        );
        let aerial = self.atmosphere.prepare_aerial(
            &self.gpu.atmosphere,
            &self.gpu.device,
            &mut encoder,
            frame.sky.as_ref(),
            frame.camera.eye,
            &mut pass_queries,
        );
        if let Some(probe) = &sky_probe {
            let groups = self.gpu.environment.sky_bind_group(
                &self.gpu.device,
                probe,
                &ambient_buf,
                &local_probes,
            );
            environment_bg = groups.scene;
            probe_environment_bg = groups.relight;
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

        let geometry = self.geometry.as_ref().expect("frame geometry uploaded");
        let vertex_buf = geometry.vertices.clone();
        let index_buf = geometry.indices.clone();
        let ranges = geometry.ranges.clone();
        let mesh_bounds = geometry.bounds.clone();

        // Outdoors, the stage's height field: redrawn only when its geometry
        // changes. Its presence switches the scene pass's sky term to the
        // occluded split (`globals.room_falloff.y`).
        let sky_field_redraw = self.sky_visibility.update_field(
            &self.gpu.device,
            frame,
            opaque,
            caster_hash,
            &mesh_bounds,
            frame.sky.is_some(),
        );
        globals.room_falloff[1] = f32::from(u8::from(self.sky_visibility.active()));
        let sun_direction = frame
            .directional
            .map(|light| light.direction.normalize_or_zero());
        let sky_height_buf = self.storage(
            &mut encoder,
            &[self.sky_visibility.height_params(sun_direction)],
            wgpu::BufferUsages::UNIFORM,
            "sky-height-params",
        );
        let ambient_params_buf = self.storage(
            &mut encoder,
            &[crate::sky_visibility::ao_params(
                frame.camera.eye,
                camera_forward,
                frame.camera.fov_y_deg.to_radians(),
                [width, height],
                [CAMERA_NEAR, camera_far],
                self.sky_visibility.active(),
            )],
            wgpu::BufferUsages::UNIFORM,
            "ambient-visibility-params",
        );

        let instances: Vec<Instance> = frame
            .draws
            .iter()
            .map(|draw| instance_of(frame, draw))
            .collect();
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
                if key.floor {
                    &self.gpu.floor_sampler
                } else {
                    &self.gpu.texture_sampler
                },
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
        let lit_bg =
            self.scene_bind_group(&globals_buf, &instance_buf, true, hard_shadows, &aerial);
        let unlit_bg = self.scene_bind_group(&globals_buf, &instance_buf, false, false, &aerial);
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
                    self.scene_bind_group(&buffer, &instance_buf, false, false, &aerial),
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
            fog_grid,
            fog_integral,
            fog_transmittance,
            fog_tau,
            fog_columns,
            fog_candidates,
            output_view,
            channels,
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
                fog_tile,
                budget.msaa,
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
                t.fog.incident.clone(),
                t.fog.integral.clone(),
                t.fog.transmittance.clone(),
                t.fog.tau.clone(),
                t.fog.columns.clone(),
                t.fog.candidates.clone(),
                presentation.view().clone(),
                presentation.channels(),
                finish,
                t.bytes_per_row,
            )
        };
        let targets_done = Instant::now();
        let ambient_visibility = self.sky_visibility.output(&self.gpu.device, width, height);
        // Outdoor haze with a sun: the sun-shaft target, which the scene pass
        // reads per fragment through binding 16. A texel of full sun stands
        // in when the pass does not run.
        let shafts = (frame.sky.is_some() && medium.min[3] > 0.0).then(|| {
            self.sun_shafts
                .target(&self.gpu.device, t_width, t_height, frame.quality)
        });
        let shaft_view = shafts
            .clone()
            .unwrap_or_else(|| self.gpu.sun_shafts.off.clone());
        // Built after the targets: binding 13 is this frame's fog prefix.
        // The probe relight's copy binds every cone in source order in place
        // of the light index's in-view subset (`probes.rs`).
        self.probes
            .set_lights(&self.gpu.device, &self.gpu.queue, &cores, &rests);
        let probe_work = self.probes.prepare(&self.gpu.queue, frame, opaque);
        let probe_debug_balls = self.probes.debug_balls();
        let cluster_group = |label: &str, core: &wgpu::Buffer, rest: &wgpu::Buffer| {
            self.gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &self.gpu.cluster_layout,
                    entries: &[
                        binding(0, core.as_entire_binding()),
                        binding(1, rest.as_entire_binding()),
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
                        binding(15, wgpu::BindingResource::TextureView(&ambient_visibility)),
                        binding(16, wgpu::BindingResource::TextureView(&shaft_view)),
                    ],
                })
        };
        let cluster_bg = cluster_group(
            "surface-clusters",
            &index_bindings.core,
            &index_bindings.rest,
        );
        let probe_cluster_bg = self
            .probes
            .cone_buffers()
            .map(|(core, rest)| cluster_group("probe-clusters", core, rest));
        // This block runs to the end of the function: `encode_scene` below
        // captures its draw closures and is called either here or, when the
        // surface pass reads the fog grid, after `fog-integrate`.
        {
            let mut pending_start = profile_resources.as_ref().map(|(queries, ..)| queries);
            let all_opaque: Vec<usize> = (0..opaque).collect();
            // Under a floor material the ground has its own fragment shader.
            let (floor_draws, lit_draws): (Vec<usize>, Vec<usize>) =
                all_opaque.iter().partition(|&&index| {
                    frame.floor.is_some()
                        && crate::frame::is_ground(&frame.meshes[frame.draws[index].mesh].key)
                });
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
                    .map(|draw| world_sphere(draw, mesh_bounds[draw.mesh]))
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
                                cone.apex(),
                                direction,
                                cone.apex_reach(),
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
            //
            // The sun cascades leave the ground out (`with_ground` false): it
            // only receives, and its own depth in the map was what a grazing
            // sun needed a large receiver offset against.
            let draw_depth = |pass: &mut wgpu::RenderPass, with_ground: bool| {
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
                    if with_ground || !crate::frame::is_ground(&frame.meshes[mesh].key) {
                        let (first, last, base) = ranges[mesh];
                        pass.draw_indexed(first..last, base, start as u32..end as u32);
                    }
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
                        self.fixture_shadow_drawn[index] = shadow_slots[index].map(|cone| {
                            (fixture_cones[cone].position, fixture_shadow_matrices[index])
                        });
                    }
                }
            }

            // A cascade's depth map only changes when its projection or its
            // casters do, exactly as for a fixture map. Redrawing all three
            // every frame was the sun path missing the check the fixture path
            // already had.
            let cascade_keys = cascade_cache_keys(&light_view_proj, depth_hash);
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
                    draw_depth(&mut pass, false);
                }
            }

            if let Some(redraw) = &sky_field_redraw {
                for (index, (matrix, layer)) in
                    redraw.matrices.iter().zip(&redraw.layers).enumerate()
                {
                    let mut field_globals = globals;
                    field_globals.light_view_proj[0] = matrix.to_cols_array_2d();
                    let buffer = self.storage(
                        &mut encoder,
                        &[field_globals],
                        wgpu::BufferUsages::UNIFORM,
                        &format!("sky-height-globals-{index}"),
                    );
                    let bind_group =
                        self.scene_bind_group(&buffer, &instance_buf, false, false, &aerial);
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("sky-height"),
                        color_attachments: &[],
                        depth_stencil_attachment: Some(depth_attachment(layer)),
                        timestamp_writes: pass_queries
                            .render("sky-height", claim_start_timestamp(&mut pending_start)),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.shadow_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &self.gpu.white_material, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    pass.set_vertex_buffer(0, vertex_buf.slice(..));
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(start, end) in &redraw.runs {
                        let (first, last, base) = ranges[frame.draws[start].mesh];
                        pass.draw_indexed(first..last, base, start as u32..end as u32);
                    }
                }
            }
            self.sky_visibility.encode_field(
                &self.gpu.sky_visibility,
                &self.gpu.device,
                &mut encoder,
                &sky_height_buf,
                sun_direction,
                &mut pass_queries,
            );

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
                draw_depth(&mut pass, true);
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
                    pass.set_pipeline(&self.gpu.scene_pipelines(budget.msaa).surface_depth);
                    pass.set_bind_group(0, &lit_bg, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_depth(&mut pass, true);
                }
                self.light_index.refine_surface(
                    &self.gpu.light_index_pipelines,
                    &self.gpu.device,
                    &mut encoder,
                    msaa_surface_depth,
                    budget.msaa > 1,
                    pass_queries.compute("surface-light-index", None),
                );
            }

            // Ambient visibility reads the depth prepass; the scene pass
            // reads it back per pixel through the cluster group.
            self.sky_visibility.encode_screen(
                &self.gpu.sky_visibility,
                &self.gpu.device,
                &mut encoder,
                &sky_height_buf,
                &ambient_params_buf,
                &depth_view,
                &mut pass_queries,
            );

            // --- sun shafts ------------------------------------------------------
            // How much of each ray's haze the sun reaches past the stage and
            // the clouds. Reads the depth prepass, the shadow cascades and
            // the cloud shadow map, all drawn above; the scene pass reads it.
            let inv_view_proj = view_proj.inverse();
            if shafts.is_some() {
                self.sun_shafts.encode(
                    &self.gpu.sun_shafts,
                    &self.gpu.device,
                    &mut encoder,
                    &crate::sun_shafts::FrameInput {
                        globals: &globals_buf,
                        shadow_map: &self.shadow_map,
                        shadow_sampler: &self.gpu.hard_shadow_sampler,
                        haze_field: (&self.gpu.haze_field.view, &self.gpu.haze_field.sampler),
                        aerial: &aerial,
                        depth: &depth_view,
                        inv_view_proj,
                        quality: frame.quality,
                    },
                    &mut pass_queries,
                );
            }

            // The local reflection probes (`probes.rs`): this frame's share
            // of any capture, then every probe relit and prefiltered, before
            // the scene pass samples them.
            if self.probes.live() {
                let capture = probe_work.capture.clone();
                if !capture.is_empty() {
                    let spheres: Vec<(Vec3, f32)> = frame.draws[..opaque]
                        .iter()
                        .map(|draw| world_sphere(draw, mesh_bounds[draw.mesh]))
                        .collect();
                    self.probes.encode_capture(
                        &self.gpu.probes,
                        &self.gpu.device,
                        &self.gpu.queue,
                        &mut encoder,
                        &instance_buf,
                        capture,
                        pass_queries.render("probe-capture", None),
                        |pass, at, face, texels| {
                            pass.set_vertex_buffer(0, vertex_buf.slice(..));
                            pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                            for (i, (centre, radius)) in spheres.iter().enumerate() {
                                if !crate::probes::face_sees(face, *centre - at, *radius, texels) {
                                    continue;
                                }
                                pass.set_bind_group(1, &materials[i], &[]);
                                let (first, last, base) = ranges[frame.draws[i].mesh];
                                pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                            }
                        },
                    );
                }
                if let Some(probe_clusters) = &probe_cluster_bg {
                    self.probes.encode_relight(
                        &self.gpu.probes,
                        &self.gpu.device,
                        &self.gpu.queue,
                        &mut encoder,
                        &lit_bg,
                        &probe_environment_bg,
                        probe_clusters,
                        pass_queries.compute("probe-relight", None),
                        pass_queries.compute("probe-filter", None),
                    );
                }
            }

            // The scene pass reads nothing the haze chain writes unless the
            // surface transmittance comes from the fog grid; then it is
            // encoded after `fog-transmittance` instead of here, still ahead
            // of the haze pass, so the legacy timestamp slots keep their order.
            let samples = budget.msaa;
            let encode_scene = scene_encoder(|encoder, pass_queries, gpu, queries| {
                let repeat_count = gpu.profile_copies("scene");
                for repeat_index in 0..repeat_count {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("scene"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: msaa_color.as_ref().unwrap_or(&scene_view),
                            resolve_target: msaa_color.as_ref().map(|_| &scene_view),
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
                    let pipelines = gpu.scene_pipelines(samples);
                    pass.set_bind_group(0, &lit_bg, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    if !floor_draws.is_empty() {
                        pass.set_pipeline(&pipelines.ground);
                        draw_range(&mut pass, &floor_draws);
                    }
                    pass.set_pipeline(&pipelines.scene);
                    draw_range(&mut pass, &lit_draws);
                    if let Some(balls) = probe_debug_balls {
                        pass.set_pipeline(&pipelines.probe_debug);
                        pass.set_bind_group(1, &gpu.white_material, &[]);
                        pass.draw(0..crate::probes::DEBUG_BALL_VERTICES, 0..balls);
                    }
                    for (slot, kind) in frame.transparent.iter().enumerate() {
                        pass.set_pipeline(match kind {
                            crate::frame::Transparent::Grid => &pipelines.grid,
                            crate::frame::Transparent::Compass => &pipelines.compass,
                            crate::frame::Transparent::Cables => &pipelines.cables,
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
            let subframes = subframes.max(1);
            // The compute kernels write each pixel once; the fragment pass
            // accumulates its subframes.
            let weight = if per_ray { 1.0 / subframes as f32 } else { 1.0 };
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
            let grid_write = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fog-grid-write"),
                    layout: &self.gpu.fog_grid_write_layout,
                    entries: &[
                        binding(0, wgpu::BindingResource::TextureView(&fog_grid)),
                        binding(1, wgpu::BindingResource::TextureView(&fog_columns)),
                        binding(2, fog_candidates.as_entire_binding()),
                    ],
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
            // Compute-lane order: on a grid-fed surface frame the medium cache
            // and the compaction classify are encoded after the scene pass.
            // Nothing ahead of the scene reads either — `fog-prepare` and
            // `fog-transmittance` bind the cache slot but never sample it, and
            // `fog-grid`, the residual and the hot kernel all follow the scene —
            // while ahead of `fog-prepare` they only delay the transmittance
            // prefix the scene's fragment stage waits on.
            let medium_cache_late = scene_after_fog && grid_fog;
            if !medium_cache_late {
                encode_medium_cache(&mut encoder, &mut pass_queries, &self.gpu);
            }

            let haze_passes = if per_ray { subframes } else { 1 };
            let (cache_uniform, cache_header, cache_claims, cache_table) = self.plan_interval_cache(
                &mut encoder,
                frame,
                grid_fog
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
            );
            // Residual compaction needs a settled cache (some slot reading or
            // writing; a moving camera traverses every pair, which the fused
            // kernel does as fast), and list entries carry a 22-bit pixel
            // index.
            let cache = self.interval_cache.stats();
            let compact_plan = if grid_fog {
                // Shadow slot → this frame's light-index id, for the residual list.
                let source_to_sorted = self.light_index.source_to_sorted();
                let slot_lights: Vec<u32> = shadow_slots
                    .iter()
                    .map(|resident| {
                        resident
                            .and_then(|index| source_to_sorted.get(index).copied().flatten())
                            .unwrap_or(u32::MAX)
                    })
                    .collect();
                // A slot's classification depends on its interval-cache
                // generation (header and payload state) and on nothing else
                // that can change while that holds.
                let slot_words: Vec<u32> = self
                    .interval_cache_entries
                    .iter()
                    .map(|entry| {
                        if entry[3] & 0xFF == crate::interval_cache::MODE_OFF {
                            u32::MAX
                        } else {
                            entry[3] >> 8
                        }
                    })
                    .collect();
                self.compaction.plan(
                    &self.gpu.device,
                    &mut encoder,
                    cache.read + cache.write > 0
                        && u64::from(haze_size.0) * u64::from(haze_size.1) <= 1 << 22,
                    haze_size,
                    &slot_words,
                    &slot_lights,
                )
            } else {
                self.compaction.idle();
                None
            };
            let compact_pipes = &self.gpu.haze_compact_pipelines;
            let compact_uniform = compact_plan.as_ref().map(|plan| {
                self.storage(
                    &mut encoder,
                    &[plan.uniform],
                    wgpu::BufferUsages::UNIFORM,
                    "haze-compact-params",
                )
            });
            let compact_bind_group = |indirect: bool, hot: bool| -> wgpu::BindGroup {
                self.compaction.bind_group(
                    &self.gpu.device,
                    &compact_pipes.layout,
                    &crate::haze_compact::FrameBindings {
                        uniform: compact_uniform.as_ref().expect("compact frame"),
                        grid: &fog_integral,
                        static_field: &self.gpu.haze_field.view,
                        output: &haze_view,
                        cache: [&cache_uniform, &cache_header, &cache_claims, &cache_table],
                    },
                    indirect,
                    hot,
                )
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
                        (k & 4095) as f32,
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
                        0.0,
                        0.0,
                    ],
                    depth: [
                        CAMERA_NEAR,
                        camera_far,
                        haze_density * Transport::EXTINCTION,
                        0.0,
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
                    quality: budget.haze(),
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
                let haze_bind_group = |buffer: &wgpu::Buffer| {
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze"),
                            layout: &self.gpu.haze_layout,
                            entries: &[
                                binding(0, buffer.as_entire_binding()),
                                binding(
                                    10,
                                    wgpu::BindingResource::TextureView(&self.medium_cache.view),
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
                        })
                };
                let bind_group = haze_bind_group(&haze_buf);
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
                    // Classify needs shadow maps, depth and the light index. It
                    // samples neither the medium cache nor the grid, so it
                    // overlaps the medium-cache pass and the scene, ahead of
                    // everything the residual needs.
                    let encode_classify = |encoder: &mut wgpu::CommandEncoder,
                                           pass_queries: &mut crate::pass_profile::PassQueries<
                        '_,
                    >| {
                        if compact_plan.as_ref().is_some_and(|plan| plan.classify) {
                            self.compaction.encode_classify(
                                encoder,
                                pass_queries,
                                compact_pipes,
                                &crate::haze_compact::Groups {
                                    haze: &bind_group_early,
                                    light_index: &light_index_bg,
                                    grid: &grid_read_transmittance,
                                },
                                &compact_bind_group(true, false),
                            );
                        }
                    };
                    // Encoded after the scene on a grid-fed surface frame: the
                    // classify writes only the interval cache and the residual
                    // planes/list, whose first readers are the residual and hot
                    // kernels behind `fog-integrate`.
                    if !scene_after_fog {
                        encode_classify(&mut encoder, &mut pass_queries);
                    }
                    // The prefix passes never sample the medium cache; the early
                    // group binds the static field in its slot so they do not wait
                    // for the medium-cache pass.
                    let prefix_group = &bind_group_early;
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
                            width.div_ceil(fog_tile).div_ceil(8),
                            height.div_ceil(fog_tile).div_ceil(8),
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
                            width.div_ceil(fog_tile),
                            height.div_ceil(fog_tile),
                            1,
                        );
                        drop(pass);
                        encode_scene(
                            &mut encoder,
                            &mut pass_queries,
                            &self.gpu,
                            profile_resources.as_ref().map(|(queries, ..)| queries),
                        );
                        encode_classify(&mut encoder, &mut pass_queries);
                    }
                    if medium_cache_late {
                        encode_medium_cache(&mut encoder, &mut pass_queries, &self.gpu);
                    }
                    self.fog_blocks_valid = true;
                    let repeat_count = self.gpu.profile_copies("fog-classify");
                    for _repeat_index in 0..repeat_count {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("fog-classify"),
                            timestamp_writes: pass_queries.compute("fog-classify", None),
                        });
                        pass.set_pipeline(&self.gpu.fog_classify_pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.set_bind_group(1, &light_index_bg, &[]);
                        pass.set_bind_group(2, &grid_classify, &[]);
                        pass.dispatch_workgroups(
                            width
                                .div_ceil(fog_tile)
                                .div_ceil(crate::fog_grid::BLOCK_SIDE),
                            height
                                .div_ceil(fog_tile)
                                .div_ceil(crate::fog_grid::BLOCK_SIDE),
                            crate::fog_grid::SLICES.div_ceil(crate::fog_grid::BLOCK_SIDE),
                        );
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
                                .div_ceil(fog_tile)
                                .div_ceil(crate::fog_grid::BLOCK_SIDE),
                            height
                                .div_ceil(fog_tile)
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
                            width.div_ceil(fog_tile),
                            height.div_ceil(fog_tile),
                            1,
                        );
                    }
                    if let Some(plan) = compact_plan.as_ref() {
                        self.compaction.encode_residual(
                            &mut encoder,
                            &mut pass_queries,
                            compact_pipes,
                            &crate::haze_compact::Groups {
                                haze: &bind_group,
                                light_index: &light_index_bg,
                                grid: &grid_read,
                            },
                            [
                                &compact_bind_group(true, false),
                                &compact_bind_group(false, false),
                            ],
                            plan.classify,
                        );
                    }
                }
                // Deterministic transport emits each pixel exactly once, as
                // direct stores; the fragment pass below accumulates the MIS
                // estimator's subframes.
                if !per_ray {
                    let output_entries = [
                        binding(0, wgpu::BindingResource::TextureView(&fog_integral)),
                        binding(1, wgpu::BindingResource::TextureView(&haze_view)),
                        binding(4, cache_uniform.as_entire_binding()),
                        binding(5, cache_header.as_entire_binding()),
                        binding(6, cache_claims.as_entire_binding()),
                        binding(7, cache_table.as_entire_binding()),
                    ];
                    let output = self
                        .gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("haze-compute"),
                            layout: &self.gpu.haze_compute_layout,
                            entries: &output_entries,
                        });
                    let groups = crate::haze_compact::Groups {
                        haze: &bind_group,
                        light_index: &light_index_bg,
                        grid: &grid_read,
                    };
                    let repeat_count = self.gpu.profile_copies("haze-compute");
                    for repeat_index in 0..repeat_count {
                        let timestamp_writes = profile_resources
                            .as_ref()
                            .filter(|_| repeat_index + 1 == repeat_count)
                            .map(|(queries, ..)| wgpu::ComputePassTimestampWrites {
                                query_set: queries,
                                beginning_of_pass_write_index: None,
                                end_of_pass_write_index: Some(2),
                            });
                        if let Some(plan) = compact_plan.as_ref() {
                            self.compaction.encode_output(
                                &mut encoder,
                                &mut pass_queries,
                                compact_pipes,
                                &groups,
                                [
                                    &compact_bind_group(true, false),
                                    &compact_bind_group(false, true),
                                ],
                                (&self.gpu.haze_compute_pipeline, &output),
                                timestamp_writes,
                                plan.readback && repeat_index + 1 == repeat_count,
                            );
                            continue;
                        }
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("haze-compute"),
                            timestamp_writes: pass_queries.compute("haze-compute", timestamp_writes),
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
                // The reference's subframes accumulate additively.
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("haze"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &haze_view,
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
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: pass_queries.render(
                        "haze",
                        profile_resources.as_ref().and_then(|(queries, ..)| {
                            (k + 1 == haze_passes).then_some(wgpu::RenderPassTimestampWrites {
                                query_set: queries,
                                beginning_of_pass_write_index: None,
                                end_of_pass_write_index: Some(2),
                            })
                        }),
                    ),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.haze_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_bind_group(1, &light_index_bg, &[]);
                pass.set_bind_group(2, &grid_read, &[]);
                pass.set_bind_group(3, &grid_read, &[]);
                pass.draw(0..3, 0..1);
                drop(pass);
            }

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
                display: [
                    destination.headroom(),
                    f32::from(u8::from(shafts.is_some())),
                    0.0,
                    0.0,
                ],
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
                        binding(2, wgpu::BindingResource::TextureView(&haze_view)),
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
                        binding(7, wgpu::BindingResource::TextureView(&shaft_view)),
                    ],
                });
            {
                // The frame's last timestamp belongs to whichever pass writes
                // the output: the composite, or the post chain after it.
                let last_timestamp = profile_resources.as_ref().map(|(queries, ..)| {
                    wgpu::RenderPassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: Some(3),
                    }
                });
                // Diagnostic views are exact channel dumps; the camera stays
                // out of them.
                let post = (frame.look.needs_post()
                    && frame.debug_view == crate::scene_desc::DebugView::Pbr
                    && !frame.cluster_debug)
                    .then(|| self.post.scene_target(&self.gpu.device, t_width, t_height));
                {
                    let (target, pipeline, timestamps) = match &post {
                        Some(scene) => (
                            scene,
                            &self.gpu.composite_linear_pipeline,
                            pass_queries.render("composite", None),
                        ),
                        None => (
                            &output_view,
                            &self.gpu.composite_pipelines[channels.index()],
                            pass_queries.render("composite", last_timestamp.clone()),
                        ),
                    };
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("composite"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: timestamps,
                        ..Default::default()
                    });
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.set_bind_group(1, &environment_bg, &[]);
                    pass.set_bind_group(2, &sky_bg, &[]);
                    pass.draw(0..3, 0..1);
                }
                if post.is_some() {
                    self.post.encode(
                        &self.gpu.post,
                        &self.gpu.device,
                        &self.gpu.queue,
                        &mut encoder,
                        &crate::post::PostFrame {
                            look: frame.look,
                            quality: frame.quality,
                            cones: &frame.fixture_cones,
                            view_proj,
                            eye: frame.camera.eye,
                            fov_y_deg: frame.camera.fov_y_deg,
                            near: CAMERA_NEAR,
                            far: camera_far,
                            width: t_width,
                            height: t_height,
                            headroom: destination.headroom(),
                            output: channels.index(),
                            temporal,
                            time: frame.time,
                            // The lens's veil from a sun off the frame is
                            // the sun's light through the clouds, or under
                            // a deck the veil glows from a sun nobody sees.
                            sun: frame.sky.as_ref().map(|sky| {
                                let through = crate::atmosphere::clouds::sun_visibility(
                                    sky.clouds,
                                    frame.camera.eye,
                                    sky.sun_direction,
                                    frame.time,
                                );
                                (sky.sun_direction, sky.sun_radiance * through)
                            }),
                        },
                        &depth_view,
                        &output_view,
                        &mut pass_queries,
                        last_timestamp,
                    );
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
            if compact_plan.as_ref().is_some_and(|plan| plan.readback) {
                self.compaction.start_readback();
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

    /// Decide which shadow slots' cached traversal output is still exact this
    /// frame, upload the per-slot table and return the compute kernel's cache
    /// bindings. With `active` false (no lit compute haze, or no cached
    /// shadows) every slot is off.
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
    ) -> (wgpu::Buffer, wgpu::Buffer, wgpu::Buffer, wgpu::Buffer) {
        use crate::interval_cache::{FrameKey, SlotKey, block_rect};
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

    fn scene_bind_group(
        &self,
        globals: &wgpu::Buffer,
        instances: &wgpu::Buffer,
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
                    let linear =
                        |value: u8| crate::coords::srgb_to_linear(f32::from(value) / 255.0);
                    let mean = (linear(at(x0, y0))
                        + linear(at(x1, y0))
                        + linear(at(x0, y1))
                        + linear(at(x1, y1)))
                        * 0.25;
                    let encoded = crate::coords::linear_to_srgb(mean);
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
/// line topology, bit 1 is [`OverlayDepth::Free`], bits 2 and up are the
/// output's [`Channels::index`].
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
        Camera, DirectionalLight, Draw, FixtureCone, Frame, MaterialTextures, MeshData,
    };
    use crate::light_index::cone_reaches_sphere;
    use crate::overlay::{Overlay, OverlayDepth};
    use crate::scene_desc::DebugView;

    use super::{
        CAMERA_FAR, CAMERA_NEAR, CASCADE_COUNT, Channels, CompositeUniform, FixtureShadowMatrix,
        Globals, Gpu, HazeUniform, LightCore, LightRest, Renderer, SHADOW_SIZE,
        SurfaceClusterUniform, TextureEncoding, cascade_matrices, downsample,
        specialize_wgsl_overrides,
    };

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
        assert!(
            values
                .iter()
                .all(|v| v.iter().all(|x| (0.0..1.0).contains(x)))
        );
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
            assert!(
                targets.destination == super::Destination::Compositor(super::DisplayRange::Sdr)
            );
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
        use wgpu::naga;
        // The size naga lays `name` out at in `source`, the module the GPU
        // compiles, so a field added on one side only fails here.
        fn wgsl_size(source: &str, name: &str) -> usize {
            let module = naga::front::wgsl::parse_str(source).expect("WGSL parses");
            let (_, ty) = module
                .types
                .iter()
                .find(|(_, ty)| ty.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("no WGSL struct {name}"));
            ty.inner.size(module.to_ctx()) as usize
        }
        let scene = super::scene_wgsl();
        // The head of the haze module, in the order `Gpu::new` builds it.
        let transport = format!(
            "{}{}{}{}{}",
            crate::haze_field::prelude(),
            include_str!("shaders/fixture_light.wgsl"),
            include_str!("shaders/light_index.wgsl"),
            include_str!("shaders/visibility.wgsl")
                .replace("@group(3) @binding(11)", "@group(0) @binding(8)"),
            super::beam_transport_wgsl()
        );
        let composite = super::composite_wgsl();
        let compact = format!(
            "{transport}{}{}{}",
            include_str!("shaders/haze.wgsl"),
            include_str!("shaders/haze_cache.wgsl"),
            include_str!("shaders/haze_compact.wgsl")
        );
        for (cpu, source, name) in [
            (std::mem::size_of::<Globals>(), &scene, "Globals"),
            (std::mem::size_of::<LightCore>(), &scene, "FixtureLightCore"),
            (std::mem::size_of::<LightRest>(), &scene, "FixtureLightRest"),
            (
                std::mem::size_of::<FixtureShadowMatrix>(),
                &scene,
                "FixtureShadowMatrix",
            ),
            (
                std::mem::size_of::<SurfaceClusterUniform>(),
                &scene,
                "SurfaceClusterParams",
            ),
            (std::mem::size_of::<LightCore>(), &transport, "LightCore"),
            (std::mem::size_of::<LightRest>(), &transport, "LightRest"),
            (std::mem::size_of::<HazeUniform>(), &transport, "Haze"),
            (
                std::mem::size_of::<CompositeUniform>(),
                &composite,
                "Composite",
            ),
            (
                std::mem::size_of::<crate::haze_compact::Uniform>(),
                &compact,
                "CompactParams",
            ),
        ] {
            assert_eq!(cpu, wgsl_size(source, name), "{name}");
        }
    }

    #[test]
    fn reverse_z_projection_is_monotonic_and_maps_bounded_planes() {
        let projection =
            Mat4::perspective_rh(48f32.to_radians(), 16.0 / 9.0, CAMERA_FAR, CAMERA_NEAR);
        let depth = |distance: f32| projection.project_point3(Vec3::new(0.0, 0.0, -distance)).z;
        assert!((depth(CAMERA_NEAR) - 1.0).abs() < 1e-5);
        assert!(depth(CAMERA_FAR).abs() < 1e-6);
        let samples = [CAMERA_NEAR, 1.0, 10.0, 100.0, CAMERA_FAR];
        assert!(
            samples
                .windows(2)
                .all(|pair| depth(pair[0]) > depth(pair[1]))
        );
    }

    #[test]
    fn cascades_are_finite_bounded_and_texel_stable_under_small_translation() {
        let eye = Vec3::new(4.5, -5.0, 3.0);
        let forward = (Vec3::new(0.0, 0.8, 0.0) - eye).normalize();
        let to_light = Vec3::new(2.0, -3.0, 6.0).normalize();
        let matrices =
            cascade_matrices(eye, forward, 48f32.to_radians(), 16.0 / 9.0, to_light, &[]);
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
            &[],
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
    fn post_agx_overlay_keeps_authored_srgb_across_scene_lighting_and_output_formats()
    -> anyhow::Result<()> {
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
    fn a_ten_bit_frame_is_the_eight_bit_picture_with_finer_steps() -> anyhow::Result<()> {
        const AUTHORED: [u8; 3] = [0x33, 0x99, 0xe6];
        // Red in the low ten bits, then green, blue and two bits of alpha.
        let unpack = |pixel: &[u8]| {
            let word = u32::from_le_bytes(pixel.try_into().unwrap());
            [
                word & 1023,
                (word >> 10) & 1023,
                (word >> 20) & 1023,
                word >> 30,
            ]
        };
        let mut renderer = Renderer::new()?;
        // The composite pass writes the neutral look; the post chain the stage one.
        for look in [
            crate::scene_desc::Look::NEUTRAL,
            crate::scene_desc::Look::STAGE,
        ] {
            let mut frame = overlay_test_frame(true, OverlayDepth::Tested, -1.0, Vec3::splat(0.02));
            frame.look = look;
            let mut rgba = Vec::new();
            renderer.render_into(&frame, 96, 96, 1, Channels::Rgba, &mut rgba)?;
            let mut rgb10 = Vec::new();
            renderer.render_into(&frame, 96, 96, 1, Channels::Rgb10, &mut rgb10)?;
            assert_eq!(rgb10.len(), rgba.len());

            let mut off_grid = 0;
            let mut worst = 0.0_f32;
            for (ten, eight) in rgb10.chunks_exact(4).zip(rgba.chunks_exact(4)) {
                let [r, g, b, a] = unpack(ten);
                assert_eq!(a, 3, "opaque");
                for (code, byte) in [r, g, b].into_iter().zip(eight) {
                    let as_eight = code as f32 * 255.0 / 1023.0;
                    worst = worst.max((as_eight - f32::from(*byte)).abs());
                    off_grid += usize::from((as_eight - as_eight.round()).abs() > 0.1);
                }
            }
            let offset = center_offset(96, 96);
            let [r, g, b, _] = unpack(&rgb10[offset..offset + 4]);
            let center = [r, g, b].map(|code| (code as f32 * 255.0 / 1023.0).round() as u8);
            assert_eq!(center, AUTHORED, "the overlay's colour, sRGB-encoded");
            // Each frame within its own dither of the other: one 8-bit code
            // for the 8-bit frame, a quarter of one for the 10-bit frame.
            assert!(worst <= 2.0, "the 10-bit frame strays {worst} 8-bit codes");
            assert!(off_grid > 0, "every 10-bit value sits on the 8-bit grid");
        }
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
    fn shared_washes_preserve_colour_energy_and_reset_on_cues() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        // One much brighter emitter and mixed colours challenge the shared
        // grid and quad sharing against the per-ray reference.
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
        let reference = renderer.render_reference(&frame, 160, 120, 32)?;
        let shared = renderer.render(&frame, 160, 120, 1)?;
        let energy = |p: &[u8], channel: usize| {
            p.chunks_exact(4)
                .map(|p| f64::from(p[channel]))
                .sum::<f64>()
        };
        for channel in 0..3 {
            let expected = energy(&reference, channel);
            let actual = energy(&shared, channel);
            assert!(expected > 1000.0, "test must contain visible fog");
            assert!(
                (actual - expected).abs() / expected < 0.05,
                "sharing changed channel {channel} energy: {actual} vs {expected}"
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
    fn residual_compaction_encodes_more_than_256_resident_lights() -> anyhow::Result<()> {
        // A payload table of 256 four-way buckets cannot hold every pair, so
        // the lost entries exercise the exact-traversal segment on any
        // adapter rather than only where the interval counts happen to
        // overflow `CACHE_K`.
        let mut gpu = Gpu::build()?;
        gpu.interval_cache_table_bits = 10;
        let mut renderer = Renderer::on(Arc::new(gpu));
        renderer.compaction.group_columns = 2;
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
        renderer.compaction.suspended = 5;
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
        assert!(completed.segments.iter().all(|count| *count > 0));
        assert!(
            completed
                .segments
                .into_iter()
                .zip(crate::haze_compact::RESID_LANES)
                .any(|(count, lanes)| count > lanes * renderer.compaction.group_columns),
            "no segment crossed an artificial dispatch row: {completed:?}"
        );
        Ok(())
    }

    #[test]
    fn residual_overflow_falls_back_on_its_first_frame() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
        renderer.compaction.capacity = 64;
        let mut frame = fixture_surface_frame(300);
        frame.geometry_shadows = true;
        frame.fixture_surface_lighting = false;
        frame.haze_density = 0.3;
        frame.haze_resolution = 1.0;

        // Warm a stable fused oracle in this renderer without allocating
        // compact pools. The odd dimensions exercise partial edge groups.
        renderer.compaction.suspended = 6;
        for _ in 0..4 {
            renderer.render(&frame, 97, 73, 1)?;
        }
        let fused = renderer.render(&frame, 97, 73, 1)?;
        renderer.compaction.suspended = 0;

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
            completed.list_count > 64,
            "total overflow was not exercised"
        );
        Ok(())
    }

    #[test]
    fn shared_residual_queue_repacks_incremental_and_gc_lists() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
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
        assert_eq!(baseline.segments.iter().sum::<u32>(), baseline.list_count);

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

        renderer.compaction.full_count = 1;
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
        Ok(())
    }

    #[test]
    fn compact_output_dispatch_tracks_equal_area_reshape() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
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
        renderer.compaction.suspended = 5;
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

    /// A moving head is opaque geometry in the sun cascades. Its body must
    /// invalidate them when it moves, or the sun keeps its old shadow.
    #[test]
    fn sun_cascades_redraw_when_a_fixture_body_moves() -> anyhow::Result<()> {
        let mut frame = fixture_surface_frame(0);
        frame.fixture_shadows = false;
        frame.fixture_surface_lighting = false;
        frame.directional = Some(DirectionalLight {
            direction: Vec3::new(0.3, -0.2, 1.0).normalize(),
            radiance: Vec3::splat(3.0),
            shadow_eye: Vec3::ZERO,
            shadows: true,
            shadow_softness: 1.0,
        });
        let body = frame.meshes.len();
        frame.meshes.push(MeshData {
            key: "::moving-head-body".into(),
            ..frame.meshes[0].clone()
        });
        frame.draws.push(Draw {
            mesh: body,
            model: Mat4::from_translation(Vec3::new(-2.0, 0.0, 1.0))
                * Mat4::from_scale(Vec3::new(0.2, 0.2, 1.0)),
            material: frame.draws[0].material,
            textures: MaterialTextures::default(),
            editor_object: Some(crate::frame::EditorObject::Fixture("head".into())),
        });
        let opaque = frame.draws.len();
        let before_hash = super::opaque_depth_hash(&frame, opaque);
        let fixture_casters = crate::shadow::fixture_shadow_caster_hash(&frame, opaque);

        let mut renderer = Renderer::new()?;
        let before = renderer.render(&frame, 160, 120, 1)?;
        frame.draws[1].model = Mat4::from_translation(Vec3::new(2.0, 0.0, 1.0))
            * Mat4::from_scale(Vec3::new(0.2, 0.2, 1.0));
        assert_eq!(
            crate::shadow::fixture_shadow_caster_hash(&frame, opaque),
            fixture_casters,
            "fixture maps deliberately ignore fixture bodies"
        );
        assert_ne!(super::opaque_depth_hash(&frame, opaque), before_hash);
        let moved = renderer.render(&frame, 160, 120, 1)?;
        let fresh = Renderer::new()?.render(&frame, 160, 120, 1)?;
        assert_ne!(before, fresh, "the moved body must change the picture");
        assert_eq!(moved, fresh, "a moved fixture body left a stale sun shadow");
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
            light.color = Vec3::splat(0.05);
            light.intensity = 0.01;
            light.gobo = 2;
        }
        let shading_only = renderer.render(&frame, 160, 120, 1)?;
        assert_eq!(first, shading_only);
        assert!(first.chunks_exact(4).any(|pixel| pixel[2] > 32));
        // A dark cone is not a light at all: it leaves the index.
        for light in &mut frame.fixture_cones {
            light.color = Vec3::ZERO;
        }
        let dark = renderer.render(&frame, 160, 120, 1)?;
        assert_ne!(dark, first);
        Ok(())
    }

    /// The interval cache, its write generations and the compaction's
    /// per-slot classification carry state from frame to frame. Through
    /// transitions (a moved occluder, a resize, haze off, a camera move, a gobo
    /// toggle, a dark cone, shadows off) a settled frame must equal what the
    /// same renderer settles to from cold haze caches, bit for bit. The
    /// unsettled steps in between render once each, so state such as a
    /// classification kept across a resize meets the frames that reuse it.
    #[test]
    fn haze_state_survives_frame_transitions() -> anyhow::Result<()> {
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
        // The reference sees the same frames, so state outside the haze
        // (shadow-slot residency, cached maps) has the same history; only its
        // haze caches start cold at every probe.
        let gpu = Arc::new(Gpu::build()?);
        let mut renderer = Renderer::on(gpu.clone());
        let mut reference = Renderer::on(gpu);
        let mut first = Vec::new();
        let original_intensity = frame.fixture_cones[1].intensity;
        for (probe, (width, height), settle) in [
            (0, (127, 97), true),
            (1, (127, 97), false),
            (2, (127, 97), false),
            (3, (65, 49), false),
            (4, (127, 97), false),
            (5, (127, 97), false),
            (6, (127, 97), false),
            (7, (127, 97), true),
            (8, (127, 97), true),
            (9, (127, 97), false),
            (10, (127, 97), true),
            (11, (127, 97), true),
        ] {
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
                    renderer.visibility_reference = true;
                    reference.visibility_reference = true;
                }
                _ => {}
            }
            if !settle {
                let actual = renderer.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
                reference.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
                if probe == 4 {
                    assert_ne!(actual, first, "haze-off transition did not change the image");
                }
                continue;
            }
            // Cold, write, read: three frames settle a fresh cache.
            reference.forget_haze_history();
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            for _ in 0..3 {
                actual = renderer.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
                expected = reference.render_next(&frame, width, height, crate::LIVE_SUBFRAMES)?;
            }
            if actual != expected {
                let pixels = actual
                    .chunks_exact(4)
                    .zip(expected.chunks_exact(4))
                    .filter(|(a, b)| a != b)
                    .count();
                let max = actual.iter().zip(&expected).map(|(a, b)| a.abs_diff(*b)).max();
                panic!("probe {probe}: {pixels} pixels differ from a cold haze cache, max {max:?}");
            }
            if probe == 0 {
                first = actual;
            }
        }
        Ok(())
    }

    #[test]
    fn fog_block_stats_follow_frames_and_resizes() -> anyhow::Result<()> {
        let mut renderer = Renderer::new()?;
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
        let first = renderer.fog_block_stats()?.expect("classified frame");
        assert_eq!(first.image_size, [127, 97]);
        let tile = crate::fog_grid::TILE_SIZE;
        let first_grid = [127_u32.div_ceil(tile), 97_u32.div_ceil(tile), 128];
        let first_blocks = first_grid.map(|n| n.div_ceil(4));
        assert_eq!(first.grid_size, first_grid);
        assert_eq!(first.blocks, first_blocks);
        assert_eq!(
            first.counts.len(),
            first_blocks.into_iter().product::<u32>() as usize
        );
        assert!(first.counts.iter().any(|counts| counts[0] > 0));
        assert!(
            first
                .counts
                .iter()
                .all(|counts| counts[1] <= counts[0] && counts[0] <= 3)
        );
        assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        assert_eq!(renderer.fog_block_stats()?.unwrap().counts, first.counts);
        // The lighting shader trusts these source eligibility filters. A cue
        // must clear candidates that become ineligible, then restore them.
        frame.fixture_cones[0].wash = crate::fog_grid::BROAD_WASH - 0.01;
        frame.fixture_cones[1].gobo = 1;
        frame.fixture_cones[2].haze_gain = 0.0;
        renderer.render_next(&frame, 127, 97, crate::LIVE_SUBFRAMES)?;
        assert!(
            renderer
                .fog_block_stats()?
                .unwrap()
                .counts
                .iter()
                .all(|counts| *counts == [0, 0])
        );
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
        assert!(
            renderer
                .targets
                .as_ref()
                .unwrap()
                .msaa_surface_depth
                .is_none()
        );
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
                    lens: crate::luminaire::Lens::POINT,
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
            floor: None,
            fixture_cones,
            fixture_shadow_capacity_hint: 0,
            fixture_surface_lighting: true,
            beam_proxy: false,
            fixture_shadows: true,
            geometry_shadows: false,
            cluster_debug: false,
            probes: crate::scene_desc::ProbeView::default(),
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
            quality: Default::default(),
            time: 0.0,
            debug_view: DebugView::Pbr,
            look: crate::scene_desc::Look::NEUTRAL,
            camera: Camera {
                eye: Vec3::new(0.0, -6.0, 4.5),
                target: Vec3::ZERO,
                fov_y_deg: 48.0,
            },
        }
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
            floor: None,
            overlays: vec![Overlay {
                mesh: 0,
                model: Mat4::from_translation(Vec3::Y * overlay_y),
                lines: false,
                color: hex_srgb(0x33_99_e6),
                opacity: 1.0,
                depth,
            }],
            fixture_cones: Vec::new(),
            fixture_shadow_capacity_hint: 0,
            fixture_surface_lighting: false,
            beam_proxy: false,
            fixture_shadows: true,
            geometry_shadows: false,
            cluster_debug: false,
            probes: crate::scene_desc::ProbeView::default(),
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
            quality: Default::default(),
            time: 0.0,
            debug_view: DebugView::Pbr,
            look: crate::scene_desc::Look::NEUTRAL,
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
            Channels::Hdr | Channels::Rgb10 => unreachable!("read back only as RGBA8 or BGRA8"),
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
            let [
                start,
                scene_end,
                haze_end,
                composite_end,
                index0,
                index1,
                grid0,
                grid1,
                prepared,
                lit,
            ] = <[u64; 10]>::try_from(&timestamps[..10]).expect("base queries");
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

fn instance_of(frame: &Frame, draw: &Draw) -> Instance {
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
            f32::from(u8::from(crate::frame::is_ground(
                &frame.meshes[draw.mesh].key,
            ))),
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

/// The scene pass's multisampled pipelines. Low quality renders single
/// sampled, and the sample count is baked into a pipeline.
struct ScenePipelines {
    scene: wgpu::RenderPipeline,
    /// The ground under a floor material: `scene.wgsl`'s `fs_ground`.
    ground: wgpu::RenderPipeline,
    surface_depth: wgpu::RenderPipeline,
    grid: wgpu::RenderPipeline,
    compass: wgpu::RenderPipeline,
    cables: wgpu::RenderPipeline,
    /// The reflection probes as small balls (`probe_debug.wgsl`).
    probe_debug: wgpu::RenderPipeline,
}

/// Everything [`ScenePipelines`] is built from, kept so a second sample count
/// can be built when it is first asked for.
struct ScenePipelineSource {
    layout: wgpu::PipelineLayout,
    scene: wgpu::ShaderModule,
    grid: wgpu::ShaderModule,
    cables: wgpu::ShaderModule,
    /// `LUMA_PROFILE_OMIT` switches of the scene shader.
    skips: [f64; 3],
}

const MESH_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 4] = [
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
];

fn mesh_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: 48,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MESH_VERTEX_ATTRIBUTES,
    }
}

impl ScenePipelineSource {
    fn build(&self, device: &wgpu::Device, samples: u32) -> ScenePipelines {
        let lit = |label: &str, entry_point: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&self.layout),
                vertex: wgpu::VertexState {
                    module: &self.scene,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(mesh_vertex_layout())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.scene,
                    entry_point: Some(entry_point),
                    targets: &[Some(SCENE_FORMAT.into())],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &[
                            ("PROFILE_SKIP_SURFACE_CLOUDS", self.skips[0]),
                            ("PROFILE_SKIP_FIXTURES", self.skips[1]),
                            ("PROFILE_SKIP_SURFACE_SHADOWS", self.skips[2]),
                        ],
                        ..Default::default()
                    },
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(depth_state(true)),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let scene = lit("scene", "fs_main");
        let ground = lit("ground", "fs_ground");

        let surface_depth = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("surface-depth"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.scene,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.scene,
                entry_point: Some("fs_surface_depth"),
                targets: &[Some(wgpu::TextureFormat::R16Float.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let probe_debug = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("probe-debug"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.scene,
                entry_point: Some("vs_probe_debug"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.scene,
                entry_point: Some("fs_probe_debug"),
                targets: &[Some(SCENE_FORMAT.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(depth_state(true)),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let grid = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.grid,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.grid,
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
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        // The grid's own state, with the grid's own module, down to a second
        // fragment entry point: the compass is the same quad, blended the same
        // way, and a divergent depth or blend choice here would be a second
        // answer to a settled question.
        let compass = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("compass"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.grid,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.grid,
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
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        // The grid's own state, with the grid's shader swapped out: both are
        // transparent scene-space affordances that test depth and never write
        // it, and a second blend or depth choice here would be a second answer
        // to the same question.
        let cables = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cables"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.cables,
                entry_point: Some("vs_main"),
                buffers: &[Some(mesh_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.cables,
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
                count: samples,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        ScenePipelines {
            scene,
            ground,
            surface_depth,
            grid,
            compass,
            cables,
            probe_debug,
        }
    }
}

impl Gpu {
    /// The scene pipelines for `samples` per pixel: [`MSAA_SAMPLES`] or one.
    fn scene_pipelines(&self, samples: u32) -> &ScenePipelines {
        if samples == MSAA_SAMPLES {
            &self.scene_pipelines
        } else {
            self.scene_pipelines_single
                .get_or_init(|| self.scene_source.build(&self.device, samples))
        }
    }
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
        lens: crate::luminaire::Lens {
            radius: finite(light.lens.radius, 0.0).clamp(0.0, crate::luminaire::Lens::MAX_RADIUS_M),
        },
    }
}

/// The shared bind-group declarations every scene-geometry shader opens with;
/// see `scene_bindings.wgsl`.
fn scene_bindings_wgsl() -> String {
    format!(
        "{}{}{}{}{}{}{}{}",
        crate::haze_field::prelude(),
        include_str!("shaders/medium.wgsl"),
        include_str!("shaders/scene_bindings.wgsl"),
        crate::atmosphere::surface_prelude(),
        include_str!("shaders/horizon.wgsl"),
        include_str!("shaders/haze_daylight.wgsl"),
        include_str!("shaders/shaft_upsample.wgsl"),
        include_str!("shaders/outdoor_surface.wgsl"),
    )
}

/// The volumetric transport (ray reconstruction, per-light integral and
/// group-0 layout) that both volumetric passes prepend.
fn beam_transport_wgsl() -> String {
    format!(
        "{}{}{}",
        include_str!("shaders/medium.wgsl"),
        crate::fog_grid::prelude(),
        include_str!("shaders/beam_transport.wgsl")
    )
}

/// The composite module's source.
fn composite_wgsl() -> String {
    format!(
        "{}{}{}{}{}{}{}",
        crate::haze_field::prelude(),
        include_str!("shaders/medium.wgsl"),
        crate::atmosphere::composite_prelude(),
        include_str!("shaders/haze_daylight.wgsl"),
        include_str!("shaders/shaft_upsample.wgsl"),
        include_str!("shaders/tone.wgsl"),
        include_str!("shaders/composite.wgsl")
    )
}

/// The scene module's source.
///
/// The local reflection probes (`probes.rs`) are in it: the scene pass
/// samples them, and their relight is this module with its entry points
/// appended, so a probe texel is lit by the functions that light the surface
/// it stands for. The light-index prelude is authored against group 1 (the
/// haze pass's slot); the surface pass carries the same bindings inside its
/// group 3, so its copy is rebound by this one documented replace.
pub(crate) fn scene_wgsl() -> String {
    format!(
        "{}{}{}{}{}{}{}{}{}",
        scene_bindings_wgsl(),
        crate::probes::prelude(),
        include_str!("shaders/probe_sample.wgsl"),
        include_str!("shaders/light_index.wgsl").replace("@group(1)", "@group(3)"),
        include_str!("shaders/fixture_light.wgsl"),
        include_str!("shaders/visibility.wgsl"),
        include_str!("shaders/scene.wgsl"),
        include_str!("shaders/floor.wgsl"),
        include_str!("shaders/probe_debug.wgsl"),
    )
}

pub(crate) fn shader(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
    })
}

pub(crate) fn specialize_wgsl_overrides(source: &str, values: &[(&str, f64)]) -> String {
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

/// The camera exactly as the scene pass sees it.
pub(crate) struct CameraMatrices {
    /// The far plane: bounded under a sky, where the atmosphere ends.
    pub far: f32,
    pub view: Mat4,
    /// Finite reverse-Z: near maps to one, `far` to zero.
    pub proj: Mat4,
}

/// The frame's camera matrices at `aspect`. One function, so a debug export
/// of the camera cannot drift from what the renderer drew.
pub(crate) fn camera_matrices(frame: &Frame, aspect: f32) -> CameraMatrices {
    let far = if frame.sky.is_some() {
        crate::atmosphere::MAX_DISTANCE_M
    } else {
        CAMERA_FAR
    };
    CameraMatrices {
        far,
        // Passing the bounds in reverse order produces a finite reverse-Z
        // projection: near maps to one and the bounded far plane maps to zero.
        proj: Mat4::perspective_rh(
            frame.camera.fov_y_deg.to_radians(),
            aspect,
            far,
            CAMERA_NEAR,
        ),
        view: Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z),
    }
}

/// The sun's shadow casters: every opaque draw but the ground, with its world
/// bounding sphere. `mesh_bounds` are the meshes' local spheres, by index.
pub(crate) fn sun_casters<'a>(
    frame: &'a Frame,
    mesh_bounds: &'a [(Vec3, f32)],
) -> impl Iterator<Item = (&'a Draw, (Vec3, f32))> + 'a {
    let opaque = frame.draws.len() - frame.transparent.len();
    frame.draws[..opaque]
        .iter()
        .filter(|draw| !crate::frame::is_ground(&frame.meshes[draw.mesh].key))
        .map(|draw| (draw, world_sphere(draw, mesh_bounds[draw.mesh])))
}

/// A draw's world-space bounding sphere from its mesh's local one, scaled by
/// the model's largest axis.
fn world_sphere(draw: &Draw, (local, radius): (Vec3, f32)) -> (Vec3, f32) {
    let scale = draw
        .model
        .to_scale_rotation_translation()
        .0
        .abs()
        .max_element();
    (draw.model.transform_point3(local), radius * scale)
}

/// Fit three stable orthographic sun cameras to bounded view-frustum slices.
///
/// Each slice uses a quantized bounding sphere instead of a tight AABB. Camera
/// translation then changes only the snapped light-space centre; small camera
/// rotations cannot resize the projection and make every shadow texel swim.
///
/// The slice only bounds the receivers. The near plane reaches back to every
/// caster (`casters`: world bounding spheres) whose light-space footprint
/// overlaps the cascade, so the shadow on a receiver is the same from every
/// camera that sees it.
fn cascade_matrices(
    eye: Vec3,
    forward: Vec3,
    fov_y: f32,
    aspect: f32,
    to_light: Vec3,
    casters: &[(Vec3, f32)],
) -> [Mat4; CASCADE_COUNT] {
    cascade_fits(eye, forward, fov_y, aspect, to_light, casters).map(|fit| fit.view_proj)
}

/// One sun cascade, with the numbers its matrix was built from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CascadeFit {
    /// Camera distances of the view slice this cascade covers.
    pub slice: (f32, f32),
    /// The fixed light frame every cascade shares.
    pub light_view: Mat4,
    /// Snapped slice centre in light space, x and y.
    pub centre: (f32, f32),
    /// Quantized half-extent of the orthographic square.
    pub radius: f32,
    /// Light-space depth of the slice's nearest and furthest corner.
    pub slice_depth: (f32, f32),
    /// Nearest depth after reaching back to the casters.
    pub caster_depth: f32,
    /// The orthographic near and far planes as passed (far margin, near
    /// margin applied).
    pub planes: (f32, f32),
    pub view_proj: Mat4,
}

/// Every cascade's fit. See [`cascade_matrices`].
pub(crate) fn cascade_fits(
    eye: Vec3,
    forward: Vec3,
    fov_y: f32,
    aspect: f32,
    to_light: Vec3,
    casters: &[(Vec3, f32)],
) -> [CascadeFit; CASCADE_COUNT] {
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
        // A caster between the sun and the slice can be far from it along
        // the light: a truss 10 m up is 140 m from its shadow under a 4
        // degree sun. A near plane fitted to the slice clipped such casters,
        // and which ones depended on where the camera stood.
        let near_depth = casters
            .iter()
            .filter_map(|&(centre, reach)| {
                let centre = light_view.transform_point3(centre);
                ((centre.x - snapped_x).abs() <= radius + reach
                    && (centre.y - snapped_y).abs() <= radius + reach)
                    .then_some(-centre.z - reach)
            })
            .fold(min_depth, f32::min);
        // Swap near/far to map the nearest caster to one and the furthest
        // receiver to zero, matching the scene's GreaterEqual reverse-Z
        // convention. The signed distances are valid for an orthographic
        // projection and let this fixed light frame cover venues on either
        // side of its origin. The far margin keeps the blend band at the end
        // of the previous slice, which also samples this map, inside it.
        let planes = (max_depth + 25.0, near_depth - 1.0);
        CascadeFit {
            slice: (near, far),
            light_view,
            centre: (snapped_x, snapped_y),
            radius,
            slice_depth: (min_depth, max_depth),
            caster_depth: near_depth,
            planes,
            view_proj: Mat4::orthographic_rh(
                snapped_x - radius,
                snapped_x + radius,
                snapped_y - radius,
                snapped_y + radius,
                planes.0,
                planes.1,
            ) * light_view,
        }
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

pub(crate) fn binding(index: u32, resource: wgpu::BindingResource) -> wgpu::BindGroupEntry {
    wgpu::BindGroupEntry {
        binding: index,
        resource,
    }
}

pub(crate) fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
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

pub(crate) fn rw_storage_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
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
