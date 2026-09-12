//! LUMA LOCAL EDIT: not upstream.
//!
//! MetalFX Spatial upscaling for surfaces painted with
//! `Window::paint_upscaled_surface`: a renderer that drew a frame smaller than
//! its bounds gets a content-aware upscale instead of the compositor's
//! bilinear stretch.
//!
//! The scaler works on the surface's bytes as they are. MetalFX rejects sRGB
//! formats, so the IOSurface is wrapped as `BGRA8Unorm` — which is also how
//! the bilinear path wraps it — and processed in `Perceptual` mode, which
//! expects exactly those sRGB-encoded values. The output is the same encoding
//! in the same format, drawn through the same quad at 1:1, so a flat colour
//! comes out the code it went in.
//!
//! `LUMA_UPSCALER=bilinear` turns it off. `LUMA_UPSCALER_DUMP=<dir>` writes
//! two frames' scaler input and output as PNGs.

use std::{
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};

use block::ConcreteBlock;
use foreign_types::ForeignTypeRef as _;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{MTLCommandBuffer, MTLDevice, MTLPixelFormat, MTLTexture};
use objc2_metal_fx::{
    MTLFXSpatialScaler, MTLFXSpatialScalerBase, MTLFXSpatialScalerColorProcessingMode,
    MTLFXSpatialScalerDescriptor,
};

/// Output textures per size pair. The window keeps two drawables in flight,
/// and each one's command buffer reads the output its scaler wrote.
const OUTPUTS: usize = 2;

/// Upscaled frames `LUMA_UPSCALER_DUMP` writes: one as a still stage's haze
/// settles (it then stops drawing), one well into playback.
const DUMP_FRAMES: [u64; 2] = [16, 240];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    input: (u64, u64),
    output: (u64, u64),
}

struct Scaler {
    key: Key,
    scaler: Retained<ProtocolObject<dyn MTLFXSpatialScaler>>,
    outputs: [metal::Texture; OUTPUTS],
    next: usize,
}

pub(crate) struct SurfaceUpscaler {
    enabled: bool,
    scaler: Option<Scaler>,
    /// A pair the device refused. Remembered so a refusal costs one attempt,
    /// not one per frame; any other pair is tried afresh.
    refused: Option<Key>,
    frames: u64,
    dump: Option<PathBuf>,
}

/// Whether surfaces may be upscaled with MetalFX on `device`. Decided and
/// logged once per process.
fn enabled(device: &metal::DeviceRef) -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let requested = std::env::var("LUMA_UPSCALER").ok();
        let (on, why) = match requested.as_deref() {
            Some("bilinear") => (false, "bilinear (LUMA_UPSCALER=bilinear)"),
            Some("metalfx") | None => {
                // SAFETY: a class method with no preconditions beyond a valid
                // device, which `device` is.
                if unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(objc_device(device)) } {
                    (true, "MetalFX Spatial (perceptual)")
                } else {
                    (false, "bilinear (MetalFX Spatial unsupported on this GPU)")
                }
            }
            Some(other) => {
                eprintln!("[upscaler] ignoring LUMA_UPSCALER={other:?}; expected metalfx|bilinear");
                (false, "bilinear (unrecognised LUMA_UPSCALER)")
            }
        };
        eprintln!("[upscaler] reduced-resolution surfaces: {why}");
        on
    })
}

impl SurfaceUpscaler {
    pub(crate) fn new(device: &metal::DeviceRef) -> Self {
        Self {
            enabled: enabled(device),
            scaler: None,
            refused: None,
            frames: 0,
            dump: std::env::var_os("LUMA_UPSCALER_DUMP").map(PathBuf::from),
        }
    }

    /// Encode an upscale of `input` to `output` pixels into `command_buffer`
    /// and return the texture holding it, for the caller to draw at 1:1.
    ///
    /// `None` means draw `input` as before: upscaling is off, the sizes
    /// already agree, the surface is being shrunk, or the device refused this
    /// pair.
    pub(crate) fn encode(
        &mut self,
        device: &metal::DeviceRef,
        command_buffer: &metal::CommandBufferRef,
        input: &metal::TextureRef,
        output: (u64, u64),
    ) -> Option<metal::Texture> {
        let key = Key {
            input: (input.width(), input.height()),
            output,
        };
        if !self.enabled
            || key.input == key.output
            || key.output.0 < key.input.0
            || key.output.1 < key.input.1
            || self.refused == Some(key)
        {
            return None;
        }
        if self.scaler.as_ref().is_none_or(|scaler| scaler.key != key) {
            // Drop the old pair before building the new one: at fullscreen
            // its outputs are 30 MB apiece.
            self.scaler = None;
            match Scaler::new(device, key) {
                Some(scaler) => self.scaler = Some(scaler),
                None => {
                    eprintln!(
                        "[upscaler] MetalFX refused {}x{} -> {}x{}; drawing bilinear",
                        key.input.0, key.input.1, key.output.0, key.output.1
                    );
                    self.refused = Some(key);
                    return None;
                }
            }
        }
        let scaler = self.scaler.as_mut()?;
        let target = scaler.outputs[scaler.next].clone();
        scaler.next = (scaler.next + 1) % OUTPUTS;
        // SAFETY: both textures belong to `device`, match the sizes and
        // formats the scaler was described with, and carry the usages it
        // reported; `command_buffer` is open and not yet committed.
        unsafe {
            scaler.scaler.setColorTexture(Some(objc_texture(input)));
            scaler.scaler.setOutputTexture(Some(objc_texture(&target)));
            scaler
                .scaler
                .encodeToCommandBuffer(objc_command_buffer(command_buffer));
        }

        self.frames += 1;
        if DUMP_FRAMES.contains(&self.frames)
            && let Some(dir) = &self.dump
        {
            dump(
                device,
                command_buffer,
                input,
                &target,
                dir.clone(),
                self.frames,
            );
        }
        Some(target)
    }
}

