struct Params {
    // Device-pixel bounds, independent of the scratch allocation's capacity.
    bounds: vec4<f32>,
    clip: vec4<f32>,
    radii: vec4<f32>,
    source: vec4<f32>,
    // Active low-resolution extent, sampling scale, Gaussian sigma.
    kernel: vec4<f32>,
}
@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

fn sample_source(pixel: vec2<f32>, extent: vec2<f32>) -> vec4<f32> {
    let clamped = clamp(pixel, vec2<f32>(0.5), extent - 0.5);
    return textureSampleLevel(source_texture, source_sampler,
        clamped / vec2<f32>(textureDimensions(source_texture)), 0.0);
}

@fragment
fn downsample(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let start = params.source.xy + (position.xy - 0.5) * params.kernel.z;
    var color = vec4<f32>(0.0);
    // A box prefilter prevents fine horizontal/vertical detail aliasing when
    // the Gaussian runs at quarter resolution. Small sigmas stay full-size.
    for (var y = 0.5; y < params.kernel.z; y += 1.0) {
        for (var x = 0.5; x < params.kernel.z; x += 1.0) {
            color += sample_source(start + vec2<f32>(x, y),
                vec2<f32>(textureDimensions(source_texture)));
        }
    }
    return color / (params.kernel.z * params.kernel.z);
}

fn gaussian(position: vec2<f32>, axis: vec2<f32>) -> vec4<f32> {
    let sigma = params.kernel.w / params.kernel.z;
    let radius = i32(ceil(3.0 * sigma));
    var color = vec4<f32>(0.0);
    var weight_sum = 0.0;
    for (var i = -radius; i <= radius; i += 1) {
        let weight = exp(-0.5 * f32(i * i) / (sigma * sigma));
        color += weight * sample_source(position + axis * f32(i), params.kernel.xy);
        weight_sum += weight;
    }
    return color / weight_sum;
}

@fragment
fn horizontal(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return gaussian(position.xy, vec2<f32>(1.0, 0.0));
}

@fragment
fn vertical(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return gaussian(position.xy, vec2<f32>(0.0, 1.0));
}

@fragment
fn composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let point = position.xy;
    if (any(point < params.clip.xy) || any(point >= params.clip.xy + params.clip.zw)) {
        discard;
    }
    let half_size = params.bounds.zw * 0.5;
    let local = point - params.bounds.xy - half_size;
    let top = select(params.radii.x, params.radii.y, local.x > 0.0);
    let bottom = select(params.radii.w, params.radii.z, local.x > 0.0);
    let radius = min(select(top, bottom, local.y > 0.0), min(half_size.x, half_size.y));
    let distance = abs(local) - half_size + radius;
    if (length(max(distance, vec2<f32>(0.0))) + min(max(distance.x, distance.y), 0.0) > radius) {
        discard;
    }
    // Constant-factor blending fades between the original and blurred backdrop
    // without changing the Gaussian radius. Foreground paint follows normally.
    return sample_source((point - params.source.xy) / params.kernel.z, params.kernel.xy);
}

@fragment
fn present(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(source_texture, vec2<i32>(position.xy), 0);
}
