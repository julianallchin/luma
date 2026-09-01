// Hillaire §4: the multiple-scattering table.
//
// One texel per (sun zenith, altitude). Light that has bounced twice or more is
// treated as isotropic and altitude-only, which collapses an infinite series
// into a geometric one: the second-order estimate divided by one minus the
// per-event transfer. It is what stops the sky going flat and grey at dusk —
// the ozone-blue overhead at twilight is almost entirely this term.

@group(0) @binding(0) var transmittance_lut: texture_2d<f32>;
@group(0) @binding(1) var lut_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

/// Directions sampled over the sphere per texel. The integrand is smooth and
/// isotropic, so this converges long before it costs anything: the whole table
/// is 32x32.
const DIRECTIONS: u32 = 64u;
const MARCH_STEPS: u32 = 20u;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= MULTISCATTER_SIZE || id.y >= MULTISCATTER_SIZE {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / f32(MULTISCATTER_SIZE);
    let mu_sun = clamp(uv.x * 2.0 - 1.0, -1.0, 1.0);
    let radius = mix(
        GROUND_RADIUS_KM + 1e-3,
        TOP_RADIUS_KM - 1e-3,
        clamp(uv.y, 0.0, 1.0),
    );
    let sun = vec3<f32>(sqrt(max(0.0, 1.0 - mu_sun * mu_sun)), 0.0, mu_sun);

    var second_order = vec3<f32>(0.0);
    var transfer = vec3<f32>(0.0);
    // A Fibonacci sphere: uniform in solid angle, so the sum below is the mean
    // over the sphere and the 4pi of the integral cancels the 1/4pi of the
    // isotropic phase function.
    let golden = 3.14159265358979 * (3.0 - sqrt(5.0));
    for (var i = 0u; i < DIRECTIONS; i = i + 1u) {
        let z = 1.0 - 2.0 * (f32(i) + 0.5) / f32(DIRECTIONS);
        let rho = sqrt(max(0.0, 1.0 - z * z));
        let phi = golden * f32(i);
        let dir = vec3<f32>(rho * cos(phi), rho * sin(phi), z);
        let scattered = integrate_scattered(
            transmittance_lut,
            lut_sampler,
            radius,
            dir,
            sun,
            GROUND_ALBEDO_REFERENCE,
            MARCH_STEPS,
            vec3<f32>(0.0),
            true,
        );
        second_order += scattered.luminance;
        transfer += scattered.transfer;
    }
    second_order /= f32(DIRECTIONS);
    transfer /= f32(DIRECTIONS);

    // Sum of the geometric series in `transfer`. It is strictly below one for
    // any physical medium, but the clamp keeps a degenerate parameter set from
    // dividing by zero.
    let psi = second_order / max(vec3<f32>(1.0) - transfer, vec3<f32>(1e-4));
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(psi, 1.0));
}
