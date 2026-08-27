use crate::metal_atlas::MetalAtlas;
use anyhow::{Context as _, Result};
use block::ConcreteBlock;
use cocoa::{
    base::{NO, YES},
    foundation::{NSSize, NSUInteger},
    quartzcore::AutoresizingMask,
};
use gpui::{
    AtlasTextureId, BackdropBlur, Background, Bounds, ContentFilter, ContentMask, DevicePixels,
    DrawOrder, PaintSurface, Path, Point, PrimitiveBatch, ScaledPixels, Scene, Size, point, size,
};
#[cfg(any(test, feature = "test-support"))]
use image::RgbaImage;

use core_foundation::base::TCFType;
use core_video::{
    metal_texture::CVMetalTextureGetTexture, metal_texture_cache::CVMetalTextureCache,
    pixel_buffer::{kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange},
};
use foreign_types::{ForeignType, ForeignTypeRef};
use metal::{
    CAMetalLayer, CommandQueue, MTLGPUFamily, MTLPixelFormat, MTLResourceOptions, NSRange,
};
use objc::{self, class, msg_send, sel, sel_impl};
use parking_lot::Mutex;

use std::{cell::Cell, ffi::c_void, mem, mem::MaybeUninit, ops::Range, ptr, slice, sync::Arc};


/// Env-gated GPU timing for content-filter passes (`LUMA_FILTER_PROFILE=1`).
///
/// **An instrument, not a feature.** The automation harness reports `drawMs`,
/// which is the scene build and stops at the renderer's door — so what a
/// filtered layer costs (an offscreen render of the whole subtree, then a
/// gaussian, per filter per frame) is invisible from outside. Metal already
/// carries per-command-buffer GPU timestamps and the headless path already
/// waits for completion, so reading them changes no timing.
///
/// Thread-local rather than a renderer field so the instrument's whole
/// footprint is one module plus two call sites, removable in one edit.
mod filter_profile {
    use std::cell::RefCell;
    use std::sync::OnceLock;

    use foreign_types::ForeignTypeRef as _;
    use objc::{msg_send, sel, sel_impl};

    pub fn enabled() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("LUMA_FILTER_PROFILE").is_some())
    }

    /// One filtered layer's offscreen pass, kept until the frame is complete.
    pub struct Pass {
        pub buffer: metal::CommandBuffer,
        pub width: u64,
        pub height: u64,
        pub sigma: f32,
    }

    thread_local! {
        static PASSES: RefCell<Vec<Pass>> = const { RefCell::new(Vec::new()) };
    }

    pub fn record(pass: Pass) {
        PASSES.with(|passes| passes.borrow_mut().push(pass));
    }

    /// GPU milliseconds for a *completed* command buffer. Reading these before
    /// completion yields zeros, which is why this is only called after the
    /// frame has been waited on.
    fn gpu_ms(buffer: &metal::CommandBufferRef) -> f64 {
        unsafe {
            let object = buffer.as_ptr() as *mut objc::runtime::Object;
            let start: f64 = msg_send![object, GPUStartTime];
            let end: f64 = msg_send![object, GPUEndTime];
            (end - start) * 1000.0
        }
    }

    /// Print what this frame's filters cost, and clear the accumulator.
    pub fn report(frame: &metal::CommandBufferRef) {
        let passes: Vec<Pass> = PASSES.with(|passes| passes.borrow_mut().drain(..).collect());
        let subtree: f64 = passes.iter().map(|pass| gpu_ms(&pass.buffer)).sum();
        let geometry = passes
            .iter()
            .map(|pass| format!("{}x{}@sigma{:.0}", pass.width, pass.height, pass.sigma))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "FILTER_PROFILE filters={} frameGpuMs={:.3} subtreeGpuMs={:.3} {}",
            passes.len(),
            gpu_ms(frame),
            subtree,
            geometry
        );
    }
}

#[link(name = "MetalPerformanceShaders", kind = "framework")]
unsafe extern "C" {}

// Exported to metal
pub(crate) type PointF = gpui::Point<f32>;

#[cfg(not(feature = "runtime_shaders"))]
const SHADERS_METALLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders.metallib"));
#[cfg(feature = "runtime_shaders")]
const SHADERS_SOURCE_FILE: &str = include_str!(concat!(env!("OUT_DIR"), "/stitched_shaders.metal"));
// Use 4x MSAA, all devices support it.
// https://developer.apple.com/documentation/metal/mtldevice/1433355-supportstexturesamplecount
const PATH_SAMPLE_COUNT: u32 = 4;

/// Frames without a backdrop blur before its GPU scratch pair is released.
/// The short grace period avoids churn while a popover animates closed but
/// prevents one dialog from pinning its high-water allocation indefinitely.
const BACKDROP_SCRATCH_RELEASE_AFTER_FRAMES: u32 = 30;
/// Small size changes reuse an allocation while still placing a hard cap at
/// the drawable dimensions.
const BACKDROP_SCRATCH_QUANTUM: u64 = 256;
const BACKDROP_KERNEL_CACHE_CAPACITY: usize = 4;
const BACKDROP_KERNEL_SIGMA_QUANTUM: f32 = 0.5;
const CONTENT_FILTER_KERNEL_CACHE_CAPACITY: usize = 12;
const CONTENT_FILTER_KERNEL_SIGMA_QUANTUM: f32 = 2.0;
/// Metal requires the offset a buffer is bound at to be 256-byte aligned.
const INSTANCE_BUFFER_ALIGNMENT: usize = 256;
const MAX_INSTANCE_BUFFER_SIZE: usize = 256 * 1024 * 1024;

fn backdrop_scratch_extent(needed: u64, drawable: u64) -> u64 {
    needed
        .div_ceil(BACKDROP_SCRATCH_QUANTUM)
        .saturating_mul(BACKDROP_SCRATCH_QUANTUM)
        .min(drawable)
}

#[derive(Default)]
struct ScratchLease {
    idle_frames: u32,
}

impl ScratchLease {
    /// Returns true exactly when the idle grace has elapsed and resources
    /// should be released. Use resets the lease without touching another
    /// feature's cache.
    fn note_frame(&mut self, used: bool) -> bool {
        if used {
            self.idle_frames = 0;
            false
        } else {
            self.idle_frames = self.idle_frames.saturating_add(1);
            self.idle_frames >= BACKDROP_SCRATCH_RELEASE_AFTER_FRAMES
        }
    }
}

/// A small LRU of MPS kernels. Dialog hosts use several stable backdrop radii,
/// while content morphs sweep through a short range of animated radii; a
/// single-entry cache turns both cases into release/allocation churn.
struct GaussianKernelCache {
    entries: Vec<(f32, *mut objc::runtime::Object)>,
    capacity: usize,
    sigma_quantum: f32,
    allocations: u64,
    releases: u64,
}

