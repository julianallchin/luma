// Low quality's beams: no march. Each light's single scattering along a
// pixel's ray through uniform haze, in closed form.
//
// Along the ray the beam's radiance is `I · profile · phase · taper ·
// transmittance / d²`, where `d` is the distance from the cone's virtual
// apex. Substituting the equiangular angle `θ` about the apex,
// `t = δ + h·tan θ`, turns `dt / d²` into `dθ / h` exactly, so the
// inverse-square term — the part that varies fastest, and the reason the
// march needs many samples near the source — integrates to `(θ₁ − θ₀) / h`.
// What is left varies slowly in `θ` across the span `beam_span` clips to the
// cone: the angular profile, the phase, the range taper and the
// transmittance. A fixed three-point Gauss rule in `θ` carries them; there is
// no step loop, no texture read and no shadow walk. The haze is uniform at
// its mean extinction (inside the medium's envelope), and gobos and haze
// shadows are not drawn.

@group(2) @binding(0) var analytic_out: texture_storage_2d<rgba16float, write>;

// A face of cells (`panel.wgsl`) has no closed form: its light along the ray
// is whatever cells the ray passes. The same Gauss rule, a piece per cell
// passed, in the equiangular angle about the face's middle; far from the face
// that is one piece, as for a cone.
fn analytic_panel(li: u32, ray: SceneRay, span: vec2<f32>) -> vec3<f32> {
    let rest = light_rest[li];
    let core = light_core[li];
    let sigma = haze.depth.z;
    let oc = haze.camera_pos.xyz - core.position;
    let delta = -dot(oc, ray.dir);
    let h = sqrt(max(dot(oc, oc) - delta * delta, 1e-4));
    let th0 = atan((span.x - delta) / h);
    let th1 = atan((span.y - delta) / h);
    let pieces = panel_pieces(rest.panel, rest.direction, oc + ray.dir * span.x, oc + ray.dir * span.y);
    let half = 0.5 * (th1 - th0) / f32(pieces);
    let nodes = array<f32, 3>(-0.7745966692, 0.0, 0.7745966692);
    let weights = array<f32, 3>(0.5555555556, 0.8888888889, 0.5555555556);
    var sum = vec3<f32>(0.0);
    for (var piece = 0u; piece < pieces; piece += 1u) {
        let centre = th0 + f32(2u * piece + 1u) * half;
        for (var j = 0u; j < 3u; j += 1u) {
            let tangent = tan(centre + nodes[j] * half);
            let t = delta + h * tangent;
            let q = oc + ray.dir * t;
            let distance = length(q);
            let cells = panel_scatter(rest.panel, rest.direction, rest.cos_beam, rest.cos_field, q, ray.dir);
            let density = medium_envelope(haze.medium, haze.camera_pos.xyz + ray.dir * t);
            let transmittance = exp(-sigma * (t + distance));
            sum += panel_tint(cells) * (weights[j] * beam_range_falloff(distance, core.range)
                * density * transmittance * (1.0 + tangent * tangent));
        }
    }
    return sum * (half * h * rest.intensity * rest.haze_gain * haze.tuning.w * sigma);
}

fn analytic_beam(li: u32, ray: SceneRay) -> vec3<f32> {
    let rest = light_rest[li];
    if rest.haze_gain <= 0.0 { return vec3<f32>(0.0); }
    let span = beam_span(li, ray);
    if span.y <= span.x { return vec3<f32>(0.0); }
    if rest.panel > 0.5 { return analytic_panel(li, ray, span); }
    let core = light_core[li];
    let sigma = haze.depth.z;
    let oc = haze.camera_pos.xyz - core.position;
    // Apex-centred equiangular frame: the falloff's distance is the apex's.
    let oa = oc + rest.direction * rest.lens_distance;
    let delta = -dot(oa, ray.dir);
    let h = sqrt(max(dot(oa, oa) - delta * delta, haze.tuning.z));
    let th0 = atan((span.x - delta) / h);
    let th1 = atan((span.y - delta) / h);
    let half = 0.5 * (th1 - th0);
    let centre = th0 + half;
    let nodes = array<f32, 3>(-0.7745966692, 0.0, 0.7745966692);
    let weights = array<f32, 3>(0.5555555556, 0.8888888889, 0.5555555556);
    var sum = 0.0;
    for (var j = 0u; j < 3u; j += 1u) {
        let t = delta + h * tan(centre + nodes[j] * half);
        let q = oc + ray.dir * t;
        let distance = length(q);
        let angular = angular_profile(lens_cos_angle(q, rest.direction, rest.lens_distance, distance), rest.cos_beam, rest.cos_field);
        let phase = haze_phase(-dot(q, ray.dir) / max(distance, 1e-4));
        let density = medium_envelope(haze.medium, haze.camera_pos.xyz + ray.dir * t);
        // Camera path and light path, both at the mean extinction.
        let transmittance = exp(-sigma * (t + distance));
        sum += weights[j] * angular * phase * beam_range_falloff(distance, core.range) * density * transmittance;
    }
    let tint = beam_tint(li);
    return tint * (sum * half / h * rest.intensity * rest.haze_gain * haze.tuning.w * sigma);
}

// Linear view depth in alpha, as the marched kernels write it, so the
// composite's bilateral upsample reads this target the same way.
@compute @workgroup_size(8, 8)
fn analytic_haze(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(u32(haze.transport.w), u32(haze.transport.z));
    if any(id.xy >= size) { return; }
    let frag = vec2<f32>(id.xy) + 0.5;
    let ray = scene_ray(frag);
    var scattered = vec3<f32>(0.0);
    if haze.params.y >= 0.001 {
        var cursor = lights_along(frag * haze.tiles.xy);
        var li = 0u;
        while light_index_next(&cursor, &li) {
            scattered += analytic_beam(li, ray);
        }
    }
    textureStore(analytic_out, vec2<i32>(id.xy), vec4<f32>(scattered, ray.view_depth));
}
