// Broad-wash incident lighting in a camera-relative radial grid. A following
// column scan integrates advected density and camera extinction once per cell.
@group(2) @binding(0) var fog_grid: texture_storage_3d<rgba16float, write>;
var<workgroup> totals: array<f32, 64>;
var<workgroup> choices: array<u32, 64>;
var<workgroup> spectra: array<vec4<f32>, 64>;

fn fog_hash(input: u32) -> u32 {
    var x = input;
    x = (x ^ (x >> 16u)) * 0x7feb352du;
    x = (x ^ (x >> 15u)) * 0x846ca68bu;
    return x ^ (x >> 16u);
}
fn fog_random(input: u32) -> f32 {
    return f32(fog_hash(input) >> 8u) / 16777216.0;
}

@compute @workgroup_size(64)
fn light_grid(@builtin(workgroup_id) cell: vec3<u32>,
              @builtin(local_invocation_index) lane: u32) {
    let size = textureDimensions(fog_grid);
    let pixel = (vec2<f32>(cell.xy) + 0.5) / vec2<f32>(size.xy)
        * vec2<f32>(haze.transport.w, haze.transport.z);
    let ray = scene_ray(pixel);
    let seed = fog_hash(cell.x + cell.y * 65537u + cell.z * 747796405u
        + u32(haze.tuning.x) * 2891336453u);
    let a = f32(cell.z) / f32(size.z);
    let b = f32(cell.z + 1u) / f32(size.z);
    // Independent, uniform-in-distance stratum jitter prevents persistent
    // slice bands and remains separate from the shadow-light reservoir.
    let radius = haze.shadow.w * mix(a * a, b * b, fog_random(seed ^ 0xA341316Cu));
    let world = haze.camera_pos.xyz + radius * ray.dir;
    let view_depth = radius * ray.view_depth / max(ray.hit_dist, 1e-4);
    let cursor = lights_at(pixel * haze.tiles.xy, view_depth);
    var total = 0.0;
    var chosen = 0u;
    var spectrum = vec4<f32>(0.0);
    for (var offset = 0u; offset < 8u; offset += 1u) {
        // Adjacent lanes read adjacent light records. Striding each lane
        // through consecutive records roughly doubled this pass on RTX 5090.
        let li = offset * 64u + lane;
        if li >= light_index_params.counts.x || li < cursor.min_id || li > cursor.max_id { continue; }
        let bits = light_index_masks[cursor.base + li / 32u];
        if (bits & (1u << (li % 32u))) == 0u { continue; }
        let rest = light_rest[li];
        if rest.wash < FOG_BROAD_WASH || rest.gobo >= 0.5 || rest.haze_gain <= 0.0 { continue; }
        let core = light_core[li];
        let q = world - core.position;
        let d2 = dot(q, q);
        let dist = sqrt(d2);
        if dist >= core.range { continue; }
        let angular = angular_profile(dot(q, rest.direction) / max(dist, 1e-4), rest.cos_beam, rest.cos_field);
        if angular <= 0.0 { continue; }
        let phase = henyey_greenstein(-dot(q, ray.dir) / max(dist, 1e-4),
            haze.transport.y);
        let tint = mix(rest.color, vec3<f32>(1.0), haze.transport.x);
        let radiance = tint * (rest.intensity * rest.haze_gain * haze.tuning.w
            * angular * beam_range_falloff(dist, core.range) * phase
            * exp(-light_optical_depth(li, world)) * smoothstep(FOG_SOURCE_INNER, FOG_SOURCE_OUTER, dist) / max(d2, haze.tuning.z));
        let importance = max(max(radiance.r, radiance.g), radiance.b);
        total += importance;
        if fog_random(seed ^ (li * 157823u)) * total < importance {
            chosen = li;
            spectrum = vec4<f32>(radiance, importance);
        }
    }
    totals[lane] = total;
    choices[lane] = chosen;
    spectra[lane] = spectrum;
    workgroupBarrier();
    // Four disjoint 128-light populations. Weighted reservoir reduction makes
    // unoccluded, equal-spectrum lighting exact before grid discretisation;
    // only visibility and differing spectra contribute selection variance.
    for (var stride = 1u; stride < 16u; stride *= 2u) {
        if lane % (stride * 2u) == 0u {
            let other = lane + stride;
            let sum = totals[lane] + totals[other];
            if fog_random(seed ^ (lane * 31337u + stride * 11777u)) * sum < totals[other] {
                choices[lane] = choices[other];
                spectra[lane] = spectra[other];
            }
            totals[lane] = sum;
        }
        workgroupBarrier();
    }
    if lane % 16u == 0u {
        var radiance = vec3<f32>(0.0);
        if totals[lane] > 0.0 {
            var visibility = 1.0;
            if haze.shadow.x > 0.0 { visibility = fixture_shadow_visibility(world, choices[lane]); }
            radiance = spectra[lane].rgb * (totals[lane] / spectra[lane].a) * visibility;
        }
        spectra[lane] = vec4<f32>(radiance, 0.0);
    }
    workgroupBarrier();
    if lane == 0u {
        textureStore(fog_grid, vec3<i32>(cell), (spectra[0] + spectra[16] + spectra[32] + spectra[48]) / 256.0);
    }
}