impl GaussianKernelCache {
    fn new(capacity: usize, sigma_quantum: f32) -> Self {
        assert!(capacity > 0);
        assert!(sigma_quantum > 0.0);
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            sigma_quantum,
            allocations: 0,
            releases: 0,
        }
    }

    fn quantize(&self, sigma: f32) -> f32 {
        ((sigma.max(1.0) / self.sigma_quantum).round() * self.sigma_quantum).max(1.0)
    }

    fn kernel(
        &mut self,
        device: &metal::DeviceRef,
        requested_sigma: f32,
    ) -> *mut objc::runtime::Object {
        let sigma = self.quantize(requested_sigma);
        if let Some(index) = self
            .entries
            .iter()
            .position(|(cached, _)| (*cached - sigma).abs() < 0.01)
        {
            let entry = self.entries.remove(index);
            let kernel = entry.1;
            self.entries.push(entry);
            return kernel;
        }

        if self.entries.len() == self.capacity {
            let (_, kernel) = self.entries.remove(0);
            release_mps_kernel(kernel);
            self.releases += 1;
        }
        let kernel = new_gaussian_kernel(device, sigma);
        self.entries.push((sigma, kernel));
        self.allocations += 1;
        kernel
    }

    fn clear(&mut self) {
        for (_, kernel) in self.entries.drain(..) {
            release_mps_kernel(kernel);
            self.releases += 1;
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub type Context = Arc<Mutex<InstanceBufferPool>>;
pub type Renderer = MetalRenderer;

pub unsafe fn new_renderer(
    context: self::Context,
    _native_window: *mut c_void,
    _native_view: *mut c_void,
    _bounds: gpui::Size<f32>,
    transparent: bool,
) -> Renderer {
    MetalRenderer::new(context, transparent)
}

pub struct InstanceBufferPool {
    buffer_size: usize,
    buffers: Vec<metal::Buffer>,
}

impl Default for InstanceBufferPool {
    fn default() -> Self {
        Self {
            buffer_size: 2 * 1024 * 1024,
            buffers: Vec::new(),
        }
    }
}

pub(crate) struct InstanceBuffer {
    metal_buffer: metal::Buffer,
    size: usize,
}

impl InstanceBufferPool {
    pub(crate) fn reset(&mut self, buffer_size: usize) {
        self.buffer_size = buffer_size;
        self.buffers.clear();
    }

    pub(crate) fn acquire(
        &mut self,
        device: &metal::Device,
        unified_memory: bool,
    ) -> InstanceBuffer {
        let buffer = self.buffers.pop().unwrap_or_else(|| {
            let options = if unified_memory {
                MTLResourceOptions::StorageModeShared
                    // Buffers are write only which can benefit from the combined cache
                    // https://developer.apple.com/documentation/metal/mtlresourceoptions/cpucachemodewritecombined
                    | MTLResourceOptions::CPUCacheModeWriteCombined
            } else {
                MTLResourceOptions::StorageModeManaged
            };

            device.new_buffer(self.buffer_size as u64, options)
        });
        InstanceBuffer {
            metal_buffer: buffer,
            size: self.buffer_size,
        }
    }

    pub(crate) fn release(&mut self, buffer: InstanceBuffer) {
        if buffer.size == self.buffer_size {
            self.buffers.push(buffer.metal_buffer)
        }
    }
}

pub struct MetalRenderer {
    device: metal::Device,
    layer: Option<metal::MetalLayer>,
    is_apple_gpu: bool,
    is_unified_memory: bool,
    presents_with_transaction: bool,
    /// For headless rendering, tracks whether output should be opaque
    opaque: bool,
    command_queue: CommandQueue,
    paths_rasterization_pipeline_state: metal::RenderPipelineState,
    path_sprites_pipeline_state: metal::RenderPipelineState,
    shadows_pipeline_state: metal::RenderPipelineState,
    backdrop_blur_pipeline_state: metal::RenderPipelineState,
    content_filter_pipeline_state: metal::RenderPipelineState,
    backdrop_scratch: Option<metal::Texture>,
    backdrop_blurred: Option<metal::Texture>,
    backdrop_kernels: GaussianKernelCache,
    backdrop_lease: ScratchLease,
    backdrop_high_water_bytes: u64,
    /// One pair per filtered subtree in a frame. Pairs cannot be shared
    /// within a command buffer: a later child render would overwrite a source
    /// that an earlier, not-yet-committed MPS encode still references.
    content_filter_scratch: Vec<(metal::Texture, metal::Texture)>,
    content_filter_kernels: GaussianKernelCache,
    content_filter_lease: ScratchLease,
    content_filter_high_water_bytes: u64,
    quads_pipeline_state: metal::RenderPipelineState,
    underlines_pipeline_state: metal::RenderPipelineState,
    monochrome_sprites_pipeline_state: metal::RenderPipelineState,
    polychrome_sprites_pipeline_state: metal::RenderPipelineState,
    surfaces_pipeline_state: metal::RenderPipelineState,
    /// Surfaces whose pixels are already RGB. `surfaces_pipeline_state` exists
    /// for camera and video frames, which arrive as planar YCbCr; a surface
    /// produced by another renderer on this machine arrives as BGRA and wants
    /// no colour conversion at all.
    bgra_surfaces_pipeline_state: metal::RenderPipelineState,
    unit_vertices: metal::Buffer,
    #[allow(clippy::arc_with_non_send_sync)]
    instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    sprite_atlas: Arc<MetalAtlas>,
    core_video_texture_cache: core_video::metal_texture_cache::CVMetalTextureCache,
    path_intermediate_texture: Option<metal::Texture>,
    path_intermediate_msaa_texture: Option<metal::Texture>,
    path_sample_count: u32,
    /// Offscreen render target reused across `render_scene` calls when
    /// rendering headlessly without reading pixels back.
    #[cfg(any(test, feature = "test-support"))]
    headless_render_target: Option<metal::Texture>,
}

#[repr(C)]
pub struct PathRasterizationVertex {
    pub xy_position: Point<ScaledPixels>,
    pub st_position: Point<f32>,
    pub color: Background,
    pub bounds: Bounds<ScaledPixels>,
}

impl MetalRenderer {
    /// Creates a new MetalRenderer with a CAMetalLayer for window-based rendering.
    pub fn new(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>, transparent: bool) -> Self {
        let device = Self::create_device();

        let layer = metal::MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        // Support direct-to-display rendering if the window is not transparent
        // https://developer.apple.com/documentation/metal/managing-your-game-window-for-metal-in-macos
        layer.set_opaque(!transparent);
        // LUMA LOCAL EDIT (upstream sets 3). This bounds how many window command
        // buffers can be executing at once, and each of them samples the
        // `IOSurface` that was current when it was encoded. Luma's renderer
        // draws into its own pool of those surfaces on a *different* Metal
        // queue, and withholds the most recent `luma_render::viewport::RESERVED`
        // of them from reuse — nothing else fences the two queues.
        //
        // The requirement is `RESERVED >= this + 1`, not equality: `next_drawable`
        // blocks in paint while the surface release happens in the next frame's
        // prepaint, so the guarantee in force at release time is the *previous*
        // frame's acquire. See the `RESERVED` doc comment, which works it out.
        //
        // Lowering this to 2 is worth doing on its own — it shrinks the set of
        // possibly-executing command buffers ahead of a released surface — but
        // it does not by itself close the race, and the matching `RESERVED = 3`
        // is what does. Raising this again without raising RESERVED reopens it.
        layer.set_maximum_drawable_count(2);
        // Allow texture reading for visual tests (captures screenshots without ScreenCaptureKit)
        #[cfg(any(test, feature = "test-support"))]
        layer.set_framebuffer_only(false);
        unsafe {
            let _: () = msg_send![&*layer, setAllowsNextDrawableTimeout: NO];
            let _: () = msg_send![&*layer, setNeedsDisplayOnBoundsChange: YES];
            let _: () = msg_send![
                &*layer,
                setAutoresizingMask: AutoresizingMask::WIDTH_SIZABLE
                    | AutoresizingMask::HEIGHT_SIZABLE
            ];
        }

        Self::new_internal(device, Some(layer), !transparent, instance_buffer_pool)
    }

    /// Creates a new headless MetalRenderer for offscreen rendering without a window.
    ///
    /// This renderer can render scenes to images without requiring a CAMetalLayer,
    /// window, or AppKit. Use `render_scene_to_image()` to render scenes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_headless(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>) -> Self {
        let device = Self::create_device();
        Self::new_internal(device, None, true, instance_buffer_pool)
    }

    fn create_device() -> metal::Device {
        // Prefer low‐power integrated GPUs on Intel Mac. On Apple
        // Silicon, there is only ever one GPU, so this is equivalent to
        // `metal::Device::system_default()`.
        if let Some(d) = metal::Device::all()
            .into_iter()
            .min_by_key(|d| (d.is_removable(), !d.is_low_power()))
        {
            d
        } else {
            // For some reason `all()` can return an empty list, see https://github.com/zed-industries/zed/issues/37689
            // In that case, we fall back to the system default device.
            log::error!(
                "Unable to enumerate Metal devices; attempting to use system default device"
            );
            metal::Device::system_default().unwrap_or_else(|| {
                log::error!("unable to access a compatible graphics device");
                std::process::exit(1);
            })
        }
    }

    fn new_internal(
        device: metal::Device,
        layer: Option<metal::MetalLayer>,
        opaque: bool,
        instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    ) -> Self {
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .new_library_with_source(&SHADERS_SOURCE_FILE, &metal::CompileOptions::new())
            .expect("error building metal library");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .new_library_with_data(SHADERS_METALLIB)
            .expect("error building metal library");

        fn to_float2_bits(point: PointF) -> u64 {
            let mut output = point.y.to_bits() as u64;
            output <<= 32;
            output |= point.x.to_bits() as u64;
            output
        }

        // Shared memory can be used only if CPU and GPU share the same memory space.
        // https://developer.apple.com/documentation/metal/setting-resource-storage-modes
        let is_unified_memory = device.has_unified_memory();
        // Apple GPU families support memoryless textures, which can significantly reduce
        // memory usage by keeping render targets in on-chip tile memory instead of
        // allocating backing store in system memory.
        // https://developer.apple.com/documentation/metal/mtlgpufamily
        let is_apple_gpu = device.supports_family(MTLGPUFamily::Apple1);

        let unit_vertices = [
            to_float2_bits(point(0., 0.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(1., 1.)),
        ];
        let unit_vertices = device.new_buffer_with_data(
            unit_vertices.as_ptr() as *const c_void,
            mem::size_of_val(&unit_vertices) as u64,
            if is_unified_memory {
                MTLResourceOptions::StorageModeShared
                    | MTLResourceOptions::CPUCacheModeWriteCombined
            } else {
                MTLResourceOptions::StorageModeManaged
            },
        );

        let paths_rasterization_pipeline_state = build_path_rasterization_pipeline_state(
            &device,
            &library,
            "paths_rasterization",
            "path_rasterization_vertex",
            "path_rasterization_fragment",
            MTLPixelFormat::BGRA8Unorm,
            PATH_SAMPLE_COUNT,
        );
        let path_sprites_pipeline_state = build_path_sprite_pipeline_state(
            &device,
            &library,
            "path_sprites",
            "path_sprite_vertex",
            "path_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let shadows_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "shadows",
            "shadow_vertex",
            "shadow_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let backdrop_blur_pipeline_state = build_pipeline_state_no_blend(
            &device,
            &library,
            "backdrop_blur",
            "backdrop_blur_vertex",
            "backdrop_blur_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let content_filter_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "content_filter",
            "content_filter_vertex",
            "content_filter_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let quads_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "quads",
            "quad_vertex",
            "quad_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let underlines_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "underlines",
            "underline_vertex",
            "underline_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let monochrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "monochrome_sprites",
            "monochrome_sprite_vertex",
            "monochrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let polychrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "polychrome_sprites",
            "polychrome_sprite_vertex",
            "polychrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let surfaces_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "surfaces",
            "surface_vertex",
            "surface_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let bgra_surfaces_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "bgra_surfaces",
            "surface_vertex",
            "bgra_surface_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );

        let command_queue = device.new_command_queue();
        let sprite_atlas = Arc::new(MetalAtlas::new(device.clone(), is_apple_gpu));
        let core_video_texture_cache =
            CVMetalTextureCache::new(None, device.clone(), None).unwrap();

        Self {
            device,
            layer,
            presents_with_transaction: false,
            is_apple_gpu,
            is_unified_memory,
            opaque,
            command_queue,
            paths_rasterization_pipeline_state,
            path_sprites_pipeline_state,
            shadows_pipeline_state,
            backdrop_blur_pipeline_state,
            content_filter_pipeline_state,
            backdrop_scratch: None,
            backdrop_blurred: None,
            backdrop_kernels: GaussianKernelCache::new(
                BACKDROP_KERNEL_CACHE_CAPACITY,
                BACKDROP_KERNEL_SIGMA_QUANTUM,
            ),
            backdrop_lease: ScratchLease::default(),
            backdrop_high_water_bytes: 0,
            content_filter_scratch: Vec::new(),
            content_filter_kernels: GaussianKernelCache::new(
                CONTENT_FILTER_KERNEL_CACHE_CAPACITY,
                CONTENT_FILTER_KERNEL_SIGMA_QUANTUM,
            ),
            content_filter_lease: ScratchLease::default(),
            content_filter_high_water_bytes: 0,
            quads_pipeline_state,
            underlines_pipeline_state,
            monochrome_sprites_pipeline_state,
            polychrome_sprites_pipeline_state,
            surfaces_pipeline_state,
            bgra_surfaces_pipeline_state,
            unit_vertices,
            instance_buffer_pool,
            sprite_atlas,
            core_video_texture_cache,
            path_intermediate_texture: None,
            path_intermediate_msaa_texture: None,
            path_sample_count: PATH_SAMPLE_COUNT,
            #[cfg(any(test, feature = "test-support"))]
            headless_render_target: None,
        }
    }

    pub fn layer(&self) -> Option<&metal::MetalLayerRef> {
        self.layer.as_ref().map(|l| l.as_ref())
    }

    pub fn layer_ptr(&self) -> *mut CAMetalLayer {
        self.layer
            .as_ref()
            .map(|l| l.as_ptr())
            .unwrap_or(ptr::null_mut())
    }

    pub fn sprite_atlas(&self) -> &Arc<MetalAtlas> {
        &self.sprite_atlas
    }

    pub fn set_presents_with_transaction(&mut self, presents_with_transaction: bool) {
        self.presents_with_transaction = presents_with_transaction;
        if let Some(layer) = &self.layer {
            layer.set_presents_with_transaction(presents_with_transaction);
        }
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        if let Some(layer) = &self.layer {
            let ns_size = NSSize {
                width: size.width.0 as f64,
                height: size.height.0 as f64,
            };
            unsafe {
                let _: () = msg_send![
                    layer.as_ref(),
                    setDrawableSize: ns_size
                ];
            }
        }
        self.update_path_intermediate_textures(size);
    }

    fn update_path_intermediate_textures(&mut self, size: Size<DevicePixels>) {
        // We are uncertain when this happens, but sometimes size can be 0 here. Most likely before
        // the layout pass on window creation. Zero-sized texture creation causes SIGABRT.
        // https://github.com/zed-industries/zed/issues/36229
        if size.width.0 <= 0 || size.height.0 <= 0 {
            self.path_intermediate_texture = None;
            self.path_intermediate_msaa_texture = None;
            return;
        }

        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        self.path_intermediate_texture = Some(self.device.new_texture(&texture_descriptor));

        if self.path_sample_count > 1 {
            // https://developer.apple.com/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus
            // Rendering MSAA textures are done in a single pass, so we can use memory-less storage on Apple Silicon
            let storage_mode = if self.is_apple_gpu {
                metal::MTLStorageMode::Memoryless
            } else {
                metal::MTLStorageMode::Private
            };

            let msaa_descriptor = texture_descriptor;
            msaa_descriptor.set_texture_type(metal::MTLTextureType::D2Multisample);
            msaa_descriptor.set_storage_mode(storage_mode);
            msaa_descriptor.set_sample_count(self.path_sample_count as _);
            self.path_intermediate_msaa_texture = Some(self.device.new_texture(&msaa_descriptor));
        } else {
            self.path_intermediate_msaa_texture = None;
        }
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        self.opaque = !transparent;
        if let Some(layer) = &self.layer {
            layer.set_opaque(!transparent);
        }
    }

    pub fn destroy(&self) {
        // nothing to do
    }

    pub fn draw(&mut self, scene: &Scene) {
        let layer = match &self.layer {
            Some(l) => l.clone(),
            None => {
                log::error!(
                    "draw() called on headless renderer - use render_scene_to_image() instead"
                );
                return;
            }
        };
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        let drawable = if let Some(drawable) = layer.next_drawable() {
            drawable
        } else {
            log::error!(
                "failed to retrieve next drawable, drawable size: {:?}",
                viewport_size
            );
            return;
        };

        let command_buffer = match self.render_frame(scene, drawable.texture(), viewport_size) {
            Ok(command_buffer) => command_buffer,
            Err(error) => {
                log::error!("failed to render: {error:#}");
                return;
            }
        };

        if self.presents_with_transaction {
            command_buffer.commit();
            command_buffer.wait_until_scheduled();
            drawable.present();
        } else {
            command_buffer.present_drawable(drawable);
            command_buffer.commit();
        }
    }

    fn render_frame(
        &mut self,
        scene: &Scene,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        self.render_frame_with_clear_alpha(
            scene,
            texture,
            viewport_size,
            if self.opaque { 1.0 } else { 0.0 },
            true,
            point(DevicePixels(0), DevicePixels(0)),
        )
    }

    fn render_frame_with_clear_alpha(
        &mut self,
        scene: &Scene,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
        clear_alpha: f64,
        manage_scratch_lifecycle: bool,
        target_origin: Point<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        let mut writer = InstanceBufferWriter::new(
            &self.device,
            &self.instance_buffer_pool,
            self.is_unified_memory,
        );
        let instance_bindings = write_instances(scene, &mut writer).with_context(|| {
            format!(
                "scene too large: {} paths, {} shadows, {} quads, {} underlines, {} mono, {} poly, {} surfaces",
                scene.paths.len(),
                scene.shadows.len(),
                scene.quads.len(),
                scene.underlines.len(),
                scene.monochrome_sprites.len(),
                scene.polychrome_sprites.len(),
                scene.surfaces.len(),
            )
        })?;
        let command_buffer = self.draw_primitives_to_texture(
            scene,
            &instance_bindings,
            &mut writer,
            texture,
            viewport_size,
            clear_alpha,
            manage_scratch_lifecycle,
            target_origin,
        )?;

        let instance_buffer_pool = self.instance_buffer_pool.clone();
        let instance_buffer = Cell::new(Some(writer.finish()));
        let block = ConcreteBlock::new(move |_| {
            if let Some(instance_buffer) = instance_buffer.take() {
                instance_buffer_pool.lock().release(instance_buffer);
            }
        });
        let block = block.copy();
        command_buffer.add_completed_handler(&block);

        Ok(command_buffer)
    }

    /// Renders the scene to a texture and returns the pixel data as an RGBA image.
    /// This does not present the frame to screen - useful for visual testing
    /// where we want to capture what would be rendered without displaying it.
    ///
    /// Note: This requires a layer-backed renderer. For headless rendering,
    /// use `render_scene_to_image()` instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_to_image(&mut self, scene: &Scene) -> Result<RgbaImage> {
        let layer = self
            .layer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("render_to_image requires a layer-backed renderer"))?;
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        let drawable = layer
            .next_drawable()
            .ok_or_else(|| anyhow::anyhow!("Failed to get drawable for render_to_image"))?;

        let command_buffer = self.render_frame(scene, drawable.texture(), viewport_size)?;

        // Commit and wait for completion without presenting
        command_buffer.commit();
        command_buffer.wait_until_completed();
        if filter_profile::enabled() {
            filter_profile::report(&command_buffer);
        }

        read_texture_to_image(drawable.texture())
    }

    /// Renders a scene to an image without requiring a window or CAMetalLayer.
    ///
    /// This is the primary method for headless rendering. It creates an offscreen
    /// texture, renders the scene to it, and returns the pixel data as an RGBA image.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<RgbaImage> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene_to_image: {:?}", size);
        }

        // Update path intermediate textures for this size
        self.update_path_intermediate_textures(size);

        // Create an offscreen texture as render target
        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Managed);
        let target_texture = self.device.new_texture(&texture_descriptor);

        let command_buffer = self.render_frame(scene, &target_texture, size)?;

        // On discrete GPUs (non-unified memory), Managed textures require an
        // explicit blit synchronize before the CPU can read back the rendered
        // data. Without this, get_bytes returns stale zeros.
        if !self.is_unified_memory {
            let blit = command_buffer.new_blit_command_encoder();
            blit.synchronize_resource(&target_texture);
            blit.end_encoding();
        }

        // Commit and wait for completion
        command_buffer.commit();
        command_buffer.wait_until_completed();
        if filter_profile::enabled() {
            filter_profile::report(&command_buffer);
        }

        read_texture_to_image(&target_texture)
    }

    /// Renders a scene to a reused offscreen texture without reading pixels
    /// back or blocking on GPU completion.
    ///
    /// This mirrors the CPU cost of presenting a frame to a window (scene
    /// encoding, instance buffer writes, command submission) and is used by
    /// headless benchmark rendering, where the produced pixels are never
    /// inspected.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene: {:?}", size);
        }

        self.update_path_intermediate_textures(size);

        let needs_new_target = self.headless_render_target.as_ref().is_none_or(|texture| {
            texture.width() != size.width.0 as u64 || texture.height() != size.height.0 as u64
        });
        if needs_new_target {
            let texture_descriptor = metal::TextureDescriptor::new();
            texture_descriptor.set_width(size.width.0 as u64);
            texture_descriptor.set_height(size.height.0 as u64);
            texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
            texture_descriptor.set_usage(
                metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead,
            );
            texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
            self.headless_render_target = Some(self.device.new_texture(&texture_descriptor));
        }
        let target_texture = self
            .headless_render_target
            .clone()
            .expect("just ensured the render target exists");

        let command_buffer = self.render_frame(scene, &target_texture, size)?;

        // Commit without waiting, mirroring presentation to a real window where
        // the CPU doesn't block on the GPU.
        command_buffer.commit();
        Ok(())
    }

    fn draw_primitives_to_texture(
        &mut self,
        scene: &Scene,
        instance_bindings: &InstanceBindings,
        writer: &mut InstanceBufferWriter,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
        clear_alpha: f64,
        manage_scratch_lifecycle: bool,
        target_origin: Point<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        let command_queue = self.command_queue.clone();
        let command_buffer = command_queue.new_command_buffer();

        if !manage_scratch_lifecycle {
            assert!(
                scene.backdrop_blurs.is_empty() && scene.content_filters.is_empty(),
                "filtered subtrees cannot contain backdrop or nested content filters"
            );
        }

        if manage_scratch_lifecycle {
            if self
                .backdrop_lease
                .note_frame(!scene.backdrop_blurs.is_empty())
            {
                self.release_backdrop_resources();
            }
            if self
                .content_filter_lease
                .note_frame(!scene.content_filters.is_empty())
            {
                self.release_content_filter_resources();
            }
        }

        let mut command_encoder = new_command_encoder_for_texture_at_origin(
            command_buffer,
            texture,
            viewport_size,
            target_origin,
            Some(metal::MTLClearColor::new(0., 0., 0., clear_alpha)),
        );

        let mut next_blur = 0;
        let mut next_filter = 0;
        for batch in scene.batches() {
            let batch_order = batch_first_order(scene, &batch);
            loop {
                let blur_order = scene
                    .backdrop_blurs
                    .get(next_blur)
                    .map_or(DrawOrder::MAX, |blur| blur.order);
                let filter_order = scene
                    .content_filters
                    .get(next_filter)
                    .map_or(DrawOrder::MAX, |filter| filter.order);
                if blur_order.min(filter_order) > batch_order {
                    break;
                }
                command_encoder.end_encoding();
                if filter_order <= blur_order {
                    command_encoder = self.encode_content_filter(
                        next_filter,
                        &scene.content_filters[next_filter],
                        command_buffer,
                        texture,
                        viewport_size,
                    )?;
                    next_filter += 1;
                } else {
                    command_encoder = self.encode_backdrop_blur(
                        next_blur,
                        scene.backdrop_blurs[next_blur],
                        instance_bindings,
                        command_buffer,
                        texture,
                        viewport_size,
                    );
                    next_blur += 1;
                }
            }
            match batch {
                PrimitiveBatch::Shadows(range) => {
                    self.draw_shadows(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::Quads(range) => {
                    self.draw_quads(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::Paths(range) => {
                    let paths = &scene.paths[range];
                    command_encoder.end_encoding();

                    let did_draw = self.draw_paths_to_intermediate(
                        paths,
                        writer,
                        viewport_size,
                        command_buffer,
                    )?;

                    command_encoder = new_command_encoder_for_texture_at_origin(
                        command_buffer,
                        texture,
                        viewport_size,
                        target_origin,
                        None,
                    );

                    if did_draw {
                        if let Err(error) = self.draw_paths_from_intermediate(
                            paths,
                            writer,
                            viewport_size,
                            command_encoder,
                        ) {
                            command_encoder.end_encoding();
                            return Err(error);
                        }
                    }
                }
                PrimitiveBatch::Underlines(range) => {
                    self.draw_underlines(range, instance_bindings, viewport_size, command_encoder)
                }
                PrimitiveBatch::MonochromeSprites { texture_id, range } => self
                    .draw_monochrome_sprites(
                        texture_id,
                        range,
                        instance_bindings,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::PolychromeSprites { texture_id, range } => self
                    .draw_polychrome_sprites(
                        texture_id,
                        range,
                        instance_bindings,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::Surfaces(range) => self.draw_surfaces(
                    &scene.surfaces[range.clone()],
                    range.start,
                    instance_bindings,
                    viewport_size,
                    command_encoder,
                ),
                PrimitiveBatch::SubpixelSprites { .. } => unreachable!(),
            }
        }

        while next_blur < scene.backdrop_blurs.len() || next_filter < scene.content_filters.len() {
            command_encoder.end_encoding();
            let blur_order = scene
                .backdrop_blurs
                .get(next_blur)
                .map_or(DrawOrder::MAX, |blur| blur.order);
            let filter_order = scene
                .content_filters
                .get(next_filter)
                .map_or(DrawOrder::MAX, |filter| filter.order);
            if filter_order <= blur_order {
                command_encoder = self.encode_content_filter(
                    next_filter,
                    &scene.content_filters[next_filter],
                    command_buffer,
                    texture,
                    viewport_size,
                )?;
                next_filter += 1;
            } else {
                command_encoder = self.encode_backdrop_blur(
                    next_blur,
                    scene.backdrop_blurs[next_blur],
                    instance_bindings,
                    command_buffer,
                    texture,
                    viewport_size,
                );
                next_blur += 1;
            }
        }

        command_encoder.end_encoding();

        Ok(command_buffer.to_owned())
    }

    fn encode_content_filter<'a>(
        &mut self,
        filter_index: usize,
        filter: &ContentFilter,
        command_buffer: &'a metal::CommandBufferRef,
        texture: &'a metal::TextureRef,
        viewport_size: Size<DevicePixels>,
    ) -> Result<&'a metal::RenderCommandEncoderRef> {
        let sigma = filter.blur_radius.0.max(0.0);
        let padding = if sigma <= 0.01 {
            1.0
        } else {
            (sigma * 3.0).ceil() + 2.0
        };
        let visible = filter.bounds.intersect(&filter.content_mask.bounds);
        let drawable_width = texture.width() as i64;
        let drawable_height = texture.height() as i64;
        let x0 = ((visible.origin.x.0 - padding).floor() as i64).max(0);
        let y0 = ((visible.origin.y.0 - padding).floor() as i64).max(0);
        let x1 = (((visible.origin.x.0 + visible.size.width.0) + padding).ceil() as i64)
            .min(drawable_width);
        let y1 = (((visible.origin.y.0 + visible.size.height.0) + padding).ceil() as i64)
            .min(drawable_height);
        if x1 <= x0 || y1 <= y0 {
            return Ok(new_command_encoder_for_texture(
                command_buffer,
                texture,
                viewport_size,
                None,
            ));
        }
        let (scratch, filtered) = self.ensure_content_filter_scratch(
            filter_index,
            (x1 - x0) as u64,
            (y1 - y0) as u64,
            texture,
        );
        let copy_x = x0.min(drawable_width - scratch.width() as i64).max(0) as u64;
        let copy_y = y0.min(drawable_height - scratch.height() as i64).max(0) as u64;

        // The child scene is independent of the parent framebuffer. Rendering
        // it with alpha zero is the key semantic distinction from backdrop
        // blur: underlying card/shell pixels can never enter this texture.
        let child_command = self.render_frame_with_clear_alpha(
            &filter.scene,
            &scratch,
            viewport_size,
            0.0,
            false,
            point(DevicePixels(copy_x as i32), DevicePixels(copy_y as i32)),
        )?;
        child_command.commit();
        if filter_profile::enabled() {
            filter_profile::record(filter_profile::Pass {
                buffer: child_command.to_owned(),
                width: scratch.width(),
                height: scratch.height(),
                sigma,
            });
        }

        let source = if filter.blur_radius.0 <= 0.01 {
            scratch
        } else {
            use metal::foreign_types::ForeignType as _;
            let kernel = self
                .content_filter_kernels
                .kernel(&self.device, sigma.max(1.0));
            unsafe {
                let _: () = msg_send![
                    kernel,
                    encodeToCommandBuffer: command_buffer.as_ptr() as *mut objc::runtime::Object
                    sourceTexture: scratch.as_ptr() as *mut objc::runtime::Object
                    destinationTexture: filtered.as_ptr() as *mut objc::runtime::Object
                ];
            }
            filtered
        };

        let encoder = new_command_encoder_for_texture(command_buffer, texture, viewport_size, None);
        self.draw_content_filter(
            filter,
            viewport_size,
            Bounds {
                origin: point(ScaledPixels(copy_x as f32), ScaledPixels(copy_y as f32)),
                size: size(
                    ScaledPixels(source.width() as f32),
                    ScaledPixels(source.height() as f32),
                ),
            },
            &source,
            encoder,
        );
        Ok(encoder)
    }

    fn ensure_content_filter_scratch(
        &mut self,
        filter_index: usize,
        needed_width: u64,
        needed_height: u64,
        drawable: &metal::TextureRef,
    ) -> (metal::Texture, metal::Texture) {
        let stale = self
            .content_filter_scratch
            .get(filter_index)
            .is_none_or(|(scratch, _)| {
                scratch.width() < needed_width
                    || scratch.width() > drawable.width()
                    || scratch.height() < needed_height
                    || scratch.height() > drawable.height()
                    || scratch.pixel_format() != drawable.pixel_format()
            });
        if stale {
            let descriptor = metal::TextureDescriptor::new();
            descriptor.set_texture_type(metal::MTLTextureType::D2);
            descriptor.set_pixel_format(drawable.pixel_format());
            descriptor.set_width(backdrop_scratch_extent(needed_width, drawable.width()));
            descriptor.set_height(backdrop_scratch_extent(needed_height, drawable.height()));
            descriptor.set_usage(
                metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead,
            );
            descriptor.set_storage_mode(metal::MTLStorageMode::Private);
            let scratch = self.device.new_texture(&descriptor);
            descriptor.set_usage(
                metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::ShaderWrite,
            );
            let filtered = self.device.new_texture(&descriptor);
            if filter_index < self.content_filter_scratch.len() {
                self.content_filter_scratch[filter_index] = (scratch, filtered);
            } else {
                debug_assert_eq!(filter_index, self.content_filter_scratch.len());
                self.content_filter_scratch.push((scratch, filtered));
            }
            let retained: u64 = self
                .content_filter_scratch
                .iter()
                .map(|(scratch, _)| {
                    scratch
                        .width()
                        .saturating_mul(scratch.height())
                        .saturating_mul(8)
                })
                .sum();
            self.content_filter_high_water_bytes =
                self.content_filter_high_water_bytes.max(retained);
        }
        self.content_filter_scratch[filter_index].clone()
    }

    fn draw_content_filter(
        &self,
        filter: &ContentFilter,
        viewport_size: Size<DevicePixels>,
        source_texture_bounds: Bounds<ScaledPixels>,
        source_texture: &metal::TextureRef,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        let source_bounds = filter.bounds;
        let destination_size = size(
            ScaledPixels(source_bounds.size.width.0 * filter.scale),
            ScaledPixels(source_bounds.size.height.0 * filter.scale),
        );
        let destination_bounds = Bounds {
            origin: point(
                ScaledPixels(
                    source_bounds.origin.x.0
                        + (source_bounds.size.width.0 - destination_size.width.0) * 0.5,
                ),
                ScaledPixels(
                    source_bounds.origin.y.0
                        + (source_bounds.size.height.0 - destination_size.height.0) * 0.5,
                ),
            ),
            size: destination_size,
        };
        let composite = ContentFilterComposite {
            destination_bounds,
            source_bounds,
            source_texture_bounds,
            content_mask: filter.content_mask,
        };

        command_encoder.set_render_pipeline_state(&self.content_filter_pipeline_state);
        command_encoder.set_vertex_buffer(
            ContentFilterInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_bytes(
            ContentFilterInputIndex::Composite as u64,
            mem::size_of_val(&composite) as u64,
            &composite as *const ContentFilterComposite as *const _,
        );
        command_encoder.set_vertex_bytes(
            ContentFilterInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_bytes(
            ContentFilterInputIndex::Composite as u64,
            mem::size_of_val(&composite) as u64,
            &composite as *const ContentFilterComposite as *const _,
        );
        command_encoder.set_fragment_texture(
            ContentFilterInputIndex::SourceTexture as u64,
            Some(source_texture),
        );
        command_encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 6);
    }

    fn encode_backdrop_blur<'a>(
        &mut self,
        blur_index: usize,
        scene_blur: BackdropBlur,
        instance_bindings: &InstanceBindings,
        command_buffer: &'a metal::CommandBufferRef,
        texture: &'a metal::TextureRef,
        viewport_size: Size<DevicePixels>,
    ) -> &'a metal::RenderCommandEncoderRef {
        let blur = instance_bindings.backdrop_blurs.clone();
        let sigma = scene_blur.blur_radius.0.max(1.0);
        let padding = (sigma * 3.0).ceil() + 2.0;
        let visible = scene_blur.bounds.intersect(&scene_blur.content_mask.bounds);
        let drawable_width = texture.width() as i64;
        let drawable_height = texture.height() as i64;
        let x0 = ((visible.origin.x.0 - padding).floor() as i64).max(0);
        let y0 = ((visible.origin.y.0 - padding).floor() as i64).max(0);
        let x1 = (((visible.origin.x.0 + visible.size.width.0) + padding).ceil() as i64)
            .min(drawable_width);
        let y1 = (((visible.origin.y.0 + visible.size.height.0) + padding).ceil() as i64)
            .min(drawable_height);
        if x1 <= x0 || y1 <= y0 {
            return new_command_encoder_for_texture(command_buffer, texture, viewport_size, None);
        }
        let (scratch, blurred) =
            self.ensure_backdrop_scratch((x1 - x0) as u64, (y1 - y0) as u64, texture);
        // Fill the whole allocation with real framebuffer pixels. This keeps
        // MPS clamp-to-edge behavior correct and avoids stale texels when a
        // quantized allocation is reused for a smaller blur.
        let copy_x = x0.min(drawable_width - scratch.width() as i64).max(0) as u64;
        let copy_y = y0.min(drawable_height - scratch.height() as i64).max(0) as u64;
        let blit = command_buffer.new_blit_command_encoder();
        blit.copy_from_texture(
            texture,
            0,
            0,
            metal::MTLOrigin {
                x: copy_x,
                y: copy_y,
                z: 0,
            },
            metal::MTLSize {
                width: scratch.width(),
                height: scratch.height(),
                depth: 1,
            },
            &scratch,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
        );
        blit.end_encoding();

        use metal::foreign_types::ForeignType as _;
        let kernel = self.backdrop_kernels.kernel(&self.device, sigma);
        unsafe {
            let _: () = msg_send![
                kernel,
                encodeToCommandBuffer: command_buffer.as_ptr() as *mut objc::runtime::Object
                sourceTexture: scratch.as_ptr() as *mut objc::runtime::Object
                destinationTexture: blurred.as_ptr() as *mut objc::runtime::Object
            ];
        }

        let encoder = new_command_encoder_for_texture(command_buffer, texture, viewport_size, None);
        self.draw_backdrop_blur(
            blur_index,
            &blur,
            viewport_size,
            [
                copy_x as f32,
                copy_y as f32,
                scratch.width() as f32,
                scratch.height() as f32,
            ],
            &blurred,
            encoder,
        );
        encoder
    }

    fn ensure_backdrop_scratch(
        &mut self,
        needed_width: u64,
        needed_height: u64,
        drawable: &metal::TextureRef,
    ) -> (metal::Texture, metal::Texture) {
        let reusable = self.backdrop_scratch.as_ref().is_some_and(|scratch| {
            scratch.pixel_format() == drawable.pixel_format()
                && scratch.width() >= needed_width
                && scratch.width() <= drawable.width()
                && scratch.height() >= needed_height
                && scratch.height() <= drawable.height()
        });
        if !reusable {
            let width = backdrop_scratch_extent(needed_width, drawable.width());
            let height = backdrop_scratch_extent(needed_height, drawable.height());
            let descriptor = metal::TextureDescriptor::new();
            descriptor.set_texture_type(metal::MTLTextureType::D2);
            descriptor.set_pixel_format(drawable.pixel_format());
            descriptor.set_width(width);
            descriptor.set_height(height);
            descriptor.set_usage(metal::MTLTextureUsage::ShaderRead);
            descriptor.set_storage_mode(metal::MTLStorageMode::Private);
            self.backdrop_scratch = Some(self.device.new_texture(&descriptor));
            descriptor.set_usage(
                metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::ShaderWrite,
            );
            self.backdrop_blurred = Some(self.device.new_texture(&descriptor));
            self.backdrop_high_water_bytes = self
                .backdrop_high_water_bytes
                .max(width.saturating_mul(height).saturating_mul(8));
        }
        (
            self.backdrop_scratch.clone().unwrap(),
            self.backdrop_blurred.clone().unwrap(),
        )
    }

    fn release_backdrop_resources(&mut self) {
        self.backdrop_scratch = None;
        self.backdrop_blurred = None;
        self.backdrop_kernels.clear();
    }

    fn release_content_filter_resources(&mut self) {
        self.content_filter_scratch.clear();
        self.content_filter_kernels.clear();
    }

    fn draw_backdrop_blur(
        &self,
        blur_index: usize,
        binding: &InstanceBinding,
        viewport_size: Size<DevicePixels>,
        source_rect: [f32; 4],
        source_texture: &metal::TextureRef,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        let offset = binding.offset + blur_index * mem::size_of::<BackdropBlur>();
        command_encoder.set_render_pipeline_state(&self.backdrop_blur_pipeline_state);
        command_encoder.set_vertex_buffer(
            BackdropBlurInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            BackdropBlurInputIndex::Blurs as u64,
            Some(&binding.buffer),
            offset as u64,
        );
        command_encoder.set_fragment_buffer(
            BackdropBlurInputIndex::Blurs as u64,
            Some(&binding.buffer),
            offset as u64,
        );
        command_encoder.set_vertex_bytes(
            BackdropBlurInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_bytes(
            BackdropBlurInputIndex::SourceRect as u64,
            mem::size_of_val(&source_rect) as u64,
            source_rect.as_ptr() as *const _,
        );
        command_encoder.set_fragment_texture(
            BackdropBlurInputIndex::SourceTexture as u64,
            Some(source_texture),
        );
        command_encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 6);
    }

    fn draw_paths_to_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        writer: &mut InstanceBufferWriter,
        viewport_size: Size<DevicePixels>,
        command_buffer: &metal::CommandBufferRef,
    ) -> Result<bool> {
        if paths.is_empty() {
            return Ok(false);
        }
        let intermediate_texture = self
            .path_intermediate_texture
            .as_ref()
            .context("missing path intermediate texture")?;

        let mut vertices = Vec::new();
        for path in paths {
            vertices.extend(path.vertices.iter().map(|v| PathRasterizationVertex {
                xy_position: v.xy_position,
                st_position: v.st_position,
                color: path.color,
                bounds: path.bounds.intersect(&path.content_mask.bounds),
            }));
        }
        let vertex_instance_bindings = writer.write(&vertices)?;

        let render_pass_descriptor = metal::RenderPassDescriptor::new();
        let color_attachment = render_pass_descriptor
            .color_attachments()
            .object_at(0)
            .unwrap();
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(metal::MTLClearColor::new(0., 0., 0., 0.));

        if let Some(msaa_texture) = &self.path_intermediate_msaa_texture {
            color_attachment.set_texture(Some(msaa_texture));
            color_attachment.set_resolve_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::MultisampleResolve);
        } else {
            color_attachment.set_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::Store);
        }

        let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
        command_encoder.set_render_pipeline_state(&self.paths_rasterization_pipeline_state);
        command_encoder.set_vertex_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&vertex_instance_bindings.buffer),
            vertex_instance_bindings.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            PathRasterizationInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&vertex_instance_bindings.buffer),
            vertex_instance_bindings.offset as u64,
        );
        command_encoder.draw_primitives(
            metal::MTLPrimitiveType::Triangle,
            0,
            vertices.len() as u64,
        );

        command_encoder.end_encoding();
        Ok(true)
    }

    fn draw_shadows(
        &self,
        shadows: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if shadows.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.shadows_pipeline_state);
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_bindings.shadows.buffer),
            instance_bindings.shadows.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_bindings.shadows.buffer),
            instance_bindings.shadows.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            ShadowInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            shadows.len() as u64,
            shadows.start as u64,
        );
    }

    fn draw_quads(
        &self,
        quads: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if quads.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.quads_pipeline_state);
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_bindings.quads.buffer),
            instance_bindings.quads.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_bindings.quads.buffer),
            instance_bindings.quads.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            QuadInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            quads.len() as u64,
            quads.start as u64,
        );
    }

    fn draw_paths_from_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        writer: &mut InstanceBufferWriter,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> Result<()> {
        let Some(first_path) = paths.first() else {
            return Ok(());
        };
        let intermediate_texture = self
            .path_intermediate_texture
            .as_ref()
            .context("missing path intermediate texture")?;

        command_encoder.set_render_pipeline_state(&self.path_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.set_fragment_texture(
            SpriteInputIndex::AtlasTexture as u64,
            Some(intermediate_texture),
        );

        // When copying paths from the intermediate texture to the drawable,
        // each pixel must only be copied once, in case of transparent paths.
        //
        // If all paths have the same draw order, then their bounds are all
        // disjoint, so we can copy each path's bounds individually. If this
        // batch combines different draw orders, we perform a single copy
        // for a minimal spanning rect.
        let sprites;
        if paths.last().unwrap().order == first_path.order {
            sprites = paths
                .iter()
                .map(|path| PathSprite {
                    bounds: path.clipped_bounds(),
                })
                .collect();
        } else {
            let mut bounds = first_path.clipped_bounds();
            for path in paths.iter().skip(1) {
                bounds = bounds.union(&path.clipped_bounds());
            }
            sprites = vec![PathSprite { bounds }];
        }

        let sprite_instance_bindings = writer.write(&sprites)?;
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&sprite_instance_bindings.buffer),
            sprite_instance_bindings.offset as u64,
        );

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
        );
        Ok(())
    }

    fn draw_underlines(
        &self,
        underlines: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if underlines.is_empty() {
            return;
        }

        command_encoder.set_render_pipeline_state(&self.underlines_pipeline_state);
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_bindings.underlines.buffer),
            instance_bindings.underlines.offset as u64,
        );
        command_encoder.set_fragment_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_bindings.underlines.buffer),
            instance_bindings.underlines.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            UnderlineInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            underlines.len() as u64,
            underlines.start as u64,
        );
    }

    fn draw_monochrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if sprites.is_empty() {
            return;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.monochrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.monochrome_sprites.buffer),
            instance_bindings.monochrome_sprites.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.monochrome_sprites.buffer),
            instance_bindings.monochrome_sprites.offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
            sprites.start as u64,
        );
    }

    fn draw_polychrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: Range<usize>,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if sprites.is_empty() {
            return;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.polychrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.polychrome_sprites.buffer),
            instance_bindings.polychrome_sprites.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_bindings.polychrome_sprites.buffer),
            instance_bindings.polychrome_sprites.offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        command_encoder.draw_primitives_instanced_base_instance(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
            sprites.start as u64,
        );
    }

    fn draw_surfaces(
        &mut self,
        surfaces: &[PaintSurface],
        first_surface: usize,
        instance_bindings: &InstanceBindings,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) {
        if surfaces.is_empty() {
            return;
        }

        command_encoder.set_vertex_buffer(
            SurfaceInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SurfaceInputIndex::Surfaces as u64,
            Some(&instance_bindings.surfaces.buffer),
            instance_bindings.surfaces.offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SurfaceInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        for (index, surface) in surfaces.iter().enumerate() {
            let texture_size = size(
                DevicePixels::from(surface.image_buffer.get_width() as i32),
                DevicePixels::from(surface.image_buffer.get_height() as i32),
            );

            command_encoder.set_vertex_bytes(
                SurfaceInputIndex::TextureSize as u64,
                mem::size_of_val(&texture_size) as u64,
                &texture_size as *const Size<DevicePixels> as *const _,
            );

            // One plane per binding, and the plane layout is what picks the
            // pipeline: a video frame is two planes of luma and chroma, a frame
            // another renderer produced is one plane already in this target's
            // own format.
            let plane = |index: usize, format: MTLPixelFormat, binding: SurfaceInputIndex| {
                let plane_texture = self
                    .core_video_texture_cache
                    .create_texture_from_image(
                        surface.image_buffer.as_concrete_TypeRef(),
                        None,
                        format,
                        surface.image_buffer.get_width_of_plane(index),
                        surface.image_buffer.get_height_of_plane(index),
                        index,
                    )
                    .unwrap();
                command_encoder.set_fragment_texture(binding as u64, unsafe {
                    let texture = CVMetalTextureGetTexture(plane_texture.as_concrete_TypeRef());
                    Some(metal::TextureRef::from_ptr(texture as *mut _))
                });
                plane_texture
            };

            // Held only for its lifetime: the `CVMetalTexture` wrappers must
            // outlive the draw that binds them.
            let format = surface.image_buffer.get_pixel_format();
            let _planes = if format == kCVPixelFormatType_32BGRA {
                command_encoder.set_render_pipeline_state(&self.bgra_surfaces_pipeline_state);
                vec![plane(
                    0,
                    MTLPixelFormat::BGRA8Unorm,
                    SurfaceInputIndex::BgraTexture,
                )]
            } else {
                assert_eq!(format, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
                command_encoder.set_render_pipeline_state(&self.surfaces_pipeline_state);
                vec![
                    plane(0, MTLPixelFormat::R8Unorm, SurfaceInputIndex::YTexture),
                    plane(1, MTLPixelFormat::RG8Unorm, SurfaceInputIndex::CbCrTexture),
                ]
            };

            command_encoder.draw_primitives_instanced_base_instance(
                metal::MTLPrimitiveType::Triangle,
                0,
                6,
                1,
                (first_surface + index) as u64,
            );
        }
    }
}

impl Drop for MetalRenderer {
    fn drop(&mut self) {
        self.release_backdrop_resources();
        self.release_content_filter_resources();
    }
}

fn release_mps_kernel(kernel: *mut objc::runtime::Object) {
    unsafe {
        let _: () = msg_send![kernel, release];
    }
}

fn new_gaussian_kernel(device: &metal::DeviceRef, sigma: f32) -> *mut objc::runtime::Object {
    unsafe {
        let alloc: *mut objc::runtime::Object = msg_send![class!(MPSImageGaussianBlur), alloc];
        let kernel: *mut objc::runtime::Object = msg_send![
            alloc,
            initWithDevice: device.as_ptr() as *mut objc::runtime::Object
            sigma: sigma
        ];
        let _: () = msg_send![kernel, setEdgeMode: 1u64];
        kernel
    }
}

fn new_command_encoder_for_texture<'a>(
    command_buffer: &'a metal::CommandBufferRef,
    texture: &'a metal::TextureRef,
    viewport_size: Size<DevicePixels>,
    clear_color: Option<metal::MTLClearColor>,
) -> &'a metal::RenderCommandEncoderRef {
    new_command_encoder_for_texture_at_origin(
        command_buffer,
        texture,
        viewport_size,
        point(DevicePixels(0), DevicePixels(0)),
        clear_color,
    )
}

