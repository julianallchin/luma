// Prefix integrals of atmospheric radiance and RGB transmission. One thread
// owns a direction and integrates its distance slices in order, reusing the
// accumulated transport rather than marching each voxel from the camera.
@group(0) @binding(0) var transmittance_lut: texture_2d<f32>;
@group(0) @binding(1) var lut_sampler: sampler;
@group(0) @binding(2) var multiscatter_lut: texture_2d<f32>;
@group(0) @binding(3) var radiance_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(4) var transmittance_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(5) var<uniform> cfg: SkyUniform;

const AERIAL_STEPS: u32 = 4u;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(radiance_out);
    if any(id.xy >= size.xy) { return; }
    let uv = vec2<f32>(id.xy) / vec2<f32>(size.xy - vec2<u32>(1u));
    let sun = normalize(cfg.sun.xyz);
    let dir = aerial_direction(sun, uv);
    let radius = sky_view_radius(cfg);
    let phase_r = rayleigh_phase(dot(dir, sun));
    let phase_m = mie_phase(dot(dir, sun));
    var radiance = vec3<f32>(0.0);
    var throughput = vec3<f32>(1.0);
    var previous_km = 0.0;
    textureStore(radiance_out, vec3<i32>(vec3<u32>(id.xy, 0u)), vec4<f32>(0.0));
    textureStore(transmittance_out, vec3<i32>(vec3<u32>(id.xy, 0u)), vec4<f32>(1.0));
    for (var z = 1u; z < size.z; z += 1u) {
        let end_km = aerial_distance(f32(z) / f32(size.z - 1u)) * 0.001;
        let dt = (end_km - previous_km) / f32(AERIAL_STEPS);
        for (var step = 0u; step < AERIAL_STEPS; step += 1u) {
            let t = previous_km + (f32(step) + 0.5) * dt;
            let p = vec3<f32>(dir.xy * t, radius + dir.z * t);
            let sample_r = length(p);
            let up = p / sample_r;
            // Extend the surface medium below the ground for interpolation.
            // Visible paths end at their surface, but neighbouring angular
            // samples may cross it earlier; clipping those paths would leak
            // their shorter optical depth into grazing floor pixels.
            let r = max(sample_r, GROUND_RADIUS_KM + 0.001);
            let m = medium_at(r - GROUND_RADIUS_KM);
            let mu_sun = dot(up, sun);
            let lit = select(1.0, 0.0, ray_sphere_distance(r, mu_sun, GROUND_RADIUS_KM) > 0.0);
            let sun_transmittance = transmittance_to_top(transmittance_lut, lut_sampler, r, mu_sun);
            let ms_uv = vec2<f32>(mu_sun * 0.5 + 0.5, clamp((r - GROUND_RADIUS_KM) / (TOP_RADIUS_KM - GROUND_RADIUS_KM), 0.0, 1.0));
            let psi_ms = textureSampleLevel(multiscatter_lut, lut_sampler, ms_uv, 0.0).rgb;
            let source = (m.rayleigh_scattering * phase_r + m.mie_scattering * phase_m) * sun_transmittance * lit
                + (m.rayleigh_scattering + vec3<f32>(m.mie_scattering)) * psi_ms;
            let step_t = exp(-m.extinction * dt);
            radiance += throughput * source * (vec3<f32>(1.0) - step_t) / max(m.extinction, vec3<f32>(1e-9));
            throughput *= step_t;
        }
        let coord = vec3<i32>(vec3<u32>(id.xy, z));
        textureStore(radiance_out, coord, vec4<f32>(radiance * sky_exposure(cfg), 1.0));
        textureStore(transmittance_out, coord, vec4<f32>(throughput, 1.0));
        previous_km = end_km;
    }
}
