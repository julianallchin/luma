// Hillaire §5: the sky-view table — the whole visible sky for one sun position,
// in 192x108 texels, rebuilt only when the sun moves.
//
// It holds sky radiance in solar-irradiance units and no sun disc: the disc is
// a fraction of a texel wide and is added analytically where the sky is read.

@group(0) @binding(0) var transmittance_lut: texture_2d<f32>;
@group(0) @binding(1) var lut_sampler: sampler;
@group(0) @binding(2) var multiscatter_lut: texture_2d<f32>;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var<uniform> cfg: SkyUniform;

const MARCH_STEPS: u32 = 40u;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= SKYVIEW_WIDTH || id.y >= SKYVIEW_HEIGHT {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(f32(SKYVIEW_WIDTH), f32(SKYVIEW_HEIGHT));
    let radius = sky_view_radius(cfg);
    let angles = skyview_angles(radius, uv);
    let sun = normalize(cfg.sun.xyz);
    let dir = skyview_direction(sun, angles.x, angles.y);

    // The converged multiple-scattering radiance for this altitude and sun
    // angle, held constant along the ray. Hillaire's approximation: it varies
    // far more slowly than the single-scattering term the march resolves.
    let ms_uv = vec2<f32>(
        clamp(dot(sun, vec3<f32>(0.0, 0.0, 1.0)) * 0.5 + 0.5, 0.0, 1.0),
        clamp((radius - GROUND_RADIUS_KM) / (TOP_RADIUS_KM - GROUND_RADIUS_KM), 0.0, 1.0),
    );
    let psi_ms = textureSampleLevel(multiscatter_lut, lut_sampler, ms_uv, 0.0).rgb;

    let scattered = integrate_scattered(
        transmittance_lut,
        lut_sampler,
        radius,
        dir,
        sun,
        sky_ground_albedo(cfg),
        MARCH_STEPS,
        psi_ms,
        false,
    );
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(scattered.luminance, 1.0));
}
