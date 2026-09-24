// The post chain's last pass (`post.rs`): exposure, tone curve, then glare
// added on top of the tone-mapped picture, so a full-white core stays white
// and its halo spreads over what is around it.

struct Tonemap {
    // x: tone curve (`ToneCurve::shader_code`), y: glare strength,
    // z: star amount, w: display headroom (1 in SDR).
    params: vec4<f32>,
    // x: bloom normalisation (1 / pyramid levels), y: streak normalisation
    // (1 / streak lines).
    glare: vec4<f32>,
};

// Set for a compositor that presents HDR: half-float linear light where 1.0
// is SDR white and highlights may go up to the headroom.
override HDR_OUTPUT: bool = false;

@group(0) @binding(0) var<uniform> cfg: Tonemap;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
@group(0) @binding(2) var bloom_tex: texture_2d<f32>;
@group(0) @binding(3) var streak_tex: texture_2d<f32>;
@group(0) @binding(4) var linear_clamp: sampler;
@group(0) @binding(5) var<storage, read> exposure: vec4<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(scene_tex));
    let uv = frag.xy / size;
    let scene = textureLoad(scene_tex, vec2<i32>(frag.xy), 0).rgb * exposure.w;
    let headroom = select(1.0, max(cfg.params.w, 1.0), HDR_OUTPUT);
    var display = hdr_expand(tone_curve(scene, u32(cfg.params.x + 0.5)), headroom);
    if cfg.params.y > 0.0 {
        let bloom = textureSampleLevel(bloom_tex, linear_clamp, uv, 0.0).rgb * cfg.glare.x;
        let streak = textureSampleLevel(streak_tex, linear_clamp, uv, 0.0).rgb * cfg.glare.y;
        let glare = cfg.params.y * (bloom + cfg.params.z * streak);
        // Light added over the picture, saturating at the display's white:
        // faint glare keeps its colour, a strong one burns to white.
        display += (vec3<f32>(headroom) - display) * (vec3<f32>(1.0) - exp(-glare));
    }
    if HDR_OUTPUT {
        return vec4<f32>(display, 1.0);
    }
    return vec4<f32>(display + display_dither(display, frag.xy), 1.0);
}
