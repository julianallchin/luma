//! [`Renderer`]'s construction and its public surface: the render entry
//! points, the live submission API, and the diagnostics it reports.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::viewport::DisplayRange;

use crate::atmosphere::AtmosphereCache;
use crate::environment::EnvironmentCache;
use crate::frame::Frame;
use crate::haze_compact::CompactStats;
use crate::light_index::{LightIndex, LightIndexStats};
use crate::shadow::{fixture_shadow_texture_array, FIXTURE_SHADOW_SIZE, MAX_FIXTURE_SHADOWS};

use super::readback::{PendingFrame, ProfilerResources, ProfilerSlot};
use super::stats::{FrameTimings, ShadowStats, UploadStats};
use super::targets::shadow_texture_array;
use super::{Channels, Destination, Gpu, Renderer, CASCADE_COUNT, QUERY_COUNT, SHADOW_SIZE};

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
                    size: query_capacity as u64 * 8 + crate::post::EXPOSURE_READBACK,
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
        let bounce = crate::bounce::Bounce::new(&gpu.device);
        let haze_glow = crate::haze_glow::Glow::new(&gpu.device, &gpu.queue);
        Self {
            gpu,
            fog_blocks_valid: false,
            environment: EnvironmentCache::default(),
            atmosphere: AtmosphereCache::default(),
            sun_shafts: crate::sun_shafts::Cache::default(),
            haze_glow,
            bounce,
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
            visibility_reference: false,
            surface_depth_cull: true,
            reference: false,
            upload_stats: UploadStats {
                geometry: 0,
                textures: 0,
                environments: 0,
            },
            targets: None,
            target_memory: super::target_memory::TargetMemory::new(),
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
    pub(super) fn forget_haze_history(&mut self) {
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

    /// Whether the bounced light of the stage last shown is still being
    /// baked: until it is, surfaces show the last stage's bounce, or none.
    #[must_use]
    pub fn bounce_pending(&self) -> bool {
        self.bounce.pending()
    }

    /// Let the bounced light follow a live view: take the stage's bake
    /// when it is done, without waiting, and carry the light a bounce a
    /// frame. Off, as a renderer starts, every frame stands on its own, as a
    /// still or a film frame must.
    pub fn set_live_bounce(&mut self, live: bool) {
        self.bounce.set_live_view(live);
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
}
