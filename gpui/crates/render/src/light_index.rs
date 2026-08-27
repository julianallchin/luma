//! One clustered light index, in one structure, for every consumer that asks
//! "which fixtures can reach here".
//!
//! Drobot-style screen tiles + Z-bins (`docs/design/light-index-unification.md`):
//! an 8 px full-resolution tile grid where each tile holds a fixed 512-bit
//! light mask, and 4096 uniform view-depth bins each holding a `min|max`
//! range of *depth-sorted* light ids. Point consumers intersect mask with the
//! bin range; ray consumers walk the mask alone. The structure is fixed-size,
//! so building it cannot fail and never allocates per frame.
//!
//! The sort order is private. Consumers receive light ids in sorted space and
//! index a reordered SoA this module uploads; source order stays canonical for
//! shadow-slot residency, caster culling, and every existing test. Nothing
//! outside this module may persist a sorted id across frames.
//!
//! The GPU builder is validated bit-identical against the CPU reference
//! builder, which therefore stays permanently — it is the only thing that
//! will catch a WGSL/Rust drift in the culling tests.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use wgpu::util::DeviceExt as _;

use crate::frame::{Camera, FixtureCone, MAX_FIXTURE_CONES};

/// Screen-tile edge, in full-resolution pixels. Consumers rendering at a
/// fraction of output resolution scale their fragment coordinate; the index is
/// never rebuilt per consumer resolution.
pub const TILE_SIZE: u32 = 8;
/// Words per tile mask — 512 bits, one per possible fixture cone.
pub const MASK_WORDS: usize = 16;
/// Uniform view-depth bins over `[near, far]`.
pub const Z_BINS: usize = 4096;

// Raising the fixture ceiling without widening the mask would silently
// truncate the index; make it a build error instead.
const _: () = assert!(MAX_FIXTURE_CONES == MASK_WORDS * 32);

/// Narrow phase: after the rect overlap, test the light's cone against the
/// tile's frustum wedge restricted to that light's own depth span (Wronski's
/// cone/sphere test). The restriction is what keeps the wedge sphere
/// well-conditioned — the CSR era's fixed Z-slices produced 20:1 splinters
/// whose spheres rejected nothing. Both builders read this constant (the GPU
/// shader gets it injected at pipeline creation), so the bit-identity gate
/// covers the narrow phase too.
pub(crate) const NARROW_PHASE: bool = true;

/// Sentinel Z-bin: `min > max`, so the id-range walk is empty.
const EMPTY_BIN: u32 = 0xFFFF_0000;

const EYE_EPSILON: f32 = 1.0e-4;

/// Everything the index is a function of. One struct so a caller cannot
/// supply the camera without the viewport it was framed for.
pub struct LightIndexInput<'a> {
    /// Fixture cones in source order; entries past [`MAX_FIXTURE_CONES`] are
    /// ignored, mirroring the upload path's own truncation.
    pub cones: &'a [FixtureCone],
    /// The camera the frame renders through; sanitised on entry.
    pub camera: Camera,
    /// Full-resolution viewport; zero dimensions are clamped to one pixel.
    pub viewport: [u32; 2],
    /// View-space near plane, metres.
    pub near: f32,
    /// View-space far plane, metres.
    pub far: f32,
}

/// The reference CPU build of the index.
///
/// Deterministic and infallible: the viewport is clamped, the cone slice is
/// truncated to the mask width, and every array below has a size fixed by the
/// input dimensions alone. The GPU builder must reproduce `tile_masks` and
/// `z_bins` bit for bit.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuLightIndex {
    /// Number of 8 px tile columns.
    pub columns: u32,
    /// Number of 8 px tile rows.
    pub rows: u32,
    /// `columns * rows * MASK_WORDS` words; tile `(x, y)` owns the slice at
    /// `(y * columns + x) * MASK_WORDS`. Bit `i` means sorted light `i` can
    /// reach somewhere in the tile's frustum wedge.
    pub tile_masks: Vec<u32>,
    /// `Z_BINS` packed ranges, `min << 16 | max` in sorted-id space,
    /// [`EMPTY_BIN`] when no light spans the bin's depth slab.
    pub z_bins: Vec<u32>,
    /// Sorted id → source index into `LightIndexInput::cones`. The GPU path
    /// applies this to the SoA upload instead; tests use it to translate.
    pub sorted_to_source: Vec<u32>,
    near: f32,
    far: f32,
    view: View,
}

/// The camera-and-sort half of the build, shared verbatim by the CPU
/// reference and the GPU builder so the two cannot disagree about extents,
/// ordering, or Z-bins — only the tile-mask fill differs between them.
struct Prepared {
    columns: u32,
    rows: u32,
    near: f32,
    far: f32,
    view: View,
    /// `(source index, sanitised cone, extent)` in sorted (view-depth) order;
    /// the cone rides along for the GPU narrow phase's upload.
    extents: Vec<(u32, SanitizedCone, LightExtent)>,
    z_bins: Vec<u32>,
}

fn prepare(input: &LightIndexInput<'_>) -> Prepared {
    let viewport = [input.viewport[0].max(1), input.viewport[1].max(1)];
    let columns = viewport[0].div_ceil(TILE_SIZE);
    let rows = viewport[1].div_ceil(TILE_SIZE);
    let near = finite(input.near, 0.05).clamp(0.001, 10_000.0);
    let far = finite(input.far, near + 1.0).clamp(near + 0.001, 100_000.0);
    let view = View::new(input.camera, viewport, near, far);

    let cones = &input.cones[..input.cones.len().min(MAX_FIXTURE_CONES)];
    let mut extents: Vec<(u32, SanitizedCone, LightExtent)> = cones
        .iter()
        .enumerate()
        .filter_map(|(source, cone)| {
            let cone = SanitizedCone::new(cone);
            view.extent_for(&cone)
                .map(|extent| (source as u32, cone, extent))
        })
        .collect();
    // Depth sort is what makes a Z-bin's id range tight; ties break on
    // source index so the build is reproducible.
    extents.sort_by(|(a_src, _, a), (b_src, _, b)| a.z0.total_cmp(&b.z0).then(a_src.cmp(b_src)));

    let mut z_bins = vec![EMPTY_BIN; Z_BINS];
    for (sorted, (_, _, extent)) in extents.iter().enumerate() {
        let bin0 = depth_bin(extent.z0, near, far);
        let bin1 = depth_bin(extent.z1, near, far);
        let sorted = sorted as u32;
        for bin in &mut z_bins[bin0 as usize..=bin1 as usize] {
            let (min, max) = if *bin == EMPTY_BIN {
                (sorted, sorted)
            } else {
                ((*bin >> 16).min(sorted), (*bin & 0xFFFF).max(sorted))
            };
            *bin = min << 16 | max;
        }
    }

    Prepared {
        columns,
        rows,
        near,
        far,
        view,
        extents,
        z_bins,
    }
}

