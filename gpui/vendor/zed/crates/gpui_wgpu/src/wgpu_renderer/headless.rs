//! LUMA LOCAL EDIT: a `WgpuRenderer` with no window, for pixel tests.
//!
//! It draws a scene with the window's own pipelines into an offscreen RGBA
//! target, and reads the target back when asked. It needs a GPU but no
//! display server.

use super::WgpuRenderer;
use crate::{WgpuAtlas, WgpuContext, WgpuSurfaceConfig};
use anyhow::{Context as _, Result};
use gpui::{DevicePixels, Scene, Size};
use std::sync::{Arc, OnceLock};

/// One device for every headless renderer in the process, as the windows of
/// an app share one.
fn context() -> Result<&'static WgpuContext> {
    static CONTEXT: OnceLock<Result<WgpuContext, String>> = OnceLock::new();
    CONTEXT
        .get_or_init(|| WgpuContext::new_headless().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

pub struct WgpuHeadlessRenderer {
    context: &'static WgpuContext,
    renderer: WgpuRenderer,
    target: Option<wgpu::Texture>,
}

impl WgpuHeadlessRenderer {
    pub fn new() -> Result<Self> {
        let context = context()?;
        let mut renderer = WgpuRenderer::new_internal(
            None,
            context,
            None,
            WgpuSurfaceConfig {
                size: Size {
                    width: DevicePixels(1),
                    height: DevicePixels(1),
                },
                transparent: false,
                preferred_present_mode: None,
            },
            None,
            Arc::new(WgpuAtlas::from_context(context)),
        )?;
        // `draw` acquires a swapchain image; with no surface it must return
        // at once.
        renderer.surface_configured = false;
        Ok(Self {
            context,
            renderer,
            target: None,
        })
    }

    pub fn sprite_atlas(&self) -> &Arc<WgpuAtlas> {
        self.renderer.sprite_atlas()
    }

    /// The device every headless renderer draws with. See
    /// [`gpui::Window::wgpu_device`].
    pub fn wgpu_device(&self) -> gpui::WgpuDevice {
        self.context.shared_device()
    }

    /// Draws `scene` into the target and submits the work without waiting
    /// for it, as presenting a frame would.
    pub fn render(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("cannot render at {size:?}");
        }
        let renderer = &mut self.renderer;
        renderer.update_drawable_size(size);
        let config = &renderer.surface_config;
        let extent = wgpu::Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        };
        let device = Arc::clone(&renderer.resources().device);
        if self
            .target
            .as_ref()
            .is_none_or(|target| target.size() != extent)
        {
            self.target = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("headless_target"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
        }
        let view = self
            .target
            .as_ref()
            .expect("the target was just made")
            .create_view(&wgpu::TextureViewDescriptor::default());

        renderer.atlas.before_frame();
        renderer.ensure_intermediate_textures();
        renderer.write_globals();
        // A scope rather than the renderer's uncaptured-error hook: that hook
        // is per device, and every headless renderer shares the device.
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let recorded = renderer.record_frame(scene, &view);
        let error = gpui::block_on(validation.pop());
        recorded?;
        match error {
            Some(error) => Err(anyhow::anyhow!("GPU validation error: {error}")),
            None => Ok(()),
        }
    }

    /// Draws `scene` and returns its pixels: width, height and tightly packed
    /// RGBA rows.
    pub fn render_to_rgba(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<(u32, u32, Vec<u8>)> {
        self.render(scene, size)?;
        let target = self.target.as_ref().expect("render made the target");
        debug_assert_eq!(target.format(), wgpu::TextureFormat::Rgba8Unorm);
        let resources = self.renderer.resources();
        let (width, height) = (target.width(), target.height());
        let row = width as usize * 4;
        let padded_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = resources.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("headless_readback"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder =
            resources
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("headless_readback"),
                });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: None,
                },
            },
            target.size(),
        );
        let submission = resources.queue.submit(std::iter::once(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).ok();
        });
        resources
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .context("waiting for the frame")?;
        receiver
            .recv()
            .context("the readback was dropped")?
            .context("mapping the readback")?;
        let pixels = slice
            .get_mapped_range()
            .context("reading the readback")?
            .chunks(padded_row as usize)
            .flat_map(|padded| &padded[..row])
            .copied()
            .collect();
        Ok((width, height, pixels))
    }
}
