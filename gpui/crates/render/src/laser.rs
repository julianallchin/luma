//! Laser projectors in haze (`shaders/laser.wgsl`).
//!
//! The visualizer reads the point stream a projector would play, nothing
//! else (`docs/design/lasers.md`). An eye sees a scanned laser as the light
//! it sends over a short while, so the pass takes the points the projector
//! emits over [`PERSISTENCE_S`], runs them through a model of the mirrors,
//! and builds what that light lights:
//!
//! - where the mirrors barely move, the light stacks on one direction: a
//!   **beam**, a thin line from the projector with the power of all the time
//!   spent there;
//! - where they sweep, it spreads over the angle swept: a **sheet**, a thin
//!   triangle from the projector whose power per radian is the time spent
//!   over the angle.
//!
//! So a dwell is bright and a sweep is dim, and a scan too slow for the
//! window flickers here as it would in the room. Each is drawn additively
//! into the HDR scene before glare, with the haze's own extinction and
//! phase, and tested against the scene's depth.

use glam::Vec3;

/// How long an eye adds a scanned laser's light up for, seconds.
pub const PERSISTENCE_S: f32 = 0.05;
/// How quickly the mirrors follow a step, seconds: a first-order lag.
const GALVO_TAU_S: f32 = 0.000_12;
/// Below this angle between two points, radians, the light is one beam.
const BEAM_ANGLE: f32 = 0.002;
/// How far a beam or sheet reaches, metres, when nothing stops it first.
pub const RANGE_M: f32 = 80.0;

/// One point of a projector's stream: galvo coordinates −1 to 1 across and up
/// its field, and its light per emitter, 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaserPoint {
    /// Across the field, −1 to 1.
    pub x: f32,
    /// Up the field, −1 to 1.
    pub y: f32,
    /// Red, green and blue, 0 to 1; black is the light off.
    pub rgb: [f32; 3],
}

/// One projector's scan, in world space.
#[derive(Clone, Debug, PartialEq)]
pub struct LaserScan {
    /// Where the light leaves.
    pub origin: Vec3,
    /// Unit vector: the field's middle.
    pub forward: Vec3,
    /// Unit vector: the axis the across mirror moves the light along.
    pub right: Vec3,
    /// Unit vector: the axis the up mirror moves the light along.
    pub up: Vec3,
    /// Half the scan angle across and up, degrees.
    pub half_field: [f32; 2],
    /// Points a second.
    pub rate: f32,
    /// One scan, played on repeat.
    pub points: Vec<LaserPoint>,
    /// Where in the stream this frame's window ends, in points from the scan's
    /// start: the clock that decides which part of a long scan the eye sees.
    pub phase: f32,
}

impl LaserScan {
    /// The unit world direction of galvo coordinates `(x, y)`.
    pub fn direction(&self, x: f32, y: f32) -> Vec3 {
        let tan = |c: f32, half: f32| (c * half).to_radians().tan();
        (self.forward
            + self.right * tan(x, self.half_field[0])
            + self.up * tan(y, self.half_field[1]))
        .normalize()
    }
}

/// A beam: from `origin` along `direction`, with `power` per emitter, the
/// light it holds averaged over the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beam {
    /// Where it leaves the projector.
    pub origin: Vec3,
    /// Unit vector it travels along.
    pub direction: Vec3,
    /// Its light per emitter, averaged over the window.
    pub power: Vec3,
}

/// A sheet: from `origin` between two directions, with `density` per emitter
/// per radian of the angle between them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sheet {
    /// Where it leaves the projector.
    pub origin: Vec3,
    /// Unit vector of one edge.
    pub from: Vec3,
    /// Unit vector of the other edge.
    pub to: Vec3,
    /// Its light per emitter per radian, averaged over the window.
    pub density: Vec3,
}

