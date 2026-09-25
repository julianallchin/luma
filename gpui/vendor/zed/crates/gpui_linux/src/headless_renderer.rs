use gpui::{DevicePixels, PlatformAtlas, PlatformHeadlessRenderer, Scene, Size};
use gpui_wgpu::WgpuHeadlessRenderer;
use std::sync::Arc;

/// Returns a renderer that draws test windows offscreen with wgpu, or `None`
/// when there is no GPU to draw with.
pub fn headless_renderer() -> Option<Box<dyn PlatformHeadlessRenderer>> {
    match WgpuHeadlessRenderer::new() {
        Ok(renderer) => Some(Box::new(LinuxHeadlessRenderer(renderer))),
        Err(error) => {
            log::error!("no headless renderer: {error:#}");
            None
        }
    }
}

struct LinuxHeadlessRenderer(WgpuHeadlessRenderer);

impl PlatformHeadlessRenderer for LinuxHeadlessRenderer {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        let (width, height, pixels) = self.0.render_to_rgba(scene, size)?;
        image::RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| anyhow::anyhow!("readback does not fill {width}x{height}"))
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        self.0.render(scene, size)
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.0.sprite_atlas().clone()
    }

    fn wgpu_device(&self) -> Option<gpui::WgpuDevice> {
        Some(self.0.wgpu_device())
    }
}
