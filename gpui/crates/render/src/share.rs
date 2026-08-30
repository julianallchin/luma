//! Render targets the window compositor can sample without a copy.
//!
//! # What this buys
//!
//! Handing a frame from the renderer to the compositor used to mean
//! `copy_texture_to_buffer`, an async map, and an upload into the compositor's
//! sprite atlas — the whole viewport across the memory bus twice, at frame
//! rate, with the second crossing on the UI thread. A shared target is memory
//! both can address, so the frame crosses nothing: the renderer's composite
//! pass writes it and the compositor's draw samples it.
//!
//! # What "shared" means per platform
//!
//! This module is the one place the answer differs, behind one [`Shared`] and
//! one [`Surface`]:
//!
//! - **Metal** (macOS): the renderer's device and the compositor's are
//!   different devices. An `IOSurface` is the one allocation both can wrap as
//!   an `MTLTexture`; the compositor takes it as a `CVPixelBuffer`, the
//!   currency gpui's surface primitive already speaks.
//! - **wgpu** (Linux, FreeBSD): the compositor *is* wgpu, so the renderer
//!   draws on the compositor's own device (see [`Gpu::adopt`]) and the shared
//!   target is an ordinary texture. Same device, same queue: the compositor's
//!   draw is ordered after the renderer's submit by the queue itself, and
//!   nothing here has to fence anything.
//! - **Anything else**: no shared memory, so [`Surface`] is uninhabited and
//!   [`Shared::new`] always answers `None`. The CPU readback path is the
//!   only path, and every `match` on a surface still compiles.
//!
//! # What it costs on Metal
//!
//! Sharing removes the copy that was also acting as a fence. Nothing here
//! serialises the two devices, so a surface being sampled by the compositor
//! must not be a surface the renderer is drawing into — see
//! [`crate::viewport`], which owns that reservation because it is the only
//! place that knows which frame reached the screen. On one wgpu device the
//! reservation is harmless and unnecessary.

use crate::gpu::Gpu;

/// Set to refuse every shared surface, leaving the CPU readback path.
///
/// The fallback is the only path on a machine with no shareable memory, and
/// every such machine is one no test here runs on — so without a way to ask for
/// it, the code that keeps a bare host working would never be executed again.
/// This is that way. It is not a rendering mode: both paths draw the same
/// picture, which is exactly what a test that sets it should assert.
pub(crate) const WITHHOLD: &str = "LUMA_WITHHOLD_SHARED_SURFACES";

/// A frame's memory, addressable by the window compositor.
///
/// Retained: cloning keeps the memory alive past the target that drew it, so
/// a viewport torn down while its last frame is still on screen leaves a valid
/// picture rather than a freed one. `Send`, because the frame is drawn on the
/// renderer thread and painted on the UI thread — see [`platform`] for why
/// that is sound on each platform.
#[derive(Clone)]
pub struct Surface(platform::Handle);

impl Surface {
    /// The handle the compositor paints: exactly what `gpui`'s
    /// `Window::paint_surface` takes on this platform.
    #[must_use]
    pub fn source(&self) -> platform::Source {
        platform::source(&self.0)
    }

    /// Copy the texels out, unpadded: BGRA8, sRGB-encoded, row-major.
    ///
    /// That copy is the cost this whole module exists to avoid, so this is
    /// for callers that genuinely need bytes — a test, an encoder — and never
    /// for putting a frame on screen.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        platform::to_bytes(&self.0)
    }
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Surface").finish_non_exhaustive()
    }
}

/// One frame's worth of memory, wrapped as a colour attachment for the
/// renderer and as a [`Surface`] for the compositor.
///
/// Both views are created once and live as long as the target; neither the
/// renderer nor the compositor allocates per frame.
pub(crate) struct Shared {
    view: wgpu::TextureView,
    surface: Surface,
}

impl Shared {
    /// Allocate a shareable BGRA8 target, or `None` when `gpu` cannot produce
    /// one the compositor could see.
    ///
    /// `None` is not an error: it is the honest answer on a device the
    /// compositor does not share (a headless harness, a software fallback, a
    /// platform with no shared memory at all), and the caller's response is
    /// to keep reading frames back over the CPU, which still works. `Option`
    /// rather than `Result` is deliberate — there is no message here a user
    /// could act on, only a slower path.
    ///
    /// The renderer's view is sRGB, so the composite pass writes linear
    /// values and the hardware encodes them, exactly as it does into a staged
    /// target. The compositor's view is *not* sRGB, so it samples the encoded
    /// bytes unchanged — which is what its sprite atlas does with the same
    /// bytes. The asymmetry is the point: it is how the two paths produce one
    /// picture.
    pub(crate) fn new(gpu: &Gpu, width: u32, height: u32) -> Option<Self> {
        if std::env::var_os(WITHHOLD).is_some() {
            return None;
        }
        let (view, handle) = platform::allocate(gpu, width, height)?;
        Some(Self {
            view,
            surface: Surface(handle),
        })
    }

