// Volumetric haze — transliteration of `effects/volumetric-haze-pass.ts`.
//
// There is no global march. Each light's contribution is the 1D integral of
// single-scatter along the exact span of the ray inside that light's
// cone∩range volume (analytic ray/convex-solid intersection), estimated with
// equiangular + uniform MIS sampling. Empty pixels cost a handful of
// intersection tests and zero march steps.
//
// Two mechanical departures from the GLSL, both spec §3.1:
//   * the light array is two SoA storage buffers, not a packed `DataTexture`.
//     `LightCore` alone drives the sphere reject, so a pixel a light does not
//     reach costs one 16-byte read rather than four. That property is what
//     makes the 256-light loop viable and it survives the port.
//   * with storage buffers there is no sampling inside data-dependent control
//     flow, so WGSL's uniformity rule never comes up. The one depth read stays
//     at the top of `main`.

const MAX_SAMPLES: i32 = 32;

struct LightCore {
    position: vec3<f32>,
    range: f32,
};

struct LightRest {
    direction: vec3<f32>,
    cos_beam: f32,
    color: vec3<f32>,
    intensity: f32,
    cos_field: f32,
    wash: f32,
    _pad: vec2<f32>,
};

struct Haze {
    inv_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // x: light count, y: density, z: ray steps, w: elapsed seconds.
    params: vec4<f32>,
    // x: frame index (jitter walk), y: accumulation weight (1/subframes),
    // z: near clamp, w: beam gain.
    tuning: vec4<f32>,
    // x: white leak, y: phase g, z: viewport height, w: unused.
    transport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> haze: Haze;
@group(0) @binding(1) var<storage, read> light_core: array<LightCore>;
@group(0) @binding(2) var<storage, read> light_rest: array<LightRest>;
@group(0) @binding(3) var depth_texture: texture_depth_2d;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle; no vertex buffer.
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

fn hash3(p_in: vec3<f32>) -> vec3<f32> {
    let p = vec3<f32>(
        dot(p_in, vec3<f32>(127.1, 311.7, 74.7)),
        dot(p_in, vec3<f32>(269.5, 183.3, 246.1)),
        dot(p_in, vec3<f32>(113.5, 271.9, 124.6)),
    );
    return -1.0 + 2.0 * fract(sin(p) * 43758.5453123);
}

fn noise3d(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let g000 = dot(hash3(i + vec3<f32>(0.0, 0.0, 0.0)), f - vec3<f32>(0.0, 0.0, 0.0));
    let g100 = dot(hash3(i + vec3<f32>(1.0, 0.0, 0.0)), f - vec3<f32>(1.0, 0.0, 0.0));
    let g010 = dot(hash3(i + vec3<f32>(0.0, 1.0, 0.0)), f - vec3<f32>(0.0, 1.0, 0.0));
    let g110 = dot(hash3(i + vec3<f32>(1.0, 1.0, 0.0)), f - vec3<f32>(1.0, 1.0, 0.0));
    let g001 = dot(hash3(i + vec3<f32>(0.0, 0.0, 1.0)), f - vec3<f32>(0.0, 0.0, 1.0));
    let g101 = dot(hash3(i + vec3<f32>(1.0, 0.0, 1.0)), f - vec3<f32>(1.0, 0.0, 1.0));
    let g011 = dot(hash3(i + vec3<f32>(0.0, 1.0, 1.0)), f - vec3<f32>(0.0, 1.0, 1.0));
    let g111 = dot(hash3(i + vec3<f32>(1.0, 1.0, 1.0)), f - vec3<f32>(1.0, 1.0, 1.0));
    let x00 = mix(g000, g100, u.x);
    let x10 = mix(g010, g110, u.x);
    let x01 = mix(g001, g101, u.x);
    let x11 = mix(g011, g111, u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

/// Multiplicative density field, centred on 1. The same turbulence exists
/// everywhere including the near field; it reads clean at the source only
/// because the core is overexposed. No spatial gate anywhere.
fn haze_noise(p_world: vec3<f32>, elapsed: f32) -> f32 {
    // The turbulence is anisotropic — hash3 mixes each axis with different
    // constants and the drift is per-axis — so the field is only the same
    // field if it is sampled in the basis it was authored in. Everything else
    // here is Z-up world space; the noise alone is evaluated in three's Y-up
    // basis (`coords::world_from_three` inverted).
    let p = vec3<f32>(p_world.x, p_world.z, -p_world.y);
    let drift = vec3<f32>(elapsed * 0.4, elapsed * 0.25, elapsed * 0.15);
    let q = p * 2.0 + drift;
    let n = noise3d(q) * 0.6 + noise3d(q * 3.0 + drift + 3.7) * 0.4;
    return max(1.0 + 1.1 * n, 0.05);
}

fn world_from_ndc(ndc: vec3<f32>) -> vec3<f32> {
    let p = haze.inv_view_proj * vec4<f32>(ndc, 1.0);
    return p.xyz / p.w;
}

/// Peaked photometric profile with GDTF beam/field semantics: 100% on the
/// axis, 50% at the beam angle, smoothly cut to zero approaching the field
/// angle.
///
/// This function is the gobo seam: replace it with a texture lookup in
/// cone-local polar coordinates (default = this smooth circle) to project
/// arbitrary gobo shapes through the volume without touching any call site.
fn angular_profile(cos_angle: f32, cos_beam: f32, cos_field: f32) -> f32 {
    if cos_angle <= cos_field {
        return 0.0;
    }
    // (1-cos) scales as angle², so this ratio is (theta/thetaBeam)² — exactly
    // the Gaussian argument. exp(-ln2·t) puts the 50% point at the beam angle.
    let t = (1.0 - cos_angle) / max(1.0 - cos_beam, 1e-5);
    let peak = exp(-0.6931472 * t);
    let cut = smoothstep(cos_field, mix(cos_field, cos_beam, 0.35), cos_angle);
    return peak * cut;
}

/// Henyey-Greenstein, normalised so isotropic (g=0) == 1 rather than 1/4pi:
/// the "intensity" here is a 0..1 dimmer, not radiance in watts, so the
/// absolute scale lives in the beam gain and only the angular shape matters.
fn henyey_greenstein(cos_t: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = 1.0 + g2 - 2.0 * g * cos_t;
    return (1.0 - g2) / pow(max(denom, 1e-4), 1.5);
}

fn ign(frag: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * frag.x + 0.00583715 * frag.y));
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(frag.xy);
    let raw_depth = textureLoad(depth_texture, coord, 0);
    let weight = haze.tuning.y;
    let density = haze.params.y;

    let size = vec2<f32>(textureDimensions(depth_texture));
    let uv = frag.xy / size;
    let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);

