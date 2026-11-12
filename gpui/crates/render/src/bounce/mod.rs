//! Bounced light: what the stage's lit surfaces send on to the rest of it.
//!
//! A lit floor lights the wall beside it and the truss over it. The sky
//! probe (`environment.rs`) knows the sky and nothing nearer, so a room lit
//! only by its fixtures had black walls, and a truss reflected nothing of
//! the lit deck under it. This keeps what the stage *is* and not how it is
//! lit, so the light can change every frame at little cost:
//!
//! - **Bake** (`bake.rs`, on a thread, when the stage's geometry changes
//!   ([`key`])): every big surface as discs, and for each disc and each
//!   point in the air the share of every other surface's light that lands
//!   on it.
//! - **Relight** (`bounce_relight.wgsl`): the light landing on every disc
//!   this frame, by the frame's sun, sky and fixtures, through the scene
//!   pass's own functions. A fixture that turns red sends red off the floor
//!   that frame.
//! - **Spread** (`bounce_spread.wgsl`): each receiver adds up what its
//!   links bring it. A disc sends that on in the next frame's relight, so
//!   the light goes a bounce further each frame.
//! - **Low pictures** (`bounce_low.wgsl`): at every few pixels of the depth
//!   prepass, the bounce of the discs on the surface there.
//! - **Sample** (`bounce_sample.wgsl`): the scene pass takes a big
//!   surface's bounce from the low pictures, and anything else's (a truss,
//!   a fixture) from the points in the air.
//!
//! Beams and haze stay out: discs are surfaces, and the air is drawn over
//! the frame after them.

pub(crate) mod bake;
pub(crate) mod stage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use wgpu::util::DeviceExt;

use crate::frame::{Draw, Frame};
use crate::quality::QualityBudget;
use stage::{Stage, AIR_MAX, BLOCK};

/// The low pictures' size, texels: they are cut to the output's size over
/// the level's step, and the step grows when that would not fit.
const LOW_SIZE: (u32, u32) = (1024, 640);
/// Frames a changed stage must hold still before it is baked again, so a
/// piece being dragged does not start a bake a frame.
const SETTLE_FRAMES: u32 = 8;
/// How much an emitter's size in a disc's sky widens the disc's highlight
/// of it, so the highlights of neighbouring emitters join up.
const HIGHLIGHT_WIDENING: f32 = 0.3;
/// Bounces a still's light goes in its one frame: past these a matte room
/// has a hundredth of its light left to send.
const STILL_BOUNCES: u32 = 4;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Shared WGSL every bounce shader, and the scene module, opens with.
pub(crate) fn prelude() -> &'static str {
    include_str!("../shaders/bounce_common.wgsl")
}

/// The relight module's source: the scene module and the relight's entry.
fn relight_wgsl(scene: &str) -> String {
    format!("{scene}{}", include_str!("../shaders/bounce_relight.wgsl"))
}

/// `BounceField` in `bounce_common.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct Field {
    origin: [f32; 4],
    dims: [f32; 4],
    air_origin: [f32; 4],
    air_dims: [f32; 4],
    counts: [u32; 4],
    eye: [f32; 4],
}

/// Everything the bake depends on: the stage's geometry and materials
/// without the lights' own bodies, the floor, and what the level spends.
/// Fixture light, the sun and the clouds are the relight's, and change
/// nothing here. A wall hidden from the camera is still a wall.
fn key(frame: &Frame, opaque: usize, budget: stage::Budget) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut push = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let draw_bytes = |draw: &Draw| {
        let mut bytes = frame.meshes[draw.mesh].key.as_bytes().to_vec();
        let m = &draw.material;
        for value in draw.model.to_cols_array().into_iter().chain([
            m.base_color.x,
            m.base_color.y,
            m.base_color.z,
            m.metallic,
            m.roughness,
        ]) {
            bytes.extend(value.to_bits().to_le_bytes());
        }
        bytes
    };
    // The room's panels in one order, whichever of them the camera hides.
    let mut panels = Vec::new();
    for draw in stage::draws(frame, opaque) {
        if frame.meshes[draw.mesh]
            .key
            .starts_with(crate::frame::ROOM_PANEL)
        {
            panels.push(draw_bytes(draw));
        } else {
            push(&draw_bytes(draw));
        }
    }
    panels.sort();
    for panel in panels {
        push(&panel);
    }
    push(format!("{:?}", frame.floor.map(|floor| floor.floor)).as_bytes());
    for value in [budget.spacing, budget.discs, budget.error] {
        push(&value.to_bits().to_le_bytes());
    }
    hash
}

