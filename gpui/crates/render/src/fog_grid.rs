//! Shared configuration for the broad-wash grid and its analytic source region.

pub(crate) const TILE_SIZE: u32 = 16;
pub(crate) const SLICES: u32 = 128;
pub(crate) const SOURCE_INNER: f32 = 2.0;
pub(crate) const SOURCE_OUTER: f32 = 4.0;
pub(crate) const BROAD_WASH: f32 = 0.65;

pub(crate) fn prelude() -> String {
    format!(
        "const FOG_SLICES: u32 = {SLICES}u;\nconst FOG_SOURCE_INNER: f32 = {SOURCE_INNER:?};\nconst FOG_SOURCE_OUTER: f32 = {SOURCE_OUTER:?};\nconst FOG_BROAD_WASH: f32 = {BROAD_WASH:?};\n"
    )
}

/// Retained across blackouts; allocated at viewport size only on first use.
pub(crate) struct Targets {
    size: [u32; 2],
    pub(crate) incident: wgpu::TextureView,
    pub(crate) integral: wgpu::TextureView,
}

impl Targets {
    pub(crate) fn new(device: &wgpu::Device, viewport: Option<[u32; 2]>) -> Self {
        let size = viewport.unwrap_or([0, 0]);
        let columns = size[0].div_ceil(TILE_SIZE).max(1);
        let rows = size[1].div_ceil(TILE_SIZE).max(1);
        let slices = if viewport.is_some() { SLICES } else { 1 };
        let texture = |label, depth| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: columns,
                        height: rows,
                        depth_or_array_layers: depth,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        Self {
            size,
            incident: texture("fog-segments", slices),
            integral: texture("fog-integral", slices + 1),
        }
    }

    pub(crate) fn ensure(&mut self, device: &wgpu::Device, viewport: [u32; 2]) {
        if self.size != viewport {
            *self = Self::new(device, Some(viewport));
        }
    }
}
