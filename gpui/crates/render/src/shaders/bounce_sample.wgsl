// The scene pass's view of the bounced light (`bounce.rs`): what the lit
// stage sends on to a surface, added to the sky probe's light.
//
// A pixel on a big surface takes it from the low pictures
// (`bounce_low.wgsl`), from the texels round it that saw the same surface.
// One that no texel saw (an edge, a narrow face) looks its own discs up
// (`bounce_lookup.wgsl`). Anything no disc speaks for (a truss, a fixture)
// takes it from the nearest points in the air, each of which holds the
// light landing on six faces.

@group(2) @binding(6) var bounce_matte: texture_2d<f32>;
@group(2) @binding(7) var<uniform> bounce_field: BounceField;
@group(2) @binding(8) var bounce_mirror: texture_2d<f32>;
@group(2) @binding(9) var bounce_air: texture_3d<f32>;
@group(2) @binding(10) var<storage, read> bounce_discs: array<BounceDisc>;
@group(2) @binding(11) var<storage, read> bounce_cells: array<u32>;
@group(2) @binding(12) var<storage, read> bounce_landed: array<vec4<f32>>;

struct BounceLight {
    // Radiance along the reflection, for the split sum.
    specular: vec3<f32>,
    // Mean radiance over the normal's side: E / pi, as the sky probe's
    // irradiance cube stores it.
    diffuse: vec3<f32>,
    // Bounced light the surface mirrors toward the eye, its own highlight
    // already taken.
    mirror: vec3<f32>,
    // Whether there is bounced light here at all, 0 or 1.
    weight: f32,
};

/// The bounced light on a face turned along the unit direction `d`, at the
/// air-grid position `g`, W/m2.
fn bounce_air_light(g: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    let dims = vec3<i32>(bounce_field.air_dims.xyz);
    let c = clamp(g, vec3<f32>(0.0), vec3<f32>(dims - 1));
    let low = min(vec3<i32>(floor(c)), max(dims - 2, vec3<i32>(0)));
    let f = c - vec3<f32>(low);
    let w = d * d;
    let faces = vec3<i32>(select(1, 0, d.x >= 0.0), select(3, 2, d.y >= 0.0), select(5, 4, d.z >= 0.0));
    var out = vec3<f32>(0.0);
    for (var corner = 0; corner < 8; corner++) {
        let o = vec3<i32>(corner & 1, (corner >> 1) & 1, (corner >> 2) & 1);
        let at = min(low + o, dims - 1);
        let share = mix(1.0 - f, f, vec3<f32>(o));
        let weight = share.x * share.y * share.z;
        if weight <= 0.0 { continue; }
        out += weight * (w.x * textureLoad(bounce_air, vec3<i32>(at.xy, at.z + faces.x * dims.z), 0).rgb
            + w.y * textureLoad(bounce_air, vec3<i32>(at.xy, at.z + faces.y * dims.z), 0).rgb
            + w.z * textureLoad(bounce_air, vec3<i32>(at.xy, at.z + faces.z * dims.z), 0).rgb);
    }
    return out;
}

/// One texel of the low pictures, weighed by whether it saw the surface
/// through `world`: rgb the light on the surface, and the weight.
struct BounceTap { matte: vec3<f32>, mirror: vec3<f32>, weight: f32 };

fn bounce_tap(
    texel: vec2<i32>,
    world: vec3<f32>,
    n: vec3<f32>,
    pixel: vec2<f32>,
    dx: vec3<f32>,
    dy: vec3<f32>,
) -> BounceTap {
    var tap: BounceTap;
    tap.weight = 0.0;
    let seen = textureLoad(bounce_matte, texel, 0);
    if seen.a < 0.0 {
        return tap;
    }
    // Where this surface would be at that texel, had it gone on flat: the
    // texel saw the same surface if it is that far away.
    let forward = globals.camera_forward.xyz;
    let away = (vec2<f32>(texel) + 0.5) * bounce_field.air_dims.w - pixel;
    let there = dot(world + dx * away.x + dy * away.y - globals.camera_pos.xyz, forward);
    let depth = dot(world - globals.camera_pos.xyz, forward);
    let shine = textureLoad(bounce_mirror, texel, 0);
    // And if it faces the same way: a case against a wall is as far away
    // as the wall.
    let facing = step(0.8, dot(bounce_unpack_facing(shine.a), n));
    tap.weight = facing * saturate(1.0 - abs(seen.a - there) / (0.005 * depth + 0.03));
    tap.matte = seen.rgb;
    tap.mirror = shine.rgb;
    return tap;
}

/// The bounced light at `world`, which draws at `pixel`. `dx` and `dy` are
/// the point's screen derivatives.
fn bounce_light(
    world: vec3<f32>,
    n: vec3<f32>,
    r: vec3<f32>,
    pixel: vec2<f32>,
    dx: vec3<f32>,
    dy: vec3<f32>,
) -> BounceLight {
    var result: BounceLight;
    result.specular = vec3<f32>(0.0);
    result.diffuse = vec3<f32>(0.0);
    result.mirror = vec3<f32>(0.0);
    result.weight = 0.0;
    if bounce_field.dims.w < 0.5 {
        return result;
    }
    result.weight = 1.0;
    let step = bounce_field.air_dims.w;
    let last = vec2<i32>(ceil(globals.viewport.xy / step)) - 1;
    let at = pixel / step - 0.5;
    let corner = vec2<i32>(floor(at));
    let f = at - floor(at);
    var matte = vec3<f32>(0.0);
    var mirror = vec3<f32>(0.0);
    var total = 0.0;
    for (var k = 0; k < 4; k++) {
        let o = vec2<i32>(k & 1, k >> 1);
        let tap = bounce_tap(clamp(corner + o, vec2<i32>(0), last), world, n, pixel, dx, dy);
        let share = (mix(1.0 - f.x, f.x, f32(o.x)) * mix(1.0 - f.y, f.y, f32(o.y)) + 0.01) * tap.weight;
        matte += share * tap.matte;
        mirror += share * tap.mirror;
        total += share;
    }
    if total <= 0.05 {
        // A thin face: its own texels may be a step further out.
        matte = vec3<f32>(0.0);
        mirror = vec3<f32>(0.0);
        total = 0.0;
        for (var k = 0; k < 16; k++) {
            let o = vec2<i32>(k & 3, k >> 2) - 1;
            if (o.x == 0 || o.x == 1) && (o.y == 0 || o.y == 1) { continue; }
            let tap = bounce_tap(clamp(corner + o, vec2<i32>(0), last), world, n, pixel, dx, dy);
            matte += tap.weight * tap.matte;
            mirror += tap.weight * tap.mirror;
            total += tap.weight;
        }
    }
    if total > 0.05 {
        result.diffuse = matte / total * RECIPROCAL_PI;
        result.mirror = mirror / total;
        return result;
    }
    let found = bounce_at(world, n);
    if found.weight > 0.0 {
        result.diffuse = found.matte * RECIPROCAL_PI;
        result.mirror = found.mirror;
        return result;
    }
    let g = (world - bounce_field.air_origin.xyz) / bounce_field.air_origin.w;
    result.diffuse = bounce_air_light(g, n) * RECIPROCAL_PI;
    result.specular = bounce_air_light(g, r) * RECIPROCAL_PI;
    return result;
}
