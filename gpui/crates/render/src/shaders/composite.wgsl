// Composite + display transform, one pass (spec §2.5). Bilateral-upsample the
// haze, add, tonemap, write. Splitting these would be two full-screen passes
// doing one pass's work — `postprocessing` merged them into one `EffectPass`
// anyway.

struct Composite {
    inv_view_proj: mat4x4<f32>,
    // xy: haze buffer size, z: bilateral depth sigma, w: debug-view code.
    params: vec4<f32>,
    // xy: camera near/far planes, z: mean extinction sigma in 1/metres.
    depth: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cfg: Composite;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
@group(0) @binding(2) var haze_tex: texture_2d<f32>;
@group(0) @binding(3) var haze_sampler: sampler;
@group(0) @binding(4) var depth_tex: texture_depth_2d;

struct EnvironmentParams {
    intensity: f32,
    rotation: f32,
    enabled: f32,
    visible: f32,
};
@group(1) @binding(0) var environment_irradiance: texture_cube<f32>;
@group(1) @binding(1) var environment_specular: texture_cube<f32>;
@group(1) @binding(2) var environment_brdf: texture_2d<f32>;
@group(1) @binding(3) var environment_sampler: sampler;
@group(1) @binding(4) var<uniform> environment_params: EnvironmentParams;

fn linear_view_depth(raw_depth: f32) -> f32 {
    let near = cfg.depth.x;
    let far = cfg.depth.y;
    return near * far / max(near + raw_depth * (far - near), 1e-5);
}

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

    let total = w00 + w10 + w01 + w11;
    if total < 1e-4 {
        // Every tap disagrees with this pixel's depth. Renormalising four
        // wrong-side answers paints the far side's haze onto a near surface;
        // the least-wrong single tap is the one whose depth is closest.
        var best = s00;
        var best_err = abs(s00.a - full_depth);
        if abs(s10.a - full_depth) < best_err { best = s10; best_err = abs(s10.a - full_depth); }
        if abs(s01.a - full_depth) < best_err { best = s01; best_err = abs(s01.a - full_depth); }
        if abs(s11.a - full_depth) < best_err { best = s11; }
        return best.rgb;
    }
    let haze = w00 * s00.rgb + w10 * s10.rgb + w01 * s01.rgb + w11 * s11.rgb;
    return haze / total;
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
    var scene = textureLoad(scene_tex, coord, 0).rgb;
    let size = vec2<f32>(textureDimensions(scene_tex));
    let uv = frag.xy / size;
    // Anchored on *this* pixel's own depth, not on the depth the nearest haze
    // texel happened to record. The two agree exactly at 1:1, so this is free
    // there; below it the haze texel's depth is one sample standing in for a
    // whole quad, and weighting the taps against it makes the bilateral answer
    // per-quad instead of per-pixel — which is the one thing it exists not to
    // do. The tap depths in `haze_tex.a` are still what it compares against.
    // Subframe weights sum to 1, so the accumulated target is already a mean —
    // nothing here rescales it.
    let raw_depth = textureLoad(depth_tex, coord, 0);
    if raw_depth <= 0.0 && environment_params.visible > 0.5 {
        let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let near_h = cfg.inv_view_proj * vec4<f32>(ndc_xy, 1.0, 1.0);
        let far_h = cfg.inv_view_proj * vec4<f32>(ndc_xy, 0.0, 1.0);
        let near_world = near_h.xyz / near_h.w;
        let far_world = far_h.xyz / far_h.w;
        var direction = normalize(far_world - near_world);
        let c = cos(environment_params.rotation);
        let s = sin(environment_params.rotation);
        direction = vec3<f32>(
            c * direction.x - s * direction.y,
            s * direction.x + c * direction.y,
            direction.z,
        );
        direction = vec3<f32>(direction.x, direction.z, -direction.y);
        scene = textureSampleLevel(environment_specular, environment_sampler, direction, 0.0).rgb
            * environment_params.intensity;
    }
    let depth = linear_view_depth(raw_depth);
    let debug = u32(cfg.params.w + 0.5);
    if debug >= 1u && debug <= 5u {
        // Material probes are already display-range linear values. Keeping the
        // display transform out makes channel inspection exact.
        return vec4<f32>(scene, 1.0);
    }
    if debug == 6u {
        // Perspective depth is intentionally raw: this is the attachment the
        // haze bilateral pass consumes, not a camera-specific beauty view.
        return vec4<f32>(vec3<f32>(1.0 - raw_depth), 1.0);
    }
    let haze = upsample_haze(uv, depth);
    if debug == 7u {
        return vec4<f32>(agx(haze), 1.0);
    }
    // Beer-Lambert over the camera path, with the same sigma the haze pass
    // integrates in-scatter against. Surface radiance decays exactly as the
    // medium's own glow builds, so everything converges to one fog colour with
    // distance — geometry never silhouettes against the sky through the haze.
    return vec4<f32>(agx(scene * exp(-cfg.depth.z * depth) + haze), 1.0);
}