/// The beams and sheets a projector's light makes over the window.
pub fn light(scan: &LaserScan) -> (Vec<Beam>, Vec<Sheet>) {
    let n = scan.points.len();
    if n == 0 || scan.rate <= 0.0 {
        return (vec![], vec![]);
    }
    let window = ((PERSISTENCE_S * scan.rate).round() as usize).max(2);
    let end = scan.phase.rem_euclid(n as f32) as usize;
    let start = end + n * window.div_ceil(n) - window;
    let at = |k: usize| scan.points[k % n];
    // The mirrors lag the stream; run them over the scan before the window
    // so they arrive where a projector's would.
    let follow = 1.0 - (-1.0 / (GALVO_TAU_S * scan.rate)).exp();
    let mut mirror = [at(start + n - 1).x, at(start + n - 1).y];
    for k in start..start + n {
        let p = at(k);
        mirror[0] += (p.x - mirror[0]) * follow;
        mirror[1] += (p.y - mirror[1]) * follow;
    }
    let share = 1.0 / window as f32;
    let mut beams = Vec::new();
    let mut sheets = Vec::new();
    // The beam being gathered: its direction, and the light so far.
    let mut gathering: Option<(Vec3, Vec3)> = None;
    let flush = |gathering: &mut Option<(Vec3, Vec3)>, beams: &mut Vec<Beam>| {
        if let Some((direction, power)) = gathering.take() {
            beams.push(Beam {
                origin: scan.origin,
                direction,
                power,
            });
        }
    };
    let mut from = scan.direction(mirror[0], mirror[1]);
    for k in start..start + window {
        let p = at(k);
        mirror[0] += (p.x - mirror[0]) * follow;
        mirror[1] += (p.y - mirror[1]) * follow;
        let to = scan.direction(mirror[0], mirror[1]);
        let light = Vec3::from(p.rgb) * share;
        if light.max_element() <= 0.0 {
            flush(&mut gathering, &mut beams);
        } else {
            let angle = from.angle_between(to);
            if angle < BEAM_ANGLE {
                // A beam holds the light that stays near its own direction;
                // a slow sweep drifts off it and starts the next beam.
                if gathering.is_some_and(|(anchor, _)| anchor.angle_between(to) >= BEAM_ANGLE) {
                    flush(&mut gathering, &mut beams);
                }
                let (_, power) = gathering.get_or_insert((to, Vec3::ZERO));
                *power += light;
            } else {
                flush(&mut gathering, &mut beams);
                sheets.push(Sheet {
                    origin: scan.origin,
                    from,
                    to,
                    density: light / angle,
                });
            }
        }
        from = to;
    }
    flush(&mut gathering, &mut beams);
    (beams, sheets)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    /// x: extinction σ (1/m); y: phase g; z: gain; w: range (m).
    haze: [f32; 4],
    /// Pixels across and down; zw unused.
    viewport: [f32; 4],
}

/// A beam as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BeamInstance {
    origin: [f32; 4],
    direction: [f32; 4],
    power: [f32; 4],
}

/// One corner of a sheet triangle.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SheetVertex {
    position: [f32; 4],
    /// The sheet's unit normal, and the origin's distance along nothing: w 0.
    normal: [f32; 4],
    origin: [f32; 4],
    density: [f32; 4],
}

