// The beam transport: everything both volumetric consumers run.
//
// `haze.wgsl` (the full-screen tiled marcher) and `beam.wgsl` (the per-beam
// proxy pass, when it lands) prepend this file and call two functions:
// `scene_ray` reconstructs a fragment's camera ray, and `beam_scatter`
// integrates one light's single-scatter contribution along it. There is
// exactly one copy of the integrand, and it is a function, not an included
// fragment body — a reviewer checking the two passes draw the same beam reads
// this file and nothing else.
//
// This file also owns the group-0 layout: the passes bind the same buffers in
// the same slots, so the layout lives with the functions that read it.
//
// Everything here moved verbatim from `haze.wgsl`; the contract goldens are
// byte-exact across the move. When the emissive-only beam bucket lands it
// must be an early return inside `beam_scatter`, never a second pipeline —
// one beam pipeline is a hard rule (a shipping previz product measured 63+63
// fps fixtures collapsing to 3 fps purely on per-beam-type pipeline swaps).

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
    gobo: f32,
    gobo_rotation: f32,
    // Shadow-map layer for this cone, or negative when it has none. Must match
    // `scene_bindings.wgsl` — the two shaders read the same buffer.
    shadow_slot: f32,
    // Three scalars, not a `vec3`: a `vec3` member would take its own 16-byte
    // alignment and push the struct to 80 bytes, disagreeing with the Rust
    // stride. Scalars keep it at 64.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

struct Haze {
    inv_view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // x: light count, y: density, z: ray steps, w: elapsed seconds.
    params: vec4<f32>,
    // x: frame index (jitter walk), y: accumulation weight (1/subframes),
    // z: near clamp, w: beam gain.
    tuning: vec4<f32>,
    // x: white leak, y: phase g, zw: this target's height and width in px.
    transport: vec4<f32>,
    // xy: this pass's pixel → full-resolution pixel scale (the light index is
    // defined in full-res space), z: unused, w: fixed capture seed.
    tiles: vec4<f32>,
    // xy: camera near/far planes, z: mean extinction sigma in 1/metres.
    depth: vec4<f32>,
    // x: shadowed fixture count, y: shadow texel size.
    shadow: vec4<f32>,
};

struct FixtureShadowMatrix {
    view_proj: mat4x4<f32>,
    // xy: shadow projection near/far planes in metres.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> haze: Haze;
@group(0) @binding(1) var<storage, read> light_core: array<LightCore>;
@group(0) @binding(2) var<storage, read> light_rest: array<LightRest>;
@group(0) @binding(3) var depth_texture: texture_depth_2d;
// 4 and 5 held the per-pass tile list before the unified light index
// (`light_index.wgsl`, bound as group 1) replaced it; they now carry the baked
// density field (`haze_field.rs`, `haze_noise_bake.wgsl`).
@group(0) @binding(4) var haze_noise_field: texture_3d<f32>;
@group(0) @binding(5) var haze_noise_sampler: sampler;
@group(0) @binding(6) var<storage, read> fixture_shadow_matrices: array<FixtureShadowMatrix>;
@group(0) @binding(7) var fixture_shadow_map: texture_depth_2d_array;

/// Multiplicative density field, centred on 1. The same turbulence exists
/// everywhere including the near field; it reads clean at the source only
/// because the core is overexposed. No spatial gate anywhere.
///
/// Two octaves of gradient noise, read from a baked wrapping texture rather
/// than re-derived per sample. Evaluating the lattice here cost 87-95% of a
/// volumetric sample and therefore of the whole march; the texture fetch hides
/// under the transport's own arithmetic and costs no more than deleting the
/// density term (`docs/design/haze-noise-field.md`).
fn haze_noise(p_world: vec3<f32>, elapsed: f32) -> f32 {
    // The turbulence is anisotropic — the field mixes each axis differently
    // and the drift is per-axis — so it is only the same field if it is
    // sampled in the basis it was authored in. Everything else here is Z-up
    // world space; the noise alone is evaluated in three's Y-up basis
    // (`coords::world_from_three` inverted).
    let p = vec3<f32>(p_world.x, p_world.z, -p_world.y);
    let drift = vec3<f32>(elapsed * 0.4, elapsed * 0.25, elapsed * 0.15);
    let q = p * 2.0 + drift;
    // The field repeats every FIELD_CELLS units of q, so the divide is the
    // whole mapping. The octaves keep their own coordinates rather than
    // sharing a baked sum: that is what keeps them drifting at different rates
    // relative to each other, and so what reads as smoke rather than as a
    // sliding photograph.
    let a = textureSampleLevel(
        haze_noise_field,
        haze_noise_sampler,
        q * FIELD_INV_CELLS,
        0.0,
    ).x;
    let b = textureSampleLevel(
        haze_noise_field,
        haze_noise_sampler,
        (q * 3.0 + drift + 3.7) * FIELD_INV_CELLS,
        0.0,
    ).x;
    return max(1.0 + 1.1 * (a * 0.6 + b * 0.4), 0.05);
}

fn world_from_ndc(ndc: vec3<f32>) -> vec3<f32> {
    let p = haze.inv_view_proj * vec4<f32>(ndc, 1.0);
    return p.xyz / p.w;
}

fn fixture_shadow_visibility(world: vec3<f32>, light_index: u32) -> f32 {
    // A cone without a slot casts no shadow rather than borrowing another's.
    let slot = light_rest[light_index].shadow_slot;
    if slot < 0.0 {
        return 1.0;
    }
    let layer = i32(slot);
    let clip = fixture_shadow_matrices[layer].view_proj * vec4<f32>(world, 1.0);
    let ndc = clip.xyz / clip.w;
    if ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 1.0;
    }
    // One nearest depth lookup per integration sample keeps shadow cost
    // proportional to actual in-volume work. Reverse-Z stores the closest
    // caster as the greatest depth; a farther volume sample is therefore
    // shadowed when its reference falls below that stored value. Temporal
    // accumulation supplies the soft edge without multiplying atlas reads.
    // Fog samples are free-space points — nothing self-shadows — so the slack
    // is purely a precision guard and stays tight.
    let dimensions = vec2<i32>(textureDimensions(fixture_shadow_map));
    let coord = clamp(vec2<i32>(uv * vec2<f32>(dimensions)), vec2<i32>(0), dimensions - 1);
    let stored = textureLoad(fixture_shadow_map, coord, layer, 0);
    let planes = fixture_shadow_matrices[layer].params;
    let reference = shadow_compare_reference(ndc.z, planes.x, planes.y, 0.02);
    return select(0.0, 1.0, reference >= stored);
}

