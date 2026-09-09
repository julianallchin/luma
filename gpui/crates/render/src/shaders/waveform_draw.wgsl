struct Params {
    origin: u32, count: u32, atlas_width: u32, width: u32,
    fraction: f32, samples_per_pixel: f32, height: f32, padding: f32,
    gains: vec4<f32>, ceilings: vec4<f32>, colors: array<vec4<f32>, 4>, rows: array<vec4<u32>, 32>,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> heights: array<vec4<f32>>;
@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
    return vec4<f32>(p[index], 0.0, 1.0);
}
fn linear(encoded: vec3<f32>) -> vec3<f32> {
    return select(encoded / 12.92, pow((encoded + 0.055) / 1.055, vec3<f32>(2.4)), encoded > vec3<f32>(0.04045));
}
@fragment fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let h = heights[min(u32(position.x), params.width - 1u)];
    let distance = abs(position.y - params.height * 0.5);
    var color = params.colors[0];
    if distance < h.x { color = params.colors[1]; }
    if distance < h.y { color = params.colors[2]; }
    if distance < h.z { color = params.colors[3]; }
    return vec4<f32>(linear(color.rgb), color.a);
}
