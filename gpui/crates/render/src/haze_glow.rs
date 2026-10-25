//! The haze's glow: light the haze scatters lights the room and the haze
//! round it (`shaders/haze_glow.wgsl`, `shaders/haze_glow_sample.wgsl`).
//!
//! A beam in haze is seen because the haze sends some of its light toward
//! the camera. It sends the rest every other way, onto the walls, the
//! floor, the truss and the haze around it, and that is why a hazy room
//! with one bright beam is lit all over. The haze pass draws the first
//! part; this is the rest, for every source alike: fixture cones and laser
//! beams and sheets.
//!
//! - **Inject**: a coarse grid over the haze's bounds. Each cell holds the
//!   power its haze scatters per cubic metre: σ times its density times the
//!   light crossing it. A source narrower than a cell is spread over a
//!   cell's width, keeping its power, so no beam falls between cells.
//! - **Spread**: the grid's mips. The fluence a point gets from scattering
//!   haze falls as `e^(−σr) / r²`. Each cell gathers it along six cones,
//!   shell by shell out to the room, each shell read from the level as wide
//!   as it is deep, and each cone stopped by the geometry it passes
//!   (`glow_occupancy.rs`), so a wall or a deck shades the glow from the far
//!   side of it.
//! - **Use**: a surface takes a quarter of the fluence as irradiance (the
//!   haze's light comes from every side); the composite adds the haze's
//!   own second scattering of it along each camera ray.
//!
//! Scattering rises with density and so does the light it lights the room
//! with; the glow the camera sees rises with its square, until the haze is
//! thick enough to hide the room from itself. Haze off is no glow at all.
//!
//! A fixture cone's light stops at what it hits by its own shadow map; a
//! laser beam's at the first surface the occupancy finds on its line.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::light_index::{LightCore, LightRest};
use crate::scene_desc::Quality;

/// Cells along x, y and z: 0.6 m cells over a 40 × 40 × 20 m room at High.
const HIGH: [u32; 3] = [64, 64, 32];
const LOW: [u32; 3] = [32, 32, 16];
/// The grid's format: radiance-scale floats, filterable for the spread.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The widest a laser sheet merges to before the glow takes the next,
/// radians: a cell or so at the far side of a room.
const SHEET_MERGE: f32 = 0.06;

/// The grid as the inject pass reads it.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    min: [f32; 4],
    max: [f32; 4],
    counts: [u32; 4],
    laser: [f32; 4],
}

/// The grid as the surfaces and the composite read it; `HazeGlow` in
/// `haze_glow_sample.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Sample {
    min: [f32; 4],
    max: [f32; 4],
}