fn new_command_encoder_for_texture_at_origin<'a>(
    command_buffer: &'a metal::CommandBufferRef,
    texture: &'a metal::TextureRef,
    viewport_size: Size<DevicePixels>,
    target_origin: Point<DevicePixels>,
    clear_color: Option<metal::MTLClearColor>,
) -> &'a metal::RenderCommandEncoderRef {
    let render_pass_descriptor = metal::RenderPassDescriptor::new();
    let color_attachment = render_pass_descriptor
        .color_attachments()
        .object_at(0)
        .unwrap();
    color_attachment.set_texture(Some(texture));
    color_attachment.set_store_action(metal::MTLStoreAction::Store);
    if let Some(clear_color) = clear_color {
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(clear_color);
    } else {
        color_attachment.set_load_action(metal::MTLLoadAction::Load);
    }

    let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
    command_encoder.set_viewport(metal::MTLViewport {
        originX: -i32::from(target_origin.x) as f64,
        originY: -i32::from(target_origin.y) as f64,
        width: i32::from(viewport_size.width) as f64,
        height: i32::from(viewport_size.height) as f64,
        znear: 0.0,
        zfar: 1.0,
    });
    command_encoder
}

#[cfg(any(test, feature = "test-support"))]
fn read_texture_to_image(texture: &metal::TextureRef) -> Result<RgbaImage> {
    let width = texture.width() as u32;
    let height = texture.height() as u32;
    let bytes_per_row = width as usize * 4;
    let mut pixels = vec![0u8; height as usize * bytes_per_row];

    let region = metal::MTLRegion {
        origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
        size: metal::MTLSize {
            width: width as u64,
            height: height as u64,
            depth: 1,
        },
    };
    texture.get_bytes(
        pixels.as_mut_ptr() as *mut std::ffi::c_void,
        bytes_per_row as u64,
        region,
        0,
    );

    // Convert BGRA to RGBA (swap B and R channels)
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    RgbaImage::from_raw(width, height, pixels).context("failed to create RgbaImage from pixel data")
}

