//! Shared volume lighting, native source integration, and conservative block bounds.

/// Diagnostic spatial-resolution sweep. Fixed for the process so target
/// allocation and compute dispatch always agree. Mac uses 16-pixel far-field
/// cells after a native-pixel/motion quality sweep; other platforms retain 8.
/// Near-source transport and truss-shadow intervals stay at native resolution.
pub(crate) fn tile_size() -> u32 {
    static SIZE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("LUMA_FOG_TILE_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| matches!(n, 4 | 8 | 16 | 32))
            .unwrap_or(if cfg!(target_os = "macos") { 16 } else { 8 })
    })
}

/// Keep the diagnostic shader variant and its classification dispatch paired.
pub(crate) fn block_visibility() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| !std::env::var_os("LUMA_FOG_BLOCKS").is_some_and(|v| v == "0"))
}
pub(crate) const SLICES: u32 = 128;
pub(crate) const BLOCK_SIDE: u32 = 4;
const BLOCK_LANES: u32 = BLOCK_SIDE * BLOCK_SIDE * BLOCK_SIDE;
const BLOCK_WORDS: usize = 2 * crate::light_index::MASK_WORDS;
pub(crate) const SOURCE_INNER: f32 = 3.0;
pub(crate) const SOURCE_OUTER: f32 = 4.0;
pub(crate) const BROAD_WASH: f32 = 0.65;

/// Conservative shared-volume light counts from an explicit diagnostic readback.
/// A candidate may still fail the per-cell range, cone or shadow test.
#[derive(Debug, serde::Serialize)]
pub struct FogBlockStats {
    /// Full viewport dimensions in pixels.
    pub image_size: [u32; 2],
    /// Shared-volume cell counts along x, y and z.
    pub grid_size: [u32; 3],
    /// Cell count along each side of a classification block.
    pub block_size: u32,
    /// Classification block counts, including partially padded edge blocks.
    pub blocks: [u32; 3],
    /// X-major records: candidate count, then wholly visible candidate count.
    pub counts: Vec<[u32; 2]>,
}

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
    /// `integral`'s alpha channel alone, produced before the lit grid.
    pub(crate) transmittance: wgpu::TextureView,
    /// Per-slice optical depth (slice 0: the camera's entry transmittance),
    /// written by `fog-transmittance` and reused by `fog-integrate` so the
    /// lit prefix skips the density quadrature. R32Float keeps the f32 exact.
    pub(crate) tau: wgpu::TextureView,
}

impl Targets {
    pub(crate) fn new(device: &wgpu::Device, viewport: Option<[u32; 2]>) -> Self {
        let size = viewport.unwrap_or([0, 0]);
        let columns = size[0].div_ceil(tile_size()).max(1);
        let rows = size[1].div_ceil(tile_size()).max(1);
        let slices = if viewport.is_some() { SLICES } else { 1 };
        let typed_texture = |label, depth, format| {
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
                    format,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let texture = |label, depth| typed_texture(label, depth, wgpu::TextureFormat::Rgba16Float);
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
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            incident: texture("fog-segments", slices),
            integral: texture("fog-integral", slices + 1),
            transmittance: texture("fog-transmittance", slices + 1),
            tau: typed_texture("fog-tau", slices + 1, wgpu::TextureFormat::R32Float),
        }
    }

    pub(crate) fn ensure(&mut self, device: &wgpu::Device, viewport: [u32; 2]) {
        if self.size != viewport {
            *self = Self::new(device, Some(viewport));
        }
    }

    pub(crate) fn block_stats(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<FogBlockStats> {
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fog-block-readback"),
            size: self.candidates.size(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.candidates, 0, &readback, 0, self.candidates.size());
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(anyhow::Error::msg)?;
        receiver.recv().map_err(anyhow::Error::msg)??;
        let data = readback
            .slice(..)
            .get_mapped_range()
            .map_err(anyhow::Error::msg)?;
        let words: &[u32] = bytemuck::cast_slice(&data);
        let counts = words
            .chunks_exact(BLOCK_WORDS)
            .map(|block| {
                let (candidates, visible) = block.split_at(crate::light_index::MASK_WORDS);
                anyhow::ensure!(
                    candidates.iter().zip(visible).all(|(a, b)| b & !a == 0),
                    "visible fog lights must be candidates"
                );
                Ok([
                    candidates.iter().map(|v| v.count_ones()).sum(),
                    visible.iter().map(|v| v.count_ones()).sum(),
                ])
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let grid_size = [
            self.size[0].div_ceil(tile_size()),
            self.size[1].div_ceil(tile_size()),
            SLICES,
        ];
        Ok(FogBlockStats {
            image_size: self.size,
            grid_size,
            block_size: BLOCK_SIDE,
            blocks: grid_size.map(|n| n.div_ceil(BLOCK_SIDE)),
            counts,
        })
    }
}
