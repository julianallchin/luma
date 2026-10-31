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
/// The mirrors' natural frequency, radians a second: a damped spring each.
/// A 30 kpps scanner settles a 0.1° step in 100–130 µs with a small mirror
/// (Cambridge Technology 6210H, 6215H datasheets); a show scanner's larger
/// mirror at ILDA's 8° test angle is slower, and at 30 kpps it still just
/// draws the test pattern's corners: a rise of about three points, 85 µs,
/// which 4 kHz gives.
const GALVO_OMEGA: f32 = std::f32::consts::TAU * 4_000.0;
/// The mirrors' damping ratio: a servo tuned a little under critical, as
/// shipped, overshoots a step by about 5% and settles within 0.3 ms.
const GALVO_ZETA: f32 = 0.7;
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

impl Sheet {
    /// The angle between its edges, radians.
    pub fn angle(&self) -> f32 {
        angle(self.from, self.to)
    }
}

/// The angle between unit vectors, radians, as exact for the hundredth of a
/// degree between two points of a scan as for a right angle. `acos` of their
/// dot product, near 1, keeps only a few bits of so small an angle, and a
/// sheet's light per radian would carry that error as stripes.
pub(crate) fn angle(a: Vec3, b: Vec3) -> f32 {
    a.cross(b).length().atan2(a.dot(b))
}

/// The two mirrors: each a damped spring pulled toward the point it was last
/// sent, with the point held until the next. So it rounds a corner, slowing
/// into it and speeding out, and overshoots a jump a little before settling.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Galvo {
    position: [f32; 2],
    velocity: [f32; 2],
    /// How a step of the point period carries (offset from the target,
    /// velocity) on: the spring's exact solution over one period, rows
    /// `[offset, velocity]`.
    step: [[f32; 2]; 2],
}

impl Galvo {
    /// Mirrors at rest at `(x, y)`, sent a point `rate` times a second.
    pub(crate) fn new(x: f32, y: f32, rate: f32) -> Self {
        let t = 1.0 / rate;
        let decay = GALVO_ZETA * GALVO_OMEGA;
        let ringing = GALVO_OMEGA * (1.0 - GALVO_ZETA * GALVO_ZETA).sqrt();
        let fade = (-decay * t).exp();
        let (sin, cos) = (ringing * t).sin_cos();
        Self {
            position: [x, y],
            velocity: [0.0; 2],
            step: [
                [fade * (cos + decay / ringing * sin), fade * sin / ringing],
                [
                    -fade * GALVO_OMEGA * GALVO_OMEGA / ringing * sin,
                    fade * (cos - decay / ringing * sin),
                ],
            ],
        }
    }

    /// Send the mirrors `point` and let one point period pass; where they
    /// are then.
    pub(crate) fn follow(&mut self, point: LaserPoint) -> [f32; 2] {
        let [[a, b], [c, d]] = self.step;
        for (axis, target) in [point.x, point.y].into_iter().enumerate() {
            let offset = self.position[axis] - target;
            let velocity = self.velocity[axis];
            self.position[axis] = target + a * offset + b * velocity;
            self.velocity[axis] = c * offset + d * velocity;
        }
        self.position
    }
}