impl CpuLightIndex {
    /// Builds the index. See [`CpuLightIndex`] for the determinism and
    /// infallibility contract.
    #[must_use]
    pub fn build(input: &LightIndexInput<'_>) -> Self {
        let prepared = prepare(input);
        let columns = prepared.columns;
        let mut tile_masks = vec![0_u32; columns as usize * prepared.rows as usize * MASK_WORDS];
        for (sorted, (_, cone, extent)) in prepared.extents.iter().enumerate() {
            let (word, bit) = (sorted / 32, sorted as u32 % 32);
            for tile_y in extent.y0..=extent.y1 {
                for tile_x in extent.x0..=extent.x1 {
                    if NARROW_PHASE {
                        let (centre, radius) = prepared
                            .view
                            .wedge_sphere(tile_x, tile_y, extent.z0, extent.z1);
                        if !cone_reaches_sphere(
                            cone.position,
                            cone.direction,
                            cone.range,
                            cone.cos_field,
                            centre,
                            radius,
                        ) {
                            continue;
                        }
                    }
                    let base = (tile_y * columns + tile_x) as usize * MASK_WORDS;
                    tile_masks[base + word] |= 1 << bit;
                }
            }
        }

        Self {
            columns,
            rows: prepared.rows,
            tile_masks,
            z_bins: prepared.z_bins,
            sorted_to_source: prepared
                .extents
                .iter()
                .map(|(source, _, _)| *source)
                .collect(),
            near: prepared.near,
            far: prepared.far,
            view: prepared.view,
        }
    }

    /// Source indices of lights whose cone can reach anywhere along the ray
    /// through this full-resolution pixel — the ray consumer's query.
    #[must_use]
    pub fn lights_along(&self, pixel_x: u32, pixel_y: u32) -> Vec<u32> {
        let mask = self.tile_mask(pixel_x, pixel_y);
        self.collect(mask, 0, self.sorted_to_source.len().saturating_sub(1))
    }

    /// Source indices of lights whose cone can reach this pixel at this view
    /// depth — the point consumer's query, mask ∩ Z-bin range.
    #[must_use]
    pub fn lights_at(&self, pixel_x: u32, pixel_y: u32, view_depth: f32) -> Vec<u32> {
        let bin = self.z_bins[depth_bin(view_depth, self.near, self.far) as usize];
        if bin == EMPTY_BIN {
            return Vec::new();
        }
        let mask = self.tile_mask(pixel_x, pixel_y);
        self.collect(mask, (bin >> 16) as usize, (bin & 0xFFFF) as usize)
    }

    /// The mask words for the tile containing a full-resolution pixel;
    /// out-of-range pixels clamp to the edge tile, mirroring the shaders.
    #[must_use]
    pub fn tile_mask(&self, pixel_x: u32, pixel_y: u32) -> &[u32] {
        let x = (pixel_x / TILE_SIZE).min(self.columns - 1);
        let y = (pixel_y / TILE_SIZE).min(self.rows - 1);
        let base = (y * self.columns + x) as usize * MASK_WORDS;
        &self.tile_masks[base..base + MASK_WORDS]
    }

    fn collect(&self, mask: &[u32], min_sorted: usize, max_sorted: usize) -> Vec<u32> {
        let mut out = Vec::new();
        for (word, &bits) in mask.iter().enumerate() {
            let mut bits = bits;
            while bits != 0 {
                let sorted = word * 32 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if (min_sorted..=max_sorted).contains(&sorted) {
                    out.push(self.sorted_to_source[sorted]);
                }
            }
        }
        out
    }

    /// View depth of a world point under the build's sanitised camera —
    /// lets tests derive `lights_at` arguments from world-space samples.
    #[must_use]
    pub fn view_depth(&self, point: Vec3) -> f32 {
        (point - self.view.eye).dot(self.view.forward)
    }

    /// Full-resolution pixel of a world point, or `None` when it projects
    /// off screen or behind the eye.
    #[must_use]
    pub fn project(&self, point: Vec3) -> Option<[u32; 2]> {
        self.view.project(point)
    }
}

/// Number of 8 px tiles along one edge of a 64 px prepass big tile.
const BIG_FACTOR: u32 = 8;

/// Hot half of a fixture cone's shading SoA — position and reach, all a
/// culling or distance test needs. Mirrored in `scene_bindings.wgsl` and
/// `beam_transport.wgsl`; this module owns the upload so the buffer order and
/// the index's id space cannot disagree.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct LightCore {
    pub position: [f32; 3],
    pub range: f32,
}

/// Cold half of a fixture cone's shading SoA — photometry and shadow slot.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct LightRest {
    pub direction: [f32; 3],
    pub cos_beam: f32,
    pub color: [f32; 3],
    pub intensity: f32,
    pub cos_field: f32,
    pub wash: f32,
    pub gobo: f32,
    pub gobo_rotation: f32,
    /// Layer of this cone's shadow map, or negative when it has none.
    ///
    /// Shadow maps are capped and assigned per frame, so a cone's layer is
    /// not its index and the shaders must be told which one it got. The slot
    /// rides through this module's reorder untouched — slot assignment stays
    /// keyed to source order.
    pub shadow_slot: f32,
    /// WGSL rounds this struct to its 16-byte `vec3` alignment; Rust does
    /// not. Without the explicit tail the array strides disagree and every
    /// light after the first reads the previous one's bytes.
    pub _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct IndexParams {
    /// columns, rows, big_columns, big_rows.
    grid: [u32; 4],
    /// light_count, viewport width, viewport height, unused.
    counts: [u32; 4],
    /// near, `Z_BINS / (far - near)`, unused ×2.
    depth: [f32; 4],
    /// Camera basis for the narrow phase's tile-wedge spheres. xyz + one
    /// packed scalar each: eye (w unused), right (w: tan half fov),
    /// up (w: aspect), forward (w unused).
    eye: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    forward: [f32; 4],
}