impl Sample {
    /// No glow: the samplers return nothing.
    const OFF: Self = Self {
        min: [0.0; 4],
        max: [1.0, 1.0, 1.0, 0.0],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Beam {
    origin: [f32; 4],
    direction: [f32; 4],
    power: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Sheet {
    origin: [f32; 4],
    middle: [f32; 4],
    normal: [f32; 4],
    density: [f32; 4],
}

/// What a frame's glow is made from.
pub(crate) struct FrameInput<'a> {
    pub cores: &'a [LightCore],
    pub rests: &'a [LightRest],
    pub shadow_matrices: &'a wgpu::Buffer,
    pub shadow_map: &'a wgpu::TextureView,
    pub shadow_map_extra: &'a wgpu::TextureView,
    pub medium: crate::medium::Uniform,
    pub haze_field: &'a crate::haze_field::HazeField,
    pub lasers: &'a [crate::laser::LaserScan],
    /// Radiance per unit of a cone's intensity, as the surfaces and the
    /// haze take it.
    pub beam_gain: f32,
}

pub(crate) struct Pipelines {
    layout: wgpu::BindGroupLayout,
    inject: wgpu::ComputePipeline,
    downsample: wgpu::ComputePipeline,
    gather_layout: wgpu::BindGroupLayout,
    gather: wgpu::ComputePipeline,
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let compute = wgpu::ShaderStages::COMPUTE;
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        };
        let depth_array = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze-glow"),
            entries: &[
                crate::gpu::uniform_entry(0, compute),
                crate::gpu::uniform_entry(1, compute),
                texture(2, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: compute,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage(4),
                storage(5),
                storage(6),
                depth_array(7),
                depth_array(8),
                storage(9),
                storage(10),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: compute,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D3,
                    },
                    count: None,
                },
                texture(12, wgpu::TextureSampleType::Float { filterable: true }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("haze-glow"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let source = format!(
            "{}{}{}{}",
            crate::haze_field::prelude(),
            include_str!("shaders/medium.wgsl"),
            include_str!("shaders/fixture_light.wgsl"),
            include_str!("shaders/haze_glow.wgsl"),
        );
        let module = crate::gpu::shader(device, "haze-glow", &source);
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let gather_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("haze-glow-gather"),
            entries: &[
                crate::gpu::uniform_entry(0, compute),
                texture(1, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: compute,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(3, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: compute,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D3,
                    },
                    count: None,
                },
            ],
        });
        let gather_module = crate::gpu::shader(
            device,
            "haze-glow-gather",
            include_str!("shaders/haze_glow_gather.wgsl"),
        );
        let gather = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("gather"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("haze-glow-gather"),
                    bind_group_layouts: &[Some(&gather_layout)],
                    immediate_size: 0,
                }),
            ),
            module: &gather_module,
            entry_point: Some("gather"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            inject: pipeline("inject"),
            downsample: pipeline("downsample"),
            layout,
            gather_layout,
            gather,
        }
    }
}

