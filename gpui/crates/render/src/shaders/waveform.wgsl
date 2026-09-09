struct Params {
    // Integer origin keeps subpixel precision late in long tracks.
    origin: u32, count: u32, atlas_width: u32, width: u32,
    fraction: f32, samples_per_pixel: f32, height: f32, padding: f32,
    gains: vec4<f32>, ceilings: vec4<f32>,
    colors: array<vec4<f32>, 4>,
    // Each hierarchy level begins on a texture row.
    rows: array<vec4<u32>, 32>,
};
@group(0) @binding(0) var hierarchy: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read_write> heights: array<vec4<f32>>;
fn peak_at(level: u32, index: u32) -> vec4<f32> {
    return abs(textureLoad(hierarchy, vec2<u32>(index % params.atlas_width,
        params.rows[level].x + index / params.atlas_width), 0));
}
@compute @workgroup_size(64)
fn sample_columns(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel = id.x;
    if pixel >= params.width { return; }
    let a = params.fraction + f32(pixel) * params.samples_per_pixel;
    let b = params.fraction + f32(pixel + 1u) * params.samples_per_pixel;
    var first = min(params.count, params.origin + u32(floor(a)));
    let end = min(params.count, params.origin + u32(ceil(b)));
    var peak = vec4<f32>(0.0);
    // Exact dyadic range decomposition, including partial intervals at either
    // edge. No LOD threshold or neighboring block can smear a transient.
    loop {
        if first >= end { break; }
        let remaining = end - first;
        let by_length = 31u - countLeadingZeros(remaining);
        let by_alignment = countTrailingZeros(first);
        let level = min(by_length, by_alignment);
        peak = max(peak, peak_at(level, first >> level));
        first += 1u << level;
    }
    let normalized = clamp(peak / max(params.gains, vec4<f32>(1e-20)), vec4<f32>(0.0), vec4<f32>(1.0));
    let compressed = log2(vec4<f32>(1.0) + 9.0 * normalized) / log2(10.0);
    heights[pixel] = floor(compressed * params.ceilings * max(0.0, (params.height - params.padding) * 0.5));
}