/// Per-light culling record, sorted order, mirrored in
/// `light_index_build.wgsl`. Carries the cone and depth span the narrow phase
/// will use so switching that on is a shader change, not a layout change.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct LightCull {
    rect: [u32; 4],
    apex_range: [f32; 4],
    dir_cos: [f32; 4],
    span: [f32; 4],
}

/// The device-shared half of the GPU builder: layouts and the two build
/// pipelines. Lives beside the other shared pipelines so consumer render
/// pipelines can reference [`Self::consumer_layout`] before any renderer or
/// frame exists; the mutable per-renderer buffers are [`LightIndex`].
pub struct LightIndexPipelines {
    consumer_layout: wgpu::BindGroupLayout,
    build_layout: wgpu::BindGroupLayout,
    prepass: wgpu::ComputePipeline,
    fill: wgpu::ComputePipeline,
    count_layout: wgpu::BindGroupLayout,
    count: wgpu::ComputePipeline,
}

/// The GPU-resident index: persistent buffers and the consumer bind group.
/// See the module docs for the structure; the CPU half of a build (sanitise,
/// sort, Z-bins, screen rects) is [`prepare`], shared with [`CpuLightIndex`]
/// so the two builders cannot drift.
/// Broad-phase metrics of the latest build, derived on the CPU from the
/// per-light tile rects — free relative to the build itself. The
/// mask-popcount numbers (`mean_lights_per_fragment`, per-tile max) need the
/// GPU counter pass and land with the narrow phase (design doc §8); until
/// then `tile_references` over `total_tiles` is the honest broad-phase bound.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct LightIndexStats {
    /// Cones the frame submitted, before culling.
    pub lights_total: u32,
    /// Cones whose clipped screen rect is non-empty — everything the masks
    /// can possibly contain.
    pub lights_on_screen: u32,
    /// Σ over lights of their tile-rect area: the number of (tile, light)
    /// mask bits the broad phase set.
    pub tile_references: u64,
    /// `tile_references / total_tiles` — the ray consumer's mean candidate
    /// count under broad phase alone.
    pub mean_lights_per_tile: f64,
    /// 8 px tiles across the frame, for scaling the numbers above.
    pub total_tiles: u32,
}

impl LightIndexStats {
    fn of(prepared: &Prepared, lights_total: usize) -> Self {
        let total_tiles = prepared.columns * prepared.rows;
        let tile_references: u64 = prepared
            .extents
            .iter()
            .map(|(_, _, extent)| {
                u64::from(extent.x1 - extent.x0 + 1) * u64::from(extent.y1 - extent.y0 + 1)
            })
            .sum();
        Self {
            lights_total: lights_total.min(MAX_FIXTURE_CONES) as u32,
            lights_on_screen: prepared.extents.len() as u32,
            tile_references,
            mean_lights_per_tile: tile_references as f64 / f64::from(total_tiles.max(1)),
            total_tiles,
        }
    }
}

/// The GPU-resident index: persistent buffers and the consumer bind group.
/// The CPU half of a build (sanitise, sort, Z-bins, screen rects) is
/// [`prepare`], shared with [`CpuLightIndex`] so the two builders cannot
/// drift; this struct owns the reordered light SoA upload (§3).
pub struct LightIndex {
    stats: LightIndexStats,
    params: wgpu::Buffer,
    lights: wgpu::Buffer,
    z_bins: wgpu::Buffer,
    /// The reordered light SoA. Owned here because the index and the light
    /// buffers must agree on ordering (§3): consumers' ids index these
    /// directly, and nothing outside this module ever sees a sorted id
    /// translated back to source space.
    core: wgpu::Buffer,
    rest: wgpu::Buffer,
    fragment_counters: wgpu::Buffer,
    sized: Option<SizedBuffers>,
}

/// Buffer handles a consumer needs to compose the index into its own bind
/// group (the surface pass keeps shadow resources in the same group). Owned
/// clones (buffers are reference-counted), so the caller is not tied to the
/// index's borrow; valid only after the frame's [`LightIndex::build`].
pub(crate) struct LightIndexBindings {
    pub params: wgpu::Buffer,
    pub tile_masks: wgpu::Buffer,
    pub z_bins: wgpu::Buffer,
    pub core: wgpu::Buffer,
    pub rest: wgpu::Buffer,
    /// Two words the surface pass atomically accumulates under the profiler's
    /// flag: lit fragments, and candidates those fragments walked. Always
    /// bound (8 bytes); only written when the flag is on.
    pub fragment_counters: wgpu::Buffer,
}

/// The viewport-sized half of the buffers, recreated only on resize.
/// `tile_masks` is retained directly for consumers that compose their own
/// bind group, and for the validation test's readback.
struct SizedBuffers {
    columns: u32,
    rows: u32,
    tile_masks: wgpu::Buffer,
    build_bind_group: wgpu::BindGroup,
    consumer_bind_group: wgpu::BindGroup,
}