/// The layout entries a pass that samples the glow binds it with, from
/// `first`: the grid, its sampler and `HazeGlow`.
pub(crate) fn layout_entries(
    first: u32,
    visibility: wgpu::ShaderStages,
) -> [wgpu::BindGroupLayoutEntry; 3] {
    [
        wgpu::BindGroupLayoutEntry {
            binding: first,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: first + 1,
            visibility,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        crate::gpu::uniform_entry(first + 2, visibility),
    ]
}

/// The grid, kept across frames; reallocated only when the quality changes.
pub(crate) struct Glow {
    size: [u32; 3],
    /// Every level, for the samplers.
    view: wgpu::TextureView,
    /// One view per level, for the passes that write and read them.
    levels: Vec<wgpu::TextureView>,
    sampler: wgpu::Sampler,
    /// The gathered fluence, the one level the samplers read.
    fluence: wgpu::TextureView,
    /// What the samplers read this frame: `HazeGlow`.
    sample: wgpu::Buffer,
    /// What blocks the glow.
    occupancy: crate::glow_occupancy::Occupancy,
    on: bool,
}

impl Glow {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::with_size(device, queue, HIGH)
    }

    fn with_size(device: &wgpu::Device, queue: &wgpu::Queue, size: [u32; 3]) -> Self {
        use wgpu::util::DeviceExt;
        let count = levels(size);
        let grid = |label, mip_level_count| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: size[2],
                },
                mip_level_count,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let texture = grid("haze-glow", count);
        let levels = (0..count)
            .map(|level| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let fluence = grid("haze-glow-fluence", 1);
        Self {
            size,
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            levels,
            fluence: fluence.create_view(&wgpu::TextureViewDescriptor::default()),
            occupancy: crate::glow_occupancy::Occupancy::new(device, queue),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("haze-glow"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
            sample: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("haze-glow-sample"),
                contents: bytemuck::bytes_of(&Sample::OFF),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            }),
            on: false,
        }
    }

    /// The entries for [`layout_entries`] from `first`.
    pub(crate) fn bindings(&self, first: u32) -> [wgpu::BindGroupEntry<'_>; 3] {
        use crate::gpu::binding;
        [
            binding(first, wgpu::BindingResource::TextureView(&self.fluence)),
            binding(first + 1, wgpu::BindingResource::Sampler(&self.sampler)),
            binding(first + 2, self.sample.as_entire_binding()),
        ]
    }

    /// Size the grid for `quality`, find what blocks it in `frame`, and tell
    /// the samplers where it is this frame, before anything binds it; off
    /// when there is no haze.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        medium: &crate::medium::Uniform,
        frame: &crate::frame::Frame,
    ) {
        let quality = frame.quality;
        let size = match quality {
            Quality::High => HIGH,
            Quality::Low => LOW,
        };
        if size != self.size {
            *self = Self::with_size(device, queue, size);
        }
        let sigma = medium.min[3];
        let min = Vec3::from_slice(&medium.min[..3]);
        let max = Vec3::from_slice(&medium.max[..3]);
        self.on = sigma > 0.0 && (max - min).min_element() > 0.0;
        let sample = if self.on {
            self.occupancy.update(
                device,
                queue,
                frame,
                (min, max),
                size,
                self.levels.len() as u32,
            );
            Sample {
                min: medium.min,
                max: max.extend(1.0).to_array(),
            }
        } else {
            Sample::OFF
        };
        queue.write_buffer(&self.sample, 0, bytemuck::bytes_of(&sample));
    }

    /// Fill the grid for `input`, after its cones' shadow maps are drawn.
    /// Does nothing when [`prepare`](Self::prepare) found no haze.
    pub(crate) fn encode(
        &self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput<'_>,
    ) {
        use wgpu::util::DeviceExt;
        if !self.on {
            return;
        }
        let sigma = input.medium.min[3];
        let min = Vec3::from_slice(&input.medium.min[..3]);
        let max = Vec3::from_slice(&input.medium.max[..3]);
        let (beams, sheets) = laser_light(input.lasers, &self.occupancy.solid);
        let params = Params {
            min: min.extend(sigma).to_array(),
            max: max.extend(input.beam_gain).to_array(),
            counts: [
                input.cores.len() as u32,
                beams.len() as u32,
                sheets.len() as u32,
                0,
            ],
            laser: [crate::laser::GAIN, 0.0, 0.0, 0.0],
        };
        // A storage binding cannot be empty: an unused one holds one zero.
        fn bytes<T: Pod>(items: &[T]) -> Vec<u8> {
            if items.is_empty() {
                bytemuck::bytes_of(&T::zeroed()).to_vec()
            } else {
                bytemuck::cast_slice(items).to_vec()
            }
        }
        let buffer = |label: &str, contents: &[u8], usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        let params = buffer(
            "haze-glow-params",
            bytemuck::bytes_of(&params),
            wgpu::BufferUsages::UNIFORM,
        );
        let medium = buffer(
            "haze-glow-medium",
            bytemuck::bytes_of(&input.medium),
            wgpu::BufferUsages::UNIFORM,
        );
        let cores = buffer(
            "haze-glow-cores",
            &bytes(input.cores),
            wgpu::BufferUsages::STORAGE,
        );
        let rests = buffer(
            "haze-glow-rests",
            &bytes(input.rests),
            wgpu::BufferUsages::STORAGE,
        );
        let beams = buffer(
            "haze-glow-beams",
            &bytes(&beams),
            wgpu::BufferUsages::STORAGE,
        );
        let sheets = buffer(
            "haze-glow-sheets",
            &bytes(&sheets),
            wgpu::BufferUsages::STORAGE,
        );
        let group = |write: usize, read: usize| {
            use crate::gpu::binding;
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("haze-glow"),
                layout: &pipelines.layout,
                entries: &[
                    binding(0, params.as_entire_binding()),
                    binding(1, medium.as_entire_binding()),
                    binding(
                        2,
                        wgpu::BindingResource::TextureView(&input.haze_field.view),
                    ),
                    binding(3, wgpu::BindingResource::Sampler(&input.haze_field.sampler)),
                    binding(4, cores.as_entire_binding()),
                    binding(5, rests.as_entire_binding()),
                    binding(6, input.shadow_matrices.as_entire_binding()),
                    binding(7, wgpu::BindingResource::TextureView(input.shadow_map)),
                    binding(
                        8,
                        wgpu::BindingResource::TextureView(input.shadow_map_extra),
                    ),
                    binding(9, beams.as_entire_binding()),
                    binding(10, sheets.as_entire_binding()),
                    binding(11, wgpu::BindingResource::TextureView(&self.levels[write])),
                    binding(12, wgpu::BindingResource::TextureView(&self.levels[read])),
                ],
            })
        };
        let dispatch = |pass: &mut wgpu::ComputePass, level: usize| {
            let [x, y, z] = level_size(self.size, level as u32);
            pass.dispatch_workgroups(x.div_ceil(4), y.div_ceil(4), z.div_ceil(4));
        };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("haze-glow"),
            timestamp_writes: None,
        });
        // The inject pass reads no level; it binds the last so no level is
        // both read and written.
        let last = self.levels.len() - 1;
        pass.set_pipeline(&pipelines.inject);
        pass.set_bind_group(0, &group(0, last), &[]);
        dispatch(&mut pass, 0);
        pass.set_pipeline(&pipelines.downsample);
        for level in 1..self.levels.len() {
            pass.set_bind_group(0, &group(level, level - 1), &[]);
            dispatch(&mut pass, level);
        }
        // `Params` in `haze_glow_gather.wgsl`: the grid, σ and its levels.
        let gather_params = buffer(
            "haze-glow-gather",
            bytemuck::bytes_of(&Sample {
                min: min.extend(sigma).to_array(),
                max: max.extend(self.levels.len() as f32).to_array(),
            }),
            wgpu::BufferUsages::UNIFORM,
        );
        let gather = {
            use crate::gpu::binding;
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("haze-glow-gather"),
                layout: &pipelines.gather_layout,
                entries: &[
                    binding(0, gather_params.as_entire_binding()),
                    binding(1, wgpu::BindingResource::TextureView(&self.view)),
                    binding(2, wgpu::BindingResource::Sampler(&self.sampler)),
                    binding(3, wgpu::BindingResource::TextureView(&self.occupancy.view)),
                    binding(4, wgpu::BindingResource::TextureView(&self.fluence)),
                ],
            })
        };
        pass.set_pipeline(&pipelines.gather);
        pass.set_bind_group(0, &gather, &[]);
        dispatch(&mut pass, 0);
    }
}