fn linear_view_depth(raw_depth: f32) -> f32 {
    let near = haze.depth.x;
    let far = haze.depth.y;
    // The scene attachment is reverse-Z: near is one, infinity approaches zero.
    return near * far / max(near + raw_depth * (far - near), 1e-5);
}

/// Henyey-Greenstein, normalised so isotropic (g=0) == 1 rather than 1/4pi:
/// the "intensity" here is a 0..1 dimmer, not radiance in watts, so the
/// absolute scale lives in the beam gain and only the angular shape matters.
fn henyey_greenstein(cos_t: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = 1.0 + g2 - 2.0 * g * cos_t;
    return (1.0 - g2) / pow(max(denom, 1e-4), 1.5);
}

// Progressive best-candidate rank tile. Ranks are visited in a toroidal
// farthest-point order, giving every prefix blue-noise spacing. A per-tile
// Cranley rotation breaks the visible 8x8 repeat without changing that local
// spectrum; capture mode fixes `frame`, so this is byte deterministic.
const BLUE_NOISE_RANK = array<u32, 64>(
    20u, 52u, 25u, 33u, 31u, 32u, 28u, 34u,
    39u, 11u, 62u, 4u, 35u, 9u, 44u, 6u,
    17u, 36u, 26u, 55u, 19u, 51u, 30u, 57u,
    38u, 2u, 63u, 15u, 59u, 0u, 43u, 13u,
    21u, 50u, 29u, 53u, 23u, 40u, 22u, 56u,
    45u, 8u, 54u, 7u, 49u, 14u, 42u, 5u,
    27u, 48u, 18u, 37u, 24u, 41u, 16u, 46u,
    58u, 1u, 60u, 10u, 61u, 3u, 47u, 12u,
);

fn blue_noise(frag: vec2<f32>, frame: u32) -> f32 {
    let pixel = vec2<u32>(frag) + vec2<u32>(frame * 3u, frame * 5u);
    let index = (pixel.y & 7u) * 8u + (pixel.x & 7u);
    let tile = floor(frag / 8.0);
    let rotation = fract(sin(dot(tile, vec2<f32>(12.9898, 78.233))) * 43758.5453);
    return fract((f32(BLUE_NOISE_RANK[index]) + 0.5) / 64.0 + rotation);
}

/// One fragment's camera ray, its scene-occlusion distance and its blue-noise
/// stratum offset.
///
/// Byte-identical to what the marcher's `fs_main` did: the checkerboard
/// near/far depth pick and the flipped-row jitter coordinate are properties of
/// the *target*, not of the pass, so both consumers must not merely agree —
/// they must run the same instructions.
struct SceneRay {
    dir: vec3<f32>,
    hit_dist: f32,
    view_depth: f32,
    jitter: f32,
};

