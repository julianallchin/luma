// Bounced light, first step of a frame: the light landing on every disc
// (`bounce/bake.rs`) from this frame's sun, sky and fixtures, and what the
// disc sends back out of it.
//
// Appended to the scene module, so a disc is lit by the same functions as
// the surface it stands for: the sun through the stage's cascades and the
// cloud layer's shadow, the sky probe, and each fixture cone with its gobo
// and its shadow map. A fixture that turns red sends red off the floor in
// the frame it turns. Beams and haze are left out: discs are surfaces.
//
// A disc sends out its matte colour times the light landing on it: the
// direct light found here, and the bounced light the last frame gathered
// for it (`bounce_spread.wgsl`), so each frame carries the light one bounce
// further.
//
// Each workgroup is 64 discs that lie together. It first lists, in
// workgroup memory, the fixtures whose cone can reach the ball round them,
// so a disc walks tens of cones, not the rig.

const BOUNCE_LIGHTS: u32 = 127u;

// The ball round each workgroup's discs: xyz its centre, w its radius.
@group(1) @binding(17) var<storage, read> bounce_balls: array<vec4<f32>>;
@group(1) @binding(19) var<storage, read_write> bounce_radiosity: array<vec4<f32>>;
// x: fixture cones in `fixture_cores`. y: discs. z: 1 to send out the direct
// light alone, which starts a still's bounces from nothing. The relight's
// group 3 binds every cone of the rig in source order, not the camera's
// light index's in-view subset, so a pool behind the camera still bounces.
@group(1) @binding(20) var<uniform> bounce_total: vec4<u32>;

var<workgroup> bounce_light_count: atomic<u32>;
var<workgroup> bounce_light_list: array<u32, BOUNCE_LIGHTS>;

/// Whether a cone from `apex` along `axis`, `cos_field` wide and `range`
/// long, reaches the ball of `radius` round `centre` (Wronski's cone test).
fn bounce_cone_reaches(
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

/// List, in workgroup memory, the cones that can reach `ball`. Every
/// invocation of the workgroup calls it: it holds two barriers.
fn bounce_cull(ball: vec4<f32>, lane: u32) -> u32 {
    if lane == 0u {
        atomicStore(&bounce_light_count, 0u);
    }
    workgroupBarrier();
    if surface_clusters.flags.x > 0.5 {
        let count = bounce_total.x;
        for (var i = lane; i < count; i += 64u) {
            let core = fixture_cores[i];
            let rest = fixture_rests[i];
            if rest.intensity <= 0.0 || core.range <= 0.0 {
                continue;
            }
            let apex = core.position - rest.direction * rest.lens_distance;
            if !bounce_cone_reaches(apex, rest.direction, rest.cos_field, core.range + rest.lens_distance, ball.xyz, ball.w) {
                continue;
            }
            let slot = atomicAdd(&bounce_light_count, 1u);
            if slot < BOUNCE_LIGHTS {
                bounce_light_list[slot] = i;
            }
        }
    }
    workgroupBarrier();
    return min(atomicLoad(&bounce_light_count), BOUNCE_LIGHTS);
}

/// The listed fixtures' light landing on a disc, W/m2. The cone's shape is
/// taken at four points across the disc, so a pool's edge crossing it
/// lights a share of it; the shadow is taken once, at its middle.
fn bounce_fixture_light(disc: BounceDisc, lights: u32) -> vec3<f32> {
    var out = vec3<f32>(0.0);
    if surface_clusters.flags.x < 0.5 {
        return out;
    }
    let n = disc.normal;
    let helper = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), abs(n.x) > 0.9);
    let t = normalize(cross(helper, n)) * disc.radius * 0.5;
    let b = cross(n, t);
    let beam_gain = surface_clusters.shadow.z;
    for (var k = 0u; k < lights; k++) {
        let light_index = bounce_light_list[k];
        let core = fixture_cores[light_index];
        let rest = fixture_rests[light_index];
        var landed = 0.0;
        for (var corner = 0u; corner < 4u; corner++) {
            let world = disc.pos
                + t * select(-1.0, 1.0, (corner & 1u) == 1u)
                + b * select(-1.0, 1.0, corner >= 2u);
            let q = world - core.position;
            let distance = length(q);
            if distance <= 1e-4 || distance >= core.range {
                continue;
            }
            let from_light = q / distance;
            let dot_nl = saturate(dot(n, -from_light));
            if dot_nl <= 0.0 {
                continue;
            }
            var cos_angle = dot(from_light, rest.direction);
            if rest.lens_distance > 0.0 {
                cos_angle = lens_cos_angle(q, rest.direction, rest.lens_distance, distance, rest.cos_field);
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
            var attenuation = distance_attenuation(distance, core.range);
            if rest.lens_distance > 0.0 {
                attenuation *= max(distance * distance, 0.01)
                    / max(lens_apex_distance2(q, rest.direction, rest.lens_distance), 0.01);
            }
            landed += 0.25 * dot_nl * angular * aperture * attenuation;
        }
        if landed <= 0.0 {
            continue;
        }
        let visibility = fixture_shadow_visibility(disc.pos, n, light_index);
        out += rest.color * rest.intensity * beam_gain * landed * visibility;
    }
    return out;
}

/// The sun and the sky probe's light landing on a disc, W/m2. Nothing on
/// the ground under a sky: the sky probe's lower half is that ground, lit
/// by this sun and this sky, and the scene pass takes it from there.
fn bounce_sky_light(disc: BounceDisc) -> vec3<f32> {
    var out = vec3<f32>(0.0);
    if disc.ground > 0.5 && aerial_sky.sun.w > 0.5 {
        return out;
    }
    let n = disc.normal;
    if environment_params.enabled > 0.5 && environment_params.intensity > 0.0 {
        // The irradiance cube holds E / pi.
        out += textureSampleLevel(environment_irradiance, environment_sampler, environment_direction(n), 0.0).rgb
            * environment_params.intensity * 3.14159265;
    }
    if globals.dir_to_light.w > 0.5 {
        let dot_nl = saturate(dot(n, globals.dir_to_light.xyz));
        if dot_nl > 0.0 {
            var cloud = 1.0;
            if aerial_sky.shadow.z > 0.0 {
                cloud = cloud_shadow_at(aerial_cloud_shadow, aerial_sampler, aerial_sky.shadow, aerial_sky.sun.xyz, disc.pos);
            }
            out += dot_nl * globals.dir_color.rgb * cloud * room_glow(disc.pos)
                * shadow_factor(disc.pos, n, false);
        }
    }
    return out;
}

@compute @workgroup_size(64)
fn bounce_relight(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let lights = bounce_cull(bounce_balls[group.x], lane);
    if id.x >= bounce_total.y {
        return;
    }
    let disc = bounce_discs[id.x];
    var landing = bounce_sky_light(disc) + bounce_fixture_light(disc, lights);
    if bounce_total.z == 0u {
        // What the last frame gathered for it (`bounce_spread.wgsl`).
        landing += bounce_landed[id.x * 2u].rgb;
    }
    bounce_radiosity[id.x] = vec4<f32>(disc.matte * landing, 0.0);
}
