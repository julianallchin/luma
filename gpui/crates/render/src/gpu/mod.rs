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
//!
//! This file holds the two owners, [`Gpu`] (the device and everything compiled
//! against it) and [`Renderer`] (one scene's GPU state). The rest is split by
//! responsibility: pipeline construction (`pipelines`, `scene_pipelines`,
//! `wgsl`, `descriptors`), per-renderer resources (`targets`, `buffers`,
//! `materials`, `haze_cache`), the CPU mirrors of shader structs (`uniforms`),
//! the camera and sun cascades (`camera`), the frame's pass chain (`encode`),
//! the public entry points (`renderer`), and what comes back from a frame
//! (`readback`, `stats`).

mod buffers;
mod camera;
mod descriptors;
mod encode;
mod haze_cache;
mod materials;
mod pipelines;
mod readback;
mod renderer;
mod scene_pipelines;
mod stats;
mod targets;
#[cfg(test)]
mod tests;
mod uniforms;
mod wgsl;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glam::Vec3;

use crate::viewport::DisplayRange;

use crate::atmosphere::{AtmosphereCache, AtmospherePipelines};
use crate::environment::{EnvironmentCache, EnvironmentPipelines};
use crate::haze_field::HazeField;
use crate::light_index::{LightIndex, LightIndexPipelines};
use crate::shadow::ShadowCacheKey;

use buffers::{GrowableStorage, ResidentGeometry};
use haze_cache::IntervalCachePools;
use materials::{MaterialDefaults, MaterialKey, TextureKey};
use readback::ProfilerResources;
use scene_pipelines::{ScenePipelineSource, ScenePipelines};
use targets::Targets;
use uniforms::FixtureShadowMatrix;

pub(crate) use buffers::local_bounding_sphere;
pub(crate) use camera::{camera_matrices, cascade_fits, sun_casters};
pub(crate) use descriptors::{binding, rw_storage_entry, uniform_entry};
pub(crate) use readback::PendingFrame;
pub use stats::{CpuSpans, FrameTimings, RendererProfile, ShadowStats, UploadStats};
pub(crate) use wgsl::{shader, specialize_wgsl_overrides};

/// Three bounded layers cover the part of a venue in which directional
/// shadows remain useful. 2048² per layer costs 48 MiB in `Depth32Float`, versus
/// an unbounded camera-sized allocation or 192 MiB for three legacy 4096 maps.
pub(crate) const SHADOW_SIZE: u32 = 2048;
pub(crate) const CASCADE_COUNT: usize = 3;
pub(crate) const CASCADE_SPLITS: [f32; CASCADE_COUNT] = [12.0, 45.0, 180.0];
pub(crate) const CASCADE_BLEND: f32 = 0.1;

/// The scene-linear target the scene and composite passes write.
pub(crate) const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
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
const QUERY_COUNT: u32 = crate::pass_profile::Base::COUNT;
pub(crate) const MSAA_SAMPLES: u32 = 4;
pub(crate) const CAMERA_NEAR: f32 = 0.1;
const CAMERA_FAR: f32 = 2000.0;

/// Transport constants the look was dialled in on (spec §3.1). They are baked:
/// the keyboard dials that produced them do not port.
pub(crate) struct Transport;
impl Transport {
    /// The photometric scale of every cone (`photometry.rs`).
    const BEAM_GAIN: f32 = crate::photometry::BEAM_GAIN;
    pub(crate) const WHITE_LEAK: f32 = 0.03;
    pub(crate) const PHASE_G: f32 = 0.6;
    const NEAR_CLAMP: f32 = 0.06;
    /// Extinction per unit haze density, in 1/metres. One σ serves the whole
    /// transport: the haze pass integrates in-scatter against it and the
    /// composite attenuates the scene by it — if they disagree, surfaces and
    /// the medium disagree about how much fog sits in front of them and
    /// geometry silhouettes through the fog.
    pub(crate) const EXTINCTION: f32 = 0.06;
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
    /// The light the haze scatters (`haze_glow.rs`).
    haze_glow: crate::haze_glow::Glow,
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
    laser: crate::laser::Pipelines,
    haze_glow: crate::haze_glow::Pipelines,
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
    /// What the single-sample set is built from, on the first frame below high quality.
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
    /// Low quality's closed-form beams, in place of every march.
    haze_analytic_pipeline: wgpu::ComputePipeline,
    haze_analytic_layout: wgpu::BindGroupLayout,
    /// log2 of the payload hash table's entry count.
    interval_cache_table_bits: u32,
    /// Residual compaction of the cached kernel (`haze_compact.rs`).
    haze_compact_pipelines: crate::haze_compact::Pipelines,
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
    /// Exposure, tone curve and glare (`post.rs`).
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
        Ok(Self::on(
            context.device.clone(),
            context.queue.clone(),
            profile,
            context.lost.clone(),
            context.adopted,
            started,
        ))
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
