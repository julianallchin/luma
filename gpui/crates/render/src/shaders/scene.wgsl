// Diagnostic omissions specialize away at pipeline creation. Never quality references.
override PROFILE_SKIP_FIXTURES: bool = false;
override PROFILE_SKIP_SURFACE_SHADOWS: bool = false;

// Opaque scene pass. One material path, three.js `MeshStandardMaterial`
// semantics: metallic-roughness GGX, one ambient term, one shadowed
// directional light, plus the per-fixture face point lights.
//
// Every BRDF term below is transliterated from three's
// `bsdfs.glsl.js` / `lights_physical_pars_fragment.glsl.js`. Divergence here is
// a diffuse, hard-to-localise golden failure, so keep it literal.

const PI: f32 = 3.14159265359;
const RECIPROCAL_PI: f32 = 0.31830988618;

// glTF color maps are sRGB-decoded by their texture format. Normal,
// metallic-roughness and occlusion maps use linear UNORM views.
@group(1) @binding(0) var base_color_map: texture_2d<f32>;
@group(1) @binding(1) var normal_map: texture_2d<f32>;
@group(1) @binding(2) var metallic_roughness_map: texture_2d<f32>;
@group(1) @binding(3) var occlusion_map: texture_2d<f32>;
@group(1) @binding(4) var emissive_map: texture_2d<f32>;
@group(1) @binding(5) var material_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) @interpolate(flat) instance: u32,
    @location(3) uv: vec2<f32>,
    @location(4) tangent: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @builtin(instance_index) instance: u32,
) -> VsOut {
    let inst = instances[instance];
    let world = inst.model * vec4<f32>(position, 1.0);
    var out: VsOut;
    out.clip = globals.view_proj * world;
    out.world = world.xyz;
    out.normal = (inst.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    out.instance = instance;
    out.uv = uv;
    let model3 = mat3x3<f32>(inst.model[0].xyz, inst.model[1].xyz, inst.model[2].xyz);
    // A reflection reverses the model-space tangent frame. Preserve that
    // orientation in w so cross(N,T)*w still points along the transformed
    // bitangent. Treat a singular transform as non-mirrored; it has no stable
    // handedness (and no visible surface) to recover.
    let model_handedness = select(1.0, -1.0, determinant(model3) < -1e-8);
    out.tangent = vec4<f32>(
        (inst.model * vec4<f32>(tangent.xyz, 0.0)).xyz,
        tangent.w * model_handedness,
    );
    return out;
}

/// Depth-only entry for the shadow map and the haze pass's depth input.
@vertex
fn vs_depth(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @builtin(instance_index) instance: u32,
) -> @builtin(position) vec4<f32> {
    _ = normal;
    _ = uv;
    _ = tangent;
    return globals.light_view_proj[0] * instances[instance].model * vec4<f32>(position, 1.0);
}

/// Depth-only entry for the fixture shadow maps. Instances arrive bucketed
/// by mesh: `instance_index` walks a slice of `caster_instances`, whose
/// entries are the frame's draw indices — one instanced draw per distinct
/// mesh per map instead of one draw per caster.
@vertex
fn vs_fixture_shadow(
    @location(0) position: vec3<f32>,
    @builtin(instance_index) slot: u32,
) -> @builtin(position) vec4<f32> {
    let instance = caster_instances[slot];
    return globals.light_view_proj[0] * instances[instance].model * vec4<f32>(position, 1.0);
}

fn f_schlick(f0: vec3<f32>, f90: f32, dot_vh: f32) -> vec3<f32> {
    let fresnel = exp2((-5.55473 * dot_vh - 6.98316) * dot_vh);
    return f0 * (1.0 - fresnel) + vec3<f32>(f90) * fresnel;
}

fn v_ggx_smith_correlated(alpha: f32, dot_nl: f32, dot_nv: f32) -> f32 {
    let a2 = alpha * alpha;
    let gv = dot_nl * sqrt(a2 + (1.0 - a2) * dot_nv * dot_nv);
    let gl = dot_nv * sqrt(a2 + (1.0 - a2) * dot_nl * dot_nl);
    return 0.5 / max(gv + gl, 1e-6);
}

fn d_ggx(alpha: f32, dot_nh: f32) -> f32 {
    let a2 = alpha * alpha;
    let denom = dot_nh * dot_nh * (a2 - 1.0) + 1.0;
    return RECIPROCAL_PI * a2 / (denom * denom);
}

/// Roughness widened by how fast the normal turns across the pixel
/// (Tokuyoshi and Kaplanyan, "Improved Geometric Specular Antialiasing",
/// 2019). A truss tube a pixel or two wide turns its normal through half a
/// circle inside the pixel; shaded at its centre with the material's own
/// lobe, it reflects the sun or a beam into single bright pixels that
/// sparkle as the camera moves. The pixel's normal variance is added to the
/// GGX α², clamped so a curved edge never becomes fully rough.
fn specular_aa(n: vec3<f32>, roughness: f32) -> f32 {
    // σ² = 0.25: a pixel footprint of half a pixel's standard deviation.
    let dx = dpdx(n);
    let dy = dpdy(n);
    let variance = 0.25 * (dot(dx, dx) + dot(dy, dy));
    // κ = 0.18: the most α² a pixel's curvature may add.
    let kernel = min(2.0 * variance, 0.18);
    let alpha2 = roughness * roughness * roughness * roughness;
    return sqrt(sqrt(saturate(alpha2 + kernel)));
}

fn brdf_ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let alpha = roughness * roughness;
    let half_vector = l + v;
    let h = half_vector * inverseSqrt(max(dot(half_vector, half_vector), 1e-8));
    let dot_nl = saturate(dot(n, l));
    let dot_nv = saturate(dot(n, v));
    let dot_nh = saturate(dot(n, h));
    let dot_vh = saturate(dot(v, h));
    return f_schlick(f0, 1.0, dot_vh) * (v_ggx_smith_correlated(alpha, dot_nl, dot_nv) * d_ggx(alpha, dot_nh));
}

