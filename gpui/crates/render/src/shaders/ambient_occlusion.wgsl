// Per-pixel ambient visibility, read by the scene pass as one texel.
//
//   r: GTAO (Jimenez et al. 2016, "Practical Realtime Strategies for
//      Accurate Indirect Occlusion") on the camera depth prepass: small-scale
//      occlusion, contact and corners, within `AO_RADIUS` metres.
//   g: sky visibility from the stage height field (`sky_height.wgsl`):
//      large-scale occlusion of the upper hemisphere, e.g. a deck over a leg.
//   b: sun visibility of the ground below this point, averaged over the part
//      of the ground its lower hemisphere sees.
//   a: sky visibility of that same ground.
//
// b and a scale the ground bounce, which the sky probe otherwise assumes is
// lit, open ground everywhere. Outdoors only; indoors g, b and a are one.
//
// Prefixed by `sky_height.wgsl`.

struct AoParams {
    // xyz: camera position. w: pixels per unit of slope at view distance one,
    // i.e. half the viewport height over tan(fov / 2).
    camera: vec4<f32>,
    // xyz: camera right. w: tan of the half field of view, horizontally.
    right: vec4<f32>,
    // xyz: camera up. w: tan of the half field of view, vertically.
    up: vec4<f32>,
    // xyz: camera forward. w: 1 when the height field applies (outdoors).
    forward: vec4<f32>,
    // xy: output size in pixels, zw: camera near and far planes.
    viewport: vec4<f32>,
};

@group(0) @binding(2) var<uniform> ao: AoParams;
@group(0) @binding(3) var depth_tex: texture_depth_2d;
@group(0) @binding(4) var ground_map: texture_2d<f32>;
@group(0) @binding(5) var ground_sampler: sampler;
@group(0) @binding(6) var raw_out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(7) var raw_in: texture_2d<f32>;
@group(0) @binding(8) var final_out: texture_storage_2d<rgba8unorm, write>;

const AO_RADIUS: f32 = 0.75;
// XeGTAO's thin-occluder compensation: depth separation counts this much
// extra toward the falloff, so a thin leg in front of a ceiling does not read
// as a wall reaching back behind it.
const AO_THIN: f32 = 1.0;
const AO_SLICES: u32 = 3u;
const AO_STEPS: u32 = 4u;
const PI_AO: f32 = 3.14159265;

/// Distance along the camera's forward axis for a reverse-Z depth sample.
fn view_distance(raw: f32) -> f32 {
    let near = ao.viewport.z;
    let far = ao.viewport.w;
    return near * far / max(near + raw * (far - near), 1e-7);
}

fn world_at(pixel: vec2<f32>, distance: f32) -> vec3<f32> {
    let ndc = vec2<f32>(pixel.x * 2.0 / ao.viewport.x - 1.0, 1.0 - pixel.y * 2.0 / ao.viewport.y);
    let ray = ao.forward.xyz + ao.right.xyz * (ndc.x * ao.right.w) + ao.up.xyz * (ndc.y * ao.up.w);
    return ao.camera.xyz + ray * distance;
}

fn load_world(texel: vec2<i32>) -> vec3<f32> {
    let size = vec2<i32>(ao.viewport.xy) - 1;
    let clamped = clamp(texel, vec2<i32>(0), size);
    let raw = textureLoad(depth_tex, clamped, 0);
    return world_at(vec2<f32>(clamped) + 0.5, view_distance(max(raw, 1e-7)));
}

/// Interleaved gradient noise (Jimenez 2014): decorrelates neighbouring
/// pixels so a small spatial filter averages them.
fn gradient_noise(pixel: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(pixel, vec2<f32>(0.06711056, 0.00583715))));
}

fn fast_acos(x: f32) -> f32 {
    let c = clamp(x, -1.0, 1.0);
    let r = (-0.156583 * abs(c) + HALF_PI) * sqrt(1.0 - abs(c));
    return select(PI_AO - r, r, c >= 0.0);
}

