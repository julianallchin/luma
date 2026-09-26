// Probe relight: every texel of every probe, lit by this frame's light.
//
// Appended to the scene module, so a probe texel is lit by the same
// functions as the surface it stands for: the sun through the stage's
// cascades and the cloud layer's shadow, the sky probe, the house fill, and
// each fixture cone with its gobo and its shadow map. A fixture that turns
// red lights the probes red in the frame it turns. Beams and haze are left
// out: the probes hold surfaces, and the air is drawn over them.
//
// A texel stores what the stage changes of the sky probe along it, which
// the scene pass adds to the sky probe's light (`probe_sample.wgsl`):
//
// - open sky: nothing;
// - the stage: its light less the sky probe's radiance it hides, which is
//   less than nothing where a dark deck hides a bright sky;
// - the ground under a sky: the sun the stage's shadow takes off it, and
//   the fixtures' pools on it. The sky probe's lower half is that ground
//   open and lit, and relit here it came out warmer and brighter: a lit
//   disk round the stage wherever the probes held the ground themselves;
// - the ground indoors, which no sky probe holds: its light.
//
// Each workgroup is 8 by 8 texels of one face of one probe. It first lists,
// in workgroup memory, the fixtures whose cone can reach within
// `box_max.w` of its probe, so a texel walks tens of cones, not the rig.
// Culling in the workgroup, not in a pass of its own, saves a dispatch and
// the wait between the two, which on Low was most of the relight's cost.

const PROBE_LIGHTS: u32 = 127u;
/// Light from `delta` away reaching the probe through the air: the aerial
/// perspective and the procedural haze, as `scene_radiance` applies them
/// from the camera. A tube at eye height reflects the ground at the horizon
/// through as much air as the camera sees it through; lit and bare, that
/// ground was a bright tan band across every tower at night.
fn probe_air(color: vec3<f32>, probe: vec3<f32>, delta: vec3<f32>) -> vec3<f32> {
    let aerial = aerial_radiance(color, delta);
    if globals.medium.max.w <= 0.0 || globals.medium.min.w <= 0.0 {
        return aerial;
    }
    let distance = length(delta);
    let direction = delta / max(distance, 1e-6);
    // The height profile's mean density, not the 32-step march: a probe
    // texel is metres across and cannot hold the haze's detail.
    let span = medium_span(globals.medium, probe, direction, distance);
    var depth = 0.0;
    if span.y > span.x {
        depth = medium_height_depth(globals.medium, probe, direction, span);
    }
    let transmission = exp(-depth);
    return aerial * transmission
        + outdoor_haze_light(direction, aerial_sky.sun.xyz, globals.outdoor_sun) * (1.0 - transmission);
}

// The capture atlas (`probe_capture.wgsl`): one PROBE_SIZE cell per probe
// face, PROBE_ATLAS_COLUMNS to a row; Low fills each cell's top-left
// quarter.
@group(1) @binding(16) var probe_albedo: texture_2d<f32>;
@group(1) @binding(18) var probe_normal: texture_2d<f32>;
@group(1) @binding(19) var probe_radiance_out: texture_storage_2d_array<rgba16float, write>;
// x: fixture cones in `fixture_cores`. y: the first cube-face layer this
// frame's relight and prefilter write: on Low they take half the probes a
// frame. The relight's group 3 binds every cone
// of the rig in source order, not the camera's light index's in-view subset,
// so a pool behind the camera still shows in a probe.
@group(1) @binding(20) var<uniform> probe_light_total: vec4<u32>;

var<workgroup> probe_light_count: atomic<u32>;
var<workgroup> probe_light_list: array<u32, PROBE_LIGHTS>;

/// Whether a cone from `apex` along `axis`, `cos_field` wide and `range`
/// long, reaches the ball of `radius` round `centre` (Wronski's cone test).
fn probe_cone_reaches(
    apex: vec3<f32>,
    axis: vec3<f32>,
    cos_field: f32,
    range: f32,
    centre: vec3<f32>,
    radius: f32,
) -> bool {
    let v = centre - apex;
    let length_sq = dot(v, v);
    let along = dot(v, axis);
    if along > range + radius || along < -radius {
        return false;
    }
    if cos_field <= 0.0 {
        return length_sq < (range + radius) * (range + radius);
    }
    let sin_field = sqrt(max(1.0 - cos_field * cos_field, 0.0));
    let closest = cos_field * sqrt(max(length_sq - along * along, 0.0)) - along * sin_field;
    return closest <= radius;
}