/// Levels down to a grid two cells across its shortest side.
fn levels(size: [u32; 3]) -> u32 {
    let shortest = size.into_iter().min().unwrap_or(1).max(2);
    shortest.ilog2()
}

fn level_size(size: [u32; 3], level: u32) -> [u32; 3] {
    size.map(|n| (n >> level).max(1))
}

/// The projectors' beams and sheets as the glow takes them: each sheet run
/// merged into fans no wider than [`SHEET_MERGE`], since the grid is far too
/// coarse to tell their pieces apart.
fn laser_light(
    scans: &[crate::laser::LaserScan],
    solid: &crate::glow_occupancy::Solid,
) -> (Vec<Beam>, Vec<Sheet>) {
    let reach = |origin, direction| solid.reach(origin, direction, crate::laser::RANGE_M);
    let mut beams = Vec::new();
    let mut sheets = Vec::new();
    for scan in scans {
        let (b, s) = crate::laser::light(scan);
        beams.extend(b.into_iter().map(|beam| {
            Beam {
                origin: beam.origin.extend(0.0).to_array(),
                direction: beam
                    .direction
                    .extend(reach(beam.origin, beam.direction))
                    .to_array(),
                power: beam.power.extend(0.0).to_array(),
            }
        }));
        // A run of pieces, each starting where the last ended: its first
        // edge, its last edge so far, and the power it holds.
        let mut run: Option<(Vec3, Vec3, Vec3)> = None;
        let mut flush = |run: &mut Option<(Vec3, Vec3, Vec3)>, origin: Vec3| {
            if let Some((from, to, power)) = run.take() {
                let angle = crate::laser::angle(from, to);
                let normal = from.cross(to).normalize_or_zero();
                if angle <= 0.0 || normal == Vec3::ZERO {
                    return;
                }
                // A fan this narrow stops where its middle does.
                let middle = (from + to).normalize();
                sheets.push(Sheet {
                    origin: origin.extend(0.0).to_array(),
                    middle: middle.extend(0.5 * angle).to_array(),
                    normal: normal.extend(reach(origin, middle)).to_array(),
                    density: (power / angle).extend(0.0).to_array(),
                });
            }
        };
        for sheet in s {
            let power = sheet.density * sheet.angle();
            if let Some((from, to, held)) = &mut run {
                let joins = to.abs_diff_eq(sheet.from, 1e-5);
                // Merged pieces must stay one flat fan: the next piece
                // turns the same way about the same normal. A piece that
                // turns back, as the mirrors do overshooting a jump, starts
                // the next fan.
                let flat = sheet
                    .from
                    .cross(sheet.to)
                    .normalize_or_zero()
                    .dot(from.cross(*to).normalize_or_zero())
                    > 0.999;
                if joins && flat && crate::laser::angle(*from, sheet.to) <= SHEET_MERGE {
                    *to = sheet.to;
                    *held += power;
                    continue;
                }
            }
            flush(&mut run, scan.origin);
            run = Some((sheet.from, sheet.to, power));
        }
        flush(&mut run, scan.origin);
    }
    (beams, sheets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::laser::{LaserPoint, LaserScan};

    fn sweep(points: usize) -> LaserScan {
        LaserScan {
            origin: Vec3::ZERO,
            forward: Vec3::Y,
            right: Vec3::X,
            up: Vec3::Z,
            half_field: [30.0, 30.0],
            rate: 30_000.0,
            points: (0..points)
                .map(|i| LaserPoint {
                    x: i as f32 / points as f32 * 1.6 - 0.8,
                    y: 0.0,
                    rgb: [1.0; 3],
                })
                .collect(),
            phase: 0.0,
        }
    }

    fn sheet_power(sheets: &[Sheet]) -> f32 {
        sheets
            .iter()
            .map(|s| s.density[0] * 2.0 * s.middle[3])
            .sum()
    }

    /// Merging a sweep's pieces into a few fans keeps its power and its
    /// angle.
    #[test]
    fn merged_sheets_keep_the_sweeps_light() {
        let scan = sweep(400);
        let (_, pieces) = crate::laser::light(&scan);
        let (_, merged) = laser_light(&[scan], &crate::glow_occupancy::Solid::empty());
        assert!(
            merged.len() * 10 < pieces.len(),
            "{} of {}",
            merged.len(),
            pieces.len()
        );
        let power: f32 = pieces.iter().map(|s| s.density.x * s.angle()).sum();
        assert!((sheet_power(&merged) - power).abs() < 1e-4 * power.max(1.0));
        let angle: f32 = merged.iter().map(|s| 2.0 * s.middle[3]).sum();
        let swept: f32 = pieces.iter().map(crate::laser::Sheet::angle).sum();
        assert!((angle - swept).abs() < 1e-3, "{angle} vs {swept}");
    }

    /// A fan that doubles back is two fans: merging never folds one piece
    /// over another.
    #[test]
    fn a_fan_that_turns_back_is_not_merged_over_itself() {
        let mut scan = sweep(200);
        let back: Vec<LaserPoint> = scan.points.iter().rev().copied().collect();
        scan.points.extend(back);
        let (_, merged) = laser_light(&[scan], &crate::glow_occupancy::Solid::empty());
        for sheet in &merged {
            assert!(2.0 * sheet.middle[3] <= SHEET_MERGE + 1e-4);
        }
        let ups = merged.iter().filter(|s| s.normal[2] > 0.0).count();
        assert!(ups > 0 && ups < merged.len(), "{ups} of {}", merged.len());
    }

    /// The coarsest level is two cells across the grid's shortest side: the
    /// spread reaches across the room and no further.
    #[test]
    fn the_last_level_is_two_cells_across() {
        for size in [HIGH, LOW] {
            let last = level_size(size, levels(size) - 1);
            assert_eq!(last.into_iter().min(), Some(2), "{size:?}");
        }
    }
}
