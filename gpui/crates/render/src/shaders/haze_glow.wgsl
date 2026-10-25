// The light the haze scatters, gathered in a coarse world grid
// (`haze_glow.rs`). `inject` writes each cell's scattered power per cubic
// metre from every fixture cone and laser; `downsample` averages a level
// into the next, so `haze_glow_sample.wgsl` can spread it over the room.
//
// A cell is far wider than a narrow beam, so a cell centre would see a
// beam that passed between centres as nothing. Every source is spread over
// at least a cell's width instead, keeping its power: a narrow cone is
// widened about its axis and dimmed by the same share, a laser beam is a
// Gaussian a cell wide, and a laser sheet is a fan blurred a cell thick.

struct Params {
    // xyz: the grid's low corner; w: mean extinction σ (1/m).
    min: vec4<f32>,
    // xyz: the grid's high corner; w: the beams' gain.
    max: vec4<f32>,
    // x: fixture cones, y: laser beams, z: laser sheets; w unused.
    counts: vec4<u32>,
    // x: the lasers' gain; yzw unused.
    laser: vec4<f32>,
};

struct LightCore {
    position: vec3<f32>,
    range: f32,
};

struct LightRest {
    direction: vec3<f32>,
    cos_beam: f32,
    color: vec3<f32>,
    intensity: f32,
    cos_field: f32,
    wash: f32,
    gobo: f32,
    gobo_rotation: f32,
    shadow_slot: f32,
    haze_gain: f32,
    inverse_right_length: f32,
    field_tangent: f32,
    lens_distance: f32,
    lens_reserved0: f32,
    lens_reserved1: f32,
    lens_reserved2: f32,
};

struct ShadowMatrix {
    view_proj: mat4x4<f32>,
    // xy: near and far planes, metres.
    params: vec4<f32>,
};

struct Beam {
    origin: vec4<f32>,
    // xyz: unit direction; w: reach, metres.
    direction: vec4<f32>,
    power: vec4<f32>,
};

struct Sheet {
    origin: vec4<f32>,
    // xyz: the fan's middle direction; w: half its angle, radians.
    middle: vec4<f32>,
    // xyz: the fan's unit normal; w: reach, metres.
    normal: vec4<f32>,
    // Power per radian.
    density: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<uniform> medium: ProceduralMedium;
@group(0) @binding(2) var haze_noise_field: texture_3d<f32>;
@group(0) @binding(3) var haze_noise_sampler: sampler;
@group(0) @binding(4) var<storage, read> cores: array<LightCore>;
@group(0) @binding(5) var<storage, read> rests: array<LightRest>;
@group(0) @binding(6) var<storage, read> shadow_matrices: array<ShadowMatrix>;
@group(0) @binding(7) var shadow_map: texture_depth_2d_array;
@group(0) @binding(8) var shadow_map_extra: texture_depth_2d_array;
@group(0) @binding(9) var<storage, read> beams: array<Beam>;
@group(0) @binding(10) var<storage, read> sheets: array<Sheet>;
@group(0) @binding(11) var cells: texture_storage_3d<rgba16float, write>;
@group(0) @binding(12) var finer: texture_3d<f32>;

const TAU: f32 = 6.2831853;

// Whether `world` sees the cone's light past its shadow map, by one texel:
// a cell is far coarser than the map, and the light it carries is spread
// over the room after.
fn cone_visible(li: u32, world: vec3<f32>, slack: f32) -> f32 {
    let slot = rests[li].shadow_slot;
    if slot < 0.0 {
        return 1.0;
    }
    let layer = i32(slot);
    let matrix = shadow_matrices[layer];
    let clip = matrix.view_proj * vec4<f32>(world, 1.0);
    let ndc = clip.xyz / clip.w;
    if clip.w <= 0.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 1.0;
    }
    var stored = 0.0;
    if layer < 256 {
        let size = vec2<f32>(textureDimensions(shadow_map));
        let texel = vec2<i32>(min(uv * size, size - 1.0));
        stored = textureLoad(shadow_map, texel, layer, 0);
    } else {
        let size = vec2<f32>(textureDimensions(shadow_map_extra));
        let texel = vec2<i32>(min(uv * size, size - 1.0));
        stored = textureLoad(shadow_map_extra, texel, layer - 256, 0);
    }
    let reference = shadow_compare_reference(ndc.z, matrix.params.x, matrix.params.y, slack);
    return select(0.0, 1.0, reference >= stored);
}

