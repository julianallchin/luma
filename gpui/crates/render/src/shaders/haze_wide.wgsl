// Adds the wide beams' reduced-resolution haze into the native haze target.
// The upsample is the composite's depth-aware bilateral one: each of the four
// nearest taps is weighted by its bilinear position and by how close the view
// depth it saw is to this pixel's, so a wide beam does not bleed across a
// silhouette. The wide target's alpha carries that depth (`haze.wgsl`).

struct WideMerge {
    // xy: wide target size, z: bilateral depth sigma in metres.
    params: vec4<f32>,
    // xy: camera near/far planes.
    depth: vec4<f32>,
};

@group(0) @binding(0) var<uniform> merge: WideMerge;
@group(0) @binding(1) var wide_tex: texture_2d<f32>;
@group(0) @binding(2) var wide_sampler: sampler;
@group(0) @binding(3) var depth_tex: texture_depth_2d;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

fn linear_view_depth(raw_depth: f32) -> f32 {
    let near = merge.depth.x;
    let far = merge.depth.y;
    return near * far / max(near + raw_depth * (far - near), 1e-5);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let full_depth = linear_view_depth(textureLoad(depth_tex, vec2<i32>(frag.xy), 0));
    let uv = frag.xy / vec2<f32>(textureDimensions(depth_tex));
    let r = merge.params.xy;
    let lr = uv * r - 0.5;
    let base = floor(lr);
    let f = lr - base;

    // Exact texel centres, so linear filtering returns unblended depths.
    let s00 = textureSampleLevel(wide_tex, wide_sampler, (base + vec2<f32>(0.5, 0.5)) / r, 0.0);
    let s10 = textureSampleLevel(wide_tex, wide_sampler, (base + vec2<f32>(1.5, 0.5)) / r, 0.0);
    let s01 = textureSampleLevel(wide_tex, wide_sampler, (base + vec2<f32>(0.5, 1.5)) / r, 0.0);
    let s11 = textureSampleLevel(wide_tex, wide_sampler, (base + vec2<f32>(1.5, 1.5)) / r, 0.0);

    let k = 1.0 / max(merge.params.z, 1e-5);
    let w00 = (1.0 - f.x) * (1.0 - f.y) * exp(-abs(s00.a - full_depth) * k);
    let w10 = f.x * (1.0 - f.y) * exp(-abs(s10.a - full_depth) * k);
    let w01 = (1.0 - f.x) * f.y * exp(-abs(s01.a - full_depth) * k);
    let w11 = f.x * f.y * exp(-abs(s11.a - full_depth) * k);

    let total = w00 + w10 + w01 + w11;
    var haze: vec3<f32>;
    if total < 1e-4 {
        // Every tap disagrees with this pixel's depth: take the closest one
        // rather than renormalising four wrong-side answers.
        var best = s00;
        var best_err = abs(s00.a - full_depth);
        if abs(s10.a - full_depth) < best_err { best = s10; best_err = abs(s10.a - full_depth); }
        if abs(s01.a - full_depth) < best_err { best = s01; best_err = abs(s01.a - full_depth); }
        if abs(s11.a - full_depth) < best_err { best = s11; }
        haze = best.rgb;
    } else {
        haze = (w00 * s00.rgb + w10 * s10.rgb + w01 * s01.rgb + w11 * s11.rgb) / total;
    }
    // Additive blend; zero alpha leaves the native target's depth unchanged.
    return vec4<f32>(haze, 0.0);
}
