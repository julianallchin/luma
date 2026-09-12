// Shared single-scattering transport and camera/fixture shadow queries.
// Live ungoboed beams integrate visible shadow-map intervals deterministically;
// gobos and exhaustive reference captures use the progressive MIS estimator.
// The same density, photometry, and extinction model feed both integrators.

override GRID_FOG: bool = false;
// The compute kernel is selected only for ungoboed transport with complete
// shadow coverage. Its compiled path needs neither jitter nor MIS fallback.
override NATIVE_DETERMINISTIC: bool = false;
override HAZE_WORK_COUNTS: bool = false;
// Diagnostic only: candidates, intersections, shadow segments, depth reads,
// lit intervals, quadrature taps, shadow rays, whole-span first-block proofs.
var<private> haze_work: array<u32, 8>;
// Diagnostic histogram mode (`LUMA_HAZE_WORK_COUNTS=2`): per (pixel, light)
// pair, the number of non-empty `lit_interval` calls the shadow traversal
// makes (bins 0,1,2,3,4,5-8,9+), and how many pairs are exactly one call over
// the full span. Sizes the lit-interval cache payload.
override HAZE_WORK_HIST: bool = false;
var<private> haze_hist: array<u32, 8>;

// Lit-interval cache (`docs/design/haze-lit-interval-cache.md`). The shadow
// traversal records every interval it hands to `lit_interval`, including empty
// tails, in call order so the compute kernel can store and replay the sequence
// bit-for-bit on later frames. The traversal itself is unchanged; only the
// recording hook sits between it and the quadrature. Bindings and the cache
// branch live in `haze.wgsl`, which alone reaches them.
override INTERVAL_CACHE: bool = false;
// The compaction's fill pass (`haze_compact.wgsl`) records the traversal's
// call list without integrating anything.
override FILL_ONLY: bool = false;
// Period-1 arithmetic-floor control for temporal residual reuse. Only the
// three residual evaluator pipelines enable this; whole-lit and fused paths
// retain their established RGB arithmetic.
override RESID_SCALAR_K: bool = false;
const CACHE_K: u32 = 8u;
// Every call, empty tails included: the replay must add the same terms in
// the same expression shape, or float contraction rounds differently.
var<private> cache_count: u32;
var<private> cache_nonempty: u32;
var<private> cache_intervals: array<vec2<f32>, CACHE_K>;
// Lane within the 8×4 workgroup (one subgroup), its cache block, and the
// block's coordinates in the 8×4 block grid. Set by the compute entry points.
var<private> cache_lane: u32;
var<private> cache_block: u32;
var<private> cache_bx: u32;
var<private> cache_by: u32;

override PROFILE_SKIP_NATIVE_SHADOWS: bool = false;
override PROFILE_SKIP_NATIVE_INTEGRALS: bool = false;
override PROFILE_SKIP_NATIVE_CLOUDS: bool = false;
override PROFILE_SKIP_NATIVE_LIGHT_DEPTH: bool = false;
override PROFILE_SKIP_NATIVE_CAMERA_DEPTH: bool = false;
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
    // Scattering multiplier; stage fixtures and house lamps both use one.
    haze_gain: f32,
    // Two scalars, not a `vec3`: a `vec3` member would take its own 16-byte
    // alignment and push the struct to 80 bytes, disagreeing with the Rust
    // stride. Scalars keep it at 64.
    inverse_right_length: f32,
    field_tangent: f32,
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
    // defined in full-res space), z: 2 on the first grid pass, 1 otherwise;
    // w: fixed capture seed.
    tiles: vec4<f32>,
    // xy: camera near/far planes, z: mean extinction sigma in 1/metres, w: light sampling group size.
    depth: vec4<f32>,
    // x: shadowed fixture count, y: shadow texel size, z: reference tracer sample budget, w: fog-grid radial extent.
    shadow: vec4<f32>,
    medium: ProceduralMedium,
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
@group(0) @binding(9) var fixture_shadow_map_extra: texture_depth_2d_array;

