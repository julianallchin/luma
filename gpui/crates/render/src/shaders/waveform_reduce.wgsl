struct Reduction { source_row: u32, count: u32, width: u32, _pad: u32 };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: Reduction;
fn read(index: u32) -> vec4<f32> {
    if index >= params.count { return vec4<f32>(0.0); }
    return abs(textureLoad(source, vec2<u32>(index % params.width, params.source_row + index / params.width), 0));
}
@compute @workgroup_size(16, 16)
fn reduce(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.y * params.width + id.x;
    if id.x >= params.width || index >= (params.count + 1u) / 2u { return; }
    textureStore(output, id.xy, max(read(index * 2u), read(index * 2u + 1u)));
}
