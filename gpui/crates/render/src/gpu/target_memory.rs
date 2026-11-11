//! Memory for the render targets whose size follows the viewport.
//!
//! # Why the targets do not come straight from the device
//!
//! A resize drag hands the renderer a size it has never seen on every frame,
//! and every frame reallocates the targets sized to it. A fresh Metal
//! allocation is cheap to ask for and expensive to first use: the kernel maps
//! and zero-fills its pages when the command buffer that touches it is
//! submitted, outside every GPU timestamp and every CPU span the renderer
//! records. At a retina panel's size that doubled the frame for as long as the
//! drag lasted (`resize_probe`).
//!
//! A heap is memory that is already mapped. A texture placed in one costs a
//! sub-allocation and nothing at submit, and when the old size's textures die
//! their space is handed to the next size's. So on Metal the sized targets are
//! placed in heaps this type keeps; elsewhere, and for any texture it cannot
//! describe, it is `Device::create_texture`.
//!
//! # What a caller must know
//!
//! A placed texture starts with whatever the heap held, not zeroes, and wgpu
//! believes it initialised (a texture made from a backend handle has no lazy
//! clear). So [`TargetMemory::texture`] places only textures that can be
//! rendered to, and clears each before the frame that allocated it uses it
//! ([`TargetMemory::clear`]); [`TargetMemory::written_texture`] places any,
//! uncleared, for a caller whose passes write every texel before reading.

/// Where the sized targets are allocated, and the textures it placed that
/// have not been cleared yet.
pub(crate) struct TargetMemory {
    #[cfg(target_os = "macos")]
    heaps: metal::Heaps,
    /// Placed since the last [`Self::clear`]: their first contents are the
    /// heap's previous tenants.
    unclear: Vec<wgpu::Texture>,
}

impl TargetMemory {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            heaps: metal::Heaps::default(),
            unclear: Vec::new(),
        }
    }

    /// A texture as `descriptor` describes it, placed in a heap where the
    /// platform has them and the texture can be cleared by a render pass.
    pub(crate) fn texture(
        &mut self,
        device: &wgpu::Device,
        descriptor: &wgpu::TextureDescriptor<'_>,
    ) -> wgpu::Texture {
        #[cfg(target_os = "macos")]
        if descriptor
            .usage
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            if let Some(texture) = self.heaps.place(device, descriptor) {
                self.unclear.push(texture.clone());
                return texture;
            }
        }
        device.create_texture(descriptor)
    }

    /// A texture placed in a heap without a clear, where the platform has
    /// heaps. Only for a texture whose every texel the frame that uses it
    /// writes before it reads any: it starts with the heap's previous tenant.
    pub(crate) fn written_texture(
        &mut self,
        device: &wgpu::Device,
        descriptor: &wgpu::TextureDescriptor<'_>,
    ) -> wgpu::Texture {
        #[cfg(target_os = "macos")]
        if let Some(texture) = self.heaps.place(device, descriptor) {
            return texture;
        }
        device.create_texture(descriptor)
    }

    /// Clear everything placed since the last call, before anything reads it.
    /// One render pass per texture, each a load-clear with nothing drawn.
    pub(crate) fn clear(&mut self, encoder: &mut wgpu::CommandEncoder) {
        for texture in self.unclear.drain(..) {
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let depth = texture.format().is_depth_stencil_format();
            let color = (!depth).then(|| wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            });
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("target-clear"),
                color_attachments: &[color],
                depth_stencil_attachment: depth.then(|| wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
        }
    }
}

#[cfg(target_os = "macos")]
mod metal {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{
        MTLDevice, MTLHazardTrackingMode, MTLHeap, MTLHeapDescriptor, MTLHeapType, MTLPixelFormat,
        MTLStorageMode, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage,
    };

    /// The smallest heap worth making: a few targets of an ordinary panel.
    const MIN_HEAP: usize = 64 << 20;

    /// The heaps placed textures live in, newest last.
    #[derive(Default)]
    pub(super) struct Heaps {
        heaps: Vec<Retained<ProtocolObject<dyn MTLHeap>>>,
    }