/// The beams and sheets a projector's light makes over the window.
pub fn light(scan: &LaserScan) -> (Vec<Beam>, Vec<Sheet>) {
    let n = scan.points.len();
    if n == 0 || scan.rate <= 0.0 {
        return (vec![], vec![]);
    }
    // A scan that repeats inside the window shows the eye its average, one
    // whole scan's worth whatever the phase. A window cut mid-scan would
    // count part of it once more than the rest, and that part would flicker
    // as the phase moved.
    let persistence = ((PERSISTENCE_S * scan.rate).round() as usize).max(2);
    let window = if n <= persistence { n } else { persistence };
    let end = scan.phase.rem_euclid(n as f32) as usize;
    let start = end + n * window.div_ceil(n) - window;
    let at = |k: usize| scan.points[k % n];
    // The mirrors lag the stream; run them over the scan before the window
    // so they arrive where a projector's would.
    let last = at(start + n - 1);
    let mut galvo = Galvo::new(last.x, last.y, scan.rate);
    for k in start..start + n {
        galvo.follow(at(k));
    }
    // A scan longer than the window is seen a stretch at a time. The eye
    // forgets gradually, not all at once, so the stretch fades in and out
    // and a point does not pop as the window passes it.
    let weight = |i: usize| {
        if window == n {
            1.0
        } else {
            let t = (i as f32 + 0.5) / window as f32;
            (std::f32::consts::PI * t).sin().powi(2)
        }
    };
    let total: f32 = (0..window).map(weight).sum();
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
    let [x, y] = galvo.position;
    let mut from = scan.direction(x, y);
    for k in start..start + window {
        let p = at(k);
        let [x, y] = galvo.follow(p);
        let to = scan.direction(x, y);
        let light = Vec3::from(p.rgb) * (weight(k - start) / total);
        if light.max_element() <= 0.0 {
            flush(&mut gathering, &mut beams);
        } else {
            let step = angle(from, to);
            if step < BEAM_ANGLE {
                // A beam holds the light that stays near its own direction;
                // a slow sweep drifts off it and starts the next beam.
                if gathering.is_some_and(|(anchor, _)| angle(anchor, to) >= BEAM_ANGLE) {
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
                    density: light / step,
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
    /// x: extinction σ (1/m); y unused; z: gain; w: range (m).
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
    /// The haze's density field, as the beams and the fog read it.
    pub medium: crate::medium::Uniform,
    pub haze_field: &'a crate::haze_field::HazeField,
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
        let fragment = wgpu::ShaderStages::FRAGMENT;
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
            entries: &[
                crate::gpu::uniform_entry(0, both),
                storage(1),
                storage(2),
                crate::gpu::uniform_entry(3, fragment),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: fragment,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("laser"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let source = format!(
            "const PIECES: u32 = {BEAM_PIECES}u;\n{}{}{}",
            crate::haze_field::prelude(),
            include_str!("shaders/medium.wgsl"),
            include_str!("shaders/laser.wgsl"),
        );
        let module = crate::gpu::shader(device, "laser", &source);
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
                0.0,
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
        let medium = buffer(
            "laser-medium",
            bytemuck::bytes_of(&input.medium),
            wgpu::BufferUsages::UNIFORM,
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("laser"),
            layout: &self.layout,
            entries: &[
                crate::gpu::binding(0, uniform.as_entire_binding()),
                crate::gpu::binding(1, beam_buffer.as_entire_binding()),
                crate::gpu::binding(2, sheet_buffer.as_entire_binding()),
                crate::gpu::binding(3, medium.as_entire_binding()),
                crate::gpu::binding(
                    4,
                    wgpu::BindingResource::TextureView(&input.haze_field.view),
                ),
                crate::gpu::binding(5, wgpu::BindingResource::Sampler(&input.haze_field.sampler)),
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
            pass.draw(0..6 * BEAM_PIECES, 0..beams.len() as u32);
        }
    }
}

/// Pieces `vs_beam` cuts a beam's strip into along its length, so its width
/// follows its distance from the camera and from the projector.
const BEAM_PIECES: u32 = 16;

/// Radiance per unit of a projector's light: puts a full-white beam on the
/// same scale as a stage beam in the same haze.
pub(crate) const GAIN: f32 = 80.0;

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
        let sheet_power: f32 = sheets.iter().map(|s| s.density.x * s.angle()).sum();
        assert!(sheet_power > beam_power, "{sheet_power} {beam_power}");
    }

    /// An even sweep is an even sheet: each piece holds the light per
    /// radian of the pieces beside it, so the fan shows no stripes.
    #[test]
    fn an_even_sweep_is_an_even_sheet() {
        // 0.02 of the field a point, the planner's pace for a lit line; back
        // in black, held still at either end, as the planner plays it.
        let mut points = vec![dot(-0.6, false); 10];
        points.extend((0..60).map(|i| dot(i as f32 * 0.02 - 0.6, true)));
        points.extend(vec![dot(0.6, false); 10]);
        points.extend((0..30).map(|i| dot(0.56 - i as f32 * 0.04, false)));
        let (_, sheets) = light(&scan(points));
        // A stripe is a piece brighter or dimmer than the pieces beside it.
        // The mirrors still speeding up out of the turn change the light
        // gradually, which is no stripe.
        let middle = &sheets[sheets.len() / 4..sheets.len() * 3 / 4];
        for pair in middle.windows(2) {
            let ratio = pair[1].density.x / pair[0].density.x;
            assert!((ratio - 1.0).abs() < 0.005, "{ratio}");
        }
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

    /// A scan shorter than the window shows the same light at every phase:
    /// no beam takes more of the window one frame than the next.
    #[test]
    fn a_short_scan_holds_steady_across_phases() {
        let mut points = Vec::new();
        for x in [-0.5, 0.0, 0.5] {
            points.extend(std::iter::repeat_n(dot(x, false), 24));
            points.extend(std::iter::repeat_n(dot(x, true), 100));
        }
        let n = points.len();
        let mut s = scan(points);
        let powers = |s: &LaserScan| -> Vec<i32> {
            let mut by_x = [0.0f32; 3];
            for beam in light(s).0 {
                let i = ((beam.direction.x * 4.0).round() as i32 + 1) as usize;
                by_x[i] += beam.power.x;
            }
            by_x.iter().map(|p| (p * 1000.0).round() as i32).collect()
        };
        s.phase = 0.0;
        let first = powers(&s);
        for phase in (0..n).step_by(37) {
            s.phase = phase as f32;
            assert_eq!(powers(&s), first, "phase {phase}");
        }
    }

    /// A scan longer than the window: moving the window a point changes the
    /// light by little, since its ends fade.
    #[test]
    fn a_long_scan_changes_smoothly_with_phase() {
        let mut points = Vec::new();
        for x in [-0.5, 0.5] {
            points.extend(std::iter::repeat_n(dot(x, false), 24));
            points.extend(std::iter::repeat_n(dot(x, true), 1500));
        }
        let mut s = scan(points);
        let left = |s: &LaserScan| -> f32 {
            light(s)
                .0
                .iter()
                .filter(|b| b.direction.x < 0.0)
                .map(|b| b.power.x)
                .sum()
        };
        let mut previous = left(&s);
        for phase in 1..3048 {
            s.phase = phase as f32;
            let now = left(&s);
            assert!(
                (now - previous).abs() < 0.01,
                "phase {phase}: {previous} to {now}"
            );
            previous = now;
        }
    }

    /// Sent a jump and held there, the mirrors overshoot a little and
    /// settle on it, at any point rate: the spring never runs away.
    #[test]
    fn a_step_settles() {
        for rate in [1_000.0, 30_000.0, 100_000.0] {
            let mut galvo = Galvo::new(-0.5, 0.0, rate);
            let path: Vec<f32> = (0..(rate * 0.01) as usize)
                .map(|_| galvo.follow(dot(0.5, true))[0])
                .collect();
            let most = path.iter().copied().fold(f32::MIN, f32::max);
            assert!(most < 0.5 + 0.1, "{rate}: overshoots to {most}");
            let end = path.last().copied().unwrap_or(0.0);
            assert!((end - 0.5).abs() < 1e-4, "{rate}: ends at {end}");
        }
    }

    /// Galvo coordinates of a direction from the test projector.
    fn galvo_of(direction: Vec3) -> [f32; 2] {
        let angle = |c: f32| c.atan2(direction.y).to_degrees() / 30.0;
        [angle(direction.x), angle(direction.z)]
    }

    /// A lit square, `side` points along each edge, corner to corner.
    fn square(side: usize) -> Vec<LaserPoint> {
        let corners = [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
        (0..4)
            .flat_map(|edge| {
                let [a, b] = [corners[edge], corners[(edge + 1) % 4]];
                (0..side).map(move |i| {
                    let t = i as f32 / side as f32;
                    LaserPoint {
                        x: a[0] + (b[0] - a[0]) * t,
                        y: a[1] + (b[1] - a[1]) * t,
                        rgb: [1.0; 3],
                    }
                })
            })
            .collect()
    }

    /// How far a piece of a sheet ends from galvo point `at`.
    fn off(sheet: &Sheet, at: [f32; 2]) -> f32 {
        let [x, y] = galvo_of(sheet.to);
        ((x - at[0]).powi(2) + (y - at[1]).powi(2)).sqrt()
    }

    /// Scanned fast, a square's corners round off, and the mirrors slow
    /// through them, so the light dwells there: the corners are brighter
    /// than the middles of the edges. Scanned slowly, it keeps its corners.
    #[test]
    fn a_fast_square_has_round_bright_corners() {
        let corner = [0.5, 0.5];
        let miss = |sheets: &[Sheet]| {
            sheets
                .iter()
                .map(|s| off(s, corner))
                .fold(f32::MAX, f32::min)
        };
        let (_, fast) = light(&scan(square(10)));
        let (_, slow) = light(&scan(square(200)));
        let (fast_miss, slow_miss) = (miss(&fast), miss(&slow));
        assert!(fast_miss > 10.0 * slow_miss, "{fast_miss} vs {slow_miss}");
        let brightest = |at: [f32; 2]| {
            fast.iter()
                .filter(|s| off(s, at) < 0.15)
                .map(|s| s.density.x)
                .fold(0.0, f32::max)
        };
        let (round, middle) = (brightest(corner), brightest([0.0, 0.5]));
        assert!(round > 1.2 * middle, "{round} vs {middle}");
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