fn layout_entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

fn storage(read_only: bool) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn uniform() -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn storage_texture(view_dimension: wgpu::TextureViewDimension) -> wgpu::BindingType {
    wgpu::BindingType::StorageTexture {
        access: wgpu::StorageTextureAccess::WriteOnly,
        format: FORMAT,
        view_dimension,
    }
}

fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

/// What the bounce pipelines are built against.
pub(crate) struct PipelineSources<'a> {
    pub scene_layout: &'a wgpu::BindGroupLayout,
    pub environment_layout: &'a wgpu::BindGroupLayout,
    pub cluster_layout: &'a wgpu::BindGroupLayout,
    /// The scene module's source, which the relight is appended to.
    pub scene_source: &'a str,
}

/// The device's bounce pipelines.
pub(crate) struct Pipelines {
    relight_layout: wgpu::BindGroupLayout,
    relight: wgpu::ComputePipeline,
    spread_layout: wgpu::BindGroupLayout,
    /// `pull_patches`, `pull_groups`, `gather_discs`, `gather_air`.
    spread: [wgpu::ComputePipeline; 4],
    low_layout: wgpu::BindGroupLayout,
    low: wgpu::ComputePipeline,
}

impl Pipelines {
    pub(crate) fn new(device: &wgpu::Device, sources: &PipelineSources<'_>) -> Self {
        let shader = |label: &str, source: String| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        };
        let layout = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let pipeline = |label: &str,
                        groups: &[Option<&wgpu::BindGroupLayout>],
                        module: &wgpu::ShaderModule,
                        entry_point: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: groups,
                        immediate_size: 0,
                    }),
                ),
                module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };

        let relight_layout = layout(
            "bounce-relight",
            &[
                layout_entry(16, storage(true)),
                layout_entry(17, storage(true)),
                layout_entry(18, storage(true)),
                layout_entry(19, storage(false)),
                layout_entry(20, uniform()),
            ],
        );
        let relight = pipeline(
            "bounce-relight",
            &[
                Some(sources.scene_layout),
                Some(&relight_layout),
                Some(sources.environment_layout),
                Some(sources.cluster_layout),
            ],
            &shader("bounce-relight", relight_wgsl(sources.scene_source)),
            "bounce_relight",
        );

        let spread_layout = layout(
            "bounce-spread",
            &[
                layout_entry(0, uniform()),
                layout_entry(1, storage(true)),
                layout_entry(2, storage(true)),
                layout_entry(3, storage(true)),
                layout_entry(4, storage(false)),
                layout_entry(5, storage(true)),
                layout_entry(6, storage(true)),
                layout_entry(7, storage(false)),
                layout_entry(8, storage(false)),
                layout_entry(9, storage_texture(wgpu::TextureViewDimension::D3)),
            ],
        );
        let spread_module = shader(
            "bounce-spread",
            format!(
                "{}{}",
                prelude(),
                include_str!("../shaders/bounce_spread.wgsl")
            ),
        );
        let spread =
            ["pull_patches", "pull_groups", "gather_discs", "gather_air"].map(|entry_point| {
                pipeline(
                    "bounce-spread",
                    &[Some(&spread_layout)],
                    &spread_module,
                    entry_point,
                )
            });

        let low_layout = layout(
            "bounce-low",
            &[
                layout_entry(0, uniform()),
                layout_entry(1, uniform()),
                layout_entry(
                    2,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                layout_entry(3, storage(true)),
                layout_entry(4, storage(true)),
                layout_entry(5, storage(true)),
                layout_entry(6, storage(true)),
                layout_entry(7, storage(true)),
                layout_entry(8, storage_texture(wgpu::TextureViewDimension::D2)),
                layout_entry(9, storage_texture(wgpu::TextureViewDimension::D2)),
            ],
        );
        let low = pipeline(
            "bounce-low",
            &[Some(&low_layout)],
            &shader(
                "bounce-low",
                format!(
                    "{}{}",
                    prelude(),
                    include_str!("../shaders/bounce_low.wgsl")
                ),
            ),
            "bounce_low",
        );
        Self {
            relight_layout,
            relight,
            spread_layout,
            spread,
            low_layout,
            low,
        }
    }
}