@group(0) @binding(10) var medium_cache: texture_3d<f32>;
@group(0) @binding(11) var shadow_ranges: texture_2d_array<f32>;
@group(0) @binding(12) var shadow_ranges_extra: texture_2d_array<f32>;

@group(3) @binding(0) var camera_fog: texture_3d<f32>;

fn transport_transmittance(ray: SceneRay, li: u32, t: f32, world: vec3<f32>) -> f32 {
    let light_depth = light_optical_depth(li, world);
    if PROFILE_SKIP_NATIVE_CAMERA_DEPTH { return exp(-light_depth); }
    if GRID_FOG {
        let size = vec3<f32>(textureDimensions(camera_fog));
        let span = ray.fog_span;
        let radial = sqrt(clamp((t - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
        let z = (radial * (size.z - 1.0) + 0.5) / size.z;
        let uv = clamp(ray.uv, 0.5 / size.xy, 1.0 - 0.5 / size.xy);
        let transmittance = textureSampleLevel(camera_fog, haze_noise_sampler, vec3<f32>(uv, z), 0.0).a;
        return transmittance * exp(-light_depth);
    }
    return exp(-medium_depth(ray.medium, t) - light_depth);
}

fn haze_density_at(p: vec3<f32>) -> f32 {
    if PROFILE_SKIP_NATIVE_CLOUDS { return medium_envelope(haze.medium, p); }
    return medium_density(haze.medium, p);
}

fn light_optical_depth(li: u32, world: vec3<f32>) -> f32 {
    if PROFILE_SKIP_NATIVE_LIGHT_DEPTH { return 0.0; }
    let core = light_core[li];
    let q = world - core.position;
    let dist = length(q);
    // A segment wholly inside the uniform core has exact analytic extinction.
    // Paths through the boundary fade use the same density cache as clouds.
    if haze.medium.shape.x <= 0.0 && medium_interior(haze.medium, core.position)
        && medium_interior(haze.medium, world) {
        return haze.medium.min.w * dist;
    }
    let direction = light_rest[li].direction;
    let helper = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), abs(direction.z) > 0.98);
    let right = cross(direction, helper) * light_rest[li].inverse_right_length;
    let up = cross(right, direction);
    let tangent = light_rest[li].field_tangent;
    let angles = haze.medium.shape.w;
    let uv = clamp((vec2<f32>(dot(q, right), dot(q, up)) / max(dot(q, direction) * tangent, 1e-5) * 0.5 + 0.5) * (angles - 1.0), vec2<f32>(0.0), vec2<f32>(angles - 1.0));
    let z = clamp(dist / max(core.range, 1e-5) * 32.0, 0.0, 32.0);
    let origin = vec2<f32>(vec2<u32>(li % 16u, li / 16u)) * angles;
    let coordinate = vec3<f32>(origin + uv + 0.5, z + 0.5) / vec3<f32>(textureDimensions(medium_cache));
    return textureSampleLevel(medium_cache, haze_noise_sampler, coordinate, 0.0).r;
}

fn world_from_ndc(ndc: vec3<f32>) -> vec3<f32> {
    let p = haze.inv_view_proj * vec4<f32>(ndc, 1.0);
    return p.xyz / p.w;
}

fn fixture_shadow_visibility(world: vec3<f32>, light_index: u32) -> f32 {
    // Reference-only triangle visibility; production supplies cached maps.
    let slot = light_rest[light_index].shadow_slot;
    if slot < 0.0 {
        return stage_visibility(world, light_core[light_index].position);
    }
    let layer = i32(slot);
    let clip = fixture_shadow_matrices[layer].view_proj * vec4<f32>(world, 1.0);
    return projected_shadow_visibility(clip, layer);
}

fn projected_shadow_visibility(clip: vec4<f32>, layer: i32) -> f32 {
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
    var stored: f32;
    if layer < 256 { stored = textureLoad(fixture_shadow_map, coord, layer, 0); }
    else { stored = textureLoad(fixture_shadow_map_extra, coord, layer - 256, 0); }
    let planes = fixture_shadow_matrices[layer].params;
    let reference = shadow_compare_reference(ndc.z, planes.x, planes.y, 0.02);
    return select(0.0, 1.0, reference >= stored);
}

// Use the cached conservative depth bounds to prove a whole segment lit or
// shadowed. Only a segment containing a shadow edge needs the four samples.
fn segment_shadow_visibility_layer(ray_dir: vec3<f32>, a: f32, b: f32, layer: i32) -> f32 {
    let matrix = fixture_shadow_matrices[layer].view_proj;
    let origin = matrix * vec4<f32>(haze.camera_pos.xyz, 1.0);
    let direction = matrix * vec4<f32>(ray_dir, 0.0);
    let first = origin + direction * mix(a, b, 0.125);
    let last = origin + direction * mix(a, b, 0.875);
    let p = first.xyz / first.w;
    let q = last.xyz / last.w;
    if first.w > 0.0 && last.w > 0.0 && min(p.z, q.z) >= 0.0 && max(p.z, q.z) <= 1.0
        && all(abs(p.xy) <= vec2<f32>(1.0)) && all(abs(q.xy) <= vec2<f32>(1.0)) {
        let dims = textureDimensions(fixture_shadow_map);
        let c0 = min(vec2<u32>((p.xy * vec2<f32>(0.5, -0.5) + 0.5) * vec2<f32>(dims)), dims - 1u);
        let c1 = min(vec2<u32>((q.xy * vec2<f32>(0.5, -0.5) + 0.5) * vec2<f32>(dims)), dims - 1u);
        let difference = (c0.x ^ c1.x) | (c0.y ^ c1.y);
        let level = max(i32(firstLeadingBit(difference)), 0);
        let coordinate = vec2<i32>(c0 >> vec2<u32>(u32(level + 1)));
        var depths: vec2<f32>;
        if layer < 256 { depths = textureLoad(shadow_ranges, coordinate, layer, level).rg; }
        else { depths = textureLoad(shadow_ranges_extra, coordinate, layer - 256, level).rg; }
        let planes = fixture_shadow_matrices[layer].params;
        let ref0 = shadow_compare_reference(p.z, planes.x, planes.y, 0.02);
        let ref1 = shadow_compare_reference(q.z, planes.x, planes.y, 0.02);
        if min(ref0, ref1) >= depths.y { return 1.0; }
        if max(ref0, ref1) < depths.x { return 0.0; }
    }
    let delta = (last - first) / 3.0;
    return (projected_shadow_visibility(first, layer)
        + projected_shadow_visibility(first + delta, layer)
        + projected_shadow_visibility(first + delta * 2.0, layer)
        + projected_shadow_visibility(last, layer)) * 0.25;
}

fn segment_shadow_visibility(ray_dir: vec3<f32>, a: f32, b: f32, li: u32) -> f32 {
    let layer = i32(light_rest[li].shadow_slot);
    if layer >= 0 {
        return segment_shadow_visibility_layer(ray_dir, a, b, layer);
    }
    var visibility = 0.0;
    for (var i = 0u; i < 4u; i += 1u) {
        let t = mix(a, b, (f32(i) + 0.5) * 0.25);
        visibility += fixture_shadow_visibility(haze.camera_pos.xyz + ray_dir * t, li) * 0.25;
    }
    return visibility;
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
    let inverse = inverseSqrt(max(denom, 1e-4));
    return (1.0 - g2) * inverse * inverse * inverse;
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
    // Moving an 8x8 tile repeats after eight frames. Advance the stratum
    // itself too, so temporal accumulation and reference captures converge.
    let temporal_rotation = fract(f32(frame) * 0.61803398875);
    return fract((f32(BLUE_NOISE_RANK[index]) + 0.5) / 64.0 + rotation + temporal_rotation);
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
    medium: MediumRay,
    uv: vec2<f32>,
    fog_span: vec2<f32>,
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
    var j = 0.0;
    if !NATIVE_DETERMINISTIC {
        j = blue_noise(vec2<f32>(frag.x, haze.transport.z - frag.y), u32(haze.tuning.x + haze.tiles.w));
    }
    return SceneRay(ray_dir, hit_dist, view_depth, j, MediumRay(), uv, medium_lighting_span(haze.medium, haze.camera_pos.xyz, ray_dir, haze.shadow.w));
}

fn beam_span(li: u32, ray: SceneRay) -> vec2<f32> {
    let ray_dir = ray.dir;
    let hit_dist = ray.hit_dist;

    let core = light_core[li];
    let oc = haze.camera_pos.xyz - core.position;
    let b = dot(oc, ray_dir);
    let oo = dot(oc, oc);
    let near_only = GRID_FOG && light_rest[li].wash >= FOG_BROAD_WASH && light_rest[li].gobo < 0.5;
    let range = select(core.range, min(core.range, FOG_SOURCE_OUTER), near_only);
    let disc = b * b - (oo - range * range);
    if disc <= 0.0 {
        return vec2<f32>(0.0);
    }
    let sq = sqrt(disc);
    let s0 = max(-b - sq, 0.0);
    let s1 = min(-b + sq, hit_dist);   // geometry occludes the beam
    if s1 <= s0 {
        return vec2<f32>(0.0);
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
        return vec2<f32>(0.0);
    }
    return vec2<f32>(t_a, t_b);
}

// Unshadowed equiangular midpoint estimate guides reference light selection.
// Positive support over the complete analytic span keeps
// selection unbiased even where haze pockets or blockers defeat the estimate.
fn beam_importance(li: u32, ray: SceneRay, sigma: f32) -> f32 {
    let span = beam_span(li, ray);
    if span.y <= span.x { return 0.0; }
    let core = light_core[li];
    let rest = light_rest[li];
    let oc = haze.camera_pos.xyz - core.position;
    let b = dot(oc, ray.dir);
    let h = sqrt(max(dot(oc, oc) - b * b, haze.tuning.z));
    let theta_a = atan((span.x + b) / h);
    let theta_b = atan((span.y + b) / h);
    let t = -b + h * tan((theta_a + theta_b) * 0.5);
    let q = oc + ray.dir * t;
    let dist = max(length(q), 1e-4);
    let angular = angular_profile(dot(q, rest.direction) / dist, rest.cos_beam, rest.cos_field);
    let phase = henyey_greenstein(-(b + t) / dist, haze.transport.y);
    let tint = mix(rest.color, vec3<f32>(1.0), haze.transport.x);
    let spectrum = max(max(tint.r, tint.g), tint.b);
    let energy = rest.intensity * rest.haze_gain * spectrum;
    return energy * max(1e-6, (theta_b - theta_a) / h * angular
        * beam_range_falloff(dist, core.range) * phase * transport_transmittance(ray, li, t, haze.camera_pos.xyz + ray.dir * t));
}

/// Single-scattering radiance this ray receives from light `li`, already
/// multiplied by sigma. Returns zero when the ray misses the light's
/// cone∩ball, or the span it does cross is occluded by geometry.
fn beam_scatter(li: u32, ray: SceneRay, sigma: f32) -> vec3<f32> {
    if HAZE_WORK_COUNTS { haze_work[0] += 1u; }
    // A source that does not scatter leaves before the sphere test. House
    // downlights reach every pixel in the room, so this is the first branch,
    // not a factor folded into the radiance at the end.
    let haze_gain = light_rest[li].haze_gain;
    if haze_gain <= 0.0 {
        return vec3<f32>(0.0);
    }

    let span = beam_span(li, ray);
    let t_a = span.x;
    let t_b = span.y;
    if t_b <= t_a || PROFILE_SKIP_NATIVE_INTEGRALS { return vec3<f32>(0.0); }
    if HAZE_WORK_COUNTS { haze_work[1] += 1u; }
    if NATIVE_DETERMINISTIC {
        if haze.shadow.x <= 0.0 || PROFILE_SKIP_NATIVE_SHADOWS { return lit_interval(li, ray, span.x, span.y); }
        return beam_shadow_integral(li, ray, span);
    }
    if GRID_FOG && light_rest[li].gobo < 0.5 {
        if haze.shadow.x <= 0.0 || PROFILE_SKIP_NATIVE_SHADOWS { return lit_interval(li, ray, span.x, span.y); }
        if light_rest[li].shadow_slot >= 0.0 { return beam_shadow_integral(li, ray, span); }
    }
    let ray_dir = ray.dir;
    let near_clamp = haze.tuning.z;
    let beam_gain = haze.tuning.w * haze_gain;
    let core = light_core[li];
    let rest = light_rest[li];
    let oc = haze.camera_pos.xyz - core.position;
    let b = dot(oc, ray_dir);
    let oo = dot(oc, oc);
    let seg_len = t_b - t_a;

    var sample_count = i32(clamp(haze.params.z, 1.0, f32(MAX_SAMPLES)));
    if rest.shadow_slot < 0.0 && haze.shadow.z > 0.0 {
        // Same MIS estimator and exact visibility, fewer samples for the
        // software-traced fallback. Temporal reconstruction reduces variance.
        sample_count = min(sample_count, i32(haze.shadow.z));
    }
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

    let g = haze.transport.y;
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
        let taper = beam_range_falloff(dist, core.range);
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
        if GRID_FOG && rest.wash >= FOG_BROAD_WASH && rest.gobo < 0.5 {
            radiance *= 1.0 - smoothstep(FOG_SOURCE_INNER, FOG_SOURCE_OUTER, dist);
        }
        let nz = haze_density_at(sample_world);
        // dot(sample->source, rayDir) = -(b + t)/dist, since q = oc + t·rayDir.
        let phase = henyey_greenstein(-(b + t) / max(dist, 1e-4), g);
        acc += tint * (radiance * phase * nz * transport_transmittance(ray, li, t, haze.camera_pos.xyz + ray.dir * t) * mis_w);
    }

    return acc * sigma;
}

