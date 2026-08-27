//! Render targets the window compositor can sample without a copy.
//!
//! # What this buys
//!
//! The renderer's device and the compositor's device are different devices on
//! different threads. Handing a frame from one to the other used to mean
//! `copy_texture_to_buffer`, an async map, and an upload into the compositor's
//! sprite atlas — the whole viewport across the memory bus twice, at frame
//! rate, with the second crossing on the UI thread. An `IOSurface` is the one
//! macOS allocation both devices can wrap as an `MTLTexture`, so the frame
//! crosses nothing: the renderer's composite pass writes it and the
//! compositor's draw samples it.
//!
//! # What it costs
//!
//! Sharing removes the copy that was also acting as a fence. Nothing here
//! serialises the two devices, so a surface being sampled by the compositor
//! must not be a surface the renderer is drawing into — see
//! [`crate::viewport`], which owns that reservation because it is the only
//! place that knows which frame reached the screen.
//!
//! # Why a `CVPixelBuffer` and not a bare `IOSurface`
//!
//! `CVPixelBuffer` is the currency gpui's surface primitive already speaks, and
//! it is a thin wrapper over the same `IOSurface`. Using it means the
//! compositor side needs no new type, no new primitive and no new element —
//! only the ability to read one more pixel format.

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

/// `kCVPixelFormatType_32BGRA` / `'BGRA'`, the one format this module makes.
///
/// It matches [`crate::gpu::Channels::Bgra`] byte for byte and it matches the
/// format gpui's polychrome atlas already stores, so a shared frame and an
/// uploaded frame are the same pixels — which is what lets the CPU path stay a
/// fallback rather than a second look.
///
/// An `IOSurface` pixel format names a byte layout and says nothing about
/// transfer function, which is why one surface can carry an sRGB-encoding write
/// view and a raw-byte read view — see [`Shared::new`].
const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241;

/// Set to refuse every shared surface, leaving the CPU readback path.
///
/// The fallback is the only path on a machine with no shareable memory, and
/// every such machine is one no test here runs on — so without a way to ask for
/// it, the code that keeps a non-Metal host working would never be executed
/// again. This is that way. It is not a rendering mode: both paths draw the
/// same picture, which is exactly what a test that sets it should assert.
pub(crate) const WITHHOLD: &str = "LUMA_WITHHOLD_SHARED_SURFACES";

