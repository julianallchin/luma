@group(0) @binding(0) var source_depth: texture_depth_2d_array;
@group(0) @binding(1) var source_range: texture_2d_array<f32>;
@group(0) @binding(2) var destination: texture_storage_2d_array<rg32float, write>;
@group(0) @binding(3) var<storage, read> dirty_layers: array<u32>;

@compute @workgroup_size(8, 8, 1)
fn from_depth(@builtin(global_invocation_id) cell: vec3<u32>) {
    if cell.z >= arrayLength(&dirty_layers) || any(cell.xy >= textureDimensions(destination)) { return; }
    let layer = i32(dirty_layers[cell.z]);
    let p = vec2<i32>(cell.xy * 2u);
    let a = textureLoad(source_depth, p, layer, 0);
    let b = textureLoad(source_depth, p + vec2<i32>(1, 0), layer, 0);
    let c = textureLoad(source_depth, p + vec2<i32>(0, 1), layer, 0);
    let d = textureLoad(source_depth, p + vec2<i32>(1, 1), layer, 0);
    textureStore(destination, vec2<i32>(cell.xy), layer, vec4<f32>(min(min(a,b),min(c,d)), max(max(a,b),max(c,d)), 0.0, 0.0));
}

@compute @workgroup_size(8, 8, 1)
fn reduce(@builtin(global_invocation_id) cell: vec3<u32>) {
    if cell.z >= arrayLength(&dirty_layers) || any(cell.xy >= textureDimensions(destination)) { return; }
    let layer = i32(dirty_layers[cell.z]);
    let p = vec2<i32>(cell.xy * 2u);
    let a = textureLoad(source_range, p, layer, 0).rg;
    let b = textureLoad(source_range, p + vec2<i32>(1, 0), layer, 0).rg;
    let c = textureLoad(source_range, p + vec2<i32>(0, 1), layer, 0).rg;
    let d = textureLoad(source_range, p + vec2<i32>(1, 1), layer, 0).rg;
    textureStore(destination, vec2<i32>(cell.xy), layer, vec4<f32>(min(min(a.x,b.x),min(c.x,d.x)), max(max(a.y,b.y),max(c.y,d.y)), 0.0, 0.0));
}
