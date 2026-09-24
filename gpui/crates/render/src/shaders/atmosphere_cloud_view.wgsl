// The cloud layer as the camera sees it, traced every frame from the real
// camera through the real layer, so it has parallax and its bases turn in
// perspective as the camera moves.
//
// Cheap the way Unreal's and HDRP's are: the buffer is a fraction of the
// output's resolution, and each frame traces one pixel of every 2x2 block
// (a Bayer order over four frames). The other three reproject the last
// frame's answer through the camera's motion, using the depth each pixel
// recorded. A pixel with no history — the first frame, a still capture, a
// region the camera has just turned toward — is traced at once. Steps are
// jittered per pixel and per frame with blue noise, so the step pattern
// averages out over the frames instead of showing as bands.

struct CloudView {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    // xyz: the camera, km, in the venue frame.
    camera: vec4<f32>,
    // x: frame index, y: 1 to trace every pixel, z: 1 when the history is
    // usable, w: weight of a new sample against its history.
    frame: vec4<f32>,
};

@group(0) @binding(9) var<uniform> view: CloudView;
@group(0) @binding(10) var history_color: texture_2d<f32>;
@group(0) @binding(11) var history_depth: texture_2d<f32>;
@group(0) @binding(12) var out_color: texture_storage_2d<rgba16float, write>;
@group(0) @binding(13) var out_depth: texture_storage_2d<r32float, write>;
@group(0) @binding(14) var blue_noise: texture_2d<f32>;
@group(0) @binding(16) var view_cloud_shadow: texture_2d<f32>;

/// Steps of the march through the air in front of the clouds.
const SHAFT_STEPS: u32 = 24u;

/// The light the air in front of the clouds does not scatter because the
/// cloud layer is over it: what the clear sky table counts and the air
/// under the layer does not have.
///
/// The sky behind the clouds comes from the clear sky table, whose air is
/// lit by the sun all the way and by the whole clear sky. Under the layer
/// it is not: where a cloud's shadow crosses the view ray the sun is
/// missing, and where the gaps let it through it is not (the crepuscular
/// rays); the layer's grey holds back the sky's diffuse light and adds the
/// sun it diffused on the way down. The aerial volume
/// (`atmosphere_aerial.wgsl`) gives the air in front of every surface
/// exactly these terms, so this march takes the same ones off the table,
/// out to the cloud or, past the last cloud, as far as the aerial volume
/// reaches. Stopping short of it left the sky at the horizon brighter than
/// the ground just under it, one row that is all the far ground. The
/// composite takes the result off the sky (`clouds.rgb` may go below zero
/// for it).
fn shaded_air(origin: vec3<f32>, dir: vec3<f32>, distance: f32, jitter: f32) -> vec3<f32> {
    if cfg.shadow.z <= 0.0 && cfg.clouds.y >= 1.0 && cfg.clouds.z <= 0.0 {
        return vec3<f32>(0.0);
    }
    let sun = normalize(cfg.sun.xyz);
    let cos_theta = dot(dir, sun);
    let phase_r = rayleigh_phase(cos_theta);
    let phase_m = mie_phase(cos_theta);
    // No cloud on the ray: the march's cap stands for the open sky, and
    // the air under the layer runs on to where the ray leaves it through
    // its top: some kilometres looking up, and at the horizon, where the
    // earth's curve lifts the ray to the layer, near two hundred. The
    // ground's air (`atmosphere_aerial.wgsl`) runs to a hundred.
    var reach = distance;
    if distance >= CLOUD_MAX_KM {
        let radius = GROUND_RADIUS_KM + max(cloud_altitude(origin), 0.0);
        let top = ray_sphere_distance(radius, dir.z, GROUND_RADIUS_KM + max(cfg.clouds.w, cfg.clouds.x));
        reach = clamp(select(AERIAL_MAX_KM, top, top > 0.0), 1e-3, 400.0);
    }
    var lost = vec3<f32>(0.0);
    var throughput = vec3<f32>(1.0);
    var previous = 0.0;
    // Steps grow away from the camera: shafts are widest near it.
    for (var i = 0u; i < SHAFT_STEPS; i = i + 1u) {
        let f = (f32(i) + jitter) / f32(SHAFT_STEPS);
        let t = reach * f * f;
        let dt = t - previous;
        previous = t;
        let p = origin + dir * t;
        let r = max(length(p + vec3<f32>(0.0, 0.0, GROUND_RADIUS_KM)), GROUND_RADIUS_KM + 0.001);
        let h = r - GROUND_RADIUS_KM;
        let m = medium_at(h);
        let up = normalize(p + vec3<f32>(0.0, 0.0, GROUND_RADIUS_KM));
        let sun_mu = dot(up, sun);
        let lit = select(1.0, 0.0, ray_sphere_distance(r, sun_mu, GROUND_RADIUS_KM) > 0.0);
        let sunlight = transmittance_to_top(transmittance_lut, lut_sampler, r, sun_mu) * lit;
        let ms_uv = vec2<f32>(sun_mu * 0.5 + 0.5, clamp(h / (TOP_RADIUS_KM - GROUND_RADIUS_KM), 0.0, 1.0));
        let psi_ms = textureSampleLevel(multiscatter_lut, lut_sampler, ms_uv, 0.0).rgb;
        let cloud = cloud_shadow_in_air(view_cloud_shadow, lut_sampler, cfg.shadow, cfg.clouds.xw, sun, p * 1000.0);
        let under = 1.0 - smoothstep(cfg.clouds.x, max(cfg.clouds.w, cfg.clouds.x + 1e-3), h);
        let diffuse = (1.0 - cloud) * cfg.clouds.z * max(sun_mu, 0.0) * INV_PI * 0.5;
        // The table's source less the aerial volume's, step for step.
        let missing = (m.rayleigh_scattering * phase_r + m.mie_scattering * phase_m) * sunlight * (1.0 - cloud)
            + (m.rayleigh_scattering + vec3<f32>(m.mie_scattering))
                * (psi_ms * (1.0 - cfg.clouds.y) * under - sunlight * diffuse);
        let step_t = exp(-m.extinction * dt);
        lost += throughput * missing * (vec3<f32>(1.0) - step_t) / max(m.extinction, vec3<f32>(1e-9));
        throughput *= step_t;
    }
    return lost;
}

