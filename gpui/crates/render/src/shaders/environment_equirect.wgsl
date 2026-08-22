struct Params { size: vec4<u32> };
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(3) var<uniform> params: Params;

fn face_direction(face: u32, uv: vec2<f32>) -> vec3<f32> {
    let p = uv * 2.0 - 1.0;
    switch face {
        case 0u: { return normalize(vec3<f32>(1.0, -p.y, -p.x)); }
        case 1u: { return normalize(vec3<f32>(-1.0, -p.y, p.x)); }
        case 2u: { return normalize(vec3<f32>(p.x, 1.0, p.y)); }
        case 3u: { return normalize(vec3<f32>(p.x, -1.0, -p.y)); }
        case 4u: { return normalize(vec3<f32>(p.x, -p.y, 1.0)); }
        default: { return normalize(vec3<f32>(-p.x, -p.y, -1.0)); }
    }
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.size.x || id.y >= params.size.x || id.z >= 6u { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / f32(params.size.x);
    let d = face_direction(id.z, uv);
    let source_uv = vec2<f32>(atan2(d.z, d.x) / 6.28318530718 + 0.5, acos(clamp(d.y, -1.0, 1.0)) / 3.14159265359);
    textureStore(output_tex, vec2<i32>(id.xy), i32(id.z), textureSampleLevel(source, source_sampler, source_uv, 0.0));
}
