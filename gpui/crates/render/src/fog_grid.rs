//! Shared volume lighting, native source integration, and conservative block bounds.

/// Far-field cell size in pixels at high quality. 16-pixel cells visibly
/// soften a truss shadow inside a wide beam. Near-source transport and
/// truss-shadow intervals stay at native resolution either way.
pub(crate) const TILE_SIZE: u32 = 8;

/// The far-field cell size in pixels at the medium quality setting.
pub(crate) const LOW_TILE_SIZE: u32 = 16;
pub(crate) const SLICES: u32 = 128;
pub(crate) const BLOCK_SIDE: u32 = 4;
const BLOCK_LANES: u32 = BLOCK_SIDE * BLOCK_SIDE * BLOCK_SIDE;
const BLOCK_WORDS: usize = 2 * crate::light_index::MASK_WORDS;
/// Distance from a broad wash's source, in metres, over which the per-pixel
/// near-source pass hands its beam to the shared grid. At 3/4 m a view full of
/// Sunstrip cells behind truss spent 11.8 ms in that pass; at 1.5/2 m the
/// frame took 4.6 ms and close-ups of the bars and strobes showed no visible
/// change (largest mean difference 0.06 of 256 levels).
pub(crate) const SOURCE_INNER: f32 = 1.5;
pub(crate) const SOURCE_OUTER: f32 = 2.0;
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

use crate::gpu::target_memory::TargetMemory;

/// Retained across blackouts; allocated at viewport size only on first use.
pub(crate) struct Targets {
    size: [u32; 2],
    /// Cell size in pixels the grid was allocated for.
    tile: u32,
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
    /// The grid for a `viewport` (or a placeholder before the first use),
    /// one cell per `tile` pixels.
    ///
    /// Its volumes and columns are placed in reused memory without a clear
    /// (`TargetMemory::written_texture`): on every frame that uses the grid,
    /// the pass producing each of them stores every texel before any pass
    /// reads it, one invocation per cell.
    pub(crate) fn new(
        device: &wgpu::Device,
        memory: &mut TargetMemory,
        viewport: Option<[u32; 2]>,
        tile: u32,
    ) -> Self {
        let size = viewport.unwrap_or([0, 0]);
        let columns = size[0].div_ceil(tile).max(1);
        let rows = size[1].div_ceil(tile).max(1);
        let slices = if viewport.is_some() { SLICES } else { 1 };
        let mut typed_texture = |label, (dimension, depth), format| {
            memory
                .written_texture(
                    device,
                    &wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width: columns,
                            height: rows,
                            depth_or_array_layers: depth,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension,
                        format,
                        usage: wgpu::TextureUsages::STORAGE_BINDING
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                )
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let volume = |depth| (wgpu::TextureDimension::D3, depth);
        let half = wgpu::TextureFormat::Rgba16Float;
        Self {
            size,
            tile,
            columns: typed_texture(
                "fog-columns",
                (wgpu::TextureDimension::D2, 1),
                wgpu::TextureFormat::Rgba32Float,
            ),
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
            incident: typed_texture("fog-segments", volume(slices), half),
            integral: typed_texture("fog-integral", volume(slices + 1), half),
            transmittance: typed_texture("fog-transmittance", volume(slices + 1), half),
            tau: typed_texture("fog-tau", volume(slices + 1), wgpu::TextureFormat::R32Float),
        }
    }

    /// Size the grid for `viewport`. A viewport that moves by less than a
    /// cell keeps the grid it has: the passes divide the viewport by the cell
    /// count, so the grid is the same either way.
    pub(crate) fn ensure(
        &mut self,
        device: &wgpu::Device,
        memory: &mut TargetMemory,
        viewport: [u32; 2],
        tile: u32,
    ) {
        let cells = |size: [u32; 2]| size.map(|side| side.div_ceil(tile).max(1));
        if self.size == [0, 0] || self.tile != tile || cells(self.size) != cells(viewport) {
            *self = Self::new(device, memory, Some(viewport), tile);
        }
        self.size = viewport;
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
            self.size[0].div_ceil(self.tile),
            self.size[1].div_ceil(self.tile),
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