impl LightIndexPipelines {
    /// Compiles the two build pipelines and both layouts.
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        let storage = |read_only| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty,
            count: None,
        };
        let build_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light-index-build"),
            entries: &[
                entry(0, wgpu::ShaderStages::COMPUTE, uniform),
                entry(1, wgpu::ShaderStages::COMPUTE, storage(true)),
                entry(2, wgpu::ShaderStages::COMPUTE, storage(false)),
                entry(3, wgpu::ShaderStages::COMPUTE, storage(false)),
            ],
        });
        let consumer_stages = wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE;
        // Binding numbers 8–10 match the prelude (`shaders/light_index.wgsl`),
        // which starts high so it can share a group with a consumer's own
        // low-numbered bindings.
        let consumer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light-index"),
            entries: &[
                entry(8, consumer_stages, uniform),
                entry(9, consumer_stages, storage(true)),
                entry(10, consumer_stages, storage(true)),
            ],
        });

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("light-index-build"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "const NARROW_PHASE: bool = {NARROW_PHASE};\n{}",
                    include_str!("shaders/light_index_build.wgsl")
                )
                .into(),
            ),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("light-index-build"),
            bind_group_layouts: &[Some(&build_layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };

        // Profiler-only fragment counting: its own pass and layout so the
        // read-write counter buffer never touches the hot passes (bound
        // there, Metal serialised every pass sharing it).
        let count_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light-index-count"),
            entries: &[
                entry(0, wgpu::ShaderStages::COMPUTE, uniform),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                entry(2, wgpu::ShaderStages::COMPUTE, storage(false)),
            ],
        });
        let count_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("light-index-count"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}{}",
                    include_str!("shaders/light_index.wgsl"),
                    include_str!("shaders/light_index_count.wgsl")
                )
                .into(),
            ),
        });
        let count = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("light-index-count"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("light-index-count"),
                    bind_group_layouts: &[Some(&count_layout), Some(&consumer_layout)],
                    immediate_size: 0,
                }),
            ),
            module: &count_module,
            entry_point: Some("count_fragments"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        Self {
            prepass: pipeline("big_tile_prepass"),
            fill: pipeline("tile_fill"),
            consumer_layout,
            build_layout,
            count_layout,
            count,
        }
    }

    /// Layout for consumer pipeline creation, before any frame exists.
    #[must_use]
    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.consumer_layout
    }
}

impl LightIndex {
    /// Creates the fixed-size half of the buffers; the viewport-sized half is
    /// allocated lazily on the first [`Self::build`].
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-params"),
            size: std::mem::size_of::<IndexParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lights = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-lights"),
            size: (MAX_FIXTURE_CONES * std::mem::size_of::<LightCull>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let z_bins = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-zbins"),
            size: (Z_BINS * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let core = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-core"),
            size: (MAX_FIXTURE_CONES * std::mem::size_of::<LightCore>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let rest = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-rest"),
            size: (MAX_FIXTURE_CONES * std::mem::size_of::<LightRest>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fragment_counters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-fragment-counters"),
            size: 8,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            stats: LightIndexStats::default(),
            params,
            lights,
            z_bins,
            core,
            rest,
            fragment_counters,
            sized: None,
        }
    }

    /// Metrics of the latest [`Self::build`].
    #[must_use]
    pub fn stats(&self) -> LightIndexStats {
        self.stats
    }

    /// Records the profiler's fragment-count pass: one thread per depth
    /// texel, walking `lights_at` exactly as the surface pass does, into the
    /// counter pair [`LightIndexBindings::fragment_counters`]. Measurement
    /// path only — the encoder must already contain the frame's depth
    /// prepass and this frame's [`Self::build`].
    pub(crate) fn record_fragment_count(
        &self,
        pipelines: &LightIndexPipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        depth_view: &wgpu::TextureView,
        near: f32,
        far: f32,
        viewport: [u32; 2],
    ) {
        let sized = self.sized.as_ref().expect("light index built this frame");
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("light-index-count-params"),
            contents: bytemuck::cast_slice(&[near, far, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("light-index-count"),
            layout: &pipelines.count_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.fragment_counters.as_entire_binding(),
                },
            ],
        });
        encoder.clear_buffer(&self.fragment_counters, 0, None);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("light-index-count"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipelines.count);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_bind_group(1, &sized.consumer_bind_group, &[]);
        pass.dispatch_workgroups(viewport[0].div_ceil(8), viewport[1].div_ceil(8), 1);
    }

    /// The buffers a consumer composes into its own bind group.
    ///
    /// # Panics
    /// Panics before the first [`Self::build`] of a frame has allocated the
    /// viewport-sized buffers.
    pub(crate) fn bindings(&self) -> LightIndexBindings {
        let sized = self.sized.as_ref().expect("light index built this frame");
        LightIndexBindings {
            params: self.params.clone(),
            tile_masks: sized.tile_masks.clone(),
            z_bins: self.z_bins.clone(),
            core: self.core.clone(),
            rest: self.rest.clone(),
            fragment_counters: self.fragment_counters.clone(),
        }
    }

