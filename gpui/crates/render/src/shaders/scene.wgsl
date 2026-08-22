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

/// 3x3 PCF, matching the kernel three's `PCFSoftShadowMap` settles on closely
/// enough that only the outermost shadow pixel differs.
fn cascade_shadow(world: vec3<f32>, n: vec3<f32>, cascade: u32) -> f32 {
    // `shadow-normalBias={0.01}`: push the sample along the surface normal so
    // near-grazing faces do not shadow themselves.
    let biased = world + n * 0.01;
    let clip = globals.light_view_proj[cascade] * vec4<f32>(biased, 1.0);
    let ndc = clip.xyz / clip.w;
    if ndc.z > 1.0 || ndc.z < 0.0 {
        return 1.0;
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 1.0;
    }
    let texel = globals.params.y;
    var sum = 0.0;
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            let offset = vec2<f32>(f32(i), f32(j)) * texel;
            sum += textureSampleCompareLevel(
                shadow_map,
                shadow_sampler,
                uv + offset,
                i32(cascade),
                ndc.z + 0.0015,
            );
        }
    }
    return sum / 9.0;
}

fn shadow_factor(world: vec3<f32>, n: vec3<f32>) -> f32 {
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
    let current = cascade_shadow(world, n, cascade);
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
    return mix(current, cascade_shadow(world, n, cascade + 1u), blend);
}

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let inst = instances[in.instance];
    let v = normalize(globals.camera_pos.xyz - in.world);

    // three's `normal_fragment_begin`: a flat-shaded material takes the
    // geometric normal from screen-space derivatives and never consults
    // `gl_FrontFacing`, so the result always faces the viewer.
    var n: vec3<f32>;
    if inst.flags.x > 0.5 {
        n = normalize(cross(dpdx(in.world), dpdy(in.world)));
        if dot(n, v) < 0.0 {
            n = -n;
        }
    } else {
        n = normalize(in.normal);
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
    }

    let base_color = inst.base_color.rgb
        * textureSample(base_color_map, material_sampler, in.uv).rgb;
    let mr = textureSample(metallic_roughness_map, material_sampler, in.uv);
    let metallic = inst.base_color.a * mr.b;
    // three clamps roughness to 0.0525 before squaring.
    let roughness = max(inst.emissive.a * mr.g, 0.0525);
    let ao_sample = textureSample(occlusion_map, material_sampler, in.uv).r;
    let ao = mix(1.0, ao_sample, inst.flags.z);
    let diffuse_color = base_color * (1.0 - metallic);
    let f0 = mix(vec3<f32>(0.04), base_color, metallic);
    let shadow = shadow_factor(in.world, n);

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
    out += globals.ambient.rgb * diffuse_color * RECIPROCAL_PI * ao;
    if environment_params.enabled > 0.5 && environment_params.intensity > 0.0 {
        let dot_nv = saturate(dot(n, v));
        let fresnel = f_schlick(f0, 1.0, dot_nv);
        let diffuse_ibl = textureSampleLevel(
            environment_irradiance,
            environment_sampler,
            environment_direction(n),
            0.0,
        ).rgb * diffuse_color;
        let reflected = reflect(-v, n);
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
        let specular_ibl = prefiltered * (f0 * brdf.x + brdf.y);
        out += (diffuse_ibl * (vec3<f32>(1.0) - fresnel) + specular_ibl)
            * environment_params.intensity * ao;
    }

    if globals.dir_to_light.w > 0.5 {
        let l = globals.dir_to_light.xyz;
        let dot_nl = saturate(dot(n, l));
        if dot_nl > 0.0 {
            let irradiance = dot_nl * globals.dir_color.rgb * shadow;
            out += irradiance * diffuse_color * RECIPROCAL_PI;
            out += irradiance * brdf_ggx(n, v, l, f0, roughness);
        }
    }

    let count = u32(globals.params.x);
    for (var i = 0u; i < count; i = i + 1u) {
        let light = point_lights[i];
        let delta = light.position.xyz - in.world;
        let d = length(delta);
        if d > light.position.w {
            continue;
        }
        let l = delta / max(d, 1e-4);
        let dot_nl = saturate(dot(n, l));
        if dot_nl <= 0.0 {
            continue;
        }
        let irradiance = dot_nl * light.color.rgb * distance_attenuation(d, light.position.w);
        out += irradiance * diffuse_color * RECIPROCAL_PI;
        out += irradiance * brdf_ggx(n, v, l, f0, roughness);
    }

    return vec4<f32>(out, 1.0);
}