// Integrate a visible interval with Gauss-Legendre quadrature in equiangular
// coordinates. Shadow boundaries are supplied by the shadow-map traversal.
fn lit_interval(li: u32, ray: SceneRay, a: f32, b: f32) -> vec3<f32> {
    if b <= a { return vec3<f32>(0.0); }
    if HAZE_WORK_COUNTS { haze_work[4] += 1u; }
    let core = light_core[li];
    let rest = light_rest[li];
    let oc = haze.camera_pos.xyz - core.position;
    let delta = -dot(oc, ray.dir);
    let h = sqrt(max(dot(oc, oc) - delta * delta, haze.tuning.z));
    let th0 = atan((a - delta) / h);
    let th1 = atan((b - delta) / h);
    let pieces = clamp(u32(ceil(max((b - a) / max(haze.medium.shape.y * 0.5, 0.1), (th1 - th0) / 0.4))), 1u, 32u);
    if HAZE_WORK_COUNTS { haze_work[5] += pieces * 4u; }
    let nodes = array<f32, 4>(-0.8611363116, -0.3399810436, 0.3399810436, 0.8611363116);
    let weights = array<f32, 4>(0.3478548451, 0.6521451549, 0.6521451549, 0.3478548451);
    let tint = mix(rest.color, vec3<f32>(1.0), haze.transport.x);
    var sum = 0.0;
    let width = (th1 - th0) / f32(pieces);
    // The two extinction terms — camera transmittance and source optical depth —
    // come from coarse trilinear volumes (a 16-pixel × 128-slice camera grid and
    // a 16×16×32 per-light angular cache). Sampling them once per Gauss node
    // carries no information those textures hold, so evaluate their product once
    // per piece boundary and interpolate linearly in theta across the piece's
    // four nodes. The previous piece's right edge is reused as the next piece's
    // left edge, so the interpolated integrand stays C0 and no seam can appear
    // at a boundary. Cloud density keeps its per-node fetch: the baked field
    // carries metre-scale structure that the wash cones' contrast depends on.
    // The analytic per-tap factors (angular profile, phase, range falloff, 1/d²,
    // source weight) keep their original node positions, count and arithmetic.
    let t_first = delta + h * tan(th0);
    var left_field = transport_transmittance(ray, li, t_first, haze.camera_pos.xyz + ray.dir * t_first);
    for (var piece = 0u; piece < pieces; piece += 1u) {
        let center = th0 + (f32(piece) + 0.5) * width;
        let t_right = delta + h * tan(th0 + f32(piece + 1u) * width);
        let world_right = haze.camera_pos.xyz + ray.dir * t_right;
        let right_field = transport_transmittance(ray, li, t_right, world_right);
        for (var j = 0u; j < 4u; j += 1u) {
            let tangent = tan(center + nodes[j] * width * 0.5);
            let t = delta + h * tangent;
            let q = oc + ray.dir * t;
            let d2 = dot(q, q);
            let distance = sqrt(d2);
            let angular = angular_profile(dot(q, rest.direction) / max(distance, 1e-4), rest.cos_beam, rest.cos_field);
            let phase = henyey_greenstein(-dot(q, ray.dir) / max(distance, 1e-4), haze.transport.y);
            let source_weight = select(1.0, 1.0 - smoothstep(FOG_SOURCE_INNER, FOG_SOURCE_OUTER, distance), rest.wash >= FOG_BROAD_WASH);
            let world = haze.camera_pos.xyz + ray.dir * t;
            let field = mix(left_field, right_field, nodes[j] * 0.5 + 0.5);
            let value = angular * phase * beam_range_falloff(distance, core.range) * source_weight
                * haze_density_at(world) * field
                / max(d2, haze.tuning.z);
            sum += value * h * (1.0 + tangent * tangent) * width * 0.5 * weights[j];
        }
        left_field = right_field;
    }
    if RESID_SCALAR_K { return vec3<f32>(sum); }
    return tint * (sum * rest.intensity * rest.haze_gain * haze.tuning.w * haze.depth.z);
}

