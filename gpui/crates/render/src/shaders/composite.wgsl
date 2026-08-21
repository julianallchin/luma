// Composite + display transform, one pass (spec §2.5). Bilateral-upsample the
// haze, add, tonemap, write. Splitting these would be two full-screen passes
// doing one pass's work — `postprocessing` merged them into one `EffectPass`
// anyway.

struct Composite {
    // xy: haze buffer size in px, z: bilateral depth sigma, w: unused.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cfg: Composite;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
@group(0) @binding(2) var haze_tex: texture_2d<f32>;
@group(0) @binding(3) var haze_sampler: sampler;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

/// Depth-aware upsample of the (possibly low-res) haze. Each of the four
/// nearest taps is weighted by its bilinear position and by how close the
/// depth it saw is to this pixel's — that keeps haze from bleeding across
/// silhouettes the way plain bilinear would.
fn upsample_haze(uv: vec2<f32>, full_depth: f32) -> vec3<f32> {
    let r = cfg.params.xy;
    let lr = uv * r - 0.5;
    let base = floor(lr);
    let f = lr - base;

    // Sample at exact texel centres so linear filtering returns un-blended depths.
    let s00 = textureSampleLevel(haze_tex, haze_sampler, (base + vec2<f32>(0.5, 0.5)) / r, 0.0);
    let s10 = textureSampleLevel(haze_tex, haze_sampler, (base + vec2<f32>(1.5, 0.5)) / r, 0.0);
    let s01 = textureSampleLevel(haze_tex, haze_sampler, (base + vec2<f32>(0.5, 1.5)) / r, 0.0);
    let s11 = textureSampleLevel(haze_tex, haze_sampler, (base + vec2<f32>(1.5, 1.5)) / r, 0.0);

    let k = 1.0 / max(cfg.params.z, 1e-5);
    let w00 = (1.0 - f.x) * (1.0 - f.y) * exp(-abs(s00.a - full_depth) * k);
    let w10 = f.x * (1.0 - f.y) * exp(-abs(s10.a - full_depth) * k);
    let w01 = (1.0 - f.x) * f.y * exp(-abs(s01.a - full_depth) * k);
    let w11 = f.x * f.y * exp(-abs(s11.a - full_depth) * k);

    let haze = w00 * s00.rgb + w10 * s10.rgb + w01 * s01.rgb + w11 * s11.rgb;
    return haze / max(w00 + w10 + w01 + w11, 1e-4);
}

const LINEAR_SRGB_TO_LINEAR_REC2020 = mat3x3<f32>(
    vec3<f32>(0.6274, 0.0691, 0.0164),
    vec3<f32>(0.3293, 0.9195, 0.0880),
    vec3<f32>(0.0433, 0.0113, 0.8956),
);
const LINEAR_REC2020_TO_LINEAR_SRGB = mat3x3<f32>(
    vec3<f32>(1.6605, -0.1246, -0.0182),
    vec3<f32>(-0.5876, 1.1329, -0.1006),
    vec3<f32>(-0.0728, -0.0083, 1.1187),
);
const AGX_INSET = mat3x3<f32>(
    vec3<f32>(0.856627153315983, 0.137318972929847, 0.11189821299995),
    vec3<f32>(0.0951212405381588, 0.761241990602591, 0.0767994186031903),
    vec3<f32>(0.0482516061458583, 0.101439036467562, 0.811302368396859),
);
const AGX_OUTSET = mat3x3<f32>(
    vec3<f32>(1.1271005818144368, -0.1413297634984383, -0.14132976349843826),
    vec3<f32>(-0.11060664309660323, 1.157823702216272, -0.11060664309660294),
    vec3<f32>(-0.016493938717834573, -0.016493938717834257, 1.2519364065950405),
);

fn agx_contrast(x: vec3<f32>) -> vec3<f32> {
    let x2 = x * x;
    let x4 = x2 * x2;
    return 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232;
}

/// three's `AgXToneMapping` at exposure 1, term for term. This is the pairing
/// the haze's HDR core was designed against: white-hot is the display
/// transform's answer, not a shader gate.
fn agx(color_in: vec3<f32>) -> vec3<f32> {
    let min_ev = -12.47393;
    let max_ev = 4.026069;
    var color = LINEAR_SRGB_TO_LINEAR_REC2020 * color_in;
    color = AGX_INSET * color;
    color = log2(max(color, vec3<f32>(1e-10)));
    color = clamp((color - min_ev) / (max_ev - min_ev), vec3<f32>(0.0), vec3<f32>(1.0));
    color = agx_contrast(color);
    color = AGX_OUTSET * color;
    color = pow(max(color, vec3<f32>(0.0)), vec3<f32>(2.2));
    color = LINEAR_REC2020_TO_LINEAR_SRGB * color;
    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(frag.xy);
    let scene = textureLoad(scene_tex, coord, 0).rgb;
    let size = vec2<f32>(textureDimensions(scene_tex));
    let uv = frag.xy / size;
    // The composite reads the same depth the haze texel recorded, so the
    // bilateral weight is exact at 1:1 and degrades gracefully below it.
    // Subframe weights sum to 1, so the accumulated target is already a mean —
    // nothing here rescales it.
    let depth = textureLoad(haze_tex, vec2<i32>(uv * cfg.params.xy), 0).a;
    return vec4<f32>(agx(scene + upsample_haze(uv, depth)), 1.0);
}