// The irradiance a cone puts on a cell `width` across at `world`, its light
// spread over at least that width.
fn cone_irradiance(li: u32, world: vec3<f32>, width: f32) -> vec3<f32> {
    let core = cores[li];
    let rest = rests[li];
    if rest.intensity <= 0.0 || rest.haze_gain <= 0.0 {
        return vec3<f32>(0.0);
    }
    let q = world - core.position;
    let distance = length(q);
    if distance >= core.range || distance <= 1e-4 {
        return vec3<f32>(0.0);
    }
    let axial = dot(q, rest.direction);
    if axial <= 0.0 {
        return vec3<f32>(0.0);
    }
    // The cone's radius here, widened to the cell's, and the cell's point
    // pulled toward the axis by the same share: what the narrow cone lights
    // the cell with is its own light spread over the wider disc.
    let across = q - rest.direction * axial;
    let radius = (axial + rest.lens_distance) * rest.field_tangent;
    let half_width = 0.5 * width;
    let widened = sqrt(radius * radius + half_width * half_width);
    let share = radius / max(widened, 1e-6);
    let pulled = rest.direction * axial + across * share;
    let pulled_distance = max(length(pulled), 1e-4);
    let cos_angle = lens_cos_angle(pulled, rest.direction, rest.lens_distance, pulled_distance);
    let angular = angular_profile(cos_angle, rest.cos_beam, rest.cos_field);
    if angular <= 0.0 {
        return vec3<f32>(0.0);
    }
    let falloff = beam_range_falloff(distance, core.range)
        / max(lens_apex_distance2(pulled, rest.direction, rest.lens_distance), 0.01);
    let visible = cone_visible(li, core.position + pulled, half_width);
    return rest.color * rest.intensity * rest.haze_gain * params.max.w
        * angular * falloff * share * share * visible;
}

// A laser beam's irradiance on the cell at `world`: its power as a Gaussian
// `spread` metres wide about its line, out to its reach.
fn beam_irradiance(beam: Beam, world: vec3<f32>, spread: f32) -> vec3<f32> {
    let q = world - beam.origin.xyz;
    let along = dot(q, beam.direction.xyz);
    if along <= 0.0 || along >= beam.direction.w {
        return vec3<f32>(0.0);
    }
    let off = q - beam.direction.xyz * along;
    let d2 = dot(off, off);
    let s2 = spread * spread;
    return beam.power.rgb * exp(-0.5 * d2 / s2) / (TAU * s2);
}

fn erf_approx(x: f32) -> f32 {
    // Bowling et al. 2009's logistic fit, within 1e-4.
    return 2.0 / (1.0 + exp(-1.5976 * x - 0.07056 * x * x * x)) - 1.0;
}

// A laser sheet's irradiance on the cell at `world`: its power per radian
// over its fan, blurred `spread` metres across the fan and at its edges.
fn sheet_irradiance(sheet: Sheet, world: vec3<f32>, spread: f32) -> vec3<f32> {
    let q = world - sheet.origin.xyz;
    let normal = sheet.normal.xyz;
    let off = dot(q, normal);
    let in_plane = q - normal * off;
    let r = length(in_plane);
    if r <= 1e-3 || r >= sheet.normal.w {
        return vec3<f32>(0.0);
    }
    let middle = sheet.middle.xyz;
    if dot(in_plane, middle) <= 0.0 {
        return vec3<f32>(0.0);
    }
    // Arc length from the fan's middle, and the fan's half width, at r.
    let side = cross(normal, middle);
    let x = r * atan2(dot(in_plane, side), dot(in_plane, middle));
    let half_arc = r * sheet.middle.w;
    let k = 1.0 / (sqrt(2.0) * spread);
    // The share of a box `2·half_arc` wide that a Gaussian at x sees, per
    // metre of arc: the fan's light per metre, blurred at its edges.
    let across_fan = 0.5 * (erf_approx((half_arc - x) * k) + erf_approx((half_arc + x) * k));
    let power = sheet.density.rgb * 2.0 * sheet.middle.w;
    let per_metre = power * across_fan / max(2.0 * half_arc, 1e-4);
    return per_metre * exp(-0.5 * off * off / (spread * spread)) / (sqrt(TAU) * spread);
}

@compute @workgroup_size(4, 4, 4)
fn inject(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(cells);
    if any(id >= size) {
        return;
    }
    let extent = params.max.xyz - params.min.xyz;
    let cell = extent / vec3<f32>(size);
    let world = params.min.xyz + (vec3<f32>(id) + 0.5) * cell;
    let width = max(cell.x, max(cell.y, cell.z));
    let sigma = params.min.w;
    let density = medium_density(medium, world);
    if sigma <= 0.0 || density <= 0.0 {
        textureStore(cells, vec3<i32>(id), vec4<f32>(0.0));
        return;
    }
    var irradiance = vec3<f32>(0.0);
    for (var li = 0u; li < params.counts.x; li += 1u) {
        irradiance += cone_irradiance(li, world, width);
    }
    let spread = 0.5 * width;
    for (var i = 0u; i < params.counts.y; i += 1u) {
        irradiance += params.laser.x * beam_irradiance(beams[i], world, spread);
    }
    for (var i = 0u; i < params.counts.z; i += 1u) {
        irradiance += params.laser.x * sheet_irradiance(sheets[i], world, spread);
    }
    // What the haze here takes out of that light and sends every way.
    textureStore(cells, vec3<i32>(id), vec4<f32>(sigma * density * irradiance, 0.0));
}

@compute @workgroup_size(4, 4, 4)
fn downsample(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(cells);
    if any(id >= size) {
        return;
    }
    let source = vec3<i32>(textureDimensions(finer)) - 1;
    var sum = vec4<f32>(0.0);
    for (var i = 0; i < 8; i += 1) {
        let child = min(vec3<i32>(id) * 2 + vec3<i32>(i & 1, (i >> 1) & 1, (i >> 2) & 1), source);
        sum += textureLoad(finer, child, 0);
    }
    textureStore(cells, vec3<i32>(id), sum * 0.125);
}
