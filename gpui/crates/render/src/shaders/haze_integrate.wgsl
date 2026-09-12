// Parallel prefix integration: one workgroup per camera column and one lane
// per radial slice. Density quadrature runs concurrently over the volume.
@group(2) @binding(0) var integrated_fog: texture_storage_3d<rgba16float, write>;
@group(2) @binding(1) var incident_fog: texture_3d<f32>;
@group(2) @binding(2) var fog_columns: texture_2d<f32>;
var<workgroup> segments: array<vec4<f32>, FOG_SLICES>;
// The `fog-transmittance` variant runs the same prefix without lighting,
// straight after `fog-prepare`, so the surface pass can read camera
// transmittance without waiting for the lit grid. Same taps, same scan.
override TRANSMITTANCE_ONLY: bool = false;
// FOG_TAU_MODE (injected with `fog_tau_read`/`fog_tau_write` from `gpu.rs`):
// 1 = this prefix also records each slice's optical depth τ (slice 0: the
// camera's entry transmittance) in the `fog-tau` texture; 2 = read those
// back instead of re-tapping the density; 0 = neither.

@compute @workgroup_size(FOG_SLICES)
fn integrate_grid(@builtin(workgroup_id) column: vec3<u32>,
                  @builtin(local_invocation_index) z: u32) {
    let size = textureDimensions(incident_fog);
    let info = textureLoad(fog_columns, vec2<i32>(column.xy), 0);
    let ray_dir = info.xyz;
    let a = f32(z) / f32(size.z);
    let b = f32(z + 1u) / f32(size.z);
    var lighting = vec3<f32>(0.0);
    if !TRANSMITTANCE_ONLY {
        lighting = textureLoad(incident_fog, vec3<i32>(vec3<u32>(column.xy, z)), 0).rgb;
    }
    var tau = 0.0;
    // Outdoors, camera extinction begins before the lighting work volume.
    // Its boundary schedules work; it is not the start of the atmosphere.
    var entry_t = 1.0;
    if FOG_TAU_MODE == 2u {
        tau = fog_tau_read(column.xy, z + 1u);
        if z == 0u {
            entry_t = fog_tau_read(column.xy, 0u);
        }
    } else {
        let span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, ray_dir, haze.shadow.w);
        let start = mix(span.x, span.y, a * a);
        let end = mix(span.x, span.y, b * b);
        var density_integral = 0.0;
        for (var tap = 0u; tap < 4u && a < info.w + 1e-6; tap += 1u) {
            let t = mix(start, end, (f32(tap) + 0.5) * 0.25);
            density_integral += haze_density_at(haze.camera_pos.xyz + ray_dir * t);
        }
        tau = haze.depth.z * density_integral * (end - start) * 0.25;
        if z == 0u && haze.medium.max.w > 0.0 {
            entry_t = exp(-medium_optical_depth(haze.medium, haze.camera_pos.xyz, ray_dir, span.x));
        }
        if FOG_TAU_MODE == 1u {
            fog_tau_write(column.xy, z + 1u, tau);
            if z == 0u {
                fog_tau_write(column.xy, 0u, entry_t);
            }
        }
    }
    // Store local radiance and transmittance, then compose front-to-back.
    segments[z] = vec4<f32>(lighting * (1.0 - exp(-tau)), exp(-tau));
    if z == 0u {
        segments[z] *= entry_t;
        textureStore(integrated_fog, vec3<i32>(vec3<u32>(column.xy, 0u)), vec4<f32>(0.0, 0.0, 0.0, entry_t));
    }
    workgroupBarrier();
    for (var stride = 1u; stride < FOG_SLICES; stride *= 2u) {
        var prior = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        if z >= stride { prior = segments[z - stride]; }
        workgroupBarrier();
        segments[z] = vec4<f32>(prior.rgb + prior.a * segments[z].rgb, prior.a * segments[z].a);
        workgroupBarrier();
    }
    textureStore(integrated_fog, vec3<i32>(vec3<u32>(column.xy, z + 1u)), segments[z]);
}
