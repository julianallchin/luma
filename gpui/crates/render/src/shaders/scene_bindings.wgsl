// Bind group 0 of the scene pass, shared by every pipeline that draws scene
// geometry (`scene.wgsl`, `grid.wgsl`). It lives in its own file because a
// storage array's element layout must match the Rust `Instance` byte for byte:
// two hand-kept copies of this struct drift, and when they do the symptom is a
// mesh drawn with another mesh's transform, not a compile error.

struct Globals {
    view_proj: mat4x4<f32>,
    light_view_proj: array<mat4x4<f32>, 3>,
    camera_pos: vec4<f32>,
    camera_forward: vec4<f32>,
    // xyz: far distance for each cascade, w: transition fraction.
    cascade_splits: vec4<f32>,
    ambient: vec4<f32>,
    // xy: centre of the room `ambient` and the directional light belong to,
    // world XY. zw: its half-extents. See `room_glow` in `scene.wgsl`.
    room: vec4<f32>,
    // x: metres past that room over which both die to nothing. Zero when
    // nothing bounds them — outdoors, or with no house rig at all. yzw unused.
    room_falloff: vec4<f32>,
    // xyz: direction toward the light. w: 1 when the directional light exists.
    dir_to_light: vec4<f32>,
    // rgb: directional radiance, w: shadow-filter radius in texels.
    dir_color: vec4<f32>,
    // x: point-light count, y: shadow-map texel size, z: shadows enabled,
    // w: material debug-view code.
    params: vec4<f32>,
    medium: ProceduralMedium,
    outdoor_sun: vec4<f32>,
    // Outdoor surface transmittance source. x: the fog grid's radial extent
    // in metres (the haze pass's `shadow.w`), y: 0 marches `medium_optical_depth`
    // per fragment, 1 reads the grid's camera transmittance, 2 reads the grid
    // only beyond z metres and marches nearer fragments. w unused.
    surface_fog: vec4<f32>,
    // xy: output size in pixels, zw: its reciprocal.
    viewport: vec4<f32>,
};

struct Instance {
    model: mat4x4<f32>,
    // Inverse-transpose of `model`; non-uniform physical-dimension scaling
    // makes this differ from `model`.
    normal_matrix: mat4x4<f32>,
    // rgb: base colour (linear), a: metallic.
    base_color: vec4<f32>,
    // rgb: emissive radiance, a: roughness.
    emissive: vec4<f32>,
    // x: flat shading, y: normal-map scale, z: occlusion strength.
    flags: vec4<f32>,
};

struct PointLightData {
    // xyz: world position, w: cutoff distance.
    position: vec4<f32>,
    // rgb: colour * intensity, a: unused.
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(0) @binding(2) var<storage, read> point_lights: array<PointLightData>;
@group(0) @binding(3) var shadow_map: texture_depth_2d_array;
@group(0) @binding(4) var shadow_sampler: sampler_comparison;
@group(0) @binding(10) var haze_noise_field: texture_3d<f32>;
@group(0) @binding(11) var haze_noise_sampler: sampler;
// Fixture-shadow pass only: draw indices bucketed by mesh, so one instanced
// draw covers every caster sharing a mesh. Other pipelines never reference
// it, so their layouts carry no entry for it.
@group(0) @binding(5) var<storage, read> caster_instances: array<u32>;

struct EnvironmentParams {
    intensity: f32,
    rotation: f32,
    enabled: f32,
    visible: f32,
};
@group(2) @binding(0) var environment_irradiance: texture_cube<f32>;
@group(2) @binding(1) var environment_specular: texture_cube<f32>;
@group(2) @binding(2) var environment_brdf: texture_2d<f32>;
@group(2) @binding(3) var environment_sampler: sampler;
@group(2) @binding(4) var<uniform> environment_params: EnvironmentParams;
// The probe's frame-constant mean lobe radiance (`environment_ambient.wgsl`).
@group(2) @binding(5) var<uniform> outdoor_ambient_mean: vec4<f32>;

struct FixtureLightCore {
    position: vec3<f32>,
    range: f32,
};

struct FixtureLightRest {
    direction: vec3<f32>,
    cos_beam: f32,
    color: vec3<f32>,
    intensity: f32,
    cos_field: f32,
    wash: f32,
    gobo: f32,
    gobo_rotation: f32,
    // Shadow-map layer for this cone, or negative when it has none: maps are
    // capped, so a cone's layer is not its index.
    shadow_slot: f32,
    // Scattering multiplier: one for stage fixtures and house lamps,
    // zero to disable scattering.
    haze_gain: f32,
    // Two scalars, not a `vec3`: a `vec3` member would take its own 16-byte
    // alignment and push the struct to 80 bytes, disagreeing with the Rust
    // stride. Scalars keep it at 64.
    inverse_right_length: f32,
    field_tangent: f32,
};

struct SurfaceClusterParams {
    // x: fixture surface lighting enabled, y: cluster-occupancy debug, z: depth-refined surface mask,
    // w: surface depth buckets per tile (0 = one sphere, 2 = split planes; see `light_index_build.wgsl`).
    // The culling structure itself is the shared light index (group 3,
    // bindings 8–10); only the pass flags remain here.
    flags: vec4<f32>,
    // x: shadowed fixture count, y: shadow texel size, z: beam gain — the
    // dimmer-to-radiance scale shared with the haze march.
    shadow: vec4<f32>,
};

struct FixtureShadowMatrix {
    view_proj: mat4x4<f32>,
    // xy: shadow projection near/far planes in metres.
    params: vec4<f32>,
};

// Binding 3 held a CSR cluster list before the unified light index
// (bindings 8–10, declared in `light_index.wgsl`) replaced it; the number
// stays reserved so the surviving slots keep their ids. The light SoA below
// is uploaded by the index in its own sorted order — ids from the index walk
// are direct offsets into it.
@group(3) @binding(0) var<storage, read> fixture_cores: array<FixtureLightCore>;
@group(3) @binding(1) var<storage, read> fixture_rests: array<FixtureLightRest>;
@group(3) @binding(4) var<uniform> surface_clusters: SurfaceClusterParams;
@group(3) @binding(5) var<storage, read> fixture_shadow_matrices: array<FixtureShadowMatrix>;
@group(3) @binding(6) var fixture_shadow_map: texture_depth_2d_array;
@group(3) @binding(7) var fixture_depth_sampler: sampler;

@group(3) @binding(12) var fixture_shadow_map_extra: texture_depth_2d_array;
// The far-field fog prefix (`haze_integrate.wgsl`): cumulative camera
// transmittance in `.a` per fog column and quadratic radial slice. Read by
// `surface_transmittance` when `globals.surface_fog.y` selects the grid.
@group(3) @binding(13) var surface_fog_grid: texture_3d<f32>;
// Per-tile surface depth split (`light_index_build.wgsl::surface_fill`):
// the depth between the tile's two surface mask buckets, or a large sentinel.
@group(3) @binding(14) var<storage, read> surface_splits: array<f32>;
