//! What a renderer reports about its own work: upload counts, adapter
//! identity, GPU and CPU frame timings, and fixture-shadow draw counts.

use std::time::Duration;

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
    pub(super) fn of(adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
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
    /// merging every caster into one index buffer is the next step.
    pub mesh_draws: usize,
}
