//! Shared volume lighting, native source integration, and conservative block bounds.

/// Diagnostic spatial-resolution sweep. Fixed for the process so target
/// allocation and compute dispatch always agree; production defaults to 8.
pub(crate) fn tile_size() -> u32 {
    static SIZE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("LUMA_FOG_TILE_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| matches!(n, 4 | 8 | 16 | 32))
            .unwrap_or(8)
    })
}
pub(crate) const SLICES: u32 = 128;
pub(crate) const BLOCK_SIDE: u32 = 4;
const BLOCK_LANES: u32 = BLOCK_SIDE * BLOCK_SIDE * BLOCK_SIDE;
const BLOCK_WORDS: usize = 2 * crate::light_index::MASK_WORDS;
pub(crate) const SOURCE_INNER: f32 = 3.0;
pub(crate) const SOURCE_OUTER: f32 = 4.0;
pub(crate) const BROAD_WASH: f32 = 0.65;

pub(crate) fn prelude() -> String {
    format!(
        "const FOG_SLICES: u32 = {SLICES}u;\nconst FOG_BLOCK_SIDE: u32 = {BLOCK_SIDE}u;\nconst FOG_BLOCK_LANES: u32 = {BLOCK_LANES}u;\nconst FOG_BLOCK_WORDS: u32 = {BLOCK_WORDS}u;\nconst FOG_SOURCE_INNER: f32 = {SOURCE_INNER:?};\nconst FOG_SOURCE_OUTER: f32 = {SOURCE_OUTER:?};\nconst FOG_BROAD_WASH: f32 = {BROAD_WASH:?};\n"
    )
}

/// Retained across blackouts; allocated at viewport size only on first use.
pub(crate) struct Targets {
    size: [u32; 2],
    pub(crate) incident: wgpu::TextureView,
    pub(crate) columns: wgpu::TextureView,
    pub(crate) candidates: wgpu::Buffer,
    pub(crate) integral: wgpu::TextureView,
}

impl Targets {
    pub(crate) fn new(device: &wgpu::Device, viewport: Option<[u32; 2]>) -> Self {
        let size = viewport.unwrap_or([0, 0]);
        let columns = size[0].div_ceil(tile_size()).max(1);
        let rows = size[1].div_ceil(tile_size()).max(1);
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
        let column_info = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("fog-columns"),
                size: wgpu::Extent3d {
                    width: columns,
                    height: rows,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            size,
            columns: column_info,
            candidates: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fog-block-candidates"),
                size: u64::from(
                    columns.div_ceil(BLOCK_SIDE)
                        * rows.div_ceil(BLOCK_SIDE)
                        * slices.div_ceil(BLOCK_SIDE),
                ) * BLOCK_WORDS as u64
                    * 4,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }),
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