/// What a frame's laser pass draws with.
pub(crate) struct FrameInput<'a> {
    pub scans: &'a [LaserScan],
    pub view_proj: glam::Mat4,
    pub eye: Vec3,
    pub haze_density: f32,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct Pipelines {
    layout: wgpu::BindGroupLayout,
    beams: wgpu::RenderPipeline,
    sheets: wgpu::RenderPipeline,
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let both = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: both,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("laser"),
            entries: &[crate::gpu::uniform_entry(0, both), storage(1), storage(2)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("laser"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = crate::gpu::shader(device, "laser", include_str!("shaders/laser.wgsl"));
        let pipeline = |label: &str, vs: &str, fs: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::Zero,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: crate::gpu::DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Pipelines {
            beams: pipeline("laser-beams", "vs_beam", "fs_beam"),
            sheets: pipeline("laser-sheets", "vs_sheet", "fs_sheet"),
            layout,
        }
    }

    /// Draw `input`'s lasers into `target`, tested against `depth`. Does
    /// nothing, and binds nothing, when there is no light to draw.
    pub(crate) fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        input: &FrameInput<'_>,
    ) {
        use wgpu::util::DeviceExt;
        let mut beams = Vec::new();
        let mut sheets = Vec::new();
        for scan in input.scans {
            let (b, s) = light(scan);
            beams.extend(b.into_iter().map(|beam| BeamInstance {
                origin: beam.origin.extend(0.0).to_array(),
                direction: beam.direction.extend(0.0).to_array(),
                power: beam.power.extend(0.0).to_array(),
            }));
            for sheet in s {
                let normal = sheet.from.cross(sheet.to).normalize_or_zero();
                let corner = |position: Vec3| SheetVertex {
                    position: position.extend(1.0).to_array(),
                    normal: normal.extend(0.0).to_array(),
                    origin: sheet.origin.extend(1.0).to_array(),
                    density: sheet.density.extend(0.0).to_array(),
                };
                sheets.push(corner(sheet.origin));
                sheets.push(corner(sheet.origin + sheet.from * RANGE_M));
                sheets.push(corner(sheet.origin + sheet.to * RANGE_M));
            }
        }
        if beams.is_empty() && sheets.is_empty() {
            return;
        }
        let uniform = Uniform {
            view_proj: input.view_proj.to_cols_array_2d(),
            camera_pos: input.eye.extend(1.0).to_array(),
            haze: [
                input.haze_density * crate::gpu::Transport::EXTINCTION,
                crate::gpu::Transport::PHASE_G,
                GAIN,
                RANGE_M,
            ],
            viewport: [input.width as f32, input.height as f32, 0.0, 0.0],
        };
        // A storage binding cannot be empty: an unused one holds one zero.
        fn bytes<T: bytemuck::Pod + bytemuck::Zeroable>(items: &[T]) -> Vec<u8> {
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
        let uniform = buffer(
            "laser-uniform",
            bytemuck::bytes_of(&uniform),
            wgpu::BufferUsages::UNIFORM,
        );
        let beam_buffer = buffer("laser-beams", &bytes(&beams), wgpu::BufferUsages::STORAGE);
        let sheet_buffer = buffer("laser-sheets", &bytes(&sheets), wgpu::BufferUsages::STORAGE);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("laser"),
            layout: &self.layout,
            entries: &[
                crate::gpu::binding(0, uniform.as_entire_binding()),
                crate::gpu::binding(1, beam_buffer.as_entire_binding()),
                crate::gpu::binding(2, sheet_buffer.as_entire_binding()),
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lasers"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_bind_group(0, &bind_group, &[]);
        if !sheets.is_empty() {
            pass.set_pipeline(&self.sheets);
            pass.draw(0..sheets.len() as u32, 0..1);
        }
        if !beams.is_empty() {
            pass.set_pipeline(&self.beams);
            pass.draw(0..6, 0..beams.len() as u32);
        }
    }
}

/// Radiance per unit of a projector's light: puts a full-white beam on the
/// same scale as a stage beam in the same haze.
const GAIN: f32 = 40.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(points: Vec<LaserPoint>) -> LaserScan {
        LaserScan {
            origin: Vec3::ZERO,
            forward: Vec3::Y,
            right: Vec3::X,
            up: Vec3::Z,
            half_field: [30.0, 30.0],
            rate: 30_000.0,
            points,
            phase: 0.0,
        }
    }

    fn dot(x: f32, lit: bool) -> LaserPoint {
        LaserPoint {
            x,
            y: 0.0,
            rgb: if lit { [1.0; 3] } else { [0.0; 3] },
        }
    }

    /// A projector that dwells on two spots and jumps between them in black
    /// makes two beams and no sheet.
    #[test]
    fn dwells_make_beams() {
        let mut points = Vec::new();
        for x in [-0.5, 0.5] {
            points.extend(std::iter::repeat_n(dot(x, false), 24));
            points.extend(std::iter::repeat_n(dot(x, true), 40));
        }
        let (beams, sheets) = light(&scan(points));
        assert!(sheets.is_empty(), "{sheets:?}");
        let mut xs: Vec<i32> = beams
            .iter()
            .map(|b| (b.direction.x * 10.0).round() as i32)
            .collect();
        xs.sort_unstable();
        xs.dedup();
        assert_eq!(xs.len(), 2, "{beams:?}");
    }

    /// A slow sweep is light spread over its angle, never gathered into one
    /// beam: no beam holds more than its own short stretch.
    #[test]
    fn a_slow_sweep_spreads_its_light() {
        let points: Vec<LaserPoint> = (0..1000)
            .map(|i| dot(i as f32 / 1000.0 - 0.5, true))
            .collect();
        let (beams, _) = light(&scan(points));
        let most = beams.iter().map(|b| b.power.x).fold(0.0, f32::max);
        assert!(most < 0.05, "{most}");
    }

    /// A lit sweep spreads its light over the angle: sheets, dimmer per
    /// radian than the beam the same time would make.
    #[test]
    fn sweeps_make_dim_sheets() {
        let points: Vec<LaserPoint> = (0..100).map(|i| dot(i as f32 / 50.0 - 1.0, true)).collect();
        let (beams, sheets) = light(&scan(points));
        assert!(sheets.len() > 50, "{}", sheets.len());
        let beam_power: f32 = beams.iter().map(|b| b.power.x).sum();
        let sheet_power: f32 = sheets
            .iter()
            .map(|s| s.density.x * s.from.angle_between(s.to))
            .sum();
        assert!(sheet_power > beam_power, "{sheet_power} {beam_power}");
    }

    /// The light over the window adds to the scan's lit share: a scan lit half
    /// the time holds half a beam's worth, however it is split.
    #[test]
    fn the_window_holds_the_lit_share() {
        let mut points = vec![dot(0.0, true); 50];
        points.extend(vec![dot(0.0, false); 50]);
        let (beams, _) = light(&scan(points));
        let total: f32 = beams.iter().map(|b| b.power.x).sum();
        assert!((total - 0.5).abs() < 0.05, "{total}");
    }

    #[test]
    fn galvo_coordinates_follow_the_field() {
        let s = scan(vec![]);
        assert!(s.direction(0.0, 0.0).abs_diff_eq(Vec3::Y, 1e-6));
        let edge = s.direction(1.0, 0.0);
        assert!((edge.angle_between(Vec3::Y).to_degrees() - 30.0).abs() < 1e-3);
        assert!(s.direction(0.0, 1.0).z > 0.0);
    }

    #[test]
    fn no_points_no_light() {
        let (beams, sheets) = light(&scan(vec![]));
        assert!(beams.is_empty() && sheets.is_empty());
    }
}