/// List, in workgroup memory, the cones that can reach `probe`. Every
/// invocation of the workgroup calls it: it holds two barriers.
fn probe_cull(probe: u32, lane: u32) -> u32 {
    if lane == 0u {
        atomicStore(&probe_light_count, 0u);
    }
    workgroupBarrier();
    if surface_clusters.flags.x > 0.5 {
        let centre = probe_grid.positions[probe].xyz;
        let reach = probe_grid.box_max.w;
        let count = probe_light_total.x;
        for (var i = lane; i < count; i += 64u) {
            let core = fixture_cores[i];
            let rest = fixture_rests[i];
            if rest.intensity <= 0.0 || core.range <= 0.0 {
                continue;
            }
            let apex = core.position - rest.direction * rest.lens_distance;
            if !probe_cone_reaches(apex, rest.direction, rest.cos_field, core.range + rest.lens_distance, centre, reach) {
                continue;
            }
            let slot = atomicAdd(&probe_light_count, 1u);
            if slot < PROBE_LIGHTS {
                probe_light_list[slot] = i;
            }
        }
    }
    workgroupBarrier();
    return min(atomicLoad(&probe_light_count), PROBE_LIGHTS);
}

/// The sky probe along `dir`, or nothing where there is none.
fn probe_sky(dir: vec3<f32>) -> vec3<f32> {
    if environment_params.enabled < 0.5 || environment_params.intensity <= 0.0 {
        return vec3<f32>(0.0);
    }
    return textureSampleLevel(environment_specular, environment_sampler, environment_direction(dir), 0.0).rgb
        * environment_params.intensity;
}

/// The sun on a captured surface point, before the stage's shadow: rgb.
/// The stage's shadow there: a. Nothing when there is no sun.
fn probe_sun_light(
    world: vec3<f32>,
    v: vec3<f32>,
    n: vec3<f32>,
    diffuse_color: vec3<f32>,
    f0: vec3<f32>,
    roughness: f32,
) -> vec4<f32> {
    if globals.dir_to_light.w < 0.5 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let l = globals.dir_to_light.xyz;
    let dot_nl = saturate(dot(n, l));
    if dot_nl <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    var cloud = 1.0;
    if aerial_sky.shadow.z > 0.0 {
        cloud = cloud_shadow_at(aerial_cloud_shadow, aerial_sampler, aerial_sky.shadow, aerial_sky.sun.xyz, world);
    }
    let irradiance = dot_nl * globals.dir_color.rgb * cloud * room_glow(world);
    return vec4<f32>(
        irradiance * (diffuse_color * RECIPROCAL_PI + brdf_ggx(n, v, l, f0, roughness)),
        shadow_factor(world, n, false),
    );
}

/// The listed fixtures' light on a captured surface point.
fn probe_fixture_light(
    world: vec3<f32>,
    v: vec3<f32>,
    n: vec3<f32>,
    diffuse_color: vec3<f32>,
    f0: vec3<f32>,
    roughness: f32,
    lights: u32,
) -> vec3<f32> {
    var out = vec3<f32>(0.0);
    if surface_clusters.flags.x < 0.5 {
        return out;
    }
    let beam_gain = surface_clusters.shadow.z;
    for (var k = 0u; k < lights; k++) {
        let light_index = probe_light_list[k];
        let core = fixture_cores[light_index];
        let rest = fixture_rests[light_index];
        let q = world - core.position;
        let distance = length(q);
        if distance <= 1e-4 || distance >= core.range {
            continue;
        }
        let from_light = q / distance;
        var cos_angle = dot(from_light, rest.direction);
        if rest.lens_distance > 0.0 {
            cos_angle = lens_cos_angle(q, rest.direction, rest.lens_distance, distance);
        }
        let angular = angular_profile(cos_angle, rest.cos_beam, rest.cos_field);
        if angular <= 0.0 {
            continue;
        }
        let l = -from_light;
        let dot_nl = saturate(dot(n, l));
        if dot_nl <= 0.0 {
            continue;
        }
        let aperture = gobo_transmission(
            from_lens_apex(q, rest.direction, rest.lens_distance),
            rest.direction,
            rest.cos_field,
            rest.gobo,
            rest.gobo_rotation,
        );
        if aperture <= 0.0 {
            continue;
        }
        var attenuation = distance_attenuation(distance, core.range);
        if rest.lens_distance > 0.0 {
            attenuation *= max(distance * distance, 0.01)
                / max(lens_apex_distance2(q, rest.direction, rest.lens_distance), 0.01);
        }
        // Low's probes are 32 texels a face: a fixture's shadow is finer
        // than that, and its nine-tap filter is most of a light's cost.
        var visibility = 1.0;
        if probe_grid.step.w < 0.5 {
            visibility = fixture_shadow_visibility(world, n, light_index);
        }
        let irradiance = dot_nl * rest.color * rest.intensity * beam_gain
            * angular * aperture * attenuation * visibility;
        out += irradiance * (diffuse_color * RECIPROCAL_PI + brdf_ggx(n, v, l, f0, roughness));
    }
    return out;
}