    /// Rebuilds the index for this frame. Records the two build dispatches
    /// into `encoder`; the caller must submit it before any pass bound to the
    /// returned bind group.
    ///
    /// Infallible by construction: the viewport is clamped, the cone slice is
    /// truncated to the mask width, and the structure is fixed-size, so there
    /// is no allocation that can fail and no dimension that can overflow.
    /// `cores`/`rests` are the shading SoA in *source* cone order (parallel
    /// to `input.cones`); this module reorders and uploads them (§3).
    /// `timestamps` brackets the build pass for the frame profiler; the pass
    /// runs either way.
    pub(crate) fn build(
        &mut self,
        pipelines: &LightIndexPipelines,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        input: &LightIndexInput<'_>,
        cores: &[LightCore],
        rests: &[LightRest],
        timestamps: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) -> &wgpu::BindGroup {
        let prepared = prepare(input);
        let (columns, rows) = (prepared.columns, prepared.rows);
        let big_columns = columns.div_ceil(BIG_FACTOR);
        let big_rows = rows.div_ceil(BIG_FACTOR);
        self.stats = LightIndexStats::of(&prepared, input.cones.len());

        if self
            .sized
            .as_ref()
            .is_none_or(|sized| (sized.columns, sized.rows) != (columns, rows))
        {
            self.sized =
                Some(self.allocate(pipelines, device, columns, rows, big_columns, big_rows));
        }

        let view = &prepared.view;
        let params = IndexParams {
            grid: [columns, rows, big_columns, big_rows],
            counts: [
                prepared.extents.len() as u32,
                view.viewport[0],
                view.viewport[1],
                0,
            ],
            depth: [
                prepared.near,
                Z_BINS as f32 / (prepared.far - prepared.near),
                0.0,
                0.0,
            ],
            eye: view.eye.extend(0.0).to_array(),
            right: view.right.extend(view.tan_half_fov).to_array(),
            up: view.up.extend(view.aspect).to_array(),
            forward: view.forward.extend(0.0).to_array(),
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        queue.write_buffer(&self.z_bins, 0, bytemuck::cast_slice(&prepared.z_bins));
        let lights: Vec<LightCull> = prepared
            .extents
            .iter()
            .map(|(_, cone, extent)| LightCull {
                rect: [extent.x0, extent.y0, extent.x1, extent.y1],
                apex_range: [
                    cone.position.x,
                    cone.position.y,
                    cone.position.z,
                    cone.range,
                ],
                dir_cos: [
                    cone.direction.x,
                    cone.direction.y,
                    cone.direction.z,
                    cone.cos_field,
                ],
                span: [extent.z0, extent.z1, 0.0, 0.0],
            })
            .collect();
        if !lights.is_empty() {
            queue.write_buffer(&self.lights, 0, bytemuck::cast_slice(&lights));
            let (sorted_cores, sorted_rests): (Vec<LightCore>, Vec<LightRest>) = prepared
                .extents
                .iter()
                .map(|&(source, ..)| (cores[source as usize], rests[source as usize]))
                .unzip();
            queue.write_buffer(&self.core, 0, bytemuck::cast_slice(&sorted_cores));
            queue.write_buffer(&self.rest, 0, bytemuck::cast_slice(&sorted_rests));
        }

        let sized = self.sized.as_ref().expect("allocated above");
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("light-index-build"),
            timestamp_writes: timestamps,
        });
        pass.set_bind_group(0, &sized.build_bind_group, &[]);
        pass.set_pipeline(&pipelines.prepass);
        pass.dispatch_workgroups(big_columns, big_rows, 1);
        pass.set_pipeline(&pipelines.fill);
        pass.dispatch_workgroups(big_columns, big_rows, 1);
        drop(pass);

        &sized.consumer_bind_group
    }

    fn allocate(
        &self,
        pipelines: &LightIndexPipelines,
        device: &wgpu::Device,
        columns: u32,
        rows: u32,
        big_columns: u32,
        big_rows: u32,
    ) -> SizedBuffers {
        let words = |tiles: u64| tiles * MASK_WORDS as u64 * 4;
        let big_masks = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-big-masks"),
            size: words(u64::from(big_columns) * u64::from(big_rows)),
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let tile_masks = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-tile-masks"),
            size: words(u64::from(columns) * u64::from(rows)),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let bind = |layout, label, entries: &[(u32, &wgpu::Buffer)]| {
            let entries: Vec<wgpu::BindGroupEntry> = entries
                .iter()
                .map(|&(binding, buffer)| wgpu::BindGroupEntry {
                    binding,
                    resource: buffer.as_entire_binding(),
                })
                .collect();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &entries,
            })
        };
        SizedBuffers {
            columns,
            rows,
            build_bind_group: bind(
                &pipelines.build_layout,
                "light-index-build",
                &[
                    (0, &self.params),
                    (1, &self.lights),
                    (2, &big_masks),
                    (3, &tile_masks),
                ],
            ),
            consumer_bind_group: bind(
                &pipelines.consumer_layout,
                "light-index",
                &[(8, &self.params), (9, &tile_masks), (10, &self.z_bins)],
            ),
            tile_masks,
        }
    }
}

/// Broad-phase result for one light: its clipped screen rect in tile
/// coordinates and its view-depth span.
#[derive(Debug, Clone, Copy)]
struct LightExtent {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    z0: f32,
    z1: f32,
}

/// The sanitised camera frame the whole build projects through.
///
/// The clamp ceiling on cone range is 100 m — the same bound the SoA upload
/// applies — so the culler can never disagree with the shaded light about how
/// far it reaches. (A validity bound, not a beam-length clamp: nothing scales
/// it by content.)
#[derive(Debug, Clone, Copy, PartialEq)]
struct View {
    eye: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    tan_half_fov: f32,
    aspect: f32,
    viewport: [u32; 2],
    columns: u32,
    rows: u32,
    near: f32,
    far: f32,
}

impl View {
    fn new(camera: Camera, viewport: [u32; 2], near: f32, far: f32) -> Self {
        let camera = sanitize_camera(camera);
        let forward = (camera.target - camera.eye).normalize_or(Vec3::Y);
        let world_up = if forward.z.abs() > 0.99 {
            Vec3::Y
        } else {
            Vec3::Z
        };
        let right = forward.cross(world_up).normalize_or(Vec3::X);
        let up = right.cross(forward).normalize_or(Vec3::Z);
        Self {
            eye: camera.eye,
            right,
            up,
            forward,
            tan_half_fov: (camera.fov_y_deg.to_radians() * 0.5).tan(),
            aspect: viewport[0] as f32 / viewport[1] as f32,
            viewport,
            columns: viewport[0].div_ceil(TILE_SIZE),
            rows: viewport[1].div_ceil(TILE_SIZE),
            near,
            far,
        }
    }