fn gtao(pixel: vec2<i32>, p: vec3<f32>, n: vec3<f32>, distance: f32, noise: vec2<f32>) -> f32 {
    let v = normalize(ao.camera.xyz - p);
    let radius_px = AO_RADIUS * ao.camera.w / distance;
    if radius_px < 1.5 {
        return 1.0;
    }
    let radius = min(radius_px, 0.25 * ao.viewport.y);
    let falloff_range = 0.6 * AO_RADIUS;
    let falloff_mul = -1.0 / falloff_range;
    let falloff_add = (AO_RADIUS - falloff_range) / falloff_range + 1.0;
    let centre = vec2<f32>(pixel) + 0.5;
    // Visible and unoccluded cosine-weighted arcs, summed over slices. Their
    // ratio removes the few-slice quadrature error that would otherwise dim
    // an open plane by a percent or two.
    var visibility = 0.0;
    var open = 0.0;
    for (var slice = 0u; slice < AO_SLICES; slice = slice + 1u) {
        let phi = (f32(slice) + noise.x) * PI_AO / f32(AO_SLICES);
        let omega = vec2<f32>(cos(phi), sin(phi));
        // Screen y runs down; world `up` runs up the screen.
        let direction = ao.right.xyz * omega.x - ao.up.xyz * omega.y;
        let ortho = direction - dot(direction, v) * v;
        let axis = normalize(cross(ortho, v));
        let projected = n - axis * dot(n, axis);
        let projected_length = length(projected);
        if projected_length < 1e-4 {
            continue;
        }
        let sign_n = select(-1.0, 1.0, dot(ortho, projected) >= 0.0);
        let cos_n = saturate(dot(projected, v) / projected_length);
        let angle_n = sign_n * fast_acos(cos_n);
        // The tangent plane is the lowest either horizon can be.
        let low_plus = cos(angle_n + HALF_PI);
        let low_minus = cos(angle_n - HALF_PI);
        var horizon_plus = low_plus;
        var horizon_minus = low_minus;
        for (var step = 0u; step < AO_STEPS; step = step + 1u) {
            var s = (f32(step) + noise.y) / f32(AO_STEPS);
            s = s * s + 1.0 / radius;
            let offset = omega * (s * radius);
            let plus = load_world(vec2<i32>(floor(centre + offset))) - p;
            let minus = load_world(vec2<i32>(floor(centre - offset))) - p;
            let plus_length = length(plus);
            let minus_length = length(minus);
            let plus_depth = dot(plus, ao.forward.xyz) * AO_THIN;
            let minus_depth = dot(minus, ao.forward.xyz) * AO_THIN;
            let plus_falloff = sqrt(plus_length * plus_length + plus_depth * plus_depth);
            let minus_falloff = sqrt(minus_length * minus_length + minus_depth * minus_depth);
            let plus_cos = mix(
                low_plus,
                dot(plus, v) / max(plus_length, 1e-5),
                saturate(plus_falloff * falloff_mul + falloff_add),
            );
            let minus_cos = mix(
                low_minus,
                dot(minus, v) / max(minus_length, 1e-5),
                saturate(minus_falloff * falloff_mul + falloff_add),
            );
            horizon_plus = max(horizon_plus, plus_cos);
            horizon_minus = max(horizon_minus, minus_cos);
        }
        var h_plus = fast_acos(horizon_plus);
        var h_minus = -fast_acos(horizon_minus);
        h_plus = angle_n + min(h_plus - angle_n, HALF_PI);
        h_minus = angle_n + max(h_minus - angle_n, -HALF_PI);
        let arc_plus = cos_n + 2.0 * h_plus * sin(angle_n) - cos(2.0 * h_plus - angle_n);
        let arc_minus = cos_n + 2.0 * h_minus * sin(angle_n) - cos(2.0 * h_minus - angle_n);
        visibility += projected_length * 0.25 * (arc_plus + arc_minus);
        open += projected_length * (cos_n + angle_n * sin(angle_n));
    }
    return select(1.0, saturate(visibility / open), open > 1e-4);
}

/// World normal from the depth prepass, taking the flatter neighbour on each
/// axis so a silhouette does not bend it.
fn depth_normal(pixel: vec2<i32>, p: vec3<f32>) -> vec3<f32> {
    let left = load_world(pixel - vec2<i32>(1, 0));
    let right = load_world(pixel + vec2<i32>(1, 0));
    let up = load_world(pixel - vec2<i32>(0, 1));
    let down = load_world(pixel + vec2<i32>(0, 1));
    let dx = select(p - left, right - p, length(right - p) < length(p - left));
    let dy = select(p - up, down - p, length(down - p) < length(p - up));
    var n = normalize(cross(dy, dx));
    if dot(n, ao.camera.xyz - p) < 0.0 {
        n = -n;
    }
    return n;
}