    /// The colour attachment the composite pass renders into.
    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The compositor's handle to the same memory.
    pub(crate) fn surface(&self) -> Surface {
        self.surface.clone()
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

/// The descriptor every platform's target is built to.
///
/// No `COPY_SRC` by default: the point of this target is that nothing copies
/// out of it. A platform adds it only if its [`Surface::to_bytes`] needs it.
fn descriptor(width: u32, height: u32) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some("shared-output"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    }
}

/// `IOSurface` shared between the renderer's Metal device and the
/// compositor's.
#[cfg(target_os = "macos")]
mod platform {
    // `io-surface` is deprecated in favour of `objc2-io-surface`, but the
    // `CVPixelBuffer` constructor this module needs takes *this* crate's
    // `IOSurface`, and the `core-video` version is pinned by what gpui speaks.
    // Switching one without the other would only add a pointer cast — which is
    // exactly the cast `import` performs to hand the same surface to
    // `objc2-metal`.
    #![allow(deprecated)]

    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_video::pixel_buffer::CVPixelBuffer;
    use io_surface::IOSurface;
    use objc2_metal::{
        MTLDevice as _, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
        MTLTextureUsage,
    };

    use crate::gpu::Gpu;

    /// `kCVPixelFormatType_32BGRA` / `'BGRA'`, the one format this module
    /// makes. It matches [`crate::gpu::Channels::Bgra`] byte for byte and it
    /// matches the format gpui's polychrome atlas already stores.
    ///
    /// An `IOSurface` pixel format names a byte layout and says nothing about
    /// transfer function, which is why one surface can carry an sRGB-encoding
    /// write view and a raw-byte read view.
    const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241;

    /// `core-video`'s `CVPixelBuffer` is not `Send` — its binding is a raw
    /// pointer — but the object behind it is a CoreFoundation type backed by
    /// an `IOSurface`, which Apple documents as shareable across threads *and*
    /// processes. This newtype is where that reasoning lives.
    #[derive(Clone)]
    pub struct Handle(CVPixelBuffer);

    // SAFETY: retain, release and the `IOSurface` backing are all
    // thread-safe; the pointer is the only reason the binding is not `Send`.
    unsafe impl Send for Handle {}

    pub type Source = CVPixelBuffer;

    pub fn source(handle: &Handle) -> Source {
        handle.0.clone()
    }

    /// Any Metal device can back a texture with an `IOSurface`, so this does
    /// not care whether `gpu` is adopted — only that it is Metal.
    pub fn allocate(gpu: &Gpu, width: u32, height: u32) -> Option<(wgpu::TextureView, Handle)> {
        let surface = io_surface(width, height);
        let buffer = CVPixelBuffer::from_io_surface(&surface, None).ok()?;
        let texture = import(gpu.device(), &surface, &super::descriptor(width, height))?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some((view, Handle(buffer)))
    }

    /// Per-row, because the surface's rows are aligned for the display
    /// hardware.
    pub fn to_bytes(handle: &Handle) -> Vec<u8> {
        use core_video::pixel_buffer::{
            CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
            CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferUnlockBaseAddress,
        };
        /// `kCVPixelBufferLock_ReadOnly`.
        const READ_ONLY: u64 = 1;

        // SAFETY: the buffer is alive for this borrow; lock and unlock are
        // balanced and the base address is valid between them, which is the
        // whole contract of the lock.
        unsafe {
            let raw = handle.0.as_concrete_TypeRef();
            let width = CVPixelBufferGetWidth(raw);
            let height = CVPixelBufferGetHeight(raw);
            let row_bytes = width * 4;
            if CVPixelBufferLockBaseAddress(raw, READ_ONLY) != 0 {
                return Vec::new();
            }
            let stride = CVPixelBufferGetBytesPerRow(raw);
            let base: *const u8 = CVPixelBufferGetBaseAddress(raw).cast();
            let mut pixels = Vec::with_capacity(row_bytes * height);
            for row in 0..height {
                pixels.extend_from_slice(std::slice::from_raw_parts(
                    base.add(row * stride),
                    row_bytes,
                ));
            }
            CVPixelBufferUnlockBaseAddress(raw, READ_ONLY);
            pixels
        }
    }