/// three's `getDistanceAttenuation` (punctual.glsl.js) with decay = 2.
fn distance_attenuation(d: f32, cutoff: f32) -> f32 {
    var falloff = 1.0 / max(d * d, 0.01);
    if cutoff > 0.0 {
        let t = saturate(1.0 - pow(d / cutoff, 4.0));
        falloff *= t * t;
    }
    return falloff;
}

fn occupancy_color(count: u32) -> vec3<f32> {
    if count == 0u { return vec3<f32>(0.015, 0.02, 0.03); }
    let t = saturate(log2(f32(count) + 1.0) / 6.0);
    return mix(vec3<f32>(0.0, 0.25, 0.9), vec3<f32>(1.0, 0.12, 0.0), t);
}

fn fixture_shadow_quad(uv: vec2<f32>, layer: i32) -> vec4<f32> {
    if layer < 256 {
        return textureGather(fixture_shadow_map, fixture_depth_sampler, uv, layer);
    }
    return textureGather(fixture_shadow_map_extra, fixture_depth_sampler, uv, layer - 256);
}

// Literal tap offsets keep Metal from dynamically indexing private arrays in
// this hot filter. The caller preserves the original row-major sum order.
fn fixture_shadow_tap(base: vec2<f32>, offset: vec2<f32>, uv: vec2<f32>,
    gradient: vec2<f32>, raw_depth: f32, planes: vec2<f32>, texel: f32,
    weight: f32, depth: f32) -> f32 {
    let tap_uv = clamp((base + offset - 0.5) * texel,
        vec2<f32>(0.5 * texel), vec2<f32>(1.0 - 0.5 * texel));
    let reference = shadow_compare_reference(raw_depth + dot(gradient, tap_uv - uv), planes.x, planes.y, 0.02);
    return weight * select(0.0, 1.0, reference >= depth);
}

