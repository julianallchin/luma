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
    pub(super) presentations: [PresentationTarget; PRESENTATION_SLOTS],
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
    ) -> &Targets {
        let stale = self.targets.as_ref().is_none_or(|t| {
            t.width != width
                || t.height != height
                || !t.destination.same_targets(destination)
                || (t.haze_width, t.haze_height) != haze
                || t.samples != samples
        });
        let channels = destination.staged_channels();
        if stale {
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
                if matches!(destination, Destination::Compositor(_)) {
                    if let Some(shared) =
                        crate::share::Shared::new(&self.gpu, width, height, destination.channels())
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
                msaa_color: (samples > 1).then(|| {
                    color(
                        width,
                        height,
                        samples,
                        wgpu::TextureUsages::RENDER_ATTACHMENT,
                        "scene-msaa",
                    )
                }),
                samples,
                msaa_surface_depth: None,
                msaa_depth: depth_texture(&self.gpu.device, width, height, samples, "depth-msaa"),
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
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING,
                    "haze",
                ),
                fog: crate::fog_grid::Targets::new(&self.gpu.device, None, fog_tile),
                presentations,
                bytes_per_row,
            });
        }
        if surface_depth_cull {
            let targets = self.targets.as_mut().expect("just populated");
            if targets.msaa_surface_depth.is_none() {
                targets.msaa_surface_depth = Some(
                    self.gpu
                        .device
                        .create_texture(&wgpu::TextureDescriptor {
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
                        })
                        .create_view(&wgpu::TextureViewDescriptor::default()),
                );
            }
        }
        if grid_fog {
            self.targets.as_mut().expect("just populated").fog.ensure(
                &self.gpu.device,
                [width, height],
                fog_tile,
            );
        }
        self.targets.as_ref().expect("just populated")
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
