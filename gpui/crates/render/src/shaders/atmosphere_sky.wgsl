// Reading the sky where a frame needs it: the composite's background.
//
// Everything expensive already happened in the tables; this is two texture
// samples and a disc test. Group 2 is bound with 1x1 placeholders and
// `sun.w = 0` whenever a frame has no sky, so the composite has one pipeline.

@group(2) @binding(0) var sky_transmittance_lut: texture_2d<f32>;
@group(2) @binding(1) var sky_skyview_lut: texture_2d<f32>;
@group(2) @binding(2) var sky_sampler: sampler;
@group(2) @binding(3) var<uniform> sky: SkyUniform;

/// The solar disc's radiance, in the same solar-irradiance units the tables
/// use: unit irradiance spread over the disc's solid angle.
const SUN_DISC_RADIANCE: f32 = 1.0 / 6.807e-5;
/// Limb darkening at 550 nm, the usual quadratic fit. Without it the disc is a
/// flat sticker; with it the rim falls off the way a photograph's does.
const SUN_LIMB: f32 = 0.6;

/// Exposed sky radiance along a world-space view direction, disc included.
fn sky_radiance(direction: vec3<f32>) -> vec3<f32> {
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
        radiance += SUN_DISC_RADIANCE * limb * coverage * transmittance;
    }
    return radiance * sky_exposure(sky);
}