fn fixture_shadow_visibility(world: vec3<f32>, normal: vec3<f32>, light_index: u32) -> f32 {
    if PROFILE_SKIP_SURFACE_SHADOWS { return 1.0; }
    // The software oracle is populated only by the reference harness.
    let slot = fixture_rests[light_index].shadow_slot;
    if slot < 0.0 {
        return stage_visibility(world + normal * 0.006, fixture_cores[light_index].position);
    }
    let layer = i32(slot);
    let matrix = fixture_shadow_matrices[layer].view_proj;
    let biased = world + normal * 0.006;
    let clip = matrix * vec4<f32>(biased, 1.0);
    let ndc = clip.xyz / clip.w;
    if ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 1.0;
    }
    // Projective depth is affine across a planar receiver in shadow UV.
    // Compare each texel against that plane at its own centre: using the
    // centre pixel's depth for every tap makes a grazing floor shadow itself.
    let planes = fixture_shadow_matrices[layer].params;
    let row_x = vec3<f32>(matrix[0].x, matrix[1].x, matrix[2].x);
    let row_y = vec3<f32>(matrix[0].y, matrix[1].y, matrix[2].y);
    // Signed distance from the projection centre — the cone's virtual apex.
    let apex = fixture_cores[light_index].position
        - fixture_rests[light_index].direction * fixture_rests[light_index].lens_distance;
    let plane_distance = min(dot(normal, biased - apex), -1e-5);
    let projection_scale = planes.x * planes.y / (planes.y - planes.x);
    let gradient = projection_scale / plane_distance * vec2<f32>(
        2.0 * dot(normal, row_x) / dot(row_x, row_x),
        -2.0 * dot(normal, row_y) / dot(row_y, row_y),
    );
    let texel = surface_clusters.shadow.y;
    let at = uv / texel - 0.5;
    let base = floor(at);
    let fraction = fract(at);
    // The same 3x3 bilinear PCF footprint, expanded into its sixteen unique
    // texels so hardware filtering cannot mix comparisons at different depths.
    let weights_x = array<f32, 4>(1.0 - fraction.x, 1.0, 1.0, fraction.x);
    let weights_y = array<f32, 4>(1.0 - fraction.y, 1.0, 1.0, fraction.y);
    // Gather the same sixteen texels in four reads. Each still gets its own
    // receiver-plane comparison; gathering comparisons would mix depths.
    let top_left = fixture_shadow_quad(base * texel, layer);
    let top_right = fixture_shadow_quad((base + vec2<f32>(2.0, 0.0)) * texel, layer);
    let bottom_left = fixture_shadow_quad((base + vec2<f32>(0.0, 2.0)) * texel, layer);
    let bottom_right = fixture_shadow_quad((base + vec2<f32>(2.0)) * texel, layer);
    let depths = array<vec4<f32>, 4>(
        vec4<f32>(top_left.wz, top_right.wz),
        vec4<f32>(top_left.xy, top_right.xy),
        vec4<f32>(bottom_left.wz, bottom_right.wz),
        vec4<f32>(bottom_left.xy, bottom_right.xy),
    );
    // Explicit offsets/indexes retain all sixteen comparisons and additions.
    // A nested dynamic loop costs about 1.9 ms more in the M3 Max stress view.
    var visible = 0.0;
    visible += fixture_shadow_tap(base, vec2<f32>(0.0, 0.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[0] * weights_y[0], depths[0][0]);
    visible += fixture_shadow_tap(base, vec2<f32>(1.0, 0.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[1] * weights_y[0], depths[0][1]);
    visible += fixture_shadow_tap(base, vec2<f32>(2.0, 0.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[2] * weights_y[0], depths[0][2]);
    visible += fixture_shadow_tap(base, vec2<f32>(3.0, 0.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[3] * weights_y[0], depths[0][3]);
    visible += fixture_shadow_tap(base, vec2<f32>(0.0, 1.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[0] * weights_y[1], depths[1][0]);
    visible += fixture_shadow_tap(base, vec2<f32>(1.0, 1.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[1] * weights_y[1], depths[1][1]);
    visible += fixture_shadow_tap(base, vec2<f32>(2.0, 1.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[2] * weights_y[1], depths[1][2]);
    visible += fixture_shadow_tap(base, vec2<f32>(3.0, 1.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[3] * weights_y[1], depths[1][3]);
    visible += fixture_shadow_tap(base, vec2<f32>(0.0, 2.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[0] * weights_y[2], depths[2][0]);
    visible += fixture_shadow_tap(base, vec2<f32>(1.0, 2.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[1] * weights_y[2], depths[2][1]);
    visible += fixture_shadow_tap(base, vec2<f32>(2.0, 2.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[2] * weights_y[2], depths[2][2]);
    visible += fixture_shadow_tap(base, vec2<f32>(3.0, 2.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[3] * weights_y[2], depths[2][3]);
    visible += fixture_shadow_tap(base, vec2<f32>(0.0, 3.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[0] * weights_y[3], depths[3][0]);
    visible += fixture_shadow_tap(base, vec2<f32>(1.0, 3.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[1] * weights_y[3], depths[3][1]);
    visible += fixture_shadow_tap(base, vec2<f32>(2.0, 3.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[2] * weights_y[3], depths[3][2]);
    visible += fixture_shadow_tap(base, vec2<f32>(3.0, 3.0), uv, gradient,
        ndc.z, planes.xy, texel, weights_x[3] * weights_y[3], depths[3][3]);
    return visible / 9.0;
}

fn environment_direction(world_direction: vec3<f32>) -> vec3<f32> {
    let c = cos(environment_params.rotation);
    let s = sin(environment_params.rotation);
    let rotated = vec3<f32>(
        c * world_direction.x - s * world_direction.y,
        s * world_direction.x + c * world_direction.y,
        world_direction.z,
    );
    return vec3<f32>(rotated.x, rotated.z, -rotated.y);
}

/// Deterministic 3x3 PCF. The authored tap radius is measured in shadow-map
/// texels, so zero collapses all taps to one hard comparison while larger
/// values widen the penumbra without changing cascade projection or stability.
fn cascade_shadow(world: vec3<f32>, n: vec3<f32>, cascade: u32, ground: bool) -> f32 {
    // Normal offset, per surface. One shadow texel stores one depth for a
    // patch of receiver; a surface tilted `theta` away from the light drifts
    // `tan(theta)` texels of depth across it. Pushing the sample `d` along the
    // normal moves it `d / cos(theta)` toward the light, so it clears its own
    // texel and every PCF tap `r` texels away once
    // `d >= (r + 1.5) * texel * sin(theta)`: the half texel of the stored
    // centre, one more for the comparison sampler's bilinear footprint.
    //
    // The offset follows each surface's own N.L, not the sun's elevation. A
    // wall under a high sun is exactly as grazing as a floor under a low one,
    // and gating on elevation left every lit wall at noon striped with acne.
    // A receiver facing the light (floor under a high sun) gets almost no
    // offset, which is what keeps contact shadows attached.
    //
    // The ground gets none. It is not in the map, so it has no texels of its
    // own to clear, and under a low sun the offset was the error: lifting a
    // ground point `d` moves where it reads the map by `d / tan(elevation)`
    // along the ground, 14 d at 4 degrees. The offset scales with the
    // cascade's texel, so every shadow on the ground shortened and changed
    // shape as the camera moved and a different cascade covered it.
    let matrix = globals.light_view_proj[cascade];
    let texel = globals.params.y;
    let radius = clamp(globals.dir_color.w, 0.0, 3.0);
    // World size of one shadow texel in this cascade. The light camera is
    // orthographic, so the length of the matrix's x row is two over the slice's
    // width, and a texel's share of that is what the offset has to clear.
    let world_texel =
        2.0 * texel / max(length(vec3<f32>(matrix[0].x, matrix[1].x, matrix[2].x)), 1e-6);
    let cos_nl = clamp(dot(n, globals.dir_to_light.xyz), 0.0, 1.0);
    let sin_nl = sqrt(1.0 - cos_nl * cos_nl);
    let offset = select(0.002 + (radius + 1.5) * world_texel * sin_nl, 0.0, ground);
    let biased = world + n * offset;
    let clip = globals.light_view_proj[cascade] * vec4<f32>(biased, 1.0);
    let ndc = clip.xyz / clip.w;
    // Depth bias in metres along the light, not in NDC. The cascade's depth
    // range runs from the furthest caster toward the sun to 25 m past its
    // view slice, so a fixed NDC bias was 10 cm of light leak or more: the
    // sun lit a beam's side deep under a deck plate, with a stepped edge
    // where the leak ran out, and nothing on a table cast a shadow under
    // itself. The normal offset above already
    // clears the receiver's own texels; this only covers depth rounding.
    let depth_per_metre = length(vec3<f32>(matrix[0].z, matrix[1].z, matrix[2].z));
    let depth_bias = (0.002 + 0.5 * world_texel) * depth_per_metre;
    if ndc.z > 1.0 || ndc.z < 0.0 {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 1.0;
    }
    var sum = 0.0;
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            let offset = vec2<f32>(f32(i), f32(j)) * radius * texel;
            sum += textureSampleCompareLevel(
                shadow_map,
                shadow_sampler,
                uv + offset,
                i32(cascade),
                ndc.z + depth_bias,
            );
        }
    }
    return sum / 9.0;
}

/// How much of the room's own light reaches this point.
///
/// Bound the indoor ambient bounce approximation to the room footprint and
/// a short edge fade. Direct fixture cones have their own angular/range falloff.
/// A zero margin leaves outdoor and standalone-object lighting unbounded.
fn room_glow(world: vec3<f32>) -> f32 {
    let margin = globals.room_falloff.x;
    if margin <= 0.0 {
        return 1.0;
    }
    let outside = max(abs(world.xy - globals.room.xy) - globals.room.zw, vec2<f32>(0.0));
    return 1.0 - smoothstep(0.0, margin, length(outside));
}

fn shadow_factor(world: vec3<f32>, n: vec3<f32>, ground: bool) -> f32 {
    if globals.params.z < 0.5 {
        return 1.0;
    }
    let view_depth = dot(world - globals.camera_pos.xyz, globals.camera_forward.xyz);
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
    let current = cascade_shadow(world, n, cascade, ground);
    if cascade == 2u {
        return current;
    }
    let far = globals.cascade_splits[cascade];
    var near = 0.1;
    if cascade > 0u {
        near = globals.cascade_splits[cascade - 1u];
    }
    let blend_width = (far - near) * globals.cascade_splits.w;
    let blend = smoothstep(far - blend_width, far, view_depth);
    return mix(current, cascade_shadow(world, n, cascade + 1u, ground), blend);
}

struct OccludedSky {
    irradiance: vec3<f32>,
    // Visibility of the lit ground this point's lower hemisphere sees.
    ground: vec3<f32>,
};

/// The sky probe's irradiance for normal `n` with the stage in the way.
///
/// The probe is an open sky over open, sunlit ground. Split it into the two:
/// the ground below radiates what a downward normal receives, `E(-Z)`, and a
/// normal sees `(1 - n.z) / 2` of it; the rest is sky. The sky part is
/// scaled by the height field's sky visibility. The ground part is scaled by
/// how lit the ground under this point really is: its sun visibility for the
/// sun's share of the ground's light, its sky visibility for the rest.
fn occluded_sky(n: vec3<f32>, open: vec3<f32>, visibility: vec4<f32>) -> OccludedSky {
    let below = textureSampleLevel(environment_irradiance, environment_sampler,
        environment_direction(vec3<f32>(0.0, 0.0, -1.0)), 0.0).rgb;
    let above = textureSampleLevel(environment_irradiance, environment_sampler,
        environment_direction(vec3<f32>(0.0, 0.0, 1.0)), 0.0).rgb;
    let ground = below * (0.5 - 0.5 * n.z);
    let sky = max(open - ground, vec3<f32>(0.0));
    var sun = vec3<f32>(0.0);
    if globals.dir_to_light.w > 0.5 {
        sun = globals.dir_color.rgb * max(globals.dir_to_light.z, 0.0);
    }
    let sun_share = sun / max(sun + above * environment_params.intensity, vec3<f32>(1e-6));
    let lit = mix(vec3<f32>(visibility.a), vec3<f32>(visibility.b), sun_share);
    return OccludedSky(sky * visibility.g + ground * lit, lit);
}

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let inst = instances[in.instance];
    let v = normalize(globals.camera_pos.xyz - in.world);

    var n = normalize(in.normal);
    if !front {
        n = -n;
    }
    var t = normalize(in.tangent.xyz - n * dot(n, in.tangent.xyz));
    if !front {
        t = -t;
    }
    let b = cross(n, t) * in.tangent.w;
    var mapped = textureSample(normal_map, material_sampler, in.uv).xyz * 2.0 - 1.0;
    mapped = vec3<f32>(mapped.xy * inst.flags.y, mapped.z);
    n = normalize(t * mapped.x + b * mapped.y + n * mapped.z);

    let base_color = inst.base_color.rgb
        * textureSample(base_color_map, material_sampler, in.uv).rgb;
    let mr = textureSample(metallic_roughness_map, material_sampler, in.uv);
    let metallic = inst.base_color.a * mr.b;
    // three clamps roughness to 0.0525 before squaring.
    let roughness = specular_aa(n, max(inst.emissive.a * mr.g, 0.0525));
    let ao_sample = textureSample(occlusion_map, material_sampler, in.uv).r;
    let ao = mix(1.0, ao_sample, inst.flags.z);
    let diffuse_color = base_color * (1.0 - metallic);
    let f0 = mix(vec3<f32>(0.04), base_color, metallic);
    let shadow = shadow_factor(in.world, n, inst.flags.x > 0.5);
    // View depth for the light index's Z-bin lookup — the same forward-axis
    // distance the index binned the lights with.
    let view_depth = dot(in.world - globals.camera_pos.xyz, globals.camera_forward.xyz);

    if surface_clusters.flags.y > 0.5 {
        var probe = lights_at(in.clip.xy, view_depth);
        var probe_id = 0u;
        var count = 0u;
        while light_index_next(&probe, &probe_id) {
            count += 1u;
        }
        return vec4<f32>(occupancy_color(count), 1.0);
    }

    let debug = u32(globals.params.w + 0.5);
    if debug == 1u {
        return vec4<f32>(base_color, 1.0);
    }
    if debug == 2u {
        return vec4<f32>(n * 0.5 + 0.5, 1.0);
    }
    if debug == 3u {
        return vec4<f32>(vec3<f32>(metallic), 1.0);
    }
    if debug == 4u {
        return vec4<f32>(vec3<f32>(roughness), 1.0);
    }
    if debug == 5u {
        return vec4<f32>(vec3<f32>(shadow), 1.0);
    }

    var out = inst.emissive.rgb * textureSample(emissive_map, material_sampler, in.uv).rgb;
    // The house's reach. Applied to the fill and the key and to nothing else:
    // every fixture cone already falls off with distance, and an emissive
    // surface is its own source.
    let glow = room_glow(in.world);
    // Ambient visibility of this pixel (`ambient_occlusion.wgsl`). It dims
    // the fill and the probe only; direct light has its own shadows.
    let visibility = textureLoad(ambient_visibility, vec2<i32>(in.clip.xy), 0);
    let occlusion = ao * visibility.r;
    out += globals.ambient.rgb * glow * diffuse_color * RECIPROCAL_PI * occlusion;
    if environment_params.enabled > 0.5 && environment_params.intensity > 0.0 {
        let dot_nv = saturate(dot(n, v));
        let fresnel = f_schlick(f0, 1.0, dot_nv);
        let reflected = reflect(-v, n);
        var irradiance = textureSampleLevel(
            environment_irradiance,
            environment_sampler,
            environment_direction(n),
            0.0,
        ).rgb;
        // Lagarde and de Rousiers 2014: occlusion of a specular lobe from the
        // diffuse visibility, tighter for smooth surfaces.
        let lobe = saturate(pow(dot_nv + visibility.r, exp2(-16.0 * roughness - 1.0)) - 1.0 + visibility.r);
        var specular_visibility = vec3<f32>(lobe);
        if globals.room_falloff.y > 0.5 {
            let sky = occluded_sky(n, irradiance, visibility);
            irradiance = sky.irradiance;
            specular_visibility *= mix(sky.ground, vec3<f32>(visibility.g), smoothstep(-0.2, 0.2, reflected.z));
        }
        let diffuse_ibl = irradiance * diffuse_color * visibility.r;
        let prefiltered = textureSampleLevel(
            environment_specular,
            environment_sampler,
            environment_direction(reflected),
            roughness * 7.0,
        ).rgb;
        let brdf = textureSampleLevel(
            environment_brdf,
            environment_sampler,
            vec2<f32>(dot_nv, roughness),
            0.0,
        ).rg;
        let specular_ibl = prefiltered * (f0 * brdf.x + brdf.y) * specular_visibility;
        out += (diffuse_ibl * (vec3<f32>(1.0) - fresnel) + specular_ibl)
            * environment_params.intensity * ao;
    }

    if globals.dir_to_light.w > 0.5 {
        let l = globals.dir_to_light.xyz;
        let dot_nl = saturate(dot(n, l));
        if dot_nl > 0.0 {
            let irradiance = dot_nl * globals.dir_color.rgb * shadow * glow;
            out += irradiance * diffuse_color * RECIPROCAL_PI;
            out += irradiance * brdf_ggx(n, v, l, f0, roughness);
        }
    }

    if surface_clusters.flags.x > 0.5 && !PROFILE_SKIP_FIXTURES {
        var cursor = lights_at(in.clip.xy, view_depth);
        if surface_clusters.flags.z > 0.5 {
            // Plane 2 is the tile's surface bucket A; plane 3 bucket B beyond
            // the tile's split depth. Within the R16 margin of the split the
            // fragment's stored sample may sit in either bucket, so it keeps
            // the full-range plane 0, a superset of both.
            var plane = 2u;
            if surface_clusters.flags.w > 0.5 {
                let split = surface_splits[cursor.base / LIGHT_INDEX_WORDS];
                let margin = abs(view_depth) * 0.001 + 0.001;
                if view_depth > split + margin {
                    plane = 3u;
                } else if view_depth >= split - margin {
                    plane = 0u;
                }
            }
            if plane != 0u {
                cursor.base += plane * light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
                cursor.bits = light_index_masks[cursor.base + cursor.word];
            }
        }
        var light_index = 0u;
        while light_index_next(&cursor, &light_index) {
            let core = fixture_cores[light_index];
            let rest = fixture_rests[light_index];
            let q = in.world - core.position;
            let distance = length(q);
            if distance <= 1e-4 || distance >= core.range || rest.intensity <= 0.0 {
                continue;
            }
            let from_light = q / distance;
            // Seen from the virtual apex behind the lens, so the lit pool is
            // the beam's own footprint; falloff below stays from the lens.
            var cos_angle = dot(from_light, rest.direction);
            if rest.lens_distance > 0.0 {
                cos_angle = lens_cos_angle(q, rest.direction, rest.lens_distance, distance);
            }
            let angular = angular_profile(cos_angle, rest.cos_beam, rest.cos_field);
            if angular <= 0.0 {
                continue;
            }
            let aperture = gobo_transmission(
                from_lens_apex(q, rest.direction, rest.lens_distance),
                rest.direction,
                rest.cos_field,
                rest.gobo,
                rest.gobo_rotation,
            );
            let l = -from_light;
            let dot_nl = saturate(dot(n, l));
            if dot_nl <= 0.0 || aperture <= 0.0 {
                continue;
            }
            var attenuation = distance_attenuation(distance, core.range);
            if rest.lens_distance > 0.0 {
                // Inverse square from the virtual apex; the taper stays on the lens.
                attenuation *= max(distance * distance, 0.01)
                    / max(lens_apex_distance2(q, rest.direction, rest.lens_distance), 0.01);
            }
            let profile = angular * aperture * attenuation;
            let visibility = fixture_shadow_visibility(in.world, n, light_index);
            // `rest.intensity` is a 0..1 dimmer times the optic's gain, not
            // radiance; the beam gain is the absolute scale, and it is the
            // same one the haze march applies to the same cone.
            let beam_gain = surface_clusters.shadow.z;
            let irradiance =
                dot_nl * rest.color * rest.intensity * beam_gain * profile * visibility;
            out += irradiance * diffuse_color * RECIPROCAL_PI;
            out += irradiance * brdf_ggx(n, v, l, f0, roughness);
        }
    }

    // This opaque pipeline replaces the target, so write premultiplied
    // radiance directly. Depth still occludes geometry behind the surface.
    let coverage = horizon_coverage(view_depth);
    return vec4<f32>(surface_radiance(out, in.world - globals.camera_pos.xyz, in.clip.xy) * coverage, coverage);
}

// Store the shading invocation's centre depth separately in every covered
// MSAA sample, so subpixel geometry participates in surface light bounds.
@fragment
fn fs_surface_depth(in: VsOut) -> @location(0) f32 {
    let depth = dot(in.world - globals.camera_pos.xyz, globals.camera_forward.xyz);
    return select(depth, -1.0, depth <= 0.0);
}