    impl Heaps {
        /// Place `descriptor` in a heap, making a bigger one when none has
        /// room. `None` when this is not a Metal device or the texture is one
        /// this module does not describe; the caller allocates it as usual.
        pub(super) fn place(
            &mut self,
            device: &wgpu::Device,
            descriptor: &wgpu::TextureDescriptor<'_>,
        ) -> Option<wgpu::Texture> {
            let (format, usage, kind) = describe(descriptor)?;
            // SAFETY: `as_hal` only lends the backend device for this borrow;
            // a device on another backend yields `None`.
            let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
            let metal_device = hal.raw_device();
            let texture_descriptor = MTLTextureDescriptor::new();
            texture_descriptor.setTextureType(kind);
            texture_descriptor.setPixelFormat(format);
            // SAFETY: wgpu has validated the extent and sample count against
            // the device's limits before a caller can describe a target.
            unsafe {
                texture_descriptor.setWidth(descriptor.size.width as usize);
                texture_descriptor.setHeight(descriptor.size.height as usize);
                if kind == MTLTextureType::Type3D {
                    texture_descriptor.setDepth(descriptor.size.depth_or_array_layers as usize);
                }
                texture_descriptor.setSampleCount(descriptor.sample_count as usize);
                texture_descriptor.setMipmapLevelCount(1);
            }
            texture_descriptor.setUsage(usage);
            texture_descriptor.setStorageMode(MTLStorageMode::Private);
            // wgpu's Metal backend leaves ordering between passes to Metal's
            // own tracking, which heap resources only get when asked for.
            texture_descriptor.setHazardTrackingMode(MTLHazardTrackingMode::Tracked);
            let need = metal_device.heapTextureSizeAndAlignWithDescriptor(&texture_descriptor);

            let raw = self
                .heaps
                .iter()
                .rev()
                .filter(|heap| heap.maxAvailableSizeWithAlignment(need.align) >= need.size)
                .find_map(|heap| heap.newTextureWithDescriptor(&texture_descriptor))
                .or_else(|| {
                    // Room for twice what is placed now and this texture
                    // again: a drag keeps the frames in flight alive beside
                    // the one being drawn, so the live set is a few sizes'
                    // worth, and doubling keeps the number of heaps small.
                    let used: usize = self.heaps.iter().map(|heap| heap.usedSize()).sum();
                    let size = (2 * (used + need.size)).max(MIN_HEAP);
                    let heap_descriptor = MTLHeapDescriptor::new();
                    heap_descriptor.setType(MTLHeapType::Automatic);
                    heap_descriptor.setStorageMode(MTLStorageMode::Private);
                    heap_descriptor.setHazardTrackingMode(MTLHazardTrackingMode::Tracked);
                    // A heap the device cannot make is `nil`, which the
                    // caller answers by allocating normally.
                    heap_descriptor.setSize(size);
                    let heap = metal_device.newHeapWithDescriptor(&heap_descriptor)?;
                    let texture = heap.newTextureWithDescriptor(&texture_descriptor)?;
                    self.heaps.push(heap);
                    Some(texture)
                })?;
            drop(hal);
            // A heap nothing lives in any more is memory held for nothing,
            // unless it is the newest, which the next size will use.
            let newest = self.heaps.len().saturating_sub(1);
            let mut index = 0;
            self.heaps.retain(|heap| {
                let keep = index == newest || heap.usedSize() > 0;
                index += 1;
                keep
            });

            // SAFETY: `raw` was made on this device to `descriptor`'s format,
            // extent, sample count and usages. A texture placed in a heap
            // retains the heap, so the memory outlives it.
            let hal_texture = unsafe {
                wgpu::hal::metal::Device::texture_from_raw(
                    raw,
                    descriptor.format,
                    kind,
                    1,
                    1,
                    wgpu::hal::CopyExtent {
                        width: descriptor.size.width,
                        height: descriptor.size.height,
                        depth: if kind == MTLTextureType::Type3D {
                            descriptor.size.depth_or_array_layers
                        } else {
                            1
                        },
                    },
                    None,
                )
            };
            // SAFETY: as above; `UNINITIALIZED` because its first use is the
            // clear `TargetMemory::clear` encodes.
            Some(unsafe {
                device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                    hal_texture,
                    descriptor,
                    wgpu::TextureUses::UNINITIALIZED,
                )
            })
        }
    }

    /// The Metal format, usage and type for the targets this module places:
    /// single-level 2D textures (multisampled or not) and 3D ones, in the
    /// formats the renderer sizes to the viewport. Anything else is `None`.
    fn describe(
        descriptor: &wgpu::TextureDescriptor<'_>,
    ) -> Option<(MTLPixelFormat, MTLTextureUsage, MTLTextureType)> {
        use wgpu::TextureFormat as F;
        use wgpu::TextureUsages as U;
        let flat = descriptor.dimension == wgpu::TextureDimension::D2
            && descriptor.size.depth_or_array_layers == 1;
        let volume =
            descriptor.dimension == wgpu::TextureDimension::D3 && descriptor.sample_count == 1;
        if !(flat || volume)
            || descriptor.mip_level_count != 1
            || !descriptor.view_formats.is_empty()
            || !(U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::STORAGE_BINDING | U::COPY_SRC)
                .contains(descriptor.usage)
        {
            return None;
        }
        let format = match descriptor.format {
            F::Rgba16Float => MTLPixelFormat::RGBA16Float,
            F::R16Float => MTLPixelFormat::R16Float,
            F::Depth32Float => MTLPixelFormat::Depth32Float,
            F::R32Float => MTLPixelFormat::R32Float,
            F::Rgba32Float => MTLPixelFormat::RGBA32Float,
            _ => return None,
        };
        let mut usage = MTLTextureUsage::Unknown;
        if descriptor.usage.contains(U::RENDER_ATTACHMENT) {
            usage |= MTLTextureUsage::RenderTarget;
        }
        if descriptor
            .usage
            .intersects(U::TEXTURE_BINDING | U::STORAGE_BINDING)
        {
            usage |= MTLTextureUsage::ShaderRead;
        }
        if descriptor.usage.contains(U::STORAGE_BINDING) {
            usage |= MTLTextureUsage::ShaderWrite;
        }
        let kind = if volume {
            MTLTextureType::Type3D
        } else if descriptor.sample_count > 1 {
            MTLTextureType::Type2DMultisample
        } else {
            MTLTextureType::Type2D
        };
        Some((format, usage, kind))
    }
}