/// A bake under way on its thread.
struct Job {
    stop: Arc<AtomicBool>,
    done: Receiver<Option<Stage>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A baked stage on the GPU.
struct Resident {
    /// The field uniform, but for the eye, the step and whether it is live.
    field: Field,
    discs: u32,
    patches: u32,
    groups: u32,
    air_points: u32,
    /// The relight's group, and a still's first bounce's.
    relight: [wgpu::BindGroup; 2],
    spread: wgpu::BindGroup,
    /// What the low pictures' group names of the stage: its discs, the
    /// cells that find them, and the light on them.
    disc_buffer: wgpu::Buffer,
    cell_start: wgpu::Buffer,
    cell_items: wgpu::Buffer,
    gathered: wgpu::Buffer,
    mirrored: wgpu::Buffer,
}

/// One renderer's bounce: the stage it has baked, the bake under way, and
/// the textures the scene pass reads.
pub(crate) struct Bounce {
    matte: wgpu::TextureView,
    mirror: wgpu::TextureView,
    air: wgpu::TextureView,
    field: wgpu::Buffer,
    /// What the field uniform last held.
    written: Option<Field>,
    /// Every fixture cone of the frame in source order, for the relight's
    /// group 3, and how many there are.
    cones: Option<(wgpu::Buffer, wgpu::Buffer, usize)>,
    /// `bounce_total` in `bounce_relight.wgsl`: as each frame of a live view
    /// reads it, and as a still's first bounce does, which sends out the
    /// direct light alone.
    total: [wgpu::Buffer; 2],
    /// The frame's [`crate::scene_desc::BounceView`] and its level: off,
    /// surfaces take the house's flat fill.
    enabled: bool,
    /// The stage baked, or being baked.
    key: Option<u64>,
    /// The stage the last frame showed, and for how many frames.
    held: (Option<u64>, u32),
    job: Option<Job>,
    stage: Option<Resident>,
    /// Output pixels per texel of the low pictures this frame.
    step: u32,
    /// Whether this is a live view, which takes the bake when it comes and
    /// carries the light a bounce a frame. Otherwise each frame stands on
    /// its own, as a still or a film frame must: the bake is waited for and
    /// the light goes [`STILL_BOUNCES`] bounces in the frame.
    live_view: bool,
}

impl Bounce {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let texture = |label: &str, size: wgpu::Extent3d, dimension| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension,
                    format: FORMAT,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let low = wgpu::Extent3d {
            width: LOW_SIZE.0,
            height: LOW_SIZE.1,
            depth_or_array_layers: 1,
        };
        let uniform = |label: &str, size: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Self {
            matte: texture("bounce-matte", low, wgpu::TextureDimension::D2),
            mirror: texture("bounce-mirror", low, wgpu::TextureDimension::D2),
            air: texture(
                "bounce-air",
                wgpu::Extent3d {
                    width: AIR_MAX.x,
                    height: AIR_MAX.y,
                    depth_or_array_layers: AIR_MAX.z * 6,
                },
                wgpu::TextureDimension::D3,
            ),
            field: uniform("bounce-field", std::mem::size_of::<Field>()),
            written: None,
            cones: None,
            total: [
                uniform("bounce-total", 16),
                uniform("bounce-total-first", 16),
            ],
            enabled: false,
            key: None,
            held: (None, 0),
            job: None,
            stage: None,
            step: 4,
            live_view: false,
        }
    }