    /// Create the `IOSurface` both views are built over.
    ///
    /// The row pitch is left to `IOSurface`, which aligns it for the display
    /// hardware. Nothing in this module reads the memory linearly, so the
    /// pitch is never a number anyone here has to know.
    fn io_surface(width: u32, height: u32) -> IOSurface {
        let key = |name| unsafe { CFString::wrap_under_get_rule(name) };
        let properties = CFDictionary::from_CFType_pairs(&[
            (
                key(unsafe { io_surface::kIOSurfaceWidth }),
                CFNumber::from(width as i32).as_CFType(),
            ),
            (
                key(unsafe { io_surface::kIOSurfaceHeight }),
                CFNumber::from(height as i32).as_CFType(),
            ),
            (
                key(unsafe { io_surface::kIOSurfaceBytesPerElement }),
                CFNumber::from(4i32).as_CFType(),
            ),
            (
                key(unsafe { io_surface::kIOSurfacePixelFormat }),
                CFNumber::from(PIXEL_FORMAT_BGRA).as_CFType(),
            ),
        ]);
        io_surface::new(&properties)
    }

    /// Wrap `surface` as a texture belonging to `device`.
    ///
    /// `None` when `device` is not a Metal device, which is the only reason
    /// this can fail: every Metal device can back a texture with an
    /// `IOSurface`.
    fn import(
        device: &wgpu::Device,
        surface: &IOSurface,
        descriptor: &wgpu::TextureDescriptor<'_>,
    ) -> Option<wgpu::Texture> {
        // SAFETY: `as_hal` only lends the backend device for the duration of
        // this borrow, and the `Metal` type parameter is checked against the
        // adapter — a non-Metal device yields `None` rather than a mistyped
        // handle.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
        let raw = {
            let metal_device = hal.raw_device();
            let texture_descriptor = MTLTextureDescriptor::new();
            texture_descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm_sRGB);
            // SAFETY: the setters are `unsafe` only because a zero extent is
            // rejected at texture creation; `Shared::new` is never called
            // with one.
            unsafe {
                texture_descriptor.setWidth(descriptor.size.width as usize);
                texture_descriptor.setHeight(descriptor.size.height as usize);
            }
            texture_descriptor
                .setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
            // An IOSurface-backed texture may not be `Private`: the allocation
            // is shared by construction. Unified memory can address it
            // directly; discrete memory needs the managed round trip.
            texture_descriptor.setStorageMode(if metal_device.hasUnifiedMemory() {
                MTLStorageMode::Shared
            } else {
                MTLStorageMode::Managed
            });
            // SAFETY: the pointer `as_concrete_TypeRef` yields is the same
            // `__IOSurface` allocation `objc2_io_surface::IOSurfaceRef` names
            // — the two crates declare one CoreFoundation type — and the
            // reference only lives for this call, over which `surface` is
            // borrowed.
            let io_surface: &objc2_io_surface::IOSurfaceRef =
                unsafe { &*surface.as_concrete_TypeRef().cast() };
            // `newTextureWithDescriptor:iosurface:plane:` is the documented
            // constructor for an IOSurface-backed texture. The descriptor's
            // extent and element size match the surface `io_surface` made,
            // and plane zero is the only plane a `'BGRA'` surface has.
            metal_device.newTextureWithDescriptor_iosurface_plane(
                &texture_descriptor,
                io_surface,
                0,
            )?
        };
        // SAFETY: `raw` was created by `device`'s own Metal device, its
        // extent, format and usage were taken from `descriptor`, and Metal
        // returns it initialised.
        let hal_texture = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                wgpu::TextureFormat::Bgra8UnormSrgb,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width: descriptor.size.width,
                    height: descriptor.size.height,
                    depth: 1,
                },
                // The `IOSurface` owns the memory and `CVPixelBuffer` retains
                // it; the texture needs no drop-time hook of its own.
                None,
            )
        };
        drop(hal);
        // SAFETY: `hal_texture` wraps a texture from this device, built to
        // match `descriptor`, and Metal has initialised it. `UNINITIALIZED`
        // because the first use is a render-pass clear, which is what the
        // state machine expects of a texture nothing has written yet.
        Some(unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal_texture,
                descriptor,
                wgpu::TextureUses::UNINITIALIZED,
            )
        })
    }
}