    if density < 0.001 {
        return vec4<f32>(0.0, 0.0, 0.0, raw_depth * weight);
    }

    let far_world = world_from_ndc(vec3<f32>(ndc_xy, 0.5));
    let ray_dir = normalize(far_world - haze.camera_pos.xyz);
    let world_hit = world_from_ndc(vec3<f32>(ndc_xy, raw_depth));
    let hit_dist = length(world_hit - haze.camera_pos.xyz);

    // Mean extinction. The noise modulates in-scatter only; transmittance uses
    // the mean so it stays an analytic exp(-sigma*t) with no flicker.
    let sigma = density * 0.06;
    let near_clamp = haze.tuning.z;
    let beam_gain = haze.tuning.w;

    // Golden-ratio temporal walk on the per-pixel stratum jitter.
    // `gl_FragCoord` counts rows from the bottom and `@builtin(position)` from
    // the top, so the jitter pattern only lands on the same pixels as the
    // goldens' if the row index is flipped back.
    let j = fract(ign(vec2<f32>(frag.x, haze.transport.z - frag.y)) + haze.tuning.x * 0.61803398875);

    var scattered = vec3<f32>(0.0);

    // Ambient medium fill — diffuse haze the beams cut through. Closed-form
    // transmittance; eight stratified noise taps keep the drifting smoke
    // structure visible instead of averaging it flat.
    {
        let amb_end = min(hit_dist, 24.0);
        let amb_step = amb_end / 8.0;
        var amb = 0.0;
        for (var i = 0; i < 8; i = i + 1) {
            let t = (f32(i) + j) * amb_step;
            amb += haze_noise(haze.camera_pos.xyz + ray_dir * t, haze.params.w) * exp(-sigma * t);
        }
        scattered += vec3<f32>(0.014, 0.011, 0.009) * density * amb * sigma * amb_step;
    }

    let sample_count = i32(clamp(haze.params.z, 1.0, f32(MAX_SAMPLES)));
    // MIS split: equiangular samples own the hot near field (their density
    // cancels 1/d² exactly), uniform samples own the dim far tail where the
    // turbulence lives. Balance-heuristic weights combine them.
    let n_eq = (sample_count + 1) / 2;
    let n_un = sample_count - n_eq;
    let light_count = i32(haze.params.x);