// Every quadrature call the shadow traversal makes goes through here so the
// cache (and the diagnostic histogram) sees the exact call sequence, including
// empty tails. An empty interval returns zero before any arithmetic, and adding
// that zero is exact.
fn lit_counted(li: u32, ray: SceneRay, a: f32, b: f32) -> vec3<f32> {
    if INTERVAL_CACHE || HAZE_WORK_HIST {
        if cache_count < CACHE_K { cache_intervals[cache_count] = vec2<f32>(a, b); }
        cache_count += 1u;
        if b > a { cache_nonempty += 1u; }
    }
    if FILL_ONLY { return vec3<f32>(0.0); }
    return lit_interval(li, ray, a, b);
}

fn clip_shadow_plane(span: ptr<function, vec2<f32>>, offset: f32, slope: f32) -> bool {
    if slope > 1e-8 { (*span).x = max((*span).x, -offset / slope); }
    else if slope < -1e-8 { (*span).y = min((*span).y, -offset / slope); }
    else if offset < 0.0 { return false; }
    return true;
}

// Intersect the camera ray with the piecewise constant shadow-map height
// field. Consecutive visible texels form one interval; empty space therefore
// requires no repeated medium/phase evaluations and there is no sample noise.
fn beam_shadow_integral(li: u32, ray: SceneRay, full_span: vec2<f32>) -> vec3<f32> {
    if HAZE_WORK_COUNTS { haze_work[6] += 1u; }
    if INTERVAL_CACHE || HAZE_WORK_HIST { cache_count = 0u; cache_nonempty = 0u; }
    let layer = i32(light_rest[li].shadow_slot);
    let matrix = fixture_shadow_matrices[layer].view_proj;
    let origin = matrix * vec4<f32>(haze.camera_pos.xyz, 1.0);
    let direction = matrix * vec4<f32>(ray.dir, 0.0);
    let planes = fixture_shadow_matrices[layer].params;
    // The photometric cone can be wider than the shadow camera's capped FOV.
    // Outside that projection the existing visibility contract is unshadowed.
    // Clip once in homogeneous coordinates instead of walking outside its map.
    var span = full_span;
    // Keep the original plane order without materializing two indexed
    // private arrays for every ray/light intersection.
    if !clip_shadow_plane(&span, origin.w + origin.x, direction.w + direction.x)
        || !clip_shadow_plane(&span, origin.w - origin.x, direction.w - direction.x)
        || !clip_shadow_plane(&span, origin.w + origin.y, direction.w + direction.y)
        || !clip_shadow_plane(&span, origin.w - origin.y, direction.w - direction.y)
        || !clip_shadow_plane(&span, origin.z, direction.z)
        || !clip_shadow_plane(&span, origin.w - origin.z, direction.w - direction.z) {
        return lit_counted(li, ray, full_span.x, full_span.y);
    }
    if span.y <= span.x { return lit_counted(li, ray, full_span.x, full_span.y); }
    let dims = vec2<i32>(textureDimensions(fixture_shadow_map));
    let first = origin + direction * (span.x + 1e-6);
    let uv = vec2<f32>(first.x, -first.y) / max(first.w, 1e-6) * 0.5 + 0.5;
    var cell = clamp(vec2<i32>(floor(uv * vec2<f32>(dims))), vec2<i32>(0), dims - 1);
    let derivative = vec2<f32>(direction.x * origin.w - origin.x * direction.w,
        origin.y * direction.w - direction.y * origin.w);
    let step = vec2<i32>(sign(derivative));
    var t = span.x;
    var lit_start = -1.0;
    var lit_end = -1.0;
    var sum = lit_counted(li, ray, full_span.x, span.x) + lit_counted(li, ray, span.y, full_span.y);
    let last_clip = origin + direction * span.y;
    let last_uv = vec2<f32>(last_clip.x, -last_clip.y) / max(last_clip.w, 1e-6) * 0.5 + 0.5;
    let last_cell = clamp(vec2<i32>(floor(last_uv * vec2<f32>(dims))), vec2<i32>(0), dims - 1);
    let difference = vec2<u32>(cell ^ last_cell);
    let max_level = max(i32(firstLeadingBit(difference.x | difference.y)), 0);
    var next_level = max_level;
    // Each accepted block crosses at least one monotone texel coordinate.
    // A clipped projected line cannot visit more than width + height cells.
    for (var iteration = 0; iteration < dims.x + dims.y + 2; iteration += 1) {
        if t >= span.y { break; }
        if HAZE_WORK_COUNTS { haze_work[2] += 1u; }
        if any(cell < vec2<i32>(0)) || any(cell >= dims) {
            // Only boundary roundoff can leave the already-clipped map span.
            sum += lit_counted(li, ray, t, span.y);
            t = span.y;
            break;
        }
        var end = span.y;
        var next_x = 1e9;
        var next_y = 1e9;
        var block_base = cell;
        var block = 1;
        var visible_a = t;
        var visible_b = t;
        let start_clip = origin + direction * t;
        let start_ref = shadow_compare_reference(start_clip.z / max(start_clip.w, 1e-6), planes.x, planes.y, 0.02);
        var level = next_level;
        for (var descent = 0u; descent < textureNumLevels(shadow_ranges) + 1u; descent += 1u) {
            let shift = vec2<u32>(u32(level + 1));
            block = 1 << u32(level + 1);
            // The outer map test proves nonnegative cells. A hierarchy block
            // is a power of two, so shifts give the same integer coordinates.
            let block_cell = cell >> shift;
            block_base = block_cell << shift;
            let boundary_cell = block_base + select(vec2<i32>(0), vec2<i32>(block), step > vec2<i32>(0));
            let boundary = vec2<f32>(boundary_cell) / vec2<f32>(dims);
            let ndc = vec2<f32>(boundary.x * 2.0 - 1.0, 1.0 - boundary.y * 2.0);
            next_x = 1e9;
            next_y = 1e9;
            if step.x != 0 { next_x = (ndc.x * origin.w - origin.x) / (direction.x - ndc.x * direction.w); }
            if step.y != 0 { next_y = (ndc.y * origin.w - origin.y) / (direction.y - ndc.y * direction.w); }
            end = min(span.y, max(t, min(next_x, next_y)));
            var depth_range = vec2<f32>(0.0);
            if all(cell >= vec2<i32>(0)) && all(cell < dims) {
                if HAZE_WORK_COUNTS { haze_work[3] += 1u; }
                if level >= 0 {
                    if layer < 256 { depth_range = textureLoad(shadow_ranges, block_cell, layer, level).rg; }
                    else { depth_range = textureLoad(shadow_ranges_extra, block_cell, layer - 256, level).rg; }
                } else {
                    if layer < 256 { depth_range = vec2<f32>(textureLoad(fixture_shadow_map, cell, layer, 0)); }
                    else { depth_range = vec2<f32>(textureLoad(fixture_shadow_map_extra, cell, layer - 256, 0)); }
                }
            }
            let end_clip = origin + direction * end;
            let end_ref = shadow_compare_reference(end_clip.z / max(end_clip.w, 1e-6), planes.x, planes.y, 0.02);
            if min(start_ref, end_ref) >= depth_range.y {
                if HAZE_WORK_COUNTS && iteration == 0 && descent == 0u && end >= span.y { haze_work[7] += 1u; }
                visible_a = t; visible_b = end; break;
            }
            if max(start_ref, end_ref) < depth_range.x { break; }
            if level == -1 {
                let caster = planes.x * planes.y / max(planes.x + depth_range.x * (planes.y - planes.x), 1e-5) + 0.02;
                visible_a = t;
                visible_b = end;
                if abs(direction.w) > 1e-8 {
                    let crossing = (caster - origin.w) / direction.w;
                    if direction.w > 0.0 { visible_b = min(end, crossing); }
                    else { visible_a = max(t, crossing); }
                } else if origin.w > caster { visible_b = t; }
            }
            if level == -1 { break; }
            level -= 1;
        }
        next_level = min(level + 1, max_level);
        if visible_b > visible_a {
            if lit_start >= 0.0 && visible_a > lit_end + 1e-5 {
                sum += lit_counted(li, ray, lit_start, lit_end);
                lit_start = -1.0;
            }
            if lit_start < 0.0 { lit_start = visible_a; }
            lit_end = visible_b;
        }
        t = end;
        let next_clip = origin + direction * t;
        let next_uv = vec2<f32>(next_clip.x, -next_clip.y) / max(next_clip.w, 1e-6) * 0.5 + 0.5;
        let projected_cell = vec2<i32>(floor(next_uv * vec2<f32>(dims)));
        // Projective roundoff at a crossed boundary must never move either
        // DDA coordinate backwards: both coordinates are monotone on this ray.
        cell = select(min(cell, projected_cell), max(cell, projected_cell), step >= vec2<i32>(0));
        if next_x <= next_y { cell.x = select(block_base.x - 1, block_base.x + block, step.x > 0); }
        if next_y <= next_x { cell.y = select(block_base.y - 1, block_base.y + block, step.y > 0); }

    }
    if lit_start >= 0.0 { sum += lit_counted(li, ray, lit_start, lit_end); }
    return sum;
}
