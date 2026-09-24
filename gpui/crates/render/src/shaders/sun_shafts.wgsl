// Sun shafts in outdoor stage haze (`sun_shafts.rs`). The haze's sunlight is
// a closed form everywhere else (`haze_daylight.wgsl`): the air is lit as if
// nothing stood between it and the sun. This pass marches each view ray at
// low resolution and asks, where the ray's haze scatters, how much of it the
// sun reaches: past the truss and roof through the stage's shadow cascades,
// past the clouds through the cloud shadow map. The composite scales the
// closed form's sun term by the answer, so a roof casts a dark shaft into
// the haze and a gap in the clouds a bright one.
//
// Samples are placed where the scattered light comes from, not evenly in
// distance: each takes an equal share of `1 - T`, the fraction of the view
// ray's light that is scattered haze. Near haze in a dense field and haze a
// kilometre out on a thin one then get the samples they deserve.

struct Shafts {
    inv_view_proj: mat4x4<f32>,
    // x: samples per ray, y: offset of the jitter pattern, zw unused.
    params: vec4<f32>,
};

@group(1) @binding(0) var shaft_depth: texture_depth_2d;
@group(1) @binding(1) var shaft_out: texture_storage_2d<rgba16float, write>;
@group(1) @binding(2) var<uniform> shafts: Shafts;

/// Whether the sun reaches `p` past the stage: one tap of the cascade that
/// covers it. The haze between here and the eye blurs the edge anyway.
fn shaft_stage_sun(p: vec3<f32>) -> f32 {
    if globals.params.z < 0.5 || globals.dir_to_light.w < 0.5 {
        return 1.0;
    }
    let view_depth = dot(p - globals.camera_pos.xyz, globals.camera_forward.xyz);
    if view_depth < 0.0 || view_depth > globals.cascade_splits.z {
        return 1.0;
    }
    var cascade = 0u;
    if view_depth > globals.cascade_splits.x {
        cascade = 1u;
    }
    if view_depth > globals.cascade_splits.y {
        cascade = 2u;
    }
    let matrix = globals.light_view_proj[cascade];
    let clip = matrix * vec4<f32>(p, 1.0);
    let ndc = clip.xyz / clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if ndc.z > 1.0 || ndc.z < 0.0 || any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 1.0;
    }
    // Five centimetres along the light: the air has no surface to acne.
    let depth_per_metre = length(vec3<f32>(matrix[0].z, matrix[1].z, matrix[2].z));
    return textureSampleCompareLevel(shadow_map, shadow_sampler, uv, i32(cascade), ndc.z + 0.05 * depth_per_metre);
}

/// Jimenez's interleaved gradient noise: a different offset for each of a
/// block's neighbours, so the composite's upsample averages them.
fn shaft_jitter(pixel: vec2<u32>, frame: f32) -> f32 {
    let p = vec2<f32>(pixel) + 5.588238 * frame;
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(shaft_out);
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let full = textureDimensions(shaft_depth);
    let pixel = min(vec2<u32>((vec2<f32>(id.xy) + 0.5) * vec2<f32>(full) / vec2<f32>(size)), full - 1u);
    let raw = textureLoad(shaft_depth, pixel, 0);
    let uv = (vec2<f32>(pixel) + 0.5) / vec2<f32>(full);
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let origin = globals.camera_pos.xyz;
    let far_h = shafts.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let direction = normalize(far_h.xyz / far_h.w - origin);
    // Reversed depth: zero is the sky. The stored depth stays inside half
    // float range.
    var distance = 1e7;
    var view_depth = 6e4;
    if raw > 0.0 {
        let hit_h = shafts.inv_view_proj * vec4<f32>(ndc, raw, 1.0);
        let hit = hit_h.xyz / hit_h.w;
        distance = length(hit - origin);
        view_depth = dot(hit - origin, globals.camera_forward.xyz);
    }

    // Outdoors the haze is the mean height field (`medium.wgsl`), whose
    // optical depth along a ray has a closed form and so does its inverse:
    // the distance at which a given optical depth is reached. The noise in
    // the field is left out; it moves the samples by a few metres.
    let m = globals.medium;
    var visible = 1.0;
    if m.min.w > 0.0 && m.max.w > 0.0 {
        let span = medium_detail_span(m, origin, direction, medium_span(m, origin, direction, distance));
        let total = medium_height_depth(m, origin, direction, span);
        if total > 1e-5 && span.y > span.x {
            let scattered = 1.0 - exp(-total);
            let samples = u32(shafts.params.x);
            let jitter = shaft_jitter(id.xy, shafts.params.y);
            let slope = direction.z / m.max.w;
            let h0 = max(origin.z + direction.z * span.x, 0.0) / m.max.w;
            let length = span.y - span.x;
            var sum = 0.0;
            for (var i = 0u; i < samples; i = i + 1u) {
                // Optical depth at which this sample's share of `1 - T` ends.
                let u = (f32(i) + jitter) / f32(samples) * scattered;
                let tau = -log(max(1.0 - u, 1e-7));
                var t = span.y;
                if abs(slope * length) < 0.001 {
                    t = span.x + tau / (m.min.w * exp(-h0));
                } else {
                    let v = exp(-h0) - tau * slope / m.min.w;
                    if v > 0.0 {
                        t = span.x + (-log(v) - h0) / slope;
                    }
                }
                let p = origin + direction * clamp(t, span.x, span.y);
                sum += shaft_stage_sun(p)
                    * cloud_shadow_in_air(aerial_cloud_shadow, aerial_sampler, aerial_sky.shadow, aerial_sky.clouds.xw, aerial_sky.sun.xyz, p);
            }
            visible = sum / f32(samples);
        }
    }
    textureStore(shaft_out, vec2<i32>(id.xy), vec4<f32>(visible, view_depth, 0.0, 1.0));
}