fn build_pipeline_state_no_blend(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");
    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(false);
    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn batch_first_order(scene: &Scene, batch: &PrimitiveBatch) -> DrawOrder {
    match batch {
        PrimitiveBatch::Shadows(range) => scene.shadows[range.start].order,
        PrimitiveBatch::Quads(range) => scene.quads[range.start].order,
        PrimitiveBatch::Paths(range) => scene.paths[range.start].order,
        PrimitiveBatch::Underlines(range) => scene.underlines[range.start].order,
        PrimitiveBatch::MonochromeSprites { range, .. } => {
            scene.monochrome_sprites[range.start].order
        }
        PrimitiveBatch::SubpixelSprites { range, .. } => scene.subpixel_sprites[range.start].order,
        PrimitiveBatch::PolychromeSprites { range, .. } => {
            scene.polychrome_sprites[range.start].order
        }
        PrimitiveBatch::Surfaces(range) => scene.surfaces[range.start].order,
    }
}

fn build_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::SourceAlpha);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::One);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_sprite_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::One);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_rasterization_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
    path_sample_count: u32,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    if path_sample_count > 1 {
        descriptor.set_raster_sample_count(path_sample_count as _);
        descriptor.set_alpha_to_coverage_enabled(false);
    }
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

#[derive(Clone)]
struct InstanceBinding {
    buffer: metal::Buffer,
    offset: usize,
}

