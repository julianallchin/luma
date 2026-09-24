// Reading the sky where a frame needs it: the composite's background.
//
// Everything expensive already happened in the tables; this is two texture
// samples and a disc test. Group 2 is bound with 1x1 placeholders and
// `sun.w = 0` whenever a frame has no sky, so the composite has one pipeline.

@group(2) @binding(0) var sky_transmittance_lut: texture_2d<f32>;
@group(2) @binding(1) var sky_skyview_lut: texture_2d<f32>;
@group(2) @binding(2) var sky_sampler: sampler;
@group(2) @binding(3) var<uniform> sky: SkyUniform;
// The cloud layer as the camera sees it this frame, over the whole output
// (`atmosphere_cloud_view.wgsl`): a clear texel is (0, 0, 0, 1), and a sky
// without clouds binds exactly that.
@group(2) @binding(4) var sky_clouds: texture_2d<f32>;
@group(2) @binding(5) var sky_clouds_sampler: sampler;
// The layer's shadow map, for the sun in the air (`cloud_shadow.wgsl`).
@group(2) @binding(6) var sky_cloud_shadow: texture_2d<f32>;

/// How much of the sun a world point gets through the cloud layer.
fn sky_cloud_shadow_at(world: vec3<f32>) -> f32 {
    return cloud_shadow_at(sky_cloud_shadow, sky_sampler, sky.shadow, sky.sun.xyz, world);
}

/// The solar disc's radiance, in the same solar-irradiance units the tables
/// use: unit irradiance spread over the disc's solid angle.
const SUN_DISC_RADIANCE: f32 = 1.0 / 6.807e-5;
/// Limb darkening at 550 nm, the usual quadratic fit. Without it the disc is a
/// flat sticker; with it the rim falls off the way a photograph's does.
const SUN_LIMB: f32 = 0.6;

/// Exposed sky radiance along a world-space view direction, disc included,
/// seen through this frame's clouds at output coordinate `uv`.
fn sky_radiance(direction: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let dir = normalize(direction);
    let sun_dir = normalize(sky.sun.xyz);
    let radius = sky_view_radius(sky);
    let coords = skyview_coords(sun_dir, dir);
    var radiance = textureSampleLevel(
        sky_skyview_lut,
        sky_sampler,
        skyview_uv(radius, coords.y, coords.x),
        0.0,
    ).rgb;

    let cos_sun = dot(dir, sun_dir);
    let cos_edge = sky.params.w;
    // A ray that meets the ground first sees no disc. The transmittance
    // table's parameterisation covers only rays that escape, so without this
    // the set sun comes back as a white dot on a dark ground.
    let clear = ray_sphere_distance(radius, dir.z, GROUND_RADIUS_KM) < 0.0;
    var disc = vec3<f32>(0.0);
    if cos_sun > cos_edge && clear {
        // One texel of the sky table is about a degree; softening the rim over
        // a tenth of the disc is what keeps it from aliasing into a polygon.
        let angle = acos(clamp(cos_sun, -1.0, 1.0));
        let edge = acos(clamp(cos_edge, -1.0, 1.0));
        let t = clamp(angle / max(edge, 1e-6), 0.0, 1.0);
        let limb = 1.0 - SUN_LIMB * (1.0 - sqrt(max(0.0, 1.0 - t * t)));
        let coverage = 1.0 - smoothstep(0.9, 1.0, t);
        // The disc is seen through the whole atmosphere above it, which is what
        // turns it orange at four degrees and red at zero.
        let transmittance =
            transmittance_to_top(sky_transmittance_lut, sky_sampler, radius, dir.z);
        disc = SUN_DISC_RADIANCE * limb * coverage * transmittance;
    }
    // The sky behind the clouds. The disc is four orders brighter than the
    // sky and smaller than a cloud texel, so an interpolated leak at a
    // cloud's edge would show it. It takes the lesser of the traced layer
    // and the shadow map at the camera — the sun the stage stands in — so
    // it shows only where both see it.
    let clouds = textureSampleLevel(sky_clouds, sky_clouds_sampler, uv, 0.0);
    let sun_through = min(clouds.a, sky_cloud_shadow_at(sky.camera.xyz * 1000.0));
    // `clouds.rgb` is below zero where cloud shadows cross the air in front
    // (`atmosphere_cloud_view.wgsl`); the sum is not.
    radiance = max(radiance * clouds.a + clouds.rgb, vec3<f32>(0.0)) + disc * sun_through;
    return radiance * sky_exposure(sky);
}