fn scene_ray(frag: vec2<f32>) -> SceneRay {
    // This target may be smaller than the depth buffer (`haze_resolution`), so
    // the ray is built from *this* pass's uv and the depth comes from this
    // texel's full-res footprint — never a blend of texels, because a blended
    // depth across a silhouette is a surface that is not there. Within the
    // footprint the pick is a checkerboard of nearest/farthest: a single fixed
    // point sample makes whole low-res rows land on one side of a
    // near-horizontal silhouette, and the composite's bilateral upsample cannot
    // recover when all four of a pixel's taps chose the wrong side (dark bands
    // through beams, glow bleeding over near occluders). Alternating the pick
    // guarantees every full-res pixel has a same-side tap in its 2x2
    // neighbourhood; the upsample's depth weights do the rest.
    let size = vec2<f32>(haze.transport.w, haze.transport.z);
    let uv = frag / size;
    let depth_dims = vec2<f32>(textureDimensions(depth_texture));
    let span = max(vec2<i32>(depth_dims / size + 0.5), vec2<i32>(1)) - vec2<i32>(1);
    let corner = vec2<i32>(vec2<f32>(floor(frag)) * depth_dims / size);
    let d00 = textureLoad(depth_texture, corner, 0);
    let d10 = textureLoad(depth_texture, corner + vec2<i32>(span.x, 0), 0);
    let d01 = textureLoad(depth_texture, corner + vec2<i32>(0, span.y), 0);
    let d11 = textureLoad(depth_texture, corner + span, 0);
    // Reverse-Z: the greatest raw value is the nearest surface.
    let near_raw = max(max(d00, d10), max(d01, d11));
    let far_raw = min(min(d00, d10), min(d01, d11));
    let checker = (u32(frag.x) + u32(frag.y)) & 1u;
    let raw_depth = select(far_raw, near_raw, checker == 1u);
    let view_depth = linear_view_depth(raw_depth);

    let ndc_xy = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let far_world = world_from_ndc(vec3<f32>(ndc_xy, 0.5));
    let ray_dir = normalize(far_world - haze.camera_pos.xyz);
    let world_hit = world_from_ndc(vec3<f32>(ndc_xy, raw_depth));
    let hit_dist = length(world_hit - haze.camera_pos.xyz);

    // Golden-ratio temporal walk on the per-pixel stratum jitter.
    // `gl_FragCoord` counts rows from the bottom and `@builtin(position)` from
    // the top, so the jitter pattern only lands on the same pixels as the
    // goldens' if the row index is flipped back.
    let j = blue_noise(
        vec2<f32>(frag.x, haze.transport.z - frag.y),
        u32(haze.tuning.x + haze.tiles.w),
    );
    return SceneRay(ray_dir, hit_dist, view_depth, j);
}

/// Single-scattering radiance this ray receives from light `li`, already
/// multiplied by sigma. Returns zero when the ray misses the light's
/// cone∩ball, or the span it does cross is occluded by geometry.
fn beam_scatter(li: u32, ray: SceneRay, sigma: f32) -> vec3<f32> {
    let ray_dir = ray.dir;
    let hit_dist = ray.hit_dist;
    let near_clamp = haze.tuning.z;
    let beam_gain = haze.tuning.w;

    let core = light_core[li];
    let oc = haze.camera_pos.xyz - core.position;
    let b = dot(oc, ray_dir);
    let oo = dot(oc, oc);
    let disc = b * b - (oo - core.range * core.range);
    if disc <= 0.0 {
        return vec3<f32>(0.0);
    }
    let sq = sqrt(disc);
    let s0 = max(-b - sq, 0.0);
    let s1 = min(-b + sq, hit_dist);   // geometry occludes the beam
    if s1 <= s0 {
        return vec3<f32>(0.0);
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
        return vec3<f32>(0.0);
    }
    let seg_len = t_b - t_a;

    let sample_count = i32(clamp(haze.params.z, 1.0, f32(MAX_SAMPLES)));
    // MIS split: equiangular samples own the hot near field (their density
    // cancels 1/d² exactly), uniform samples own the dim far tail where the
    // turbulence lives. Balance-heuristic weights combine them.
    let n_eq = (sample_count + 1) / 2;
    let n_un = sample_count - n_eq;

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
    let jl = fract(ray.jitter + f32(li) * 0.7548777);

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
        let gobo = gobo_transmission(
            q,
            rest.direction,
            rest.cos_field,
            rest.gobo,
            rest.gobo_rotation,
        );

        // True HDR radiance, no clamp to display range. The tonemapper is
        // the camera; blinding values are its problem and the white-hot
        // core is its correct answer.
        let sample_world = haze.camera_pos.xyz + ray_dir * t;
        var radiance = rest.intensity * angular * taper * gobo * beam_gain
            / max(d2, near_clamp);
        // Preserve the established shadow-off arithmetic exactly: even a
        // multiply by 1 can change half-float rounding and invalidate a
        // capture without changing the authored image.
        if haze.shadow.x > 0.0 {
            radiance *= fixture_shadow_visibility(sample_world, li);
        }
        let nz = haze_noise(sample_world, haze.params.w);
        // dot(sample->source, rayDir) = -(b + t)/dist, since q = oc + t·rayDir.
        let phase = henyey_greenstein(-(b + t) / max(dist, 1e-4), g);
        acc += tint * (radiance * phase * nz * exp(-sigma * t) * mis_w);
    }

    return acc * sigma;
}