/// One frame's worth of memory, wrapped as a texture by the renderer's device
/// and as a pixel buffer by the compositor's.
///
/// The two views are created once and live as long as the target; neither the
/// renderer nor the compositor allocates per frame.
pub(crate) struct Shared {
    buffer: CVPixelBuffer,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Shared {
    /// Allocate a shareable BGRA8 target, or `None` when this device cannot
    /// produce one.
    ///
    /// `None` is not an error: it is the honest answer on a non-Metal adapter
    /// (a software fallback, a remote session), and the caller's response is to
    /// keep reading frames back over the CPU, which still works. Returning
    /// `Option` rather than `Result` is deliberate — there is no message here a
    /// user could act on, only a slower path.
    ///
    /// The write view is sRGB, so the composite pass writes linear values and
    /// the hardware encodes them, exactly as it does into a staged target. The
    /// compositor's read view is *not* sRGB, so it samples the encoded bytes
    /// unchanged — which is what its sprite atlas does with the same bytes. The
    /// asymmetry is the point: it is how the two paths produce one picture.
    pub(crate) fn new(device: &wgpu::Device, width: u32, height: u32) -> Option<Self> {
        if std::env::var_os(WITHHOLD).is_some() {
            return None;
        }
        let surface = allocate(width, height);
        let buffer = CVPixelBuffer::from_io_surface(&surface, None).ok()?;
        let descriptor = wgpu::TextureDescriptor {
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
            // No `COPY_SRC`: the point of this target is that nothing copies
            // out of it.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        };
        let texture = import(device, &surface, &descriptor)?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some(Self {
            buffer,
            texture,
            view,
        })
    }

    /// The colour attachment the composite pass renders into.
    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// A retained handle to the same memory, for the compositor to paint.
    ///
    /// Cloning retains the `CVPixelBuffer`, which keeps the `IOSurface` alive
    /// past this target's own lifetime — so a viewport torn down while its last
    /// frame is still on screen leaves a valid picture rather than a freed one.
    pub(crate) fn buffer(&self) -> CVPixelBuffer {
        self.buffer.clone()
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("size", &self.texture.size())
            .finish_non_exhaustive()
    }
}

/// Create the `IOSurface` both views are built over.
///
/// The row pitch is left to `IOSurface`, which aligns it for the display
/// hardware. Nothing in this module reads the memory linearly, so the pitch is
/// never a number anyone here has to know.
fn allocate(width: u32, height: u32) -> IOSurface {
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
/// `None` when `device` is not a Metal device, which is the only reason this
/// can fail: every Metal device can back a texture with an `IOSurface`.
fn import(
    device: &wgpu::Device,
    surface: &IOSurface,
    descriptor: &wgpu::TextureDescriptor<'_>,
) -> Option<wgpu::Texture> {
    // SAFETY: `as_hal` only lends the backend device for the duration of this
    // borrow, and the `Metal` type parameter is checked against the adapter —
    // a non-Metal device yields `None` rather than a mistyped handle.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
    let raw = {
        let metal_device = hal.raw_device();
        let texture_descriptor = MTLTextureDescriptor::new();
        texture_descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm_sRGB);
        // SAFETY: the setters are `unsafe` only because a zero extent is
        // rejected at texture creation; `Shared::new` is never called with one.
        unsafe {
            texture_descriptor.setWidth(descriptor.size.width as usize);
            texture_descriptor.setHeight(descriptor.size.height as usize);
        }
        texture_descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        // An IOSurface-backed texture may not be `Private`: the allocation is
        // shared by construction. Unified memory can address it directly;
        // discrete memory needs the managed round trip.
        texture_descriptor.setStorageMode(if metal_device.hasUnifiedMemory() {
            MTLStorageMode::Shared
        } else {
            MTLStorageMode::Managed
        });
        // SAFETY: the pointer `as_concrete_TypeRef` yields is the same
        // `__IOSurface` allocation `objc2_io_surface::IOSurfaceRef` names —
        // the two crates declare one CoreFoundation type — and the reference
        // only lives for this call, over which `surface` is borrowed.
        let io_surface: &objc2_io_surface::IOSurfaceRef =
            unsafe { &*surface.as_concrete_TypeRef().cast() };
        // `newTextureWithDescriptor:iosurface:plane:` is the documented
        // constructor for an IOSurface-backed texture. The descriptor's extent
        // and element size match the surface `allocate` made, and plane zero is
        // the only plane a `'BGRA'` surface has.
        metal_device.newTextureWithDescriptor_iosurface_plane(&texture_descriptor, io_surface, 0)?
    };
    // SAFETY: `raw` was created by `device`'s own Metal device, its extent,
    // format and usage were taken from `descriptor`, and Metal returns it
    // initialised.
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
            // The `IOSurface` owns the memory and `CVPixelBuffer` retains it;
            // the texture needs no drop-time hook of its own.
            None,
        )
    };
    drop(hal);
    // SAFETY: `hal_texture` wraps a texture from this device, built to match
    // `descriptor`, and Metal has initialised it. `UNINITIALIZED` because the
    // first use is a render-pass clear, which is what the state machine
    // expects of a texture nothing has written yet.
    Some(unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Metal>(
            hal_texture,
            descriptor,
            wgpu::TextureUses::UNINITIALIZED,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::Shared;

    /// The whole mechanism in one assertion: a texture the renderer's device
    /// can draw into, whose pixels a second, independent reader sees.
    #[test]
    fn a_rendered_shared_surface_is_readable_through_its_pixel_buffer() {
        let instance = wgpu::Instance::default();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            return;
        };
        let Some(shared) = Shared::new(&device, 8, 4) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
        queue.submit([encoder.finish()]);
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");

        let buffer = shared.buffer();
        let pixels = locked(&buffer);
        assert_eq!(
            &pixels[..4],
            &[255, 0, 0, 255],
            "blue in BGRA is B=255, R=0"
        );
    }

    /// Read the first pixel through the `CVPixelBuffer` view of the surface.
    fn locked(buffer: &core_video::pixel_buffer::CVPixelBuffer) -> Vec<u8> {
        use core_foundation::base::TCFType;
        use core_video::pixel_buffer::{
            CVPixelBufferGetBaseAddress, CVPixelBufferLockBaseAddress,
            CVPixelBufferUnlockBaseAddress,
        };
        // SAFETY: the buffer is IOSurface-backed and alive for this borrow;
        // lock/unlock are balanced and the base address is valid between them.
        unsafe {
            let raw = buffer.as_concrete_TypeRef();
            assert_eq!(CVPixelBufferLockBaseAddress(raw, 1), 0);
            let base: *const u8 = CVPixelBufferGetBaseAddress(raw).cast();
            let pixels = std::slice::from_raw_parts(base, 4).to_vec();
            assert_eq!(CVPixelBufferUnlockBaseAddress(raw, 1), 0);
            pixels
        }
    }
}
