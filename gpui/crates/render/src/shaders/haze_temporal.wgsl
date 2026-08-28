// Temporal stabilization for the low-resolution volumetric target, run by any
// caller sampling consecutive moments — the live viewport, and an offscreen
// recording through `Renderer::render_next`. A one-off frame
// (`Renderer::render`) bypasses this pass and uses fixed blue-noise seeds.
// History is rejected at depth discontinuities here and reset by the CPU
// contract when size, camera, medium density, light topology, or time
// continuity changes.

struct Temporal {
    // x: history weight, y: history valid, z: linear-depth threshold in metres.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> temporal: Temporal;
@group(0) @binding(1) var current_haze: texture_2d<f32>;
@group(0) @binding(2) var history_haze: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(frag.xy);
    let current = textureLoad(current_haze, coord, 0);
    let history = textureLoad(history_haze, coord, 0);
    let depth_ok = abs(current.a - history.a) <= temporal.params.z;
    let history_weight = select(0.0, temporal.params.x, temporal.params.y > 0.5 && depth_ok);
    return vec4<f32>(mix(current.rgb, history.rgb, history_weight), current.a);
}
