// Parallel prefix integration: one workgroup per camera column and one lane
// per radial slice. Density quadrature runs concurrently over the volume.
@group(2) @binding(0) var integrated_fog: texture_storage_3d<rgba16float, write>;
@group(2) @binding(1) var incident_fog: texture_3d<f32>;
@group(2) @binding(2) var fog_columns: texture_2d<f32>;
var<workgroup> segments: array<vec4<f32>, FOG_SLICES>;

@compute @workgroup_size(FOG_SLICES)
fn integrate_grid(@builtin(workgroup_id) column: vec3<u32>,
                  @builtin(local_invocation_index) z: u32) {
    let size = textureDimensions(incident_fog);
    let pixel = (vec2<f32>(column.xy) + 0.5) / vec2<f32>(size.xy)
        * vec2<f32>(haze.transport.w, haze.transport.z);
    let info = textureLoad(fog_columns, vec2<i32>(column.xy), 0);
    let ray_dir = info.xyz;
    let a = f32(z) / f32(size.z);
    let b = f32(z + 1u) / f32(size.z);
    let span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, ray_dir, haze.shadow.w);
    let start = mix(span.x, span.y, a * a);
    let end = mix(span.x, span.y, b * b);
    let lighting = textureLoad(incident_fog, vec3<i32>(vec3<u32>(column.xy, z)), 0).rgb;
    var density_integral = 0.0;
    for (var tap = 0u; tap < 4u && a < info.w + 1e-6; tap += 1u) {
        let t = mix(start, end, (f32(tap) + 0.5) * 0.25);
        density_integral += haze_density_at(haze.camera_pos.xyz + ray_dir * t);
    }
    let tau = haze.depth.z * density_integral * (end - start) * 0.25;
    // Store local radiance and transmittance, then compose front-to-back.
    segments[z] = vec4<f32>(lighting * (1.0 - exp(-tau)), exp(-tau));
    if z == 0u {
        // Outdoors, camera extinction begins before the lighting work volume.
        // Its boundary schedules work; it is not the start of the atmosphere.
        var entry_t = 1.0;
        if haze.medium.max.w > 0.0 {
            entry_t = exp(-medium_optical_depth(haze.medium, haze.camera_pos.xyz, ray_dir, span.x));
        }
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
