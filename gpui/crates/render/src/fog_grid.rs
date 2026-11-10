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
/// Distance from a soft beam's source, in metres, over which the per-pixel
/// near-source pass hands its beam to the shared grid: the nearest any beam is
/// handed over ([`handover`]). At 3/4 m a view full of
/// Sunstrip cells behind truss spent 11.8 ms in that pass; at 1.5/2 m the
/// frame took 4.6 ms and close-ups of the bars and strobes showed no visible
/// change (largest mean difference 0.06 of 256 levels). The haze's forward
/// peak (`haze_phase.rs`) then put the brightest light near a source — a
/// truss silhouette or shadow shaft against a wash seen toward its lens —
/// in the grid's 8-pixel cells: the truss-shadow suite's oblique view was
/// 2.8 RMSE from the converged tracer; at 1.75/2.5 m it is 1.8, the suite 0.8 to 2.0.
pub(crate) const SOURCE_INNER: f32 = 1.75;
pub(crate) const SOURCE_OUTER: f32 = 2.5;
/// The band the handover takes, as a multiple of where it starts.
pub(crate) const HANDOVER_BAND: f32 = SOURCE_OUTER / SOURCE_INNER;
/// How soft a beam is across where the grid takes it, metres: the distance
/// its brightness would take to fall from its peak to nothing at the rate of
/// the steepest part of its profile. The grid's cells blur a beam by about
/// their own size, which a beam this soft does not show. It is the softness,
/// at `SOURCE_INNER`, of the 60 degree cell the distances above were measured
/// on, so a narrower beam is handed over further out, where it has opened to
/// the same softness, and a beam that never does is never handed over.
const SOFT_EDGE: f32 = 0.55;
/// The handover of a light the grid never takes.
pub(crate) const NEVER: f32 = 1.0e9;

/// Distance from `cone`'s lens, metres, at which the shared grid starts to
/// carry its haze; it carries all of it past [`HANDOVER_BAND`] times that,
/// and the per-pixel passes carry the rest. [`NEVER`] for a gobo, whose
/// pattern is finer than its beam, and for a beam still too sharp at its
/// range.
pub(crate) fn handover(cone: &crate::frame::FixtureCone) -> f32 {
    if cone.gobo != 0 {
        return NEVER;
    }
    // The profile across the beam a metre from its apex, and its steepest
    // fall there. Further off the same profile is as many times wider.
    const STEPS: usize = 48;
    let edge = cone.cos_field.clamp(0.0, 1.0).acos().clamp(1e-3, 1.55);
    let mut steepest = 0.0_f32;
    let mut last = (0.0_f32, 1.0_f32);
    for step in 1..=STEPS {
        let angle = edge * step as f32 / STEPS as f32;
        let at = (
            angle.tan(),
            luma_scene::luminaire::angular_profile(angle.cos(), cone.cos_beam, cone.cos_field),
        );
        steepest = steepest.max((last.1 - at.1) / (at.0 - last.0));
        last = at;
    }
    let from = (steepest * SOFT_EDGE - cone.lens_distance()).max(SOURCE_INNER);
    if from < cone.range {
        from
    } else {
        NEVER
    }
}

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
        "const FOG_SLICES: u32 = {SLICES}u;\nconst FOG_BLOCK_SIDE: u32 = {BLOCK_SIDE}u;\nconst FOG_BLOCK_LANES: u32 = {BLOCK_LANES}u;\nconst FOG_BLOCK_WORDS: u32 = {BLOCK_WORDS}u;\nconst FOG_HANDOVER_BAND: f32 = {HANDOVER_BAND:?};\n"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FixtureCone;
    use glam::Vec3;
    use luma_scene::luminaire::{cone_from_opening, Lens, Luminaire};

    fn cone(field_deg: f32) -> FixtureCone {
        let opening = cone_from_opening(Luminaire {
            field_angle_deg: field_deg,
            lumens: 1.0,
        });
        FixtureCone {
            position: Vec3::ZERO,
            range: opening.range,
            direction: Vec3::NEG_Z,
            cos_beam: opening.cos_beam,
            color: Vec3::ONE,
            intensity: opening.gain,
            cos_field: opening.cos_field,
            gobo: 0,
            gobo_rotation: 0.0,
            haze_gain: 1.0,
            lens: Lens::POINT,
        }
    }

    /// The narrower the beam, the further out the grid takes it, and never
    /// nearer than the source distance.
    #[test]
    fn a_narrower_beam_is_handed_over_further_out() {
        let fields = [150.0, 90.0, 60.0, 50.0, 40.0, 30.0, 20.0, 12.0, 8.0];
        let at: Vec<f32> = fields.iter().map(|&field| handover(&cone(field))).collect();
        assert!(at.windows(2).all(|pair| pair[0] <= pair[1]), "{at:?}");
        assert!(at.iter().all(|&from| from >= SOURCE_INNER), "{at:?}");
        assert!(at[0] < at[at.len() - 1], "{at:?}");
    }

    /// A bar's 50 degree lamp is as soft as a 60 degree cell to within a
    /// tenth, and is handed over as near: there is no angle at which a beam
    /// drops out of the grid.
    #[test]
    fn beams_of_nearly_the_same_softness_are_handed_over_together() {
        let (bar, cell) = (handover(&cone(50.0)), handover(&cone(60.0)));
        assert!(bar < cone(50.0).range && (bar - cell).abs() < 0.1 * cell);
    }

    #[test]
    fn a_gobo_is_never_handed_over() {
        let mut gobo = cone(90.0);
        gobo.gobo = 1;
        assert_eq!(handover(&gobo), NEVER);
    }

    /// A beam that has not opened to the grid's softness by its range stays
    /// with the per-pixel passes whole.
    #[test]
    fn a_beam_still_sharp_at_its_range_is_never_handed_over() {
        let mut short = cone(8.0);
        short.range = 5.0;
        assert_eq!(handover(&short), NEVER);
    }

    /// A lens puts the beam's apex behind it, so the beam is already wider
    /// where it leaves: it is soft enough sooner.
    #[test]
    fn a_beam_from_a_wide_lens_is_handed_over_sooner() {
        let mut wide = cone(20.0);
        wide.lens = Lens { radius: 0.3 };
        assert!(handover(&wide) < handover(&cone(20.0)));
    }
}