struct InstanceBindings {
    backdrop_blurs: InstanceBinding,
    quads: InstanceBinding,
    shadows: InstanceBinding,
    underlines: InstanceBinding,
    monochrome_sprites: InstanceBinding,
    polychrome_sprites: InstanceBinding,
    surfaces: InstanceBinding,
}

fn write_instances(scene: &Scene, writer: &mut InstanceBufferWriter) -> Result<InstanceBindings> {
    Ok(InstanceBindings {
        backdrop_blurs: writer.write(&scene.backdrop_blurs)?,
        quads: writer.write(&scene.quads)?,
        shadows: writer.write(&scene.shadows)?,
        underlines: writer.write(&scene.underlines)?,
        monochrome_sprites: writer.write(&scene.monochrome_sprites)?,
        polychrome_sprites: writer.write(&scene.polychrome_sprites)?,
        surfaces: writer.write_iter(scene.surfaces.iter().map(|surface| SurfaceBounds {
            bounds: surface.bounds,
            content_mask: surface.content_mask,
        }))?,
    })
}

struct InstanceBufferWriter {
    device: metal::Device,
    pool: Arc<Mutex<InstanceBufferPool>>,
    unified_memory: bool,
    filled: Vec<(InstanceBuffer, usize)>,
    current: InstanceBuffer,
    offset: usize,
}

