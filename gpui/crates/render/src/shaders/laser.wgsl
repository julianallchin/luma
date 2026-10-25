// Laser beams and sheets in haze (`laser.rs`).
//
// A beam is a line from the projector: drawn as a strip that faces the
// camera, at least a pixel or so wide so it never flickers to nothing. The
// light a camera ray picks up crossing it is the beam's power times the
// haze's scattering, spread over the strip's width and divided by how
// sideways the ray crosses it.
//
// A sheet is a fan of light from the projector: drawn as its triangle,
// both faces. Its power per radian spreads over a distance `r` from the
// projector as power per metre `density / r`, and a ray crossing it at a
// grazing angle picks up more of it.
//
// Both lose light to the haze on the way out from the projector and on the
// way to the camera. What they scatter is the haze's own density where the
// light is (`medium.wgsl`), so a beam or sheet shows the clouds and wisps
// the fixtures' beams do, moving with the same wind.
//
// A beam widens as it goes, and fades to nothing across its width, so its
// edges blend into the pixels they cross instead of stepping.

struct Uniform {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // x: extinction σ (1/m); y: phase g; z: gain; w: range (m).
    haze: vec4<f32>,
    // Pixels across and down.
    viewport: vec4<f32>,
};

struct BeamInstance {
    origin: vec4<f32>,
    direction: vec4<f32>,
    power: vec4<f32>,
};

struct SheetVertex {
    position: vec4<f32>,
    normal: vec4<f32>,
    origin: vec4<f32>,
    density: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniform;
@group(0) @binding(1) var<storage, read> beams: array<BeamInstance>;
@group(0) @binding(2) var<storage, read> sheets: array<SheetVertex>;
@group(0) @binding(3) var<uniform> medium: ProceduralMedium;
@group(0) @binding(4) var haze_noise_field: texture_3d<f32>;
@group(0) @binding(5) var haze_noise_sampler: sampler;

// Half a beam's thickness at the projector, metres.
const HALF_WIDTH_M: f32 = 0.002;
// Half the angle a beam spreads by, radians: a projector's 1.5 mrad.
const HALF_DIVERGENCE: f32 = 0.00075;
// The strip is at least this many pixels wide: enough for its fade across
// to blend into the pixels rather than alias.
const MIN_PIXELS: f32 = 2.5;
// `PIECES`, the pieces a beam's strip is cut into along its length, comes
// from `laser.rs`.
// The most a grazing ray multiplies the light, so an edge-on sheet or a
// beam seen end-on stays finite.
const GRAZE: f32 = 0.05;

// Henyey-Greenstein, normalised so g = 0 gives 1 (as `beam_transport.wgsl`).
fn phase(cos_t: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = max(1.0 + g2 - 2.0 * g * cos_t, 1e-4);
    return (1.0 - g2) / (denom * sqrt(denom));
}

// The haze's light at `world`, for light travelling along `along` that
// left `origin`, scattered toward the camera.
fn scattered(world: vec3<f32>, origin: vec3<f32>, along: vec3<f32>) -> f32 {
    let sigma = u.haze.x;
    let to_camera = u.camera_pos.xyz - world;
    let toward = normalize(to_camera);
    let travelled = distance(world, origin) + length(to_camera);
    let density = medium_density(medium, world);
    return sigma * density * u.haze.z * phase(dot(along, toward), u.haze.y) * exp(-sigma * travelled);
}

struct BeamOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) instance: u32,
    @location(2) width: f32,
    // −1 to 1 across the strip.
    @location(3) across: f32,
};

@vertex
fn vs_beam(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> BeamOut {
    // Two triangles a piece: (along, side) at each corner.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vertex % 6u];
    let along = (f32(vertex / 6u) + corner.x) / f32(PIECES);
    let beam = beams[instance];
    let direction = beam.direction.xyz;
    let r = along * u.haze.w;
    let world = beam.origin.xyz + direction * r;
    let half_width = HALF_WIDTH_M + r * HALF_DIVERGENCE;
    let across = cross(direction, u.camera_pos.xyz - world);
    let reach = length(across);
    let side = select(vec3<f32>(0.0, 0.0, 1.0), across / max(reach, 1e-6), reach > 1e-5);
    let centre = u.view_proj * vec4<f32>(world, 1.0);
    let offset = (u.view_proj * vec4<f32>(side * half_width, 0.0)).xy;
    let pixels = length(offset * u.viewport.xy * 0.5) / max(centre.w, 1e-4);
    let widen = max(1.0, MIN_PIXELS * 0.5 / max(pixels, 1e-8));
    var out: BeamOut;
    out.clip = vec4<f32>(centre.xy + offset * corner.y * widen, centre.z, centre.w);
    out.world = world;
    out.instance = instance;
    out.width = 2.0 * half_width * widen;
    out.across = corner.y;
    return out;
}

@fragment
fn fs_beam(in: BeamOut) -> @location(0) vec4<f32> {
    let beam = beams[in.instance];
    let direction = beam.direction.xyz;
    let view = normalize(in.world - u.camera_pos.xyz);
    let sideways = max(length(cross(direction, view)), GRAZE);
    // A smooth bump across the strip whose mean is 1, so the beam's light
    // is the same however wide it is drawn.
    let edge = 1.0 - in.across * in.across;
    let profile = 1.875 * edge * edge;
    let light = beam.power.xyz * profile * scattered(in.world, beam.origin.xyz, direction) / (sideways * in.width);
    return vec4<f32>(light, 0.0);
}

struct SheetOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) origin: vec3<f32>,
    @location(3) density: vec3<f32>,
};

@vertex
fn vs_sheet(@builtin(vertex_index) vertex: u32) -> SheetOut {
    let corner = sheets[vertex];
    var out: SheetOut;
    out.clip = u.view_proj * vec4<f32>(corner.position.xyz, 1.0);
    out.world = corner.position.xyz;
    out.normal = corner.normal.xyz;
    out.origin = corner.origin.xyz;
    out.density = corner.density.xyz;
    return out;
}

@fragment
fn fs_sheet(in: SheetOut) -> @location(0) vec4<f32> {
    let out_from = in.world - in.origin;
    let r = max(length(out_from), 0.05);
    let view = normalize(in.world - u.camera_pos.xyz);
    let crossing = max(abs(dot(in.normal, view)), GRAZE);
    let light = in.density * scattered(in.world, in.origin, out_from / r) / (r * crossing);
    return vec4<f32>(light, 0.0);
}