    /// What the scene pass reads: the low pictures of the bounce on
    /// surfaces and off them, the field uniform, and the air's light.
    pub(crate) fn bindings(&self) -> crate::environment::LocalBounce<'_> {
        crate::environment::LocalBounce {
            matte: &self.matte,
            mirror: &self.mirror,
            air: &self.air,
            field: &self.field,
        }
    }

    /// Upload every fixture cone of the frame, in source order. The camera's
    /// light index holds only the cones in view, and light bounces from
    /// behind the camera too.
    pub(crate) fn set_lights<C: Pod, R: Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cores: &[C],
        rests: &[R],
    ) {
        let count = cores.len().min(rests.len());
        if self
            .cones
            .as_ref()
            .is_none_or(|(.., capacity)| *capacity < count)
        {
            let capacity = count.next_power_of_two().max(64);
            let buffer = |label: &str, stride: usize| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: (capacity * stride) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            };
            self.cones = Some((
                buffer("bounce-cores", std::mem::size_of::<C>()),
                buffer("bounce-rests", std::mem::size_of::<R>()),
                capacity,
            ));
        }
        let (core_buffer, rest_buffer, _) = self.cones.as_ref().expect("allocated above");
        if count > 0 {
            queue.write_buffer(core_buffer, 0, bytemuck::cast_slice(&cores[..count]));
            queue.write_buffer(rest_buffer, 0, bytemuck::cast_slice(&rests[..count]));
        }
        let discs = self.stage.as_ref().map_or(0, |stage| stage.discs);
        for (first, total) in self.total.iter().enumerate() {
            queue.write_buffer(
                total,
                0,
                bytemuck::cast_slice(&[count as u32, discs, first as u32, 0]),
            );
        }
    }

    /// The source-order cone buffers [`Self::set_lights`] filled, for the
    /// relight's copy of the cluster group.
    pub(crate) fn cone_buffers(&self) -> Option<(&wgpu::Buffer, &wgpu::Buffer)> {
        self.cones.as_ref().map(|(cores, rests, _)| (cores, rests))
    }

    /// Let the bounce follow a live view, or make every frame stand on its
    /// own ([`Self::live_view`]).
    pub(crate) fn set_live_view(&mut self, live: bool) {
        self.live_view = live;
    }

    /// Whether this frame has a baked stage to light.
    pub(crate) fn live(&self) -> bool {
        self.enabled && self.stage.is_some()
    }

    /// Whether the stage this renderer was last shown is still being baked.
    pub(crate) fn pending(&self) -> bool {
        self.enabled && (self.job.is_some() || self.held.0 != self.key)
    }

    /// Wait for the bake under way, if any, and take it in.
    pub(crate) fn finish(&mut self, pipelines: &Pipelines, device: &wgpu::Device) {
        if let Some(job) = self.job.take() {
            if let Ok(Some(stage)) = job.done.recv() {
                self.install(pipelines, device, &stage);
            }
        }
    }

    /// Take `frame`'s stage: start a bake of it when it is new, take in a
    /// bake that has finished, and tell the scene pass where things stand.
    ///
    /// A live view's first stage is baked at once. A stage that replaces
    /// one is baked when it has held still for [`SETTLE_FRAMES`], and the
    /// old one's bounce shows meanwhile. A still waits for its own.
    pub(crate) fn prepare(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &Frame,
        opaque: usize,
        size: (u32, u32),
    ) {
        let budget = QualityBudget::of(frame.quality).bounce;
        self.enabled = frame.bounce.enabled && budget.is_some();
        let Some(budget) = budget.filter(|_| self.enabled) else {
            self.write_field(queue, frame);
            return;
        };
        if let Some(job) = &self.job {
            match job.done.try_recv() {
                Ok(Some(stage)) => {
                    self.install(pipelines, device, &stage);
                    self.job = None;
                }
                Ok(None) | Err(TryRecvError::Disconnected) => self.job = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let key = Some(key(frame, opaque, budget));
        self.held = if self.held.0 == key {
            (key, self.held.1.saturating_add(1))
        } else {
            (key, 0)
        };
        let first = self.stage.is_none() && self.job.is_none();
        if key != self.key && (first || !self.live_view || self.held.1 >= SETTLE_FRAMES) {
            self.key = key;
            self.job = None;
            match stage::input(frame, opaque, budget) {
                Some(input) => {
                    let stop = Arc::new(AtomicBool::new(false));
                    let (send, done) = mpsc::channel();
                    let flag = stop.clone();
                    std::thread::spawn(move || {
                        // The renderer may be gone by the time this ends.
                        let _ = send.send(stage::build(input, &flag));
                    });
                    self.job = Some(Job { stop, done });
                }
                None => self.stage = None,
            }
            if !self.live_view {
                self.finish(pipelines, device);
            }
        }
        self.step = budget
            .step
            .max(size.0.div_ceil(LOW_SIZE.0))
            .max(size.1.div_ceil(LOW_SIZE.1));
        self.write_field(queue, frame);
    }

    /// Tell the scene pass and the bounce passes where things stand.
    fn write_field(&mut self, queue: &wgpu::Queue, frame: &Frame) {
        let mut field = self
            .stage
            .as_ref()
            .map_or_else(Field::default, |stage| stage.field);
        field.dims[3] = f32::from(u8::from(self.live()));
        field.air_dims[3] = self.step as f32;
        field.eye = Vec3::from(frame.camera.eye)
            .extend(HIGHLIGHT_WIDENING)
            .to_array();
        if self.written != Some(field) {
            self.written = Some(field);
            queue.write_buffer(&self.field, 0, bytemuck::bytes_of(&field));
        }
    }

    /// Put a baked stage on the GPU, in place of the one there.
    fn install(&mut self, pipelines: &Pipelines, device: &wgpu::Device, stage: &Stage) {
        let baked = &stage.baked;
        // A buffer cannot be empty; a spare element is never read.
        let storage_init = |label: &str, bytes: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: if bytes.is_empty() { &[0; 16] } else { bytes },
                usage: wgpu::BufferUsages::STORAGE,
            })
        };
        let zeroed = |label: &str, elements: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (elements.max(1) * 16) as u64,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        };
        let (discs, patches, groups) = (baked.discs.len(), baked.patches.len(), baked.groups.len());
        let disc_buffer = storage_init("bounce-discs", bytemuck::cast_slice(&baked.discs));
        let patch_buffer = storage_init("bounce-patches", bytemuck::cast_slice(&baked.patches));
        let group_buffer = storage_init("bounce-groups", bytemuck::cast_slice(&baked.groups));
        let starts = storage_init("bounce-starts", bytemuck::cast_slice(&baked.starts));
        let links = storage_init("bounce-links", bytemuck::cast_slice(&baked.links));
        let balls = storage_init("bounce-balls", bytemuck::cast_slice(&stage.balls));
        let cell_start = storage_init("bounce-cell-start", bytemuck::cast_slice(&stage.cell_start));
        let cell_items = storage_init("bounce-cell-items", bytemuck::cast_slice(&stage.cell_items));
        let radiosity = zeroed("bounce-radiosity", discs + patches + groups);
        let gathered = zeroed("bounce-gathered", discs);
        let mirrored = zeroed("bounce-mirrored", discs);
        let air_points = stage.air.dims.x * stage.air.dims.y * stage.air.dims.z;
        let field = Field {
            origin: stage.cells.origin.extend(stage.cells.step).to_array(),
            dims: stage.cells.dims.as_vec3().extend(0.0).to_array(),
            air_origin: stage.air.origin.extend(stage.air.step).to_array(),
            air_dims: stage.air.dims.as_vec3().extend(0.0).to_array(),
            counts: [discs as u32, patches as u32, groups as u32, air_points],
            eye: [0.0; 4],
        };
        let relight = [0, 1].map(|first: usize| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bounce-relight"),
                layout: &pipelines.relight_layout,
                entries: &[
                    entry(16, disc_buffer.as_entire_binding()),
                    entry(17, balls.as_entire_binding()),
                    entry(18, gathered.as_entire_binding()),
                    entry(19, radiosity.as_entire_binding()),
                    entry(20, self.total[first].as_entire_binding()),
                ],
            })
        });
        let spread = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bounce-spread"),
            layout: &pipelines.spread_layout,
            entries: &[
                entry(0, self.field.as_entire_binding()),
                entry(1, disc_buffer.as_entire_binding()),
                entry(2, patch_buffer.as_entire_binding()),
                entry(3, group_buffer.as_entire_binding()),
                entry(4, radiosity.as_entire_binding()),
                entry(5, starts.as_entire_binding()),
                entry(6, links.as_entire_binding()),
                entry(7, gathered.as_entire_binding()),
                entry(8, mirrored.as_entire_binding()),
                entry(9, wgpu::BindingResource::TextureView(&self.air)),
            ],
        });
        self.stage = Some(Resident {
            field,
            discs: discs as u32,
            patches: patches as u32,
            groups: groups as u32,
            air_points,
            relight,
            spread,
            disc_buffer,
            cell_start,
            cell_items,
            gathered,
            mirrored,
        });
    }

    /// Light the discs by this frame's light, then spread it: before the
    /// low pictures and the scene pass read it.
    pub(crate) fn encode_spread(
        &self,
        pipelines: &Pipelines,
        encoder: &mut wgpu::CommandEncoder,
        scene: &wgpu::BindGroup,
        environment: &wgpu::BindGroup,
        clusters: &wgpu::BindGroup,
        relight_timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
        spread_timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let Some(stage) = self.stage.as_ref().filter(|_| self.enabled) else {
            return;
        };
        let (mut relight_timestamps, mut spread_timestamps) =
            (relight_timestamps, spread_timestamps);
        for bounce in 0..if self.live_view { 1 } else { STILL_BOUNCES } {
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("bounce-relight"),
                    timestamp_writes: relight_timestamps.take(),
                });
                pass.set_pipeline(&pipelines.relight);
                pass.set_bind_group(0, scene, &[]);
                let first = usize::from(!self.live_view && bounce == 0);
                pass.set_bind_group(1, &stage.relight[first], &[]);
                pass.set_bind_group(2, environment, &[]);
                pass.set_bind_group(3, clusters, &[]);
                pass.dispatch_workgroups(stage.discs.div_ceil(BLOCK as u32), 1, 1);
            }
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("bounce-spread"),
                timestamp_writes: spread_timestamps.take(),
            });
            pass.set_bind_group(0, &stage.spread, &[]);
            for (pipeline, count) in pipelines.spread.iter().zip([
                stage.patches,
                stage.groups,
                stage.discs,
                stage.air_points,
            ]) {
                pass.set_pipeline(pipeline);
                pass.dispatch_workgroups(count.div_ceil(64), 1, 1);
            }
        }
    }

    /// Draw the low pictures of the bounce over `depth`, the frame's depth
    /// prepass, as `camera` (the ambient-visibility pass's parameters) saw
    /// it.
    pub(crate) fn encode_low(
        &self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        camera: &wgpu::Buffer,
        depth: &wgpu::TextureView,
        size: (u32, u32),
        timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let Some(stage) = self.stage.as_ref().filter(|_| self.enabled) else {
            return;
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bounce-low"),
            layout: &pipelines.low_layout,
            entries: &[
                entry(0, self.field.as_entire_binding()),
                entry(1, camera.as_entire_binding()),
                entry(2, wgpu::BindingResource::TextureView(depth)),
                entry(3, stage.disc_buffer.as_entire_binding()),
                entry(4, stage.cell_start.as_entire_binding()),
                entry(5, stage.cell_items.as_entire_binding()),
                entry(6, stage.gathered.as_entire_binding()),
                entry(7, stage.mirrored.as_entire_binding()),
                entry(8, wgpu::BindingResource::TextureView(&self.matte)),
                entry(9, wgpu::BindingResource::TextureView(&self.mirror)),
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("bounce-low"),
            timestamp_writes: timestamps,
        });
        pass.set_pipeline(&pipelines.low);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(
            size.0.div_ceil(self.step).div_ceil(8),
            size.1.div_ceil(self.step).div_ceil(8),
            1,
        );
    }
}