/// World direction through a buffer uv, from the unjittered camera.
fn view_direction(uv: vec2<f32>) -> vec3<f32> {
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near = view.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far = view.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    return normalize(far.xyz / far.w - near.xyz / near.w);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(out_color);
    if any(id.xy >= size) {
        return;
    }
    let frame = u32(view.frame.x);
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(size);
    let dir = view_direction(uv);

    // Where this pixel's cloud was last frame. A pixel with no cloud keeps
    // its depth at the march's cap, which reprojects as a direction.
    var history = vec4<f32>(0.0);
    var history_ok = view.frame.z > 0.5;
    if history_ok {
        let depth = textureLoad(history_depth, vec2<i32>(id.xy), 0).r;
        let point = view.camera.xyz + dir * max(depth, 0.01);
        // In metres, the space the camera matrices are in.
        let clip = view.prev_view_proj * vec4<f32>(point * 1000.0, 1.0);
        let previous = vec2<f32>(clip.x / clip.w * 0.5 + 0.5, 0.5 - clip.y / clip.w * 0.5);
        history_ok = clip.w > 0.0 && all(previous >= vec2<f32>(0.0)) && all(previous <= vec2<f32>(1.0));
        if history_ok {
            history = textureSampleLevel(history_color, lut_sampler, previous, 0.0);
        }
    }

    let order = array<u32, 4>(0u, 3u, 1u, 2u);
    let slot = (id.x & 1u) + 2u * (id.y & 1u);
    let scheduled = view.frame.y > 0.5 || slot == order[frame % 4u];
    if !scheduled && history_ok {
        textureStore(out_color, vec2<i32>(id.xy), history);
        // The depth moves with the colour it belongs to.
        let depth = textureLoad(history_depth, vec2<i32>(id.xy), 0).r;
        textureStore(out_depth, vec2<i32>(id.xy), vec4<f32>(depth, 0.0, 0.0, 0.0));
        return;
    }

    // Blue noise, tiled over the buffer and stepped along the golden ratio
    // each frame, so every pixel sees a new offset and neighbours differ.
    let noise_size = vec2<u32>(textureDimensions(blue_noise));
    let noise = textureLoad(blue_noise, vec2<i32>(id.xy % noise_size), 0).r;
    let jitter = fract(noise + f32(frame) * 0.618034);
    // A still, or a frame with no history, gets no later frames to average
    // its steps with, so it takes twice as many.
    let steps = u32(layer.march.x) * select(1u, 2u, view.frame.y > 0.5);
    let clouds = cloud_march(view.camera.xyz, dir, jitter, steps);
    var color = clouds.light;
    // Only the sky that shows through the clouds carries the table's air.
    let shaded = shaded_air(view.camera.xyz, dir, clouds.depth, jitter) * color.a;
    color = vec4<f32>(color.rgb - shaded, color.a);
    if history_ok {
        color = mix(history, clouds.light, view.frame.w);
    }
    textureStore(out_color, vec2<i32>(id.xy), color);
    textureStore(out_depth, vec2<i32>(id.xy), vec4<f32>(clouds.depth, 0.0, 0.0, 0.0));
}