impl InstanceBufferWriter {
    fn new(
        device: &metal::Device,
        pool: &Arc<Mutex<InstanceBufferPool>>,
        unified_memory: bool,
    ) -> Self {
        let current = pool.lock().acquire(device, unified_memory);
        Self {
            device: device.clone(),
            pool: pool.clone(),
            unified_memory,
            filled: Vec::new(),
            current,
            offset: 0,
        }
    }

    fn allocate<T>(&mut self, count: usize) -> Result<(InstanceBinding, &mut [MaybeUninit<T>])> {
        let size = mem::size_of::<T>() * count;
        let mut offset = self.offset.next_multiple_of(INSTANCE_BUFFER_ALIGNMENT);
        if offset + size > self.current.size {
            self.grow(size)?;
            offset = 0;
        }
        self.offset = offset + size;

        let binding = InstanceBinding {
            buffer: self.current.metal_buffer.clone(),
            offset,
        };
        // Safety: the reservation lies within a buffer this frame owns
        // exclusively, and never overlaps one handed out earlier.
        let values = unsafe {
            let start = (self.current.metal_buffer.contents() as *mut u8).add(offset);
            slice::from_raw_parts_mut(start.cast::<MaybeUninit<T>>(), count)
        };
        Ok((binding, values))
    }

