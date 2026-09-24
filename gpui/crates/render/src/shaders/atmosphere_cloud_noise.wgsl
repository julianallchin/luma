// The cloud layer's two tiling 3D noise volumes, baked once per device
// (Schneider, *The Real-time Volumetric Cloudscapes of Horizon Zero Dawn*,
// 2015).
//
// - shape, 128^3: r is Perlin-Worley (fBm Perlin dilated by Worley, the
//   billowy base), gba are Worley fBm at 4, 8 and 16 cells, which erode the
//   base into lobes.
// - detail, 32^3: rgb are Worley fBm at 2, 4 and 8 cells, which erode the
//   edges: wispy at a cloud's base, billowed at its top.
//
// Every octave's period divides the volume, so both tile seamlessly.

@group(0) @binding(0) var output_tex: texture_storage_3d<rgba8unorm, write>;
@group(0) @binding(1) var<uniform> bake: vec4<u32>;

fn hash3u(p: vec3<u32>) -> vec3<f32> {
    var v = p * vec3<u32>(1664525u, 1013904223u, 2654435761u) + vec3<u32>(bake.y);
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3<u32>(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return vec3<f32>(v >> vec3<u32>(8u)) / 16777216.0;
}

fn wrap_cell(c: vec3<i32>, period: i32) -> vec3<u32> {
    return vec3<u32>((c % period + period) % period);
}

/// 1 at a feature point, 0 a cell away: inverted F1 over `period` cells.
fn worley(p: vec3<f32>, period: i32) -> f32 {
    let q = p * f32(period);
    let cell = vec3<i32>(floor(q));
    var nearest = 1e9;
    for (var z = -1; z <= 1; z++) {
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                let c = cell + vec3<i32>(x, y, z);
                let point = vec3<f32>(c) + hash3u(wrap_cell(c, period));
                let d = point - q;
                nearest = min(nearest, dot(d, d));
            }
        }
    }
    return clamp(1.0 - sqrt(nearest), 0.0, 1.0);
}

fn fade(t: vec3<f32>) -> vec3<f32> {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn gradient(c: vec3<i32>, period: i32) -> vec3<f32> {
    return normalize(hash3u(wrap_cell(c, period)) * 2.0 - 1.0 + vec3<f32>(1e-4));
}

/// Tiling gradient noise, roughly -1 to 1.
fn perlin(p: vec3<f32>, period: i32) -> f32 {
    let q = p * f32(period);
    let cell = vec3<i32>(floor(q));
    let f = q - floor(q);
    let u = fade(f);
    var corners: array<f32, 8>;
    for (var i = 0; i < 8; i++) {
        let o = vec3<i32>(i & 1, (i >> 1) & 1, (i >> 2) & 1);
        corners[i] = dot(gradient(cell + o, period), f - vec3<f32>(o));
    }
    let x0 = mix(corners[0], corners[1], u.x);
    let x1 = mix(corners[2], corners[3], u.x);
    let x2 = mix(corners[4], corners[5], u.x);
    let x3 = mix(corners[6], corners[7], u.x);
    return mix(mix(x0, x1, u.y), mix(x2, x3, u.y), u.z);
}

fn worley_fbm(p: vec3<f32>, period: i32) -> f32 {
    return worley(p, period) * 0.625 + worley(p, period * 2) * 0.25 + worley(p, period * 4) * 0.125;
}

fn remap(v: f32, lo: f32, hi: f32, new_lo: f32, new_hi: f32) -> f32 {
    return new_lo + (v - lo) / (hi - lo) * (new_hi - new_lo);
}

@compute @workgroup_size(4, 4, 4)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(output_tex);
    if any(id >= size) {
        return;
    }
    let p = (vec3<f32>(id) + 0.5) / vec3<f32>(size);
    var texel: vec4<f32>;
    if bake.x == 0u {
        var perlin_fbm = 0.0;
        var amplitude = 1.0;
        var weight = 0.0;
        for (var octave = 0; octave < 4; octave++) {
            perlin_fbm += perlin(p, 4 << u32(octave)) * amplitude;
            weight += amplitude;
            amplitude *= 0.5;
        }
        perlin_fbm = clamp(perlin_fbm / weight * 0.9 + 0.5, 0.0, 1.0);
        let cells = worley_fbm(p, 4);
        // Perlin-Worley: the Perlin field, pushed up where the Worley cells
        // are, so its blobs round into billows.
        let perlin_worley = clamp(remap(perlin_fbm, cells - 1.0, 1.0, 0.0, 1.0), 0.0, 1.0);
        texel = vec4<f32>(perlin_worley, cells, worley_fbm(p, 8), worley_fbm(p, 16));
    } else {
        texel = vec4<f32>(worley_fbm(p, 2), worley_fbm(p, 4), worley_fbm(p, 8), 1.0);
    }
    textureStore(output_tex, vec3<i32>(id), texel);
}