/// One captured surface point, lit as `shade` lights it, without the air.
fn probe_surface_light(
    world: vec3<f32>,
    v: vec3<f32>,
    n: vec3<f32>,
    albedo: vec3<f32>,
    roughness: f32,
    metallic: f32,
    lights: u32,
) -> vec3<f32> {
    let diffuse_color = albedo * (1.0 - metallic);
    let f0 = mix(vec3<f32>(0.04), albedo, metallic);
    var out = globals.ambient.rgb * room_glow(world) * diffuse_color * RECIPROCAL_PI;
    if environment_params.enabled > 0.5 && environment_params.intensity > 0.0 {
        let dot_nv = saturate(dot(n, v));
        let irradiance = textureSampleLevel(
            environment_irradiance,
            environment_sampler,
            environment_direction(n),
            0.0,
        ).rgb;
        let brdf = textureSampleLevel(environment_brdf, environment_sampler, vec2<f32>(dot_nv, roughness), 0.0).rg;
        let prefiltered = textureSampleLevel(
            environment_specular,
            environment_sampler,
            environment_direction(reflect(-v, n)),
            roughness * 7.0,
        ).rgb;
        let fresnel = f_schlick(f0, 1.0, dot_nv);
        out += (irradiance * diffuse_color * (vec3<f32>(1.0) - fresnel) + prefiltered * (f0 * brdf.x + brdf.y))
            * environment_params.intensity;
    }
    let sun = probe_sun_light(world, v, n, diffuse_color, f0, roughness);
    return out + sun.rgb * sun.a + probe_fixture_light(world, v, n, diffuse_color, f0, roughness, lights);
}

/// What the stage changes of the ground at `world` under a sky, whose open,
/// sunlit look the sky probe holds: the sun its shadow takes off, and the
/// fixtures' pools. Zero on open ground in daylight, to the bit.
fn probe_ground_change(world: vec3<f32>, v: vec3<f32>, lights: u32) -> vec3<f32> {
    let n = vec3<f32>(0.0, 0.0, 1.0);
    let albedo = probe_grid.ground.rgb;
    let f0 = vec3<f32>(0.04);
    let sun = probe_sun_light(world, v, n, albedo, f0, 0.9);
    return sun.rgb * (sun.a - 1.0) + probe_fixture_light(world, v, n, albedo, f0, 0.9, lights);
}

@compute @workgroup_size(8, 8, 1)
fn probe_relight(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let base = u32(probe_grid.step.w + 0.5);
    let size = PROBE_SIZE >> base;
    // From the workgroup, so the whole group agrees on it: the cull below
    // holds barriers.
    let layer = group.z + probe_light_total.y;
    let probe = layer / 6u;
    let face = layer % 6u;
    if probe >= probe_count(probe_grid) {
        return;
    }
    let lights = probe_cull(probe, lane);
    if id.x >= size || id.y >= size {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / f32(size);
    let dir = normalize(probe_world_direction(probe_face_direction(face, uv)));
    let texel = vec2<i32>(id.xy);
    let cell = vec2<i32>(i32(layer % PROBE_ATLAS_COLUMNS), i32(layer / PROBE_ATLAS_COLUMNS)) * i32(PROBE_SIZE);
    let surface = textureLoad(probe_albedo, cell + texel, 0);
    let geometry = textureLoad(probe_normal, cell + texel, 0);
    let at = probe_grid.positions[probe].xyz;
    // Open sky, and past the floor mesh's edge open ground: no change.
    // Alpha holds how far the probe sees along this texel, for the scene
    // pass's visibility test (`probe_sample.wgsl`); the sky is far.
    var change = vec3<f32>(0.0);
    var distance = PROBE_SKY_DISTANCE;
    // Under a sky, the sky probe holds the ground (`ProbeGrid::ground`).
    let open_ground = probe_grid.ground.w > 1.5;
    if geometry.w > 0.0 {
        distance = geometry.w;
        let world = at + dir * geometry.w;
        if geometry.z < 0.0 && open_ground {
            change = probe_ground_change(world, -dir, lights);
        } else {
            var lit = probe_surface_light(
                world,
                -dir,
                probe_unoctahedral(geometry.xy),
                surface.rgb,
                max(surface.a, 0.05),
                max(geometry.z, 0.0),
                lights,
            );
            // The air between matters past a few tens of metres; nearer, it
            // is a per-texel cost for nothing.
            if geometry.w > 20.0 {
                lit = probe_air(lit, at, dir * geometry.w);
            }
            // The sky probe is the sky as the camera sees it, air included:
            // what this surface hides.
            change = lit - probe_sky(dir);
        }
    } else if dir.z < -1e-4 && probe_grid.ground.w > 0.5 {
        // Below the horizon past the floor mesh's edge: the ground plane, as
        // the main pass draws it out to the horizon (`ground_fragment`).
        distance = min(at.z / -dir.z, PROBE_SKY_DISTANCE);
        let world = at + dir * distance;
        if open_ground {
            // Without the air: what the stage changes this far out is its
            // long shadow at a low sun and little else.
            change = probe_ground_change(world, -dir, lights);
        } else {
            // Indoors no sky probe holds the ground, and its below-horizon
            // glow is not the floor.
            let lit = probe_surface_light(
                world,
                -dir,
                vec3<f32>(0.0, 0.0, 1.0),
                probe_grid.ground.rgb,
                0.9,
                0.0,
                lights,
            );
            change = probe_air(lit, at, dir * distance) - probe_sky(dir);
        }
    }
    textureStore(probe_radiance_out, texel, i32(layer), vec4<f32>(change, distance));
}