    fn write<T>(&mut self, values: &[T]) -> Result<InstanceBinding> {
        let (binding, destination) = self.allocate::<T>(values.len())?;
        unsafe {
            ptr::copy_nonoverlapping(
                values.as_ptr(),
                destination.as_mut_ptr().cast::<T>(),
                values.len(),
            );
        }
        Ok(binding)
    }

    fn write_iter<T>(
        &mut self,
        values: impl ExactSizeIterator<Item = T>,
    ) -> Result<InstanceBinding> {
        let (binding, destination) = self.allocate::<T>(values.len())?;
        for (slot, value) in destination.iter_mut().zip(values) {
            slot.write(value);
        }
        Ok(binding)
    }

    fn grow(&mut self, required: usize) -> Result<()> {
        let mut pool = self.pool.lock();
        let buffer_size = (pool.buffer_size * 2)
            .max(required.next_power_of_two())
            .min(MAX_INSTANCE_BUFFER_SIZE);
        anyhow::ensure!(
            buffer_size >= required,
            "instance buffer needs {required} bytes, above the maximum of {MAX_INSTANCE_BUFFER_SIZE}"
        );
        anyhow::ensure!(
            buffer_size > self.current.size,
            "frame instance data exceeds the {MAX_INSTANCE_BUFFER_SIZE}-byte maximum"
        );
        if buffer_size != pool.buffer_size {
            log::info!("increased instance buffer size to {buffer_size}");
            pool.reset(buffer_size);
        }
        let buffer = pool.acquire(&self.device, self.unified_memory);
        drop(pool);

        let filled = mem::replace(&mut self.current, buffer);
        self.filled.push((filled, self.offset));
        self.offset = 0;
        Ok(())
    }

    fn finish(self) -> InstanceBuffer {
        let Self {
            unified_memory,
            filled,
            current,
            offset,
            ..
        } = self;

        if !unified_memory {
            for (buffer, written) in &filled {
                if *written == 0 {
                    continue;
                }
                buffer.metal_buffer.did_modify_range(NSRange {
                    location: 0,
                    length: *written as NSUInteger,
                });
            }
            if offset > 0 {
                current.metal_buffer.did_modify_range(NSRange {
                    location: 0,
                    length: offset as NSUInteger,
                });
            }
        }

        // Metal retains encoded resources until the command buffer completes.
        // Only the final, largest buffer is worth keeping in the pool.
        drop(filled);
        current
    }
}