    /// The cone's world AABB clipped to the near plane and projected to a tile
    /// rect — the broad phase, moved from `clusters::BuildInput::bounds_for`.
    /// `None` means the light cannot touch the screen at all.
    fn extent_for(&self, cone: &SanitizedCone) -> Option<LightExtent> {
        let base = cone.position + cone.direction * cone.range;
        let radial =
            cone.range * (1.0 - cone.cos_field * cone.cos_field).max(0.0).sqrt() / cone.cos_field;
        let radial_extent = Vec3::new(
            (1.0 - cone.direction.x * cone.direction.x).max(0.0).sqrt(),
            (1.0 - cone.direction.y * cone.direction.y).max(0.0).sqrt(),
            (1.0 - cone.direction.z * cone.direction.z).max(0.0).sqrt(),
        ) * radial;
        let min = cone.position.min(base - radial_extent);
        let max = cone.position.max(base + radial_extent);

        let corners = box_corners(min, max);
        let depths = corners.map(|corner| (corner - self.eye).dot(self.forward));
        let min_depth = depths.iter().copied().fold(f32::INFINITY, f32::min);
        let max_depth = depths.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if max_depth <= EYE_EPSILON || max_depth < self.near || min_depth > self.far {
            return None;
        }

        let mut min_pixel = [f32::INFINITY; 2];
        let mut max_pixel = [f32::NEG_INFINITY; 2];
        for_each_clipped_vertex(
            &corners,
            &depths,
            self.near.max(EYE_EPSILON),
            |point, depth| {
                let relative = point - self.eye;
                let ndc_x = relative.dot(self.right) / (depth * self.tan_half_fov * self.aspect);
                let ndc_y = relative.dot(self.up) / (depth * self.tan_half_fov);
                let pixel_x = (ndc_x * 0.5 + 0.5) * self.viewport[0] as f32;
                let pixel_y = (0.5 - ndc_y * 0.5) * self.viewport[1] as f32;
                min_pixel[0] = min_pixel[0].min(pixel_x);
                min_pixel[1] = min_pixel[1].min(pixel_y);
                max_pixel[0] = max_pixel[0].max(pixel_x);
                max_pixel[1] = max_pixel[1].max(pixel_y);
            },
        );
        if min_pixel[0] > max_pixel[0] {
            return None;
        }
        if max_pixel[0] < 0.0
            || max_pixel[1] < 0.0
            || min_pixel[0] >= self.viewport[0] as f32
            || min_pixel[1] >= self.viewport[1] as f32
        {
            return None;
        }
        Some(LightExtent {
            x0: pixel_tile(min_pixel[0], self.columns),
            y0: pixel_tile(min_pixel[1], self.rows),
            x1: pixel_tile(max_pixel[0], self.columns),
            y1: pixel_tile(max_pixel[1], self.rows),
            z0: min_depth.max(self.near),
            z1: max_depth.min(self.far),
        })
    }

    /// Bounding sphere of one tile's frustum wedge clipped to a depth span.
    ///
    /// Mirrored operation-for-operation in `light_index_build.wgsl`'s
    /// `wedge_sphere` — the bit-identity gate depends on the two staying in
    /// lockstep.
    fn wedge_sphere(&self, tile_x: u32, tile_y: u32, z0: f32, z1: f32) -> (Vec3, f32) {
        let vw = self.viewport[0] as f32;
        let vh = self.viewport[1] as f32;
        let px0 = (tile_x * TILE_SIZE) as f32;
        let py0 = (tile_y * TILE_SIZE) as f32;
        let px1 = (px0 + TILE_SIZE as f32).min(vw);
        let py1 = (py0 + TILE_SIZE as f32).min(vh);
        let mut corners = [Vec3::ZERO; 8];
        let mut cursor = 0;
        for &z in &[z0, z1] {
            for &py in &[py0, py1] {
                for &px in &[px0, px1] {
                    let sx = (2.0 * px / vw - 1.0) * self.tan_half_fov * self.aspect;
                    let sy = (1.0 - 2.0 * py / vh) * self.tan_half_fov;
                    corners[cursor] =
                        self.eye + self.right * (sx * z) + self.up * (sy * z) + self.forward * z;
                    cursor += 1;
                }
            }
        }
        let center = corners.iter().copied().reduce(|a, b| a + b).unwrap() / 8.0;
        let radius_sq = corners
            .iter()
            .map(|corner| (*corner - center).length_squared())
            .fold(0.0_f32, f32::max);
        (center, radius_sq.sqrt())
    }

    fn project(&self, point: Vec3) -> Option<[u32; 2]> {
        let relative = point - self.eye;
        let depth = relative.dot(self.forward);
        if depth <= EYE_EPSILON {
            return None;
        }
        let ndc_x = relative.dot(self.right) / (depth * self.tan_half_fov * self.aspect);
        let ndc_y = relative.dot(self.up) / (depth * self.tan_half_fov);
        let pixel_x = (ndc_x * 0.5 + 0.5) * self.viewport[0] as f32;
        let pixel_y = (0.5 - ndc_y * 0.5) * self.viewport[1] as f32;
        if pixel_x < 0.0
            || pixel_y < 0.0
            || pixel_x >= self.viewport[0] as f32
            || pixel_y >= self.viewport[1] as f32
        {
            return None;
        }
        Some([pixel_x as u32, pixel_y as u32])
    }
}

/// One sanitiser for the whole index; see [`View`] for the range ceiling.
#[derive(Debug, Clone, Copy)]
struct SanitizedCone {
    position: Vec3,
    range: f32,
    direction: Vec3,
    cos_field: f32,
}

impl SanitizedCone {
    fn new(light: &FixtureCone) -> Self {
        Self {
            position: finite_vec(light.position, Vec3::ZERO)
                .clamp(Vec3::splat(-100_000.0), Vec3::splat(100_000.0)),
            range: finite(light.range, 0.05).clamp(0.05, 100.0),
            direction: finite_vec(light.direction, Vec3::NEG_Z)
                .try_normalize()
                .unwrap_or(Vec3::NEG_Z),
            cos_field: finite(light.cos_field, 0.95).clamp(0.01, 1.0),
        }
    }
}

fn sanitize_camera(camera: Camera) -> Camera {
    let eye =
        finite_vec(camera.eye, Vec3::ZERO).clamp(Vec3::splat(-100_000.0), Vec3::splat(100_000.0));
    let mut target = finite_vec(camera.target, eye + Vec3::Y)
        .clamp(Vec3::splat(-100_000.0), Vec3::splat(100_000.0));
    if target.distance_squared(eye) < 1.0e-8 {
        target = eye + Vec3::Y;
    }
    Camera {
        eye,
        target,
        fov_y_deg: finite(camera.fov_y_deg, 60.0).clamp(1.0, 179.0),
    }
}

/// Uniform view-depth bin for a depth, clamped into range. Uniform rather
/// than logarithmic: at 4096 bins the slab is thin everywhere, and uniform
/// arithmetic is one multiply in the shader.
fn depth_bin(depth: f32, near: f32, far: f32) -> u32 {
    let depth = finite(depth, near).clamp(near, far);
    let normalized = (depth - near) / (far - near);
    ((normalized * Z_BINS as f32) as u32).min(Z_BINS as u32 - 1)
}

