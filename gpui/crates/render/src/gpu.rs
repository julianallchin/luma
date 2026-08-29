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
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::assets::Image;
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

const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Timestamp query layout:
/// 0 first render pass start, 1 scene pass end, 2 haze region end (zero when
/// haze did not run), 3 composite pass end, 4/5 light-index build compute
/// pass.
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
const QUERY_COUNT: u32 = 6;
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
    dir_to_light: [f32; 4],
    dir_color: [f32; 4],
    params: [f32; 4],
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
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SurfaceClusterUniform {
    /// x: surface lighting enabled, y: occupancy debug. The culling structure
    /// itself is the shared light index; only pass flags remain here.
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
    /// Whether the adapter can provide hardware timestamp queries.
    pub timestamp_query_supported: bool,
    /// Nanoseconds represented by one timestamp tick, when supported.
    pub timestamp_period_ns: Option<f32>,
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
#[derive(Debug, Clone, Copy)]
pub struct FrameTimings {
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
    /// CPU time for scene preparation, command encoding and queue submission.
    pub cpu_encode_submit_ms: f64,
    /// CPU time spent rebuilding the deterministic surface-light cluster CSR.
    /// A cache hit reports zero.
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
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuSpans {
    /// Camera matrices, cone sanitising, shadow-slot assignment, light arrays.
    pub prepare: Duration,
    /// Clustered-light CSR rebuild. Zero on a cache hit.
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
    shadow_map: wgpu::TextureView,
    shadow_layers: [wgpu::TextureView; CASCADE_COUNT],
    fixture_shadow_map: wgpu::TextureView,
    fixture_shadow_layers: Vec<wgpu::TextureView>,
    fixture_shadow_cache: Vec<Option<ShadowCacheKey>>,
    cascade_shadow_cache: [Option<ShadowCacheKey>; CASCADE_COUNT],
    /// Which cone occupies each shadow slot, carried across frames so
    /// [`assign_shadow_slots`] can keep a resident rather than reshuffling.
    fixture_shadow_slots: [Option<usize>; MAX_FIXTURE_SHADOWS],
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
    upload_stats: UploadStats,
    targets: Option<Targets>,
    haze_history_valid: bool,
    haze_history_index: usize,
    haze_history_key: Option<HazeHistoryKey>,
    last_live_time: Option<f32>,
    live_noise_frame: u32,
    /// Measured-frame counter for the profiler's fragment-count pass cadence;
    /// deliberately not `live_noise_frame`, which resets whenever temporal
    /// history invalidates (i.e. every frame of a moving show).
    fragment_count_frame: u32,
    profiler: Option<ProfilerResources>,
    light_index: LightIndex,
    shadow_stats: ShadowStats,
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

struct Targets {
    width: u32,
    height: u32,
    /// Size of the haze target, which runs at `haze_resolution` of the output.
    haze_width: u32,
    haze_height: u32,
    destination: Destination,
    msaa_color: wgpu::TextureView,
    msaa_depth: wgpu::TextureView,
    scene: wgpu::TextureView,
    depth: wgpu::TextureView,
    haze: wgpu::TextureView,
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
    #[cfg(target_os = "macos")]
    Shared(crate::share::Shared),
}

impl PresentationTarget {
    fn view(&self) -> &wgpu::TextureView {
        match self {
            Self::Staged { view, .. } => view,
            #[cfg(target_os = "macos")]
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
    #[cfg(target_os = "macos")]
    Share(core_video::pixel_buffer::CVPixelBuffer),
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
    #[cfg(target_os = "macos")]
    Shared {
        buffer: core_video::pixel_buffer::CVPixelBuffer,
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
        encoder.resolve_query_set(&self.query_set, 0..QUERY_COUNT, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(
            &self.resolve,
            0,
            &self.readback,
            0,
            u64::from(QUERY_COUNT) * 8,
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
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_profile: RendererProfile,
    environment: EnvironmentPipelines,
    scene_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    cluster_layout: wgpu::BindGroupLayout,
    haze_layout: wgpu::BindGroupLayout,
    /// The baked volumetric density field. Belongs here rather than on the
    /// renderer because no frame mutates it — it is a function of the device
    /// and nothing else.
    haze_field: HazeField,
    light_index_pipelines: LightIndexPipelines,
    temporal_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    overlay_layout: wgpu::BindGroupLayout,
    scene_pipeline: wgpu::RenderPipeline,
    depth_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    fixture_shadow_layout: wgpu::BindGroupLayout,
    fixture_shadow_pipeline: wgpu::RenderPipeline,
    haze_pipeline: wgpu::RenderPipeline,
    temporal_pipeline: wgpu::RenderPipeline,
    /// Indexed by [`Channels::index`]: the same pass, targeting each output
    /// format.
    composite_pipelines: [wgpu::RenderPipeline; 2],
    grid_pipeline: wgpu::RenderPipeline,
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

    fn build() -> anyhow::Result<Self> {
        let started = Instant::now();
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            // Bucketed limits trade exact hardware limits for cache-key
            // stability across near-identical adapters; this device is
            // process-local and never serialised, so exact limits are fine.
            apply_limit_buckets: false,
        }))?;
        let timestamp_query_supported =
            adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        // Enabled whenever the adapter offers it, because this device is now
        // the only one: refusing the feature here would mean no renderer built
        // on it could ever be profiled. Declaring it costs nothing — the cost
        // is writing timestamps, which is per-frame and stays opt-in.
        let required_features = if timestamp_query_supported {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("luma-render"),
                required_features,
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            }))?;
        let adapter_info = adapter.get_info();
        let adapter_profile = RendererProfile {
            name: adapter_info.name,
            backend: format!("{:?}", adapter_info.backend),
            device_type: format!("{:?}", adapter_info.device_type),
            driver: adapter_info.driver,
            driver_info: adapter_info.driver_info,
            timestamp_query_supported,
            timestamp_period_ns: timestamp_query_supported.then(|| queue.get_timestamp_period()),
        };
        let lost = Arc::new(AtomicBool::new(false));
        device.set_device_lost_callback({
            let lost = Arc::clone(&lost);
            move |reason, message| {
                lost.store(true, Ordering::Relaxed);
                // stderr for the same reason the worker supervisor uses it:
                // this is the line that turns "everything went black" into a
                // diagnosis, and it must not depend on a log sink being wired.
                eprintln!("luma-render device lost ({reason:?}): {message}");
            }
        });
        let environment = EnvironmentPipelines::new(&device, &queue);
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
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
            ],
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
                comparison_sampler_entry(7, wgpu::ShaderStages::FRAGMENT),
                uniform_entry(8, wgpu::ShaderStages::FRAGMENT),
                storage_entry(9, wgpu::ShaderStages::FRAGMENT),
                storage_entry(10, wgpu::ShaderStages::FRAGMENT),
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
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                storage_entry(1, wgpu::ShaderStages::FRAGMENT),
                storage_entry(2, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage_entry(6, wgpu::ShaderStages::FRAGMENT),
                depth_array_entry(7, wgpu::ShaderStages::FRAGMENT),
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
            ],
        });

        // Both scene-geometry shaders open with the shared bind-group
        // declarations; see `scene_bindings.wgsl`.
        let bindings = include_str!("shaders/scene_bindings.wgsl");
        let fixture_light = include_str!("shaders/fixture_light.wgsl");
        // The light-index prelude is authored against group 1 (the haze
        // pass's slot); the surface pass carries the same bindings inside its
        // group 3, so its copy is rebound by this one documented replace.
        let light_index_prelude = include_str!("shaders/light_index.wgsl");
        let scene_light_index_prelude = light_index_prelude.replace("@group(1)", "@group(3)");
        let scene_module = shader(
            &device,
            "scene",
            &format!(
                "{bindings}{scene_light_index_prelude}{fixture_light}{}",
                include_str!("shaders/scene.wgsl")
            ),
        );
        // The transport (ray reconstruction + per-light integral + group-0
        // layout) is one file both volumetric passes prepend, so they cannot
        // draw two different beams.
        let beam_transport = include_str!("shaders/beam_transport.wgsl");
        // The density field's dimensions are compile-time properties of
        // `haze_field`, so they arrive as injected constants rather than as
        // uniform members nobody could see drift.
        let haze_field_prelude = crate::haze_field::prelude();
        let haze_module = shader(
            &device,
            "haze",
            &format!(
                "{haze_field_prelude}{fixture_light}{light_index_prelude}{beam_transport}{}",
                include_str!("shaders/haze.wgsl")
            ),
        );
        let temporal_module = shader(
            &device,
            "haze-temporal",
            include_str!("shaders/haze_temporal.wgsl"),
        );
        let composite_module = shader(&device, "composite", include_str!("shaders/composite.wgsl"));
        let grid_module = shader(
            &device,
            "grid",
            &format!("{bindings}{}", include_str!("shaders/grid.wgsl")),
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

        let haze_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("haze"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("haze"),
                    bind_group_layouts: &[Some(&haze_layout), Some(light_index_pipelines.layout())],
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
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

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
                bind_group_layouts: &[Some(&composite_layout), Some(environment.scene_layout())],
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
                buffers: &[Some(grid_vertex_layout)],
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

        Ok(Self {
            device,
            queue,
            adapter_profile,
            environment,
            scene_layout,
            material_layout,
            cluster_layout,
            haze_layout,
            haze_field,
            light_index_pipelines,
            temporal_layout,
            composite_layout,
            overlay_layout,
            scene_pipeline,
            depth_pipeline,
            shadow_pipeline,
            fixture_shadow_layout,
            fixture_shadow_pipeline,
            haze_pipeline,
            temporal_pipeline,
            composite_pipelines,
            grid_pipeline,
            overlay_pipelines,
            hard_shadow_sampler,
            shadow_sampler,
            dummy_shadow,
            linear_sampler,
            texture_sampler,
            white_material,
            material_defaults,
            lost,
            built_in: started.elapsed(),
        })
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
        let profiler = profiled.then(|| ProfilerResources {
            slots: std::array::from_fn(|_| ProfilerSlot {
                query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("luma-profile-timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: QUERY_COUNT,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("luma-profile-resolve"),
                    size: QUERY_COUNT as u64 * 8,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("luma-profile-readback"),
                    size: QUERY_COUNT as u64 * 8,
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
        let light_index = LightIndex::new(device);

        Self {
            gpu,
            environment: EnvironmentCache::default(),
            shadow_map,
            shadow_layers,
            fixture_shadow_map,
            fixture_shadow_layers,
            fixture_shadow_cache: vec![None; MAX_FIXTURE_SHADOWS],
            cascade_shadow_cache: [None; CASCADE_COUNT],
            fixture_shadow_slots: [None; MAX_FIXTURE_SHADOWS],
            texture_views: HashMap::new(),
            materials: HashMap::new(),
            geometry: None,
            upload_stats: UploadStats {
                geometry: 0,
                textures: 0,
                environments: 0,
            },
            targets: None,
            haze_history_valid: false,
            haze_history_index: 0,
            haze_history_key: None,
            last_live_time: None,
            live_noise_frame: 0,
            fragment_count_frame: 0,
            profiler,
            light_index,
            shadow_stats: ShadowStats::default(),
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
                #[cfg(target_os = "macos")]
                if destination == Destination::Compositor {
                    if let Some(shared) = crate::share::Shared::new(&self.gpu.device, width, height)
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
                msaa_color: color(
                    width,
                    height,
                    MSAA_SAMPLES,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                    "scene-msaa",
                ),
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
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    "haze",
                ),
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

    /// Render the *next* frame of a sequence and read it back as
    /// sRGB-encoded RGBA8, row-major, no padding.
    ///
    /// The haze march is progressive. Where [`Renderer::render`] pays for a
    /// clean volumetric in `subframes` jittered marches of one moment, this
    /// blends each march into the history the last call left behind — the
    /// live viewport's own path, which is how it gets a usable image out of
    /// [`crate::LIVE_SUBFRAMES`]. The history is discarded whenever this
    /// frame is not plausibly the successor of the last one drawn through
    /// this renderer: a different size, camera, medium, or cone geometry, a
    /// clock that jumped, or an intervening [`Renderer::render`].
    ///
    /// So a caller sweeping a time axis calls this in order and warms up over
    /// the frames it can afford to throw away, and any caller that wants one
    /// moment on its own calls [`Renderer::render`] instead — there is no
    /// ordering rule to remember, only which of the two questions is being
    /// asked.
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
        pending
            .complete_blocking(&self.gpu.device)?
            .profile
            .ok_or_else(|| anyhow::anyhow!("profile query resources were unavailable"))
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

    /// Read back the surface pass's `(lit fragments, candidates walked)`
    /// accumulation for the most recently *completed* profiled frame.
    ///
    /// Profiler-only: the counters are written solely when the frame was
    /// submitted with `measure`, and this call performs its own blocking
    /// copy + map, which is fine on a measurement path and nowhere else.
    /// Returns `None` when no fragments were counted.
    pub fn fragment_stats(&mut self) -> anyhow::Result<Option<(u64, u64)>> {
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

    /// What the fixture-shadow passes submitted on the last frame.
    #[must_use]
    pub fn shadow_stats(&self) -> ShadowStats {
        self.shadow_stats
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
        let aspect = width as f32 / height as f32;
        // Passing the bounds in reverse order produces a finite reverse-Z
        // projection: near maps to one and the bounded far plane maps to zero.
        let proj = Mat4::perspective_rh(
            frame.camera.fov_y_deg.to_radians(),
            aspect,
            CAMERA_FAR,
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
        // Which cones get a map, and therefore which layer each one renders
        // into. Slots are assigned by priority rather than by cone index, so a
        // rig larger than the cap still shadows the beams that matter.
        let shadow_slots = if frame.fixture_shadows {
            assign_shadow_slots(&fixture_cones, frame.camera.eye, &self.fixture_shadow_slots)
        } else {
            [None; MAX_FIXTURE_SHADOWS]
        };
        self.fixture_shadow_slots = shadow_slots;
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
            .map(|(index, light)| LightRest {
                direction: light
                    .direction
                    .try_normalize()
                    .unwrap_or(Vec3::NEG_Y)
                    .to_array(),
                cos_beam: light.cos_beam.clamp(-1.0, 1.0),
                color: light.color.to_array(),
                intensity: light.intensity.clamp(0.0, 100.0),
                cos_field: light.cos_field.clamp(-1.0, 1.0),
                wash: light.wash.clamp(0.0, 1.0),
                gobo: light.gobo.min(2) as f32,
                gobo_rotation: light.gobo_rotation.rem_euclid(std::f32::consts::TAU),
                shadow_slot: slot_of[index],
                _pad: [0.0; 3],
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
                    far: CAMERA_FAR,
                },
                &cores,
                &rests,
                profile_resources
                    .as_ref()
                    .map(|(queries, ..)| wgpu::ComputePassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: Some(4),
                        end_of_pass_write_index: Some(5),
                    }),
            )
            .clone();
        let clusters_done = Instant::now();
        let cpu_cluster = clusters_done - cluster_started;

        let globals = Globals {
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
        let (_environment_uniform, environment_bg) = self.environment.bind_group(
            &self.gpu.environment,
            &self.gpu.device,
            frame.environment.as_ref(),
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
            &instances,
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
        let cluster_uniform = self.storage(
            &mut encoder,
            &[SurfaceClusterUniform {
                flags: [
                    f32::from(u8::from(frame.fixture_surface_lighting)),
                    f32::from(u8::from(frame.cluster_debug)),
                    0.0,
                    0.0,
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
        let index_bindings = self.light_index.bindings();
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
                    binding(7, wgpu::BindingResource::Sampler(&self.gpu.shadow_sampler)),
                    binding(8, index_bindings.params.as_entire_binding()),
                    binding(9, index_bindings.tile_masks.as_entire_binding()),
                    binding(10, index_bindings.z_bins.as_entire_binding()),
                ],
            });
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
            self.scene_bind_group(&globals_buf, &instance_buf, &point_buf, true, hard_shadows);
        let unlit_bg = self.scene_bind_group(&globals_buf, &instance_buf, &point_buf, false, false);
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
                    self.scene_bind_group(&buffer, &instance_buf, &point_buf, false, false),
                )
            })
            .collect();
        let opaque = frame.draws.len() - frame.grid_draws;
        let caster_hash = fixture_shadow_caster_hash(frame, opaque);
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
            .map(|(index, key)| self.fixture_shadow_cache[index] != Some(*key))
            .collect();
        // Only a map that is about to be redrawn needs a projection uniform and
        // a bind group. Building all 128 regardless was most of this frame's
        // encode time on the shadowed preset.
        let fixture_shadow_bgs: Vec<_> = fixture_shadow_matrices
            .iter()
            .zip(&fixture_shadow_dirty)
            .zip(&shadow_slots)
            .enumerate()
            .map(|(slot, ((matrix, dirty), resident))| {
                (*dirty && resident.is_some()).then(|| {
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
            msaa_depth,
            scene_view,
            depth_view,
            haze_view,
            haze_history,
            output_view,
            finish,
            bytes_per_row,
        ) = {
            let t = self.targets(t_width, t_height, haze_size, destination);
            let presentation = &t.presentations[slot];
            let finish = match presentation {
                PresentationTarget::Staged {
                    output, readback, ..
                } => Finish::Copy(output.clone(), readback.clone()),
                #[cfg(target_os = "macos")]
                PresentationTarget::Shared(shared) => Finish::Share(shared.buffer()),
            };
            (
                t.msaa_color.clone(),
                t.msaa_depth.clone(),
                t.scene.clone(),
                t.depth.clone(),
                t.haze.clone(),
                t.haze_history.clone(),
                presentation.view().clone(),
                finish,
                t.bytes_per_row,
            )
        };
        let targets_done = Instant::now();
        let haze_density = frame
            .haze_density
            .is_finite()
            .then_some(frame.haze_density.clamp(0.0, 4.0))
            .unwrap_or(0.0);
        let haze_time = frame.time.is_finite().then_some(frame.time).unwrap_or(0.0);
        let history_key = haze_history_key(frame, width, height, haze_size, haze_density);
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
                            let draw = &frame.draws[index];
                            let (local, radius) = mesh_bounds[draw.mesh];
                            // A model matrix may scale non-uniformly; the
                            // largest axis is the one the sphere has to survive.
                            let scale = draw
                                .model
                                .to_scale_rotation_translation()
                                .0
                                .abs()
                                .max_element();
                            crate::light_index::cone_reaches_sphere(
                                cone.position,
                                direction,
                                cone.range,
                                cone.cos_field,
                                draw.model.transform_point3(local),
                                radius * scale,
                            )
                        })
                        .collect()
                })
                .collect();
            let redrawn_maps = shadow_casters
                .iter()
                .zip(&fixture_shadow_bgs)
                .filter(|(_, prepared)| prepared.is_some())
                .count();
            self.shadow_stats = ShadowStats {
                redrawn_maps,
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
            let has_fixture_shadow_pass = fixture_shadow_count > 0
                && opaque > 0
                && fixture_shadow_dirty.iter().any(|dirty| *dirty);
            let draw_range =
                |pass: &mut wgpu::RenderPass, range: &[usize], include_fixture_models: bool| {
                    pass.set_vertex_buffer(0, vertex_buf.slice(..));
                    pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    let mut bound: Option<usize> = None;
                    pass.set_bind_group(1, &self.gpu.white_material, &[]);
                    for &i in range {
                        let draw = &frame.draws[i];
                        if !include_fixture_models
                            && matches!(
                                &draw.editor_object,
                                Some(crate::frame::EditorObject::Fixture(_))
                            )
                        {
                            continue;
                        }
                        if bound != Some(i) {
                            bound = Some(i);
                            pass.set_bind_group(1, &materials[i], &[]);
                        }
                        let (first, last, base) = ranges[draw.mesh];
                        pass.draw_indexed(first..last, base, i as u32..i as u32 + 1);
                    }
                };

            if has_fixture_shadow_pass {
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
                        timestamp_writes: claim_start_timestamp(&mut pending_start),
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
                for (index, key) in fixture_shadow_keys.into_iter().enumerate() {
                    self.fixture_shadow_cache[index] = Some(key);
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
                        timestamp_writes: claim_start_timestamp(&mut pending_start),
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.gpu.shadow_pipeline);
                    pass.set_bind_group(0, &shadow_bgs[cascade].1, &[]);
                    pass.set_bind_group(2, &environment_bg, &[]);
                    pass.set_bind_group(3, &cluster_bg, &[]);
                    draw_range(&mut pass, &all_opaque, true);
                }
            }

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("depth-prepass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(depth_attachment(&depth_view)),
                    timestamp_writes: claim_start_timestamp(&mut pending_start),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.depth_pipeline);
                pass.set_bind_group(0, &unlit_bg, &[]);
                pass.set_bind_group(2, &environment_bg, &[]);
                pass.set_bind_group(3, &cluster_bg, &[]);
                draw_range(&mut pass, &all_opaque, true);
            }

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("scene"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &msaa_color,
                        resolve_target: Some(&scene_view),
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(frame.clear_color.x),
                                g: f64::from(frame.clear_color.y),
                                b: f64::from(frame.clear_color.z),
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(depth_attachment_transient(&msaa_depth)),
                    timestamp_writes: profile_resources.as_ref().map(|(queries, ..)| {
                        wgpu::RenderPassTimestampWrites {
                            query_set: queries,
                            beginning_of_pass_write_index: None,
                            end_of_pass_write_index: Some(1),
                        }
                    }),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.scene_pipeline);
                pass.set_bind_group(0, &lit_bg, &[]);
                pass.set_bind_group(2, &environment_bg, &[]);
                pass.set_bind_group(3, &cluster_bg, &[]);
                draw_range(&mut pass, &all_opaque, true);
                if frame.grid_draws > 0 {
                    pass.set_pipeline(&self.gpu.grid_pipeline);
                    draw_range(&mut pass, &transparent, true);
                }
            }
        }

        // --- haze ------------------------------------------------------------
        // One measured frame in sixteen: the pass costs a few hundred
        // microseconds of GPU at 1080p, and run every frame it would tax the
        // very gpu_total numbers the profiler is recording.
        if measure {
            self.fragment_count_frame = self.fragment_count_frame.wrapping_add(1);
        }
        if measure && self.fragment_count_frame % 16 == 1 {
            self.light_index.record_fragment_count(
                &self.gpu.light_index_pipelines,
                &self.gpu.device,
                &mut encoder,
                &depth_view,
                CAMERA_NEAR,
                CAMERA_FAR,
                [width, height],
            );
        }
        // The light index was built (and its SoA uploaded) before the scene
        // passes; the haze passes bind the same frame's index as group 1.
        let inv_view_proj = view_proj.inverse();
        let subframes = subframes.max(1);
        let weight = 1.0 / subframes as f32;
        for k in 0..subframes {
            let uniform = HazeUniform {
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
                    0.0,
                    0.0,
                ],
                depth: [
                    CAMERA_NEAR,
                    CAMERA_FAR,
                    haze_density * Transport::EXTINCTION,
                    0.0,
                ],
                shadow: [
                    fixture_shadow_count as f32,
                    1.0 / FIXTURE_SHADOW_SIZE as f32,
                    0.0,
                    0.0,
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
                    ],
                });
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
                // The haze region's end lands on the temporal resolve when one
                // runs, else on the last accumulation pass.
                timestamp_writes: profile_resources.as_ref().and_then(|(queries, ..)| {
                    (!temporal && k + 1 == subframes).then_some(wgpu::RenderPassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: Some(2),
                    })
                }),
                ..Default::default()
            });
            pass.set_pipeline(&self.gpu.haze_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_bind_group(1, &light_index_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        let composite_haze = if temporal {
            let read = self.haze_history_index;
            let write = 1 - read;
            let temporal_uniform = TemporalUniform {
                // Reject history when the represented surface moves by more
                // than 25 cm in linear view space.
                params: [0.82, f32::from(u8::from(history_valid)), 0.25, 0.0],
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
                timestamp_writes: profile_resources.as_ref().map(|(queries, ..)| {
                    wgpu::RenderPassTimestampWrites {
                        query_set: queries,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: Some(2),
                    }
                }),
                ..Default::default()
            });
            pass.set_pipeline(&self.gpu.temporal_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
            drop(pass);
            self.haze_history_index = write;
            self.haze_history_valid = true;
            haze_history[write].clone()
        } else {
            haze_view.clone()
        };

        // --- composite + readback --------------------------------------------
        let composite_uniform = CompositeUniform {
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
                CAMERA_FAR,
                haze_density * Transport::EXTINCTION,
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
                    binding(2, wgpu::BindingResource::TextureView(&composite_haze)),
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
                    timestamp_writes: profile_resources.as_ref().map(|(queries, ..)| {
                        wgpu::RenderPassTimestampWrites {
                            query_set: queries,
                            beginning_of_pass_write_index: None,
                            end_of_pass_write_index: Some(3),
                        }
                    }),
                    ..Default::default()
                });
                pass.set_pipeline(&self.gpu.composite_pipelines[channels.index()]);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.set_bind_group(1, &environment_bg, &[]);
                pass.draw(0..3, 0..1);
            }

            // Editor affordances are display UI, not scene radiance. Drawing
            // into the final sRGB target after AgX makes authored colours
            // independent of stage lighting and exposure. Cages load the
            // full-resolution reverse-Z prepass depth; free gizmos use Always.
            if !frame.overlays.is_empty() {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("editor-overlays"),
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
                encoder.resolve_query_set(queries, 0..QUERY_COUNT, resolve, 0);
                encoder.copy_buffer_to_buffer(
                    resolve,
                    0,
                    profile_readback,
                    0,
                    u64::from(QUERY_COUNT) * 8,
                );
            }
        }

        self.staging
            .borrow_mut()
            .finish_and_recall_on_submit(&encoder);
        self.gpu.queue.submit([encoder.finish()]);
        let cpu_encode_submit = started.elapsed();
        // `clusters` is the wall span, not `cpu_cluster` — that one reports zero
        // on a cache hit by design, which is the right answer for "what did the
        // rebuild cost" and the wrong one here, where the phases have to account
        // for every microsecond between entry and submit.
        let cpu = CpuSpans {
            prepare: cluster_started - started,
            clusters: clusters_done - cluster_started,
            upload: targets_started - clusters_done,
            targets: targets_done - targets_started,
            encode: cpu_encode_submit - (targets_done - started),
            total: cpu_encode_submit,
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
            // has to say when the surface is safe to sample. This is the only
            // fence between the two devices.
            #[cfg(target_os = "macos")]
            Finish::Share(buffer) => {
                let (done_tx, done) = mpsc::sync_channel(1);
                // Stamped in the callback, not where it is noticed: the whole
                // question is how much of `draw_time` is the GPU finishing and
                // how much is the worker getting round to looking.
                self.gpu.queue.on_submitted_work_done(move || {
                    let _ = done_tx.send(Instant::now());
                });
                Completion::Shared {
                    buffer,
                    done,
                    finished: false,
                }
            }
        };
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

    fn scene_bind_group(
        &self,
        globals: &wgpu::Buffer,
        instances: &wgpu::Buffer,
        point_lights: &wgpu::Buffer,
        shadows: bool,
        hard_shadows: bool,
    ) -> wgpu::BindGroup {
        // The shadow map is a render target during the shadow pass, so the
        // passes that write depth bind a 1x1 placeholder instead.
        let map = if shadows {
            &self.shadow_map
        } else {
            &self.gpu.dummy_shadow
        };
        self.gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scene"),
                layout: &self.gpu.scene_layout,
                entries: &[
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
                ],
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
        Camera, DirectionalLight, Draw, FixtureCone, Frame, MaterialTextures, MeshData,
    };
    use crate::light_index::cone_reaches_sphere;
    use crate::overlay::{Overlay, OverlayDepth};
    use crate::scene_desc::DebugView;

    use super::{
        cascade_matrices, downsample, Channels, CompositeUniform, FixtureShadowMatrix, Globals,
        HazeUniform, LightCore, LightRest, Renderer, SurfaceClusterUniform, TextureEncoding,
        CAMERA_FAR, CAMERA_NEAR, CASCADE_COUNT, SHADOW_SIZE,
    };

    #[test]
    fn volumetric_cpu_layouts_match_wgsl_storage_and_uniform_strides() {
        assert_eq!(std::mem::size_of::<Globals>(), 368);
        assert_eq!(std::mem::size_of::<HazeUniform>(), 176);
        assert_eq!(std::mem::size_of::<CompositeUniform>(), 96);
        assert_eq!(std::mem::size_of::<LightCore>(), 16);
        assert_eq!(std::mem::size_of::<LightRest>(), 64);
        assert_eq!(std::mem::size_of::<FixtureShadowMatrix>(), 80);
        assert_eq!(std::mem::size_of::<SurfaceClusterUniform>(), 32);
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
            grid_draws: 0,
            gizmo_pivot: None,
            overlays: Vec::new(),
            point_lights: Vec::new(),
            fixture_cones,
            fixture_surface_lighting: true,
            beam_proxy: false,
            fixture_shadows: true,
            cluster_debug: false,
            clear_color: Vec3::ZERO,
            ambient: Vec3::splat(0.002),
            environment: None,
            directional: None,
            haze_density: 0.0,
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
            grid_draws: 0,
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
            fixture_surface_lighting: false,
            beam_proxy: false,
            fixture_shadows: true,
            cluster_debug: false,
            environment: None,
            clear_color: scene_radiance,
            ambient: scene_radiance,
            directional: Some(DirectionalLight {
                direction: Vec3::new(0.0, -1.0, 1.0).normalize(),
                radiance: scene_radiance,
                shadow_eye: Vec3::new(0.0, -4.0, 5.0),
                shadows: false,
                shadow_softness: 1.0,
            }),
            haze_density: 0.0,
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

    /// Independent conservative oracle: numerical samples can prove a ray is
    /// inside a finite cone, but never claim a miss. Every proven hit must be
    /// present in the CPU-generated tile list.
    fn sampled_ray_hits_cone(eye: Vec3, ray: Vec3, light: &FixtureCone) -> bool {
        let oc = eye - light.position;
        let b = oc.dot(ray);
        let disc = b * b - (oc.length_squared() - light.range * light.range);
        if disc <= 0.0 {
            return false;
        }
        let root = disc.sqrt();
        let start = (-b - root).max(0.0);
        let end = -b + root;
        if end <= start {
            return false;
        }
        (0..=32).any(|sample| {
            let t = start + (end - start) * sample as f32 / 32.0;
            let q = oc + ray * t;
            let axial = q.dot(light.direction);
            axial > 0.0 && axial * axial >= light.cos_field * light.cos_field * q.length_squared()
        })
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
            #[cfg(target_os = "macos")]
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
            #[cfg(target_os = "macos")]
            Self::Shared { buffer, .. } => Ok(Presented::Shared(crate::viewport::Surface::new(
                buffer.clone(),
            ))),
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
        let deadline = Instant::now() + DEADLINE;
        loop {
            device.poll(wgpu::PollType::Poll)?;
            if let Some(frame) = self.try_complete()? {
                return Ok(frame);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "the GPU did not return a frame within {DEADLINE:?}"
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
            let mut timestamps = [0_u64; QUERY_COUNT as usize];
            for (timestamp, bytes) in timestamps.iter_mut().zip(view.chunks_exact(8)) {
                *timestamp =
                    u64::from_ne_bytes(bytes.try_into().expect("timestamp is eight bytes"));
            }
            drop(view);
            profile.readback.unmap();
            // Consecutive end samples partition the frame, per `QUERY_COUNT`.
            // A zero haze end means haze did not run; a whole-frame zero means
            // the driver dropped the samples.
            let [start, scene_end, haze_end, composite_end, index0, index1] = timestamps;
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
                && index1 >= index0;
            if !timestamps_valid {
                anyhow::ensure!(
                    !profile.strict_timestamps,
                    "inconsistent GPU pass timestamps: {timestamps:?}"
                );
                None
            } else {
                let milliseconds = f64::from(profile.timestamp_period_ns) / 1_000_000.0;
                let span = |begin: u64, end: u64| end.saturating_sub(begin) as f64 * milliseconds;
                Some(FrameTimings {
                    gpu_total_ms: span(start, composite_end),
                    gpu_volumetric_ms: if haze_ran {
                        span(scene_end, haze_end)
                    } else {
                        0.0
                    },
                    gpu_scene_ms: span(start, scene_end),
                    gpu_composite_ms: span(scene_end.max(haze_end), composite_end),
                    gpu_index_ms: span(index0, index1),
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
) -> HazeHistoryKey {
    let mut topology = 0xcbf2_9ce4_8422_2325u64;
    let mut push = |value: u32| {
        topology = (topology ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    };
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
    }
}

fn shader(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
    })
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

fn comparison_sampler_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
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