#[repr(C)]
enum BackdropBlurInputIndex {
    Vertices = 0,
    Blurs = 1,
    ViewportSize = 2,
    SourceTexture = 3,
    SourceRect = 4,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct ContentFilterComposite {
    destination_bounds: Bounds<ScaledPixels>,
    source_bounds: Bounds<ScaledPixels>,
    source_texture_bounds: Bounds<ScaledPixels>,
    content_mask: ContentMask<ScaledPixels>,
}

#[repr(C)]
enum ContentFilterInputIndex {
    Vertices = 0,
    Composite = 1,
    ViewportSize = 2,
    SourceTexture = 3,
}

#[repr(C)]
enum ShadowInputIndex {
    Vertices = 0,
    Shadows = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum QuadInputIndex {
    Vertices = 0,
    Quads = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum UnderlineInputIndex {
    Vertices = 0,
    Underlines = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum SpriteInputIndex {
    Vertices = 0,
    Sprites = 1,
    ViewportSize = 2,
    AtlasTextureSize = 3,
    AtlasTexture = 4,
}

#[repr(C)]
enum SurfaceInputIndex {
    Vertices = 0,
    Surfaces = 1,
    ViewportSize = 2,
    TextureSize = 3,
    YTexture = 4,
    CbCrTexture = 5,
    BgraTexture = 6,
}

#[repr(C)]
enum PathRasterizationInputIndex {
    Vertices = 0,
    ViewportSize = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PathSprite {
    pub bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SurfaceBounds {
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
}

#[cfg(any(test, feature = "test-support"))]
pub struct MetalHeadlessRenderer {
    renderer: MetalRenderer,
}

#[cfg(any(test, feature = "test-support"))]
impl MetalHeadlessRenderer {
    pub fn new() -> Self {
        let instance_buffer_pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let renderer = MetalRenderer::new_headless(instance_buffer_pool);
        Self { renderer }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl gpui::PlatformHeadlessRenderer for MetalHeadlessRenderer {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        self.renderer.render_scene_to_image(scene, size)
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        self.renderer.render_scene(scene, size)
    }

    fn sprite_atlas(&self) -> Arc<dyn gpui::PlatformAtlas> {
        self.renderer.sprite_atlas().clone()
    }

    fn scratch_diagnostics(&self) -> Option<gpui::RendererScratchDiagnostics> {
        Some(gpui::RendererScratchDiagnostics {
            backdrop: self
                .renderer
                .backdrop_scratch
                .as_ref()
                .map(|texture| (texture.width(), texture.height())),
            content_filters: self
                .renderer
                .content_filter_scratch
                .iter()
                .map(|(texture, _)| (texture.width(), texture.height()))
                .collect(),
            backdrop_high_water_bytes: self.renderer.backdrop_high_water_bytes,
            content_filter_high_water_bytes: self.renderer.content_filter_high_water_bytes,
            backdrop_idle_frames: self.renderer.backdrop_lease.idle_frames,
            content_filter_idle_frames: self.renderer.content_filter_lease.idle_frames,
            backdrop_kernel_cached: !self.renderer.backdrop_kernels.is_empty(),
            content_filter_kernel_cached: !self.renderer.content_filter_kernels.is_empty(),
        })
    }
}

#[cfg(test)]
mod scratch_region_tests {
    use super::*;

    fn region(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(ScaledPixels(x), ScaledPixels(y)),
            size: size(ScaledPixels(width), ScaledPixels(height)),
        }
    }

    fn backdrop_scene() -> Scene {
        let bounds = region(80.0, 60.0, 173.0, 111.0);
        let mut scene = Scene::default();
        scene.insert_backdrop_blur(BackdropBlur {
            order: 0,
            blur_radius: ScaledPixels(18.0),
            bounds,
            content_mask: ContentMask { bounds },
            corner_radii: gpui::Corners::all(ScaledPixels(12.0)),
        });
        scene.finish();
        scene
    }

    fn content_filter_scene(blur_radius: f32) -> Scene {
        let bounds = region(100.0, 90.0, 173.0, 111.0);
        let mut child = Scene::default();
        child.finish();
        let mut scene = Scene::default();
        scene.insert_content_filter(ContentFilter {
            order: 0,
            blur_radius: ScaledPixels(blur_radius),
            bounds,
            content_mask: ContentMask { bounds },
            scale: 0.9,
            scene: Arc::new(child),
        });
        scene.finish();
        scene
    }

    #[test]
    fn backdrop_scratch_is_quantized_but_never_exceeds_drawable() {
        assert_eq!(backdrop_scratch_extent(173, 3840), 256);
        assert_eq!(backdrop_scratch_extent(411, 3840), 512);
        assert_eq!(backdrop_scratch_extent(3700, 3840), 3840);
        assert_eq!(backdrop_scratch_extent(9000, 3840), 3840);
    }

    #[test]
    fn dialog_sized_blur_has_a_bounded_measurable_pair() {
        let width = backdrop_scratch_extent(411, 3840);
        let height = backdrop_scratch_extent(211, 2160);
        let pair_bytes = width * height * 4 * 2;
        assert_eq!((width, height), (512, 256));
        assert_eq!(pair_bytes, 1_048_576);
        assert!(pair_bytes < 3840 * 2160 * 4 * 2 / 50);
    }

    #[test]
    fn content_only_frames_keep_the_content_kernel_lease_hot() {
        let mut backdrop = ScratchLease::default();
        let mut content = ScratchLease::default();

        for _ in 0..BACKDROP_SCRATCH_RELEASE_AFTER_FRAMES - 1 {
            assert!(!backdrop.note_frame(false));
            assert!(!content.note_frame(true));
        }
        assert!(backdrop.note_frame(false));
        assert!(!content.note_frame(true));
        assert_eq!(content.idle_frames, 0);

        for _ in 0..BACKDROP_SCRATCH_RELEASE_AFTER_FRAMES - 1 {
            assert!(!content.note_frame(false));
        }
        assert!(content.note_frame(false));
    }

    #[test]
    fn metal_scratch_diagnostics_measure_independent_use_and_release() {
        let mut renderer =
            MetalRenderer::new_headless(Arc::new(Mutex::new(InstanceBufferPool::default())));
        let viewport = size(DevicePixels(640), DevicePixels(480));

        renderer.render_scene(&backdrop_scene(), viewport).unwrap();
        let backdrop = renderer
            .backdrop_scratch
            .as_ref()
            .expect("real backdrop rendering must allocate scratch");
        let backdrop_bytes = backdrop.width() * backdrop.height() * 8;
        assert_eq!(renderer.backdrop_high_water_bytes, backdrop_bytes);
        assert!(!renderer.backdrop_kernels.is_empty());
        assert!(renderer.content_filter_scratch.is_empty());
        assert!(renderer.content_filter_kernels.is_empty());

        let content = content_filter_scene(18.0);
        for _ in 0..BACKDROP_SCRATCH_RELEASE_AFTER_FRAMES {
            renderer.render_scene(&content, viewport).unwrap();
        }
        assert!(
            renderer.backdrop_scratch.is_none() && renderer.backdrop_kernels.is_empty(),
            "content-only frames kept the backdrop family alive"
        );
        let (scratch, _) = renderer
            .content_filter_scratch
            .first()
            .expect("real isolated filtering must allocate scratch");
        let content_bytes = scratch.width() * scratch.height() * 8;
        assert_eq!(renderer.content_filter_high_water_bytes, content_bytes);
        assert_eq!(renderer.content_filter_lease.idle_frames, 0);
        assert!(!renderer.content_filter_kernels.is_empty());

        let mut empty = Scene::default();
        empty.finish();
        for _ in 0..BACKDROP_SCRATCH_RELEASE_AFTER_FRAMES - 1 {
            renderer.render_scene(&empty, viewport).unwrap();
        }
        assert!(renderer.content_filter_scratch.first().is_some());
        assert!(!renderer.content_filter_kernels.is_empty());
        renderer.render_scene(&empty, viewport).unwrap();
        assert!(renderer.content_filter_scratch.is_empty());
        assert!(renderer.content_filter_kernels.is_empty());
        assert_eq!(renderer.content_filter_high_water_bytes, content_bytes);
        assert_eq!(
            renderer.content_filter_kernels.releases, renderer.content_filter_kernels.allocations,
            "idle release must release every retained MPS kernel exactly once"
        );
    }

    #[test]
    fn sustained_dialog_radii_reuse_a_bounded_kernel_working_set() {
        let mut renderer =
            MetalRenderer::new_headless(Arc::new(Mutex::new(InstanceBufferPool::default())));
        let viewport = size(DevicePixels(640), DevicePixels(480));
        let mut backdrop = Scene::default();
        for (index, sigma) in [18.0, 26.0, 44.0].into_iter().enumerate() {
            let bounds = region(30.0 + index as f32 * 8.0, 30.0, 400.0, 300.0);
            backdrop.insert_backdrop_blur(BackdropBlur {
                order: 0,
                blur_radius: ScaledPixels(sigma),
                bounds,
                content_mask: ContentMask { bounds },
                corner_radii: gpui::Corners::all(ScaledPixels(12.0)),
            });
        }
        backdrop.finish();

        for _ in 0..20 {
            renderer.render_scene(&backdrop, viewport).unwrap();
        }
        assert_eq!(renderer.backdrop_kernels.allocations, 3);
        assert_eq!(renderer.backdrop_kernels.releases, 0);
        assert_eq!(renderer.backdrop_kernels.entries.len(), 3);

        // A 0→18 morph quantizes to nine stable kernels, fitting the bounded
        // content working set. A second sweep must allocate nothing.
        let radii = (0..=18).map(|radius| radius as f32).collect::<Vec<_>>();
        for _ in 0..2 {
            for radius in &radii {
                renderer
                    .render_scene(&content_filter_scene(*radius), viewport)
                    .unwrap();
            }
        }
        assert_eq!(renderer.content_filter_kernels.allocations, 9);
        assert_eq!(renderer.content_filter_kernels.releases, 0);
        assert!(
            renderer.content_filter_kernels.entries.len() <= CONTENT_FILTER_KERNEL_CACHE_CAPACITY
        );

        renderer.release_backdrop_resources();
        renderer.release_content_filter_resources();
        assert_eq!(
            renderer.backdrop_kernels.releases,
            renderer.backdrop_kernels.allocations
        );
        assert_eq!(
            renderer.content_filter_kernels.releases,
            renderer.content_filter_kernels.allocations
        );
    }
}
