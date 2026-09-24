// Composite + display transform, one pass (spec §2.5). Bilateral-upsample the
// haze, add, tonemap, write. Splitting these would be two full-screen passes
// doing one pass's work — `postprocessing` merged them into one `EffectPass`
// anyway.

struct Composite {
    inv_view_proj: mat4x4<f32>,
    // xy: haze buffer size, z: bilateral depth sigma, w: debug-view code.
    params: vec4<f32>,
    // xy: camera near/far planes, z: mean extinction sigma in 1/metres, w: haze phase g.
    depth: vec4<f32>,
    // rgb: the frame's clear colour, i.e. what a pixel with no geometry and no
    // visible environment shows.
    background: vec4<f32>,
    medium: ProceduralMedium,
    camera_pos: vec4<f32>,
    outdoor_sun: vec4<f32>,
    // x: display headroom, the brightest output as a multiple of SDR white.
    // Read only when HDR_OUTPUT is set.
    display: vec4<f32>,
};

// Set for a compositor that presents HDR: the target is half-float linear
// light where 1.0 is SDR white, and highlights may go up to the headroom.
override HDR_OUTPUT: bool = false;

// Set when the post chain (`post.rs`) follows: the target is half-float
// scene-linear light, before exposure and before any display transform.
override LINEAR_OUTPUT: bool = false;

@group(0) @binding(0) var<uniform> cfg: Composite;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
@group(0) @binding(2) var haze_tex: texture_2d<f32>;
@group(0) @binding(3) var haze_sampler: sampler;
@group(0) @binding(4) var depth_tex: texture_depth_2d;
@group(0) @binding(5) var haze_noise_field: texture_3d<f32>;
@group(0) @binding(6) var haze_noise_sampler: sampler;
// r: the fraction of the haze's sunlight the sun reaches past the stage and
// the clouds, g: the linear view depth it stands for (`sun_shafts.wgsl`).
@group(0) @binding(7) var shaft_tex: texture_2d<f32>;

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
// The probe's frame-constant mean lobe radiance (`environment_ambient.wgsl`).
@group(1) @binding(5) var<uniform> outdoor_ambient_mean: vec4<f32>;

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

/// The sun-shaft fraction at this pixel: a tent over the four by four
/// nearest texels, each also weighted by how near its depth is to this
/// pixel's, so a shaft does not bleed across a silhouette. The tent is wider
/// than a bilinear tap because each texel's few samples are noisy; shafts
/// are soft anyway.
fn upsample_shafts(uv: vec2<f32>, full_depth: f32) -> f32 {
    let r = cfg.display.zw;
    let lr = uv * r - 0.5;
    let base = floor(lr);
    let f = lr - base;
    let tolerance = 0.25 + 0.05 * full_depth;
    let range = ground_depth_range(uv, full_depth);
    var sum = 0.0;
    var total = 0.0;
    var nearest = 1.0;
    var nearest_err = 1e9;
    for (var j = -1; j < 3; j++) {
        for (var i = -1; i < 3; i++) {
            let texel = clamp(vec2<i32>(base) + vec2<i32>(i, j), vec2<i32>(0), vec2<i32>(r) - 1);
            let s = textureLoad(shaft_tex, texel, 0);
            // Outside the span by the depth tolerance; inside it, by how far
            // in octaves the texel stands from the pixel's own centre.
            let err = max(max(range.x - s.g, s.g - range.y), 0.0) / tolerance
                + 0.25 * abs(log2(max(s.g, 1e-3) / max(full_depth, 1e-3)));
            let d = abs(vec2<f32>(f32(i), f32(j)) - f);
            let tent = max(2.0 - d.x, 0.0) * max(2.0 - d.y, 0.0);
            let w = tent * exp(-err);
            sum += w * s.r;
            total += w;
            if err < nearest_err {
                nearest = s.r;
                nearest_err = err;
            }
        }
    }
    return select(nearest, sum / total, total > 1e-4);
}

/// The view depths a pixel of open ground covers, from its lower edge to its
/// upper, or `depth` twice for anything else.
///
/// Toward the horizon a pixel of ground spans hundreds of metres, and the
/// last row from a kilometre or so to the sky. Matched on its centre's depth
/// alone, that row found no shaft texel like it — the ground below is
/// nearer, the sky above is the sky — and took the near ground's: a line
/// along the horizon. A texel anywhere in the pixel's span matches it.
fn ground_depth_range(uv: vec2<f32>, depth: f32) -> vec2<f32> {
    let camera = cfg.camera_pos.xyz;
    let centre = cfg.inv_view_proj * vec4<f32>(0.0, 0.0, 0.5, 1.0);
    let forward = normalize(centre.xyz / centre.w - camera);
    let half = 0.5 / f32(textureDimensions(scene_tex).y);
    let mid = ground_view_depth(uv, camera, forward);
    if camera.z <= 0.0 || abs(mid - depth) > 0.05 * depth {
        return vec2<f32>(depth);
    }
    let lower = ground_view_depth(uv + vec2<f32>(0.0, half), camera, forward);
    let upper = ground_view_depth(uv - vec2<f32>(0.0, half), camera, forward);
    return vec2<f32>(min(lower, upper), max(lower, upper));
}

/// View depth at which the ray through `uv` meets the ground plane, or the
/// sky's when it does not.
fn ground_view_depth(uv: vec2<f32>, camera: vec3<f32>, forward: vec3<f32>) -> f32 {
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.5, 1.0);
    let far = cfg.inv_view_proj * clip;
    let dir = normalize(far.xyz / far.w - camera);
    if dir.z >= -1e-6 {
        return 6e4;
    }
    return min(-camera.z / dir.z * dot(dir, forward), 6e4);
}