impl Scaler {
    fn new(device: &metal::DeviceRef, key: Key) -> Option<Self> {
        let started = Instant::now();
        // SAFETY: the setters take plain values; a descriptor MetalFX cannot
        // honour makes `newSpatialScalerWithDevice` return nil, not fault.
        let scaler = unsafe {
            let descriptor = MTLFXSpatialScalerDescriptor::new();
            descriptor.setInputWidth(key.input.0 as usize);
            descriptor.setInputHeight(key.input.1 as usize);
            descriptor.setOutputWidth(key.output.0 as usize);
            descriptor.setOutputHeight(key.output.1 as usize);
            descriptor.setColorTextureFormat(MTLPixelFormat::BGRA8Unorm);
            descriptor.setOutputTextureFormat(MTLPixelFormat::BGRA8Unorm);
            descriptor.setColorProcessingMode(MTLFXSpatialScalerColorProcessingMode::Perceptual);
            descriptor.newSpatialScalerWithDevice(objc_device(device))?
        };
        // SAFETY: a getter on a live scaler.
        let usage = unsafe { scaler.outputTextureUsage() }.0 as u64;
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(key.output.0);
        descriptor.set_height(key.output.1);
        descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        descriptor.set_storage_mode(metal::MTLStorageMode::Private);
        // The quad samples the output, whatever the scaler itself asks for.
        descriptor.set_usage(
            metal::MTLTextureUsage::from_bits_truncate(usage) | metal::MTLTextureUsage::ShaderRead,
        );
        let outputs = std::array::from_fn(|_| device.new_texture(&descriptor));
        eprintln!(
            "[upscaler] MetalFX scaler {}x{} -> {}x{} ready in {:.1} ms",
            key.input.0,
            key.input.1,
            key.output.0,
            key.output.1,
            ms(started.elapsed())
        );
        Some(Self {
            key,
            scaler,
            outputs,
            next: 0,
        })
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

// The casts below hand the `metal` crate's objects to `objc2` bindings. Both
// crates name the same Objective-C object through a thin pointer; the
// reference only lives as long as the borrow it was made from.

fn objc_device(device: &metal::DeviceRef) -> &ProtocolObject<dyn MTLDevice> {
    unsafe { &*(device.as_ptr() as *const ProtocolObject<dyn MTLDevice>) }
}

fn objc_texture(texture: &metal::TextureRef) -> &ProtocolObject<dyn MTLTexture> {
    unsafe { &*(texture.as_ptr() as *const ProtocolObject<dyn MTLTexture>) }
}

fn objc_command_buffer(buffer: &metal::CommandBufferRef) -> &ProtocolObject<dyn MTLCommandBuffer> {
    unsafe { &*(buffer.as_ptr() as *const ProtocolObject<dyn MTLCommandBuffer>) }
}

/// Copy `input` and `output` out after the scaler in `command_buffer` and
/// write them to `dir` as PNGs once it completes. A diagnostic.
fn dump(
    device: &metal::DeviceRef,
    command_buffer: &metal::CommandBufferRef,
    input: &metal::TextureRef,
    output: &metal::TextureRef,
    dir: PathBuf,
    frame: u64,
) {
    let readable = |texture: &metal::TextureRef| {
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(texture.width());
        descriptor.set_height(texture.height());
        descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        descriptor.set_storage_mode(metal::MTLStorageMode::Managed);
        descriptor.set_usage(metal::MTLTextureUsage::ShaderRead);
        let copy = device.new_texture(&descriptor);
        let blit = command_buffer.new_blit_command_encoder();
        blit.copy_from_texture(
            texture,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
            metal::MTLSize {
                width: texture.width(),
                height: texture.height(),
                depth: 1,
            },
            &copy,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
        );
        blit.synchronize_resource(&copy);
        blit.end_encoding();
        copy
    };
    let copies = [("input", readable(input)), ("output", readable(output))];
    let block = ConcreteBlock::new(move |_| {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            eprintln!("[upscaler] dump: cannot create {}: {error}", dir.display());
            return;
        }
        for (name, texture) in &copies {
            let (width, height) = (texture.width() as u32, texture.height() as u32);
            let mut pixels = vec![0u8; width as usize * height as usize * 4];
            texture.get_bytes(
                pixels.as_mut_ptr().cast(),
                u64::from(width) * 4,
                metal::MTLRegion {
                    origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                    size: metal::MTLSize {
                        width: u64::from(width),
                        height: u64::from(height),
                        depth: 1,
                    },
                },
                0,
            );
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let path = dir.join(format!("upscaler-{frame}-{name}-{width}x{height}.png"));
            match image::RgbaImage::from_raw(width, height, pixels).map(|image| image.save(&path)) {
                Some(Ok(())) => eprintln!("[upscaler] dump: wrote {}", path.display()),
                Some(Err(error)) => eprintln!("[upscaler] dump: {}: {error}", path.display()),
                None => eprintln!("[upscaler] dump: {name} has no pixels"),
            }
        }
    });
    command_buffer.add_completed_handler(&block.copy());
}