    for (var li = 0; li < light_count; li = li + 1) {
        let core = light_core[li];
        let oc = haze.camera_pos.xyz - core.position;
        let b = dot(oc, ray_dir);
        let oo = dot(oc, oc);
        let disc = b * b - (oo - core.range * core.range);
        if disc <= 0.0 {
            continue;
        }
        let sq = sqrt(disc);
        let s0 = max(-b - sq, 0.0);
        let s1 = min(-b + sq, hit_dist);   // geometry occludes the beam
        if s1 <= s0 {
            continue;
        }

        let rest = light_rest[li];
        let cf2 = rest.cos_field * rest.cos_field;
        let dv = dot(ray_dir, rest.direction);
        let ov = dot(oc, rest.direction);
        let qa = dv * dv - cf2;
        let qb = dv * ov - cf2 * b;
        let qc = ov * ov - cf2 * oo;

        var r0 = s0;
        var r1 = s0;
        if abs(qa) > 1e-6 {
            let qd = qb * qb - qa * qc;
            if qd > 0.0 {
                let qs = sqrt(qd);
                var a0 = (-qb - qs) / qa;
                var a1 = (-qb + qs) / qa;
                if a0 > a1 {
                    let tmp = a0;
                    a0 = a1;
                    a1 = tmp;
                }
                r0 = clamp(a0, s0, s1);
                r1 = clamp(a1, s0, s1);
            }
        } else if abs(qb) > 1e-6 {
            // Ray grazing along the cone surface: the quadratic degenerates.
            r0 = clamp(-qc / (2.0 * qb), s0, s1);
            r1 = r0;
        }

        // Solid forward cone and range ball are both convex, so the ray is
        // inside their intersection along one contiguous span. Partition
        // [s0,s1] at the cone roots and keep sub-intervals whose midpoints are
        // inside the forward cone.
        var t_a = 1e9;
        var t_b = -1e9;
        for (var k = 0; k < 3; k = k + 1) {
            var ea = s0;
            var eb = r0;
            if k == 1 {
                ea = r0;
                eb = r1;
            } else if k == 2 {
                ea = r1;
                eb = s1;
            }
            if eb - ea < 1e-5 {
                continue;
            }
            let mp = oc + ray_dir * ((ea + eb) * 0.5);
            let mm = dot(mp, rest.direction);
            if mm > 0.0 && mm * mm >= cf2 * dot(mp, mp) {
                t_a = min(t_a, ea);
                t_b = max(t_b, eb);
            }
        }
        if t_b <= t_a {
            continue;
        }
        let seg_len = t_b - t_a;

        // Equiangular substitution t = delta + h·tan(theta): sample density
        // proportional to 1/d² around the source.
        let delta = -b;
        let h = sqrt(max(oo - b * b, near_clamp));
        let th_a = atan((t_a - delta) / h);
        let th_b = atan((t_b - delta) / h);
        let d_th = th_b - th_a;

        let g = mix(haze.transport.y, haze.transport.y * 0.3, rest.wash);
        var acc = vec3<f32>(0.0);

        // Emitted spectrum: the saturated colour plus a small broadband leak —
        // a real fixture is a white source behind an imperfect filter, plus
        // lens glare. White-hot is EMERGENT from this: near the source the
        // leak's absolute radiance is enormous, all channels blow out, and AgX
        // rolls the core to white; mid-beam the leak is invisible and the true
        // colour shows. No radiance gate, no white mix.
        let tint = mix(rest.color, vec3<f32>(1.0), haze.transport.x);

        // Decorrelate jitter across lights so overlapping cones dither
        // independently — correlated jitter turns overlaps into stripes.
        let jl = fract(j + f32(li) * 0.7548777);

        for (var i = 0; i < MAX_SAMPLES; i = i + 1) {
            if i >= sample_count {
                break;
            }
            var t: f32;
            if i < n_eq {
                let u = (f32(i) + jl) / f32(n_eq);
                t = delta + h * tan(th_a + u * d_th);
            } else {
                let u = (f32(i - n_eq) + jl) / f32(n_un);
                t = t_a + u * seg_len;
            }
            // Balance heuristic over the two strategies; the equiangular pdf
            // uses the same clamped-h geometry the tan mapping sampled with.
            let dt2 = (t - delta) * (t - delta) + h * h;
            let mis_w = 1.0 / (f32(n_eq) * h / (d_th * dt2) + f32(n_un) / seg_len);

            let q = oc + ray_dir * t;
            let d2 = dot(q, q);
            let dist = sqrt(d2);
            let cos_angle = dot(q, rest.direction) / max(dist, 1e-4);

            let angular = angular_profile(cos_angle, rest.cos_beam, rest.cos_field);
            if angular <= 0.0 {
                continue;
            }

            // Soft range taper — the beam dissolves into the dark instead of
            // popping at the hard cull sphere.
            let taper = 1.0 - smoothstep(core.range * 0.7, core.range, dist);

            // True HDR radiance, no clamp to display range. The tonemapper is
            // the camera; blinding values are its problem and the white-hot
            // core is its correct answer.
            let radiance = rest.intensity * angular * taper * beam_gain / max(d2, near_clamp);
            let nz = haze_noise(haze.camera_pos.xyz + ray_dir * t, haze.params.w);
            // dot(sample->source, rayDir) = -(b + t)/dist, since q = oc + t·rayDir.
            let phase = henyey_greenstein(-(b + t) / max(dist, 1e-4), g);
            acc += tint * (radiance * phase * nz * exp(-sigma * t) * mis_w);
        }

        scattered += acc * sigma;
    }

    // Alpha carries the depth this texel saw so the composite can do a
    // depth-aware bilateral upsample without bleeding across silhouettes.
    // Both channels are pre-weighted for subframe accumulation (spec §6).
    return vec4<f32>(scattered * weight, raw_depth * weight);
}
