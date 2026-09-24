// Glare (`post.rs`): hot pixels of the exposed frame, spread by a dual-filter
// pyramid (Bjørge, "Bandwidth-efficient rendering", SIGGRAPH 2015) and by
// Kawase's directional streaks. The tonemap pass adds the result after the
// tone curve.

struct Glare {
    // xy: one texel of the texture being read, in uv. z: threshold, in
    // exposed scene light. w: soft knee width.
    texel: vec4<f32>,
    // Streaks only. xy: direction in source texels, z: tap spacing in
    // texels, w: per-texel attenuation.
    streak: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cfg: Glare;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var linear_clamp: sampler;
// The exposure state `post_exposure.wgsl` writes; w is the multiplier.
@group(0) @binding(3) var<storage, read> exposure: vec4<f32>;
// Upsample only: the pyramid level this one is added to.
@group(0) @binding(4) var detail: texture_2d<f32>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    var out: VsOut;
    out.position = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return out;
}

fn tap(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(source, linear_clamp, uv, 0.0).rgb;
}

/// The part of `c` above the threshold, with a quadratic knee so the glare
/// fades in rather than switching on.
fn hot(c: vec3<f32>) -> vec3<f32> {
    let peak = max(max(c.r, c.g), c.b);
    let threshold = cfg.texel.z;
    let knee = cfg.texel.w;
    var soft = clamp(peak - threshold + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 1e-5);
    return c * (max(soft, peak - threshold) / max(peak, 1e-5));
}

/// Karis's weight, loosened: a stray firefly cannot dominate its
/// neighbourhood, but a lens a hundred times white still glares like one.
/// Karis's own 1 / (1 + L) would flatten every small hot source to about
/// one, and small hot sources are what this glare is for.
fn karis(c: vec3<f32>) -> f32 {
    return 1.0 / (1.0 + max(max(c.r, c.g), c.b) / 256.0);
}

/// Full-resolution frame to the first level: exposure, threshold, and the
/// dual filter's five taps, each weighted by Karis.
@fragment
fn fs_prefilter(in: VsOut) -> @location(0) vec4<f32> {
    let h = cfg.texel.xy;
    let e = exposure.w;
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    let offsets = array<vec2<f32>, 5>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    for (var i = 0; i < 5; i++) {
        let c = hot(tap(in.uv + offsets[i] * h) * e);
        let w = karis(c) * select(1.0, 4.0, i == 0);
        sum += c * w;
        weight += w;
    }
    return vec4<f32>(sum / max(weight, 1e-6), 1.0);
}

/// Dual-filter downsample: the centre at four times the weight of the four
/// diagonal half-texel taps.
@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    let h = cfg.texel.xy;
    var sum = tap(in.uv) * 4.0;
    sum += tap(in.uv + vec2<f32>(-h.x, -h.y));
    sum += tap(in.uv + vec2<f32>(h.x, -h.y));
    sum += tap(in.uv + vec2<f32>(-h.x, h.y));
    sum += tap(in.uv + vec2<f32>(h.x, h.y));
    return vec4<f32>(sum / 8.0, 1.0);
}

/// Dual-filter upsample of the coarser level, added to this level's own
/// downsample. The pyramid's top therefore holds every level's weighted sum.
@fragment
fn fs_up(in: VsOut) -> @location(0) vec4<f32> {
    let h = cfg.texel.xy;
    var sum = tap(in.uv + vec2<f32>(-2.0 * h.x, 0.0));
    sum += tap(in.uv + vec2<f32>(2.0 * h.x, 0.0));
    sum += tap(in.uv + vec2<f32>(0.0, -2.0 * h.y));
    sum += tap(in.uv + vec2<f32>(0.0, 2.0 * h.y));
    sum += tap(in.uv + vec2<f32>(-h.x, -h.y)) * 2.0;
    sum += tap(in.uv + vec2<f32>(h.x, -h.y)) * 2.0;
    sum += tap(in.uv + vec2<f32>(-h.x, h.y)) * 2.0;
    sum += tap(in.uv + vec2<f32>(h.x, h.y)) * 2.0;
    let own = textureSampleLevel(detail, linear_clamp, in.uv, 0.0).rgb;
    // `streak.x` weighs the coarser levels against this one: below one,
    // each level out adds less, so the glow is concentrated near the source
    // with a long faint tail rather than an even haze.
    return vec4<f32>(sum / 12.0 * cfg.streak.x + own, 1.0);
}

/// One Kawase streak pass: seven taps along a line, symmetric, each
/// attenuated by its distance. Repeated with spacing 1, 3, 9 it reaches 39
/// texels each way.
@fragment
fn fs_streak(in: VsOut) -> @location(0) vec4<f32> {
    let step = cfg.streak.xy * cfg.streak.z * cfg.texel.xy;
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    for (var k = -3; k <= 3; k++) {
        let w = pow(cfg.streak.w, abs(f32(k)) * cfg.streak.z);
        sum += tap(in.uv + step * f32(k)) * w;
        weight += w;
    }
    return vec4<f32>(sum / weight, 1.0);
}
