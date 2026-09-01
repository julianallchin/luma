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
    // rgb: the frame's clear colour, i.e. what a pixel with no geometry and no
    // visible environment shows.
    background: vec4<f32>,
};

// The horizon dissolve. Surfaces wash into the background across this band of
// view depth, so the ground quad has no rim to see: nothing in a venue stands
// this far out, and the quad's own extent (`FLOOR_EXTENT_M`, frame.rs) is
// past the far end.
const HORIZON_NEAR_M: f32 = 100.0;
const HORIZON_FAR_M: f32 = 700.0;
// Weight toward the far end, so the band opens gently and a far truss is
// still a truss where the ground under it has begun to go.
const HORIZON_BIAS: f32 = 1.5;

/// How much of the background has taken over at this depth.
///
/// The ramp is written in *inverse* depth. A ground plane's height above the
/// horizon line falls as 1/depth, so a band in 1/depth is a band of fixed
/// height in pixels — the same ramp seen from a standing eye and from a camera
/// thirty metres up. Written in metres it is not: perspective packs the whole
/// of it into the last few rows of a standing view, and the horizon comes back
/// as the hard line this exists to remove.
fn horizon_dissolve(depth: f32) -> f32 {
    let span = 1.0 - HORIZON_NEAR_M / HORIZON_FAR_M;
    let t = saturate((1.0 - HORIZON_NEAR_M / max(depth, 1e-3)) / span);
    return pow(t, HORIZON_BIAS);
}

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

/// What stands behind the geometry along this pixel's ray: the environment
/// probe when one is meant to be seen, otherwise the frame's clear colour.
/// The horizon dissolve and the empty-pixel sky are the same question asked
/// twice, so they read it from here rather than each resolving it.
fn background_radiance(uv: vec2<f32>) -> vec3<f32> {
    if environment_params.visible < 0.5 && sky.sun.w < 0.5 {
        return cfg.background.rgb;
    }
    let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near_h = cfg.inv_view_proj * vec4<f32>(ndc_xy, 1.0, 1.0);
    let far_h = cfg.inv_view_proj * vec4<f32>(ndc_xy, 0.0, 1.0);
    let near_world = near_h.xyz / near_h.w;
    let far_world = far_h.xyz / far_h.w;
    var direction = normalize(far_world - near_world);
    // An atmosphere is the background, and it outranks a probe: the sky's own
    // table is what the sun disc, the horizon and the frame's key light are all
    // read from, so a probe painted over it would be a second sky.
    if sky.sun.w > 0.5 {
        return sky_radiance(direction);
    }
    let c = cos(environment_params.rotation);
    let s = sin(environment_params.rotation);
    direction = vec3<f32>(
        c * direction.x - s * direction.y,
        s * direction.x + c * direction.y,
        direction.z,
    );
    direction = vec3<f32>(direction.x, direction.z, -direction.y);
    return textureSampleLevel(environment_specular, environment_sampler, direction, 0.0).rgb
        * environment_params.intensity;
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(frag.xy);
    let texel = textureLoad(scene_tex, coord, 0);
    var scene = texel.rgb;
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
    let background = background_radiance(uv);
    // Swap the clear colour for the real background, by exactly the fraction
    // of this pixel the scene did not cover.
    //
    // The scene target was cleared to the clear colour with alpha zero, and
    // every draw blended over it, so it holds `a·src + (1−a)·clear` with the
    // accumulated coverage in `a`. Adding `(1−a)·(background − clear)` turns
    // that into `a·src + (1−a)·background` — the same pixel composited over
    // the background instead of over the clear colour, with no second guess at
    // what `a` was.
    //
    // Written as a correction rather than a replacement for two reasons. It
    // reaches the *partly* covered pixels — an MSAA silhouette edge, and a
    // cable or a grid line over open sky, which the transparent tail draws
    // without ever touching the opaque depth buffer this used to interrogate:
    // asking "is the depth empty here" painted the sky straight over the
    // rigging, so a flown truss outdoors hung on nothing. And where there is
    // no background to swap in, `background` *is* `clear`, the correction is
    // an exact zero, and every pixel is the byte it was before.
    scene = scene + (1.0 - texel.a) * (background - cfg.background.rgb);
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
    // Applied before the medium, so a dissolving surface and the background it
    // dissolves into are attenuated by the same fog. Surfaces only: an empty
    // pixel is already the background, and mixing it again would discard the
    // partial coverage a silhouette resolved into it.
    if raw_depth > 0.0 {
        scene = mix(scene, background, horizon_dissolve(depth));
    }
    // Beer-Lambert over the camera path, with the same sigma the haze pass
    // integrates in-scatter against. Surface radiance decays exactly as the
    // medium's own glow builds, so everything converges to one fog colour with
    // distance — geometry never silhouettes against the clear colour through
    // the haze.
    //
    // One, under a sky. The medium is a hazer's: it fills a room, not a world,
    // and `depth` at an empty pixel is the far plane — two kilometres of haze
    // nobody put there, at a mean free path of seventy metres. That takes an
    // atmosphere to black, which is how an open-air venue came back with no
    // sky in it at all. What the air does over a real kilometre is already in
    // the sky's own transmittance tables, and the horizon dissolve above still
    // carries the ground into it.
    let medium = select(exp(-cfg.depth.z * depth), 1.0, sky.sun.w > 0.5);
    let display = agx(scene * medium + haze);
    return vec4<f32>(display + sky_dither(display, frag.xy), 1.0);
}

/// One least-significant bit of triangular noise, under a sky only.
///
/// An atmosphere is a smooth gradient across hundreds of rows, and eight bits
/// quantise it into visible steps — the one artefact that gives a physically
/// integrated sky away as a shader. Every other frame this renderer draws is
/// high-contrast stage light where a step has nowhere to show, and the tracked
/// contract images have to stay byte-exact, so the noise is spent exactly where
/// it buys something.
///
/// The amplitude is in *display* code values, not in the linear ones this
/// shader returns. The target is sRGB-encoded, so a fixed linear step is a
/// dozen code values in the shadows and a third of one in the highlights —
/// grain at one end and banding still at the other. Dividing by the encoder's
/// slope makes it one code value everywhere.
fn sky_dither(display: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    if sky.sun.w < 0.5 {
        return vec3<f32>(0.0);
    }
    // Two decorrelated hashes make a triangular distribution, which has no DC
    // term: a uniform one would lift the whole frame by half a bit.
    let a = fract(sin(dot(frag, vec2<f32>(12.9898, 78.233))) * 43758.5453);
    let b = fract(sin(dot(frag, vec2<f32>(63.7264, 10.873))) * 32361.4771);
    // Inverse slope of the sRGB transfer curve, 2.4 / 1.055 * L^(1 - 1/2.4).
    let step = 2.2749 * pow(max(display, vec3<f32>(1e-4)), vec3<f32>(0.58333)) / 255.0;
    return (a - b) * step;
}
