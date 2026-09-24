// The post chain's last pass (`post.rs`): exposure, tone curve, then glare
// added on top of the tone-mapped picture, so a full-white core stays white
// and its halo spreads over what is around it.

struct Tonemap {
    // x: tone curve (`ToneCurve::shader_code`), y: glare gain (0 for none),
    // z: unused, w: display headroom (1 in SDR).
    params: vec4<f32>,
    // xy: the frame's extent in the glare texture's uv (the glare grid
    // rounds the frame up to whole texels).
    glare: vec4<f32>,
    // xyz: toward the sun in camera space (x right, y up, z forward), w: how
    // much of its veil to draw here (0 while the frame holds the sun).
    sun: vec4<f32>,
    // rgb: the sun's light times the veil's scale, w: focal length, pixels.
    sun_veil: vec4<f32>,
};

// Set for a compositor that presents HDR: half-float linear light where 1.0
// is SDR white and highlights may go up to the headroom.
override HDR_OUTPUT: bool = false;

@group(0) @binding(0) var<uniform> cfg: Tonemap;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
// The convolved glare (`post_glare.wgsl`), at the glare grid's resolution.
@group(0) @binding(2) var glare_tex: texture_2d<f32>;
@group(0) @binding(3) var linear_clamp: sampler;
@group(0) @binding(4) var<storage, read> exposure: vec4<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

/// The glare texture read through a cubic B-spline, in four bilinear taps
/// (Sigg and Hadwiger, GPU Gems 2 ch. 20). Bilinear alone would show the
/// glare grid's texels as diamonds around a small source; the B-spline is
/// smooth to its second derivative, so it adds no edge for the eye to
/// sharpen into a band.
fn bspline(uv: vec2<f32>) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(glare_tex));
    let p = uv * size - 0.5;
    let i = floor(p);
    let f = p - i;
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = (1.0 - 3.0 * f + 3.0 * f2 - f3) / 6.0;
    let w1 = (4.0 - 6.0 * f2 + 3.0 * f3) / 6.0;
    let w2 = (1.0 + 3.0 * f + 3.0 * f2 - 3.0 * f3) / 6.0;
    let w3 = f3 / 6.0;
    let g0 = w0 + w1;
    let g1 = w2 + w3;
    let h0 = (i - 0.5 + w1 / g0) / size;
    let h1 = (i + 1.5 + w3 / g1) / size;
    let a = textureSampleLevel(glare_tex, linear_clamp, vec2<f32>(h0.x, h0.y), 0.0).rgb;
    let b = textureSampleLevel(glare_tex, linear_clamp, vec2<f32>(h1.x, h0.y), 0.0).rgb;
    let c = textureSampleLevel(glare_tex, linear_clamp, vec2<f32>(h0.x, h1.y), 0.0).rgb;
    let d = textureSampleLevel(glare_tex, linear_clamp, vec2<f32>(h1.x, h1.y), 0.0).rgb;
    return g0.y * (g0.x * a + g1.x * b) + g1.y * (g0.x * c + g1.x * d);
}

/// Vos's glare spread function per square degree, unnormalised (`psf.rs`).
fn vos(theta: f32) -> f32 {
    let t = theta + 0.02;
    return 0.384 * 2.61e6 * exp(-(theta / 0.02) * (theta / 0.02))
        + 0.478 * 20.91 / (t * t * t)
        + 0.138 * 72.37 / (t * t);
}

/// The veil of a sun off the frame (`post.rs`, `off_frame_sun`): the
/// convolution sees only light inside the frame, and a lens is lit by the
/// sun past its edge too.
fn sun_veil(frag: vec2<f32>, size: vec2<f32>) -> vec3<f32> {
    if cfg.sun.w <= 0.0 {
        return vec3<f32>(0.0);
    }
    let ray = normalize(vec3<f32>(
        (frag.x - 0.5 * size.x) / cfg.sun_veil.w,
        (0.5 * size.y - frag.y) / cfg.sun_veil.w,
        1.0,
    ));
    let theta = degrees(acos(clamp(dot(ray, cfg.sun.xyz), -1.0, 1.0)));
    return cfg.sun.w * vos(theta) * cfg.sun_veil.rgb * exposure.w;
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(scene_tex));
    let uv = frag.xy / size;
    let scene = textureLoad(scene_tex, vec2<i32>(frag.xy), 0).rgb * exposure.w;
    let headroom = select(1.0, max(cfg.params.w, 1.0), HDR_OUTPUT);
    var display = hdr_expand(tone_curve(scene, u32(cfg.params.x + 0.5)), headroom);
    if cfg.params.y > 0.0 {
        let glare = cfg.params.y * (bspline(uv * cfg.glare.xy) + sun_veil(frag.xy, size));
        // Light added over the picture, saturating at the display's white:
        // faint glare keeps its colour, a strong one burns to white.
        display += (vec3<f32>(headroom) - display) * (vec3<f32>(1.0) - exp(-glare));
    }
    if HDR_OUTPUT {
        return vec4<f32>(display, 1.0);
    }
    return vec4<f32>(display + display_dither(display, frag.xy), 1.0);
}
