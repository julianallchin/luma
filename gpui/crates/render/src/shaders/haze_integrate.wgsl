// Parallel prefix integration: one workgroup per camera column and one lane
// per radial slice. Density quadrature runs concurrently over the volume.
@group(2) @binding(0) var integrated_fog: texture_storage_3d<rgba16float, write>;
@group(2) @binding(1) var incident_fog: texture_3d<f32>;
var<workgroup> segments: array<vec4<f32>, FOG_SLICES>;

@compute @workgroup_size(FOG_SLICES)
fn integrate_grid(@builtin(workgroup_id) column: vec3<u32>,
                  @builtin(local_invocation_index) z: u32) {
    let size = textureDimensions(incident_fog);
    let pixel = (vec2<f32>(column.xy) + 0.5) / vec2<f32>(size.xy)
        * vec2<f32>(haze.transport.w, haze.transport.z);
    let ray = scene_ray(pixel);
    let a = f32(z) / f32(size.z);
    let b = f32(z + 1u) / f32(size.z);
    let start = haze.shadow.w * a * a;
    let end = haze.shadow.w * b * b;
    let lighting = textureLoad(incident_fog, vec3<i32>(vec3<u32>(column.xy, z)), 0).rgb;
    var density_integral = 0.0;
    for (var tap = 0u; tap < 4u; tap += 1u) {
        let t = mix(start, end, (f32(tap) + 0.5) * 0.25);
        density_integral += haze_density_at(haze.camera_pos.xyz + ray.dir * t);
    }
    let tau = haze.depth.z * density_integral * (end - start) * 0.25;
    // Store local radiance and transmittance, then compose front-to-back.
    segments[z] = vec4<f32>(lighting * (1.0 - exp(-tau)), exp(-tau));
    workgroupBarrier();
    for (var stride = 1u; stride < FOG_SLICES; stride *= 2u) {
        var prior = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        if z >= stride { prior = segments[z - stride]; }
        workgroupBarrier();
        segments[z] = vec4<f32>(prior.rgb + prior.a * segments[z].rgb, prior.a * segments[z].a);
        workgroupBarrier();
    }
    if z == 0u { textureStore(integrated_fog, vec3<i32>(vec3<u32>(column.xy, 0u)), vec4<f32>(0.0)); }
    textureStore(integrated_fog, vec3<i32>(vec3<u32>(column.xy, z + 1u)), segments[z]);
}
