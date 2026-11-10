//! The sized render targets a frame draws into, reallocated on a size,
//! destination or sample-count change, and the shadow-map texture arrays.

use crate::viewport::PRESENTATION_SLOTS;

use super::{Channels, Destination, Renderer, CASCADE_COUNT, DEPTH_FORMAT, SCENE_FORMAT};

pub(super) struct Targets {
    pub(super) width: u32,
    pub(super) height: u32,
    /// Size of the haze target, which runs at `haze_resolution` of the output.
    pub(super) haze_width: u32,
    pub(super) haze_height: u32,
    pub(super) destination: Destination,
    /// The multisampled scene colour; `None` at one sample, where the scene
    /// pass draws straight into `scene`.
    pub(super) msaa_color: Option<wgpu::TextureView>,
    /// Scene-pass samples per pixel.
    pub(super) samples: u32,
    pub(super) msaa_surface_depth: Option<wgpu::TextureView>,
    pub(super) msaa_depth: wgpu::TextureView,
    pub(super) scene: wgpu::TextureView,
    pub(super) depth: wgpu::TextureView,
    pub(super) haze: wgpu::TextureView,
    pub(super) fog: crate::fog_grid::Targets,
    /// Independent presentation resources, one per in-flight frame.
    /// Intermediate passes may be shared because queue submissions execute in
    /// order; each submission's final output must remain private until the
    /// frame it holds is off the screen.
    ///
    /// Each is allocated the first time its slot draws at this size, not with
    /// the rest. A resize drag hands every frame a new size, so it would
    /// otherwise allocate every slot's surface on every frame and use one.
    pub(super) presentations: [Option<PresentationTarget>; PRESENTATION_SLOTS],
    /// 256-byte-aligned row pitch of a staged target's readback buffer.
    pub(super) bytes_per_row: u32,
}

/// Where one in-flight frame is written.
pub(super) enum PresentationTarget {
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
    pub(super) fn view(&self) -> &wgpu::TextureView {
        match self {
            Self::Staged { view, .. } => view,
            Self::Shared(shared) => shared.view(),
        }
    }

    /// The format this target was allocated in, which is what the passes
    /// that write it must target. It can differ from what was asked for: an
    /// HDR frame with no shareable memory is staged as SDR.
    pub(super) fn channels(&self) -> Channels {
        Channels::of(self.view().texture().format())
    }
}

impl Targets {
    /// The presentation target `slot` draws into. Allocated by
    /// [`Renderer::targets`] for the slot it was asked for.
    pub(super) fn presentation(&self, slot: usize) -> &PresentationTarget {
        self.presentations[slot]
            .as_ref()
            .expect("the slot's presentation is allocated with the targets")
    }
}

impl Renderer {
    pub(super) fn targets(
        &mut self,
        width: u32,
        height: u32,
        haze: (u32, u32),
        destination: Destination,
        grid_fog: bool,
        surface_depth_cull: bool,
        fog_tile: u32,
        samples: u32,
        slot: usize,
    ) -> &Targets {
        let stale = self.targets.as_ref().is_none_or(|t| {
            t.width != width
                || t.height != height
                || !t.destination.same_targets(destination)
                || (t.haze_width, t.haze_height) != haze
                || t.samples != samples
        });
        if stale {
            let device = &self.gpu.device;
            let memory = &mut self.target_memory;
            // Sized by its own cells, not the viewport: see `ensure`.
            let fog = match self.targets.take() {
                Some(targets) => targets.fog,
                None => crate::fog_grid::Targets::new(device, memory, None, fog_tile),
            };
            let mut sized = |(w, h), samples, format, usage, label| {
                memory
                    .texture(
                        device,
                        &wgpu::TextureDescriptor {
                            label: Some(label),
                            size: wgpu::Extent3d {
                                width: w,
                                height: h,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: samples,
                            dimension: wgpu::TextureDimension::D2,
                            format,
                            usage,
                            view_formats: &[],
                        },
                    )
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            let read =
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            let size = (width, height);
            self.targets = Some(Targets {
                width,
                height,
                haze_width: haze.0,
                haze_height: haze.1,
                destination,
                msaa_color: (samples > 1)
                    .then(|| sized(size, samples, SCENE_FORMAT, TRANSIENT, "scene-msaa")),
                samples,
                msaa_surface_depth: None,
                msaa_depth: sized(size, samples, DEPTH_FORMAT, TRANSIENT, "depth-msaa"),
                scene: sized(size, 1, SCENE_FORMAT, read, "scene"),
                depth: sized(size, 1, DEPTH_FORMAT, read, "depth"),
                haze: sized(
                    haze,
                    1,
                    SCENE_FORMAT,
                    read | wgpu::TextureUsages::STORAGE_BINDING,
                    "haze",
                ),
                fog,
                presentations: Default::default(),
                bytes_per_row: padded_row(width),
            });
        }
        if self.targets.as_ref().expect("just populated").presentations[slot].is_none() {
            let target = self.presentation_target(width, height, destination);
            self.targets.as_mut().expect("just populated").presentations[slot] = Some(target);
        }
        if surface_depth_cull {
            let targets = self.targets.as_mut().expect("just populated");
            if targets.msaa_surface_depth.is_none() {
                targets.msaa_surface_depth = Some(
                    self.target_memory
                        .texture(
                            &self.gpu.device,
                            &wgpu::TextureDescriptor {
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
                            },
                        )
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                );
            }
        }
        if grid_fog {
            self.targets.as_mut().expect("just populated").fog.ensure(
                &self.gpu.device,
                &mut self.target_memory,
                [width, height],
                fog_tile,
            );
        }
        self.targets.as_ref().expect("just populated")
    }
}

/// The 256-byte-aligned row pitch of a staged `width`-pixel readback.
fn padded_row(width: u32) -> u32 {
    (width * 4).div_ceil(256) * 256
}

/// An attachment that lives only inside the pass that clears it. The scene
/// pass's multisampled colour and depth are cleared, then resolved or
/// discarded, and no other pass reads them. On a tile-based GPU (every Apple
/// GPU) that makes them memoryless: no memory and no bandwidth. It matters
/// most in a resize drag, which reallocates the targets every frame and would
/// otherwise page in and zero-fill the two largest of them each time.
const TRANSIENT: wgpu::TextureUsages =
    wgpu::TextureUsages::RENDER_ATTACHMENT.union(wgpu::TextureUsages::TRANSIENT_ATTACHMENT);

impl Renderer {
    /// One slot's output at `width` x `height`: memory the compositor can
    /// sample where `destination` is the compositor and the platform shares
    /// memory, and a staged texture with its readback buffer otherwise.
    fn presentation_target(
        &self,
        width: u32,
        height: u32,
        destination: Destination,
    ) -> PresentationTarget {
        if matches!(destination, Destination::Compositor(_)) {
            if let Some(shared) =
                crate::share::Shared::new(&self.gpu, width, height, destination.channels())
            {
                return PresentationTarget::Shared(shared);
            }
        }
        let channels = destination.staged_channels();
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
                size: u64::from(padded_row(width) * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
        }
    }
}

pub(super) fn shadow_texture_array(
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