fn ground_tap(centre: vec2<f32>, offset: vec2<f32>, lod: f32) -> vec2<f32> {
    let extent = height.size.xy * height.origin.z;
    let uv = (centre + offset - height.origin.xy) / extent;
    return textureSampleLevel(ground_map, ground_sampler, uv, clamp(lod, 0.0, height.size.w - 1.0)).xy;
}

/// What the ground under `p` receives, over the patch of ground its lower
/// hemisphere sees. For a surface facing down from height `h`, the cosine
/// lobe's quartiles land within `0.58 h`, `1.73 h` and beyond of the point
/// straight below: one tap for the inner quarter, four at `h` for the middle
/// half, four at `2.5 h` for the outer quarter, each reading the mip whose
/// texel is about half its spacing. A tilted surface shifts the pattern the
/// way it faces. `rotation` (0..1) turns it per pixel for the denoiser.
fn ground_light(p: vec3<f32>, n: vec3<f32>, rotation: f32) -> vec2<f32> {
    let h = max(p.z, 0.0);
    let centre = p.xy + n.xy * h;
    let angle = rotation * HALF_PI;
    let a = vec2<f32>(cos(angle), sin(angle)) * h;
    let b = vec2<f32>(-a.y, a.x);
    let texel = height.size.z;
    let inner = log2(max(0.5 * h, 1e-3) / texel);
    let outer = log2(max(1.25 * h, 1e-3) / texel);
    var sum = 0.25 * ground_tap(centre, vec2<f32>(0.0), inner);
    sum += 0.125 * (ground_tap(centre, a, inner) + ground_tap(centre, -a, inner)
        + ground_tap(centre, b, inner) + ground_tap(centre, -b, inner));
    let c = (a + b) * 1.77;
    let d = (b - a) * 1.77;
    sum += 0.0625 * (ground_tap(centre, c, outer) + ground_tap(centre, -c, outer)
        + ground_tap(centre, d, outer) + ground_tap(centre, -d, outer));
    return sum;
}

@compute @workgroup_size(8, 8, 1)
fn visibility(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(vec2<f32>(id.xy) >= ao.viewport.xy) {
        return;
    }
    let pixel = vec2<i32>(id.xy);
    let raw = textureLoad(depth_tex, pixel, 0);
    if raw <= 0.0 {
        textureStore(raw_out, pixel, vec4<f32>(1.0));
        return;
    }
    let distance = view_distance(raw);
    let p = world_at(vec2<f32>(pixel) + 0.5, distance);
    let n = depth_normal(pixel, p);
    let noise = vec2<f32>(
        gradient_noise(vec2<f32>(pixel)),
        gradient_noise(vec2<f32>(pixel) + vec2<f32>(5.588238, 3.0)),
    );
    let occlusion = gtao(pixel, p, n, distance, noise);
    var outdoor = vec3<f32>(1.0);
    if ao.forward.w > 0.5 {
        let lifted = p + n * (height.march.z + height.origin.z);
        outdoor = vec3<f32>(sky_visibility(lifted, n, noise.x), ground_light(p, n, noise.y));
    }
    textureStore(raw_out, pixel, vec4<f32>(occlusion, outdoor));
}

/// Depth-aware 5x5 box: averages away the per-pixel noise of both estimates
/// without bleeding across silhouettes.
@compute @workgroup_size(8, 8, 1)
fn denoise(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(vec2<f32>(id.xy) >= ao.viewport.xy) {
        return;
    }
    let pixel = vec2<i32>(id.xy);
    let raw = textureLoad(depth_tex, pixel, 0);
    if raw <= 0.0 {
        textureStore(final_out, pixel, vec4<f32>(1.0));
        return;
    }
    let size = vec2<i32>(ao.viewport.xy) - 1;
    let distance = view_distance(raw);
    let tolerance = 0.03 * distance + 0.02;
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    for (var j = -2; j <= 2; j = j + 1) {
        for (var i = -2; i <= 2; i = i + 1) {
            let texel = clamp(pixel + vec2<i32>(i, j), vec2<i32>(0), size);
            let other = textureLoad(depth_tex, texel, 0);
            let w = saturate(1.0 - abs(view_distance(max(other, 1e-7)) - distance) / tolerance);
            sum += textureLoad(raw_in, texel, 0) * w;
            weight += w;
        }
    }
    textureStore(final_out, pixel, sum / max(weight, 1e-4));
}