fn pixel_tile(pixel: f32, tile_count: u32) -> u32 {
    ((pixel.max(0.0) as u32) / TILE_SIZE).min(tile_count - 1)
}

fn finite(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

fn finite_vec(value: Vec3, fallback: Vec3) -> Vec3 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// The eight corners of an axis-aligned box, ordered so bit 0 is X, bit 1 is Y
/// and bit 2 is Z — the order [`for_each_clipped_vertex`]'s edge table assumes.
#[must_use]
pub(crate) fn box_corners(min: Vec3, max: Vec3) -> [Vec3; 8] {
    [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
    ]
}

/// Visit every vertex of the box clipped to `depth >= clip`, with the depth to
/// project it at.
///
/// This is what keeps a volume straddling the eye plane from claiming the whole
/// screen. Clipping a convex polytope by a half-space yields vertices that are
/// either original corners inside it or points where an edge crosses the plane,
/// so visiting exactly those bounds the clipped projection with no slack.
///
/// `depths` must be the view depths of [`box_corners`] in the same order.
/// Callers own the projection: the cullers parameterise the camera
/// differently, but the clipping is one piece of geometry and lives here once.
pub(crate) fn for_each_clipped_vertex(
    corners: &[Vec3; 8],
    depths: &[f32; 8],
    clip: f32,
    mut visit: impl FnMut(Vec3, f32),
) {
    /// The twelve edges as index pairs into [`box_corners`], so the box is
    /// clipped as a wire frame rather than as a point cloud.
    const EDGES: [(usize, usize); 12] = [
        (0, 1),
        (2, 3),
        (4, 5),
        (6, 7),
        (0, 2),
        (1, 3),
        (4, 6),
        (5, 7),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    for (a, b) in EDGES {
        let (depth_a, depth_b) = (depths[a], depths[b]);
        let (inside_a, inside_b) = (depth_a >= clip, depth_b >= clip);
        if inside_a {
            visit(corners[a], depth_a);
        }
        if inside_b {
            visit(corners[b], depth_b);
        }
        if inside_a != inside_b {
            // The crossing point sits on the clip plane, so its depth is `clip`
            // by construction and the projection never divides by zero.
            let t = (clip - depth_a) / (depth_b - depth_a);
            visit(corners[a].lerp(corners[b], t), clip);
        }
    }
}

/// Does the solid cone reach the sphere?
///
/// Wronski's test: reject past the end of the throw, behind the apex, or
/// outside the opening angle, measuring from the sphere's centre to the nearest
/// point of the cone's surface so a sphere straddling the beam edge is kept.
///
/// **Conservative by contract.** Every caller uses this to decide what it may
/// skip, and the two mistakes are not symmetric: wrongly keeping something
/// costs a wasted draw or a longer light list, while wrongly dropping it is a
/// missing shadow or an unlit surface — a visible hole with no other symptom.
/// Bias any epsilon added here toward returning `true`.
#[must_use]
pub(crate) fn cone_reaches_sphere(
    apex: Vec3,
    direction: Vec3,
    range: f32,
    cos_field: f32,
    centre: Vec3,
    radius: f32,
) -> bool {
    let to_centre = centre - apex;
    let axial = to_centre.dot(direction);
    if axial > radius + range || axial < -radius {
        return false;
    }
    let perpendicular = (to_centre.length_squared() - axial * axial).max(0.0).sqrt();
    let sin_field = (1.0 - cos_field * cos_field).max(0.0).sqrt();
    cos_field * perpendicular - axial * sin_field <= radius
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera {
            eye: Vec3::ZERO,
            target: Vec3::Y,
            fov_y_deg: 60.0,
        }
    }

    fn cone(position: Vec3, direction: Vec3, range: f32, cos_field: f32) -> FixtureCone {
        FixtureCone {
            position,
            range,
            direction,
            cos_beam: (cos_field + 0.05).min(1.0),
            color: Vec3::ONE,
            intensity: 1.0,
            cos_field,
            wash: 0.0,
            gobo: 0,
            gobo_rotation: 0.0,
        }
    }

    fn input<'a>(cones: &'a [FixtureCone], camera: Camera) -> LightIndexInput<'a> {
        LightIndexInput {
            cones,
            camera,
            viewport: [1280, 720],
            near: 0.1,
            far: 100.0,
        }
    }

    /// Deterministic sample points strictly inside a cone's solid volume.
    fn interior_points(light: &FixtureCone) -> Vec<Vec3> {
        let direction = light.direction.normalize();
        let side = direction
            .cross(Vec3::X)
            .normalize_or(direction.cross(Vec3::Y).normalize());
        let up = direction.cross(side);
        let sin_field = (1.0 - light.cos_field * light.cos_field).max(0.0).sqrt();
        let mut points = Vec::new();
        for step in 1..=6 {
            let t = light.range * step as f32 / 7.0;
            let radius = t * sin_field / light.cos_field * 0.8;
            for spoke in 0..8 {
                let angle = spoke as f32 * std::f32::consts::TAU / 8.0;
                points.push(
                    light.position
                        + direction * t
                        + (side * angle.cos() + up * angle.sin()) * radius,
                );
            }
        }
        points
    }

    #[test]
    fn interior_points_are_conservatively_indexed() {
        let lights = [
            cone(
                Vec3::new(-1.5, 4.0, 1.0),
                Vec3::new(0.2, 0.4, -1.0).normalize(),
                5.0,
                0.9,
            ),
            cone(
                Vec3::new(2.0, 7.0, 2.5),
                Vec3::new(-0.3, 0.1, -1.0).normalize(),
                8.0,
                0.95,
            ),
            cone(Vec3::new(0.0, 2.0, 0.5), Vec3::Y, 4.0, 0.8),
        ];
        let index = CpuLightIndex::build(&input(&lights, camera()));
        for (source, light) in lights.iter().enumerate() {
            let source = source as u32;
            for point in interior_points(light) {
                let Some([px, py]) = index.project(point) else {
                    continue;
                };
                assert!(
                    index.lights_along(px, py).contains(&source),
                    "light {source} missing from tile mask at {point:?}"
                );
                assert!(
                    index
                        .lights_at(px, py, index.view_depth(point))
                        .contains(&source),
                    "light {source} missing from mask ∩ zbin at {point:?}"
                );
            }
        }
    }

    #[test]
    fn build_is_deterministic() {
        let lights = [
            cone(Vec3::new(-1.0, 4.0, 0.0), Vec3::Y, 3.0, 0.9),
            cone(Vec3::new(1.0, 6.0, 0.0), Vec3::Y, 2.0, 0.8),
            cone(Vec3::new(0.0, 2.0, 1.0), Vec3::Y, 5.0, 0.95),
        ];
        let first = CpuLightIndex::build(&input(&lights, camera()));
        let second = CpuLightIndex::build(&input(&lights, camera()));
        assert_eq!(first, second);
    }

    #[test]
    fn near_straddling_cone_does_not_claim_the_whole_screen() {
        // A cone whose volume crosses the eye plane off to the side: the clip
        // must bound its projection to a side band instead of surrendering the
        // screen. (A straddling volume that also crosses the view axis *does*
        // legitimately claim the whole screen — that case is the eye-inside
        // test below.)
        let lights = [cone(Vec3::new(2.0, -0.5, 0.3), Vec3::Y, 2.0, 0.9)];
        let index = CpuLightIndex::build(&input(&lights, camera()));
        let occupied = (0..index.rows)
            .flat_map(|y| (0..index.columns).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                index
                    .tile_mask(x * TILE_SIZE, y * TILE_SIZE)
                    .iter()
                    .any(|&w| w != 0)
            })
            .count();
        let total = (index.columns * index.rows) as usize;
        assert!(occupied > 0);
        assert!(occupied < total / 2, "occupied {occupied} of {total}");
    }

    #[test]
    fn eye_inside_cone_covers_the_screen() {
        // The beams-at-camera case: the eye sits inside the volume, so every
        // tile must keep the light — dropping any is a visible hole.
        let lights = [cone(Vec3::new(0.0, 6.0, 0.0), Vec3::NEG_Y, 12.0, 0.7)];
        let index = CpuLightIndex::build(&input(
            &lights,
            Camera {
                eye: Vec3::new(0.0, 1.7, 0.0),
                target: Vec3::new(0.0, 1.7, -4.0),
                fov_y_deg: 60.0,
            },
        ));
        for y in 0..index.rows {
            for x in 0..index.columns {
                assert!(
                    index
                        .tile_mask(x * TILE_SIZE, y * TILE_SIZE)
                        .iter()
                        .any(|&word| word != 0),
                    "tile ({x}, {y}) dropped a cone containing the eye"
                );
            }
        }
    }

    /// The GPU builder must reproduce the CPU reference bit for bit. This is
    /// the only thing that will catch a WGSL/Rust drift in the culling tests,
    /// so it stays permanently.
    #[test]
    fn gpu_builder_matches_cpu_reference_bit_for_bit() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("light-index-test"),
            ..Default::default()
        }))
        .expect("device");

        // Mixed population: on-screen, straddling, eye-inside, off-screen,
        // degenerate — every broad-phase branch.
        let mut lights = vec![
            cone(
                Vec3::new(-1.5, 4.0, 1.0),
                Vec3::new(0.2, 0.4, -1.0).normalize(),
                5.0,
                0.9,
            ),
            cone(Vec3::new(2.0, -0.5, 0.3), Vec3::Y, 2.0, 0.9),
            cone(Vec3::new(0.0, 6.0, 0.0), Vec3::NEG_Y, 12.0, 0.7),
            cone(Vec3::new(50.0, 3.0, 0.0), Vec3::Y, 2.0, 0.9),
            cone(Vec3::new(0.0, 2.0, 0.5), Vec3::ZERO, f32::NAN, 0.8),
        ];
        for i in 0..40 {
            let angle = i as f32 * 0.37;
            lights.push(cone(
                Vec3::new(angle.cos() * 4.0, 2.0 + i as f32 * 0.5, angle.sin() * 4.0),
                Vec3::new(angle.sin(), -0.4, angle.cos()).normalize(),
                3.0 + (i % 7) as f32,
                0.75 + (i % 5) as f32 * 0.04,
            ));
        }
        let input = input(&lights, camera());
        let reference = CpuLightIndex::build(&input);

        let cores: Vec<LightCore> = lights
            .iter()
            .map(|light| LightCore {
                position: light.position.to_array(),
                range: light.range,
            })
            .collect();
        let rests = vec![LightRest::zeroed(); lights.len()];
        let pipelines = LightIndexPipelines::new(&device);
        let mut index = LightIndex::new(&device);
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        index.build(
            &pipelines,
            &device,
            &queue,
            &mut encoder,
            &input,
            &cores,
            &rests,
            None,
        );
        let sized = index.sized.as_ref().expect("sized buffers");
        let size = sized.tile_masks.size();
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("light-index-readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&sized.tile_masks, 0, &readback, 0, size);
        queue.submit([encoder.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("map readback");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let view = readback.slice(..).get_mapped_range().expect("mapped");
        let gpu_masks: &[u32] = bytemuck::cast_slice(&view);
        assert_eq!(gpu_masks.len(), reference.tile_masks.len());
        assert_eq!(gpu_masks, &reference.tile_masks[..], "tile masks diverged");
    }

    #[test]
    fn zbin_range_excludes_far_separated_depths() {
        // A near light and a far light: at the near light's depth the bin
        // range must not include the far light.
        let near_light = cone(Vec3::new(0.0, 3.0, 0.0), Vec3::NEG_Z, 2.0, 0.9);
        let far_light = cone(Vec3::new(0.0, 80.0, 0.0), Vec3::NEG_Z, 2.0, 0.9);
        let lights = [near_light, far_light];
        let index = CpuLightIndex::build(&input(&lights, camera()));
        let point = Vec3::new(0.0, 3.5, -0.5);
        let Some([px, py]) = index.project(point) else {
            panic!("sample projects on screen");
        };
        let at = index.lights_at(px, py, index.view_depth(point));
        assert!(at.contains(&0));
        assert!(!at.contains(&1), "far light leaked into near depth bin");
    }
}