/// The display transform for this pipeline's target.
fn display_transform(color: vec3<f32>) -> vec3<f32> {
    if HDR_OUTPUT {
        return agx_hdr(color, cfg.display.x);
    }
    return agx(color);
}

/// What stands behind the geometry along this pixel's ray: the environment
/// probe when one is meant to be seen, otherwise the frame's clear colour.
/// Also fills the coverage relinquished by distant surfaces at the horizon.
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
        return sky_radiance(direction, uv);
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
    var background = background_radiance(uv);
    // Scene radiance is premultiplied by coverage, including each object's
    // distance fade and the MSAA resolve. Fill only the uncovered fraction:
    // the floor's horizon fade has already happened underneath the cables.
    scene = scene + (1.0 - texel.a) * background;
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
    var haze = upsample_haze(uv, depth);
    var medium = 1.0;
    let outdoor = sky.sun.w > 0.5;
    // Outdoor surfaces already include their own fog. Only uncovered sky
    // needs background transport here; indoor compositing keeps its path.
    let needs_medium = select(any(scene != vec3<f32>(0.0)), texel.a < 1.0 || debug == 7u, outdoor);
    if needs_medium && cfg.medium.min.w > 0.0 {
        let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.5, 1.0);
        let far = cfg.inv_view_proj * clip;
        let ray_dir = normalize(far.xyz / far.w - cfg.camera_pos.xyz);
        // Uncovered samples see the sky, even on an opaque MSAA silhouette.
        // The diagnostic instead asks for scattering up to the opaque depth.
        let end_depth = select(raw_depth, 0.0, outdoor && debug != 7u);
        let hit_clip = cfg.inv_view_proj * vec4<f32>(clip.xy, end_depth, 1.0);
        let distance = length(hit_clip.xyz / hit_clip.w - cfg.camera_pos.xyz);
        let transmission = exp(-medium_optical_depth(cfg.medium, cfg.camera_pos.xyz, ray_dir, distance));
        if outdoor {
            let scattering = outdoor_haze_light(ray_dir, sky.sun.xyz, cfg.outdoor_sun) * (1.0 - transmission);
            if debug == 7u { haze += scattering; }
            background = background * transmission + scattering;
        } else {
            medium = transmission;
        }
    }
    if debug == 7u { return vec4<f32>(display_transform(haze), 1.0); }
    if outdoor {
        var surface = texel.rgb;
        if cfg.display.y > 0.5 && cfg.medium.min.w > 0.0 && debug == 0u {
            // The scene pass lit its haze with the whole sun; take away the
            // part the stage and the clouds shade. Sky pixels take theirs
            // from the fraction too.
            let shaft_depth = select(6e4, depth, raw_depth > 0.0);
            let lit = upsample_shafts(uv, shaft_depth);
            let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.5, 1.0);
            let far = cfg.inv_view_proj * clip;
            let ray_dir = normalize(far.xyz / far.w - cfg.camera_pos.xyz);
            let sun = outdoor_haze_sun(ray_dir, sky.sun.xyz, cfg.outdoor_sun) * (1.0 - lit);
            if texel.a > 0.0 {
                let hit_clip = cfg.inv_view_proj * vec4<f32>(clip.xy, raw_depth, 1.0);
                let distance = length(hit_clip.xyz / hit_clip.w - cfg.camera_pos.xyz);
                let transmission = exp(-medium_optical_depth(cfg.medium, cfg.camera_pos.xyz, ray_dir, distance));
                surface = max(surface - texel.a * sun * (1.0 - transmission), vec3<f32>(0.0));
            }
            if texel.a < 1.0 {
                let far_clip = cfg.inv_view_proj * vec4<f32>(clip.xy, 0.0, 1.0);
                let distance = length(far_clip.xyz / far_clip.w - cfg.camera_pos.xyz);
                let transmission = exp(-medium_optical_depth(cfg.medium, cfg.camera_pos.xyz, ray_dir, distance));
                background = max(background - sun * (1.0 - transmission), vec3<f32>(0.0));
            }
        }
        scene = surface + (1.0 - texel.a) * background;
    }
    if LINEAR_OUTPUT {
        return vec4<f32>(scene * medium + haze, 1.0);
    }
    let display = display_transform(scene * medium + haze);
    // HDR dithers too. Its half-float target has no steps of its own, but
    // the compositor's 10-bit PQ swapchain and an 8-bit capture of the frame
    // do: without noise, a dim beam edge over the sky crosses them as a
    // staircase of flat runs. Below the knee the HDR frame is the SDR frame,
    // noise included. An 8-bit target clamps at zero anyway.
    return vec4<f32>(max(display + sky_dither(display, frag.xy), vec3<f32>(0.0)), 1.0);
}

/// One least-significant bit of triangular noise, under a sky only.
///
/// An atmosphere is a smooth gradient across hundreds of rows, and eight bits
/// quantise it into visible steps — the one artefact that gives a physically
/// integrated sky away as a shader. Every other frame this pass draws is
/// high-contrast stage light where a step has nowhere to show, and the tracked
/// contract images have to stay byte-exact, so the noise is spent exactly where
/// it buys something. (The post chain dithers every frame: its glare halos
/// are smooth gradients too.)
fn sky_dither(display: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    if sky.sun.w < 0.5 {
        return vec3<f32>(0.0);
    }
    return display_dither(display, frag);
}
