// Each column is sampled by fragments within one grid cell of its centre.
// Retain the farthest prefix they can request, including both sides of thin
// foreground geometry. The lighting pass may then omit only unused slices.
@group(2) @binding(0) var columns: texture_storage_2d<rgba32float, write>;
override DEPTH_CULL: bool = true;
@compute @workgroup_size(8, 8, 1)
fn prepare_columns(@builtin(global_invocation_id) column: vec3<u32>) {
    let size = textureDimensions(columns);
    if any(column.xy >= size) { return; }
    let viewport = vec2<f32>(haze.transport.w, haze.transport.z);
    let step = viewport / vec2<f32>(size);
    let center = (vec2<f32>(column.xy) + 0.5) * step;
    let first = max(vec2<i32>(floor(center - step - 0.5)), vec2<i32>(0));
    let last = min(vec2<i32>(ceil(center + step - 0.5)), vec2<i32>(viewport) - 1);
    var last_prefix = select(1.0, 0.0, DEPTH_CULL);
    for (var y = first.y; y <= last.y && last_prefix < 1.0; y += 1) {
        for (var x = first.x; x <= last.x && last_prefix < 1.0; x += 1) {
            let ray = scene_ray(vec2<f32>(f32(x), f32(y)) + 0.5);
            let span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, ray.dir, haze.shadow.w);
            let radial = sqrt(clamp((ray.hit_dist - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
            last_prefix = max(last_prefix, radial);
        }
    }
    textureStore(columns, vec2<i32>(column.xy), vec4<f32>(scene_ray(center).dir, last_prefix));
}