/// A texture on the compositor's own wgpu device.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod platform {
    use crate::gpu::Gpu;

    /// The texture, plus the device and queue that can read it back. All
    /// three are `Arc`-backed handles, and `wgpu` makes them `Send + Sync`.
    #[derive(Clone)]
    pub struct Handle {
        texture: wgpu::Texture,
        device: wgpu::Device,
        queue: wgpu::Queue,
    }

    pub type Source = wgpu::Texture;

    pub fn source(handle: &Handle) -> Source {
        handle.texture.clone()
    }

    /// Only an adopted device is one the compositor can see; a device this
    /// crate built for itself has a texture nobody else can bind.
    pub fn allocate(gpu: &Gpu, width: u32, height: u32) -> Option<(wgpu::TextureView, Handle)> {
        if !gpu.is_adopted() {
            return None;
        }
        let descriptor = wgpu::TextureDescriptor {
            // Sampled by the compositor, and readable for `to_bytes`. The
            // non-sRGB view format is what the compositor's raw-byte read
            // view is created as.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Bgra8Unorm],
            ..super::descriptor(width, height)
        };
        let texture = gpu.device().create_texture(&descriptor);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some((
            view,
            Handle {
                texture,
                device: gpu.device().clone(),
                queue: gpu.queue().clone(),
            },
        ))
    }

    /// A synchronous copy-and-map: the staged path's readback, done once on
    /// demand instead of every frame.
    pub fn to_bytes(handle: &Handle) -> Vec<u8> {
        let size = handle.texture.size();
        let row_bytes = size.width * 4;
        let bytes_per_row = row_bytes.div_ceil(256) * 256;
        let readback = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shared-readback"),
            size: u64::from(bytes_per_row) * u64::from(size.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("shared-readback"),
            });
        encoder.copy_texture_to_buffer(
            handle.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            size,
        );
        handle.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        if handle
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .is_err()
        {
            return Vec::new();
        }
        let Ok(view) = slice.get_mapped_range() else {
            return Vec::new();
        };
        let mut pixels = Vec::with_capacity((row_bytes * size.height) as usize);
        for row in 0..size.height {
            let start = (row * bytes_per_row) as usize;
            pixels.extend_from_slice(&view[start..start + row_bytes as usize]);
        }
        pixels
    }
}

/// No shared memory: nothing here can be constructed.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "freebsd")))]
mod platform {
    use crate::gpu::Gpu;

    pub type Handle = std::convert::Infallible;
    pub type Source = std::convert::Infallible;

    pub fn source(handle: &Handle) -> Source {
        match *handle {}
    }

    pub fn allocate(_gpu: &Gpu, _width: u32, _height: u32) -> Option<(wgpu::TextureView, Handle)> {
        None
    }

    pub fn to_bytes(handle: &Handle) -> Vec<u8> {
        match *handle {}
    }
}

#[cfg(test)]
mod tests {
    use super::Shared;

    /// The whole mechanism in one assertion: a texture the renderer can draw
    /// into, whose pixels a second, independent reader sees.
    ///
    /// On a wgpu compositor the device has to be adopted first, so the test
    /// plays the window: it makes a device and offers it through
    /// [`crate::Gpu::adopt`], then builds on it the way `Gpu::shared` would.
    #[test]
    fn a_rendered_shared_surface_is_readable_through_its_surface() {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let instance = wgpu::Instance::default();
            let Ok(adapter) = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            ) else {
                return;
            };
            let Ok((device, queue)) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                }))
            else {
                return;
            };
            crate::Gpu::adopt(
                std::sync::Arc::new(device),
                std::sync::Arc::new(queue),
                adapter,
                std::sync::Arc::default(),
            );
        }
        let Ok(gpu) = crate::Gpu::build() else {
            return;
        };
        let Some(shared) = Shared::new(&gpu, 8, 4) else {
            assert!(
                !cfg!(any(
                    target_os = "macos",
                    target_os = "linux",
                    target_os = "freebsd"
                )),
                "a Metal device, or an adopted wgpu device, can always share"
            );
            return;
        };
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("shared-clear"),
            });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shared-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: shared.view(),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 1.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        gpu.queue().submit([encoder.finish()]);
        gpu.device()
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");

        let pixels = shared.surface().to_bytes();
        assert_eq!(
            &pixels[..4],
            &[255, 0, 0, 255],
            "blue in BGRA is B=255, R=0"
        );
    }
}
