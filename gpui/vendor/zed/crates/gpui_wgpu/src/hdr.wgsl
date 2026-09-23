// LUMA LOCAL EDIT: not upstream. The last pass of an HDR frame: the scene
// target (extended sRGB, BT.709, 1.0 = SDR white) to the BT.2100 PQ
// swapchain. `srgb_extended.wgsl` is prepended to this file.

struct Params {
    // Luminance of colour value 1.0, in cd/m².
    sdr_white_nits: f32,
    // Highest luminance the display shows, in cd/m².
    peak_nits: f32,
    // 1 when the swapchain composites premultiplied alpha.
    premultiplied: u32,
    _pad: u32,
}

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

// Linear BT.709 to linear BT.2020 (ITU-R BT.2087, column-major).
const BT709_TO_BT2020 = mat3x3<f32>(
    vec3<f32>(0.6274039, 0.0690973, 0.0163914),
    vec3<f32>(0.3292830, 0.9195404, 0.0880133),
    vec3<f32>(0.0433131, 0.0113623, 0.8955953),
);

// SMPTE ST 2084 inverse EOTF: absolute luminance to a PQ code value.
fn pq_encode(nits: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let y = pow(clamp(nits / 10000.0, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(m1));
    return pow((c1 + c2 * y) / (1.0 + c3 * y), vec3<f32>(m2));
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = textureLoad(scene, vec2<i32>(position.xy), 0);
    var encoded = texel.rgb;
    // A premultiplied frame holds colour times alpha, and the compositor
    // divides it out again in the encoded domain, as it does for SDR. The
    // transfer function is not linear, so it applies to the straight colour.
    let premultiplied = params.premultiplied != 0u && texel.a > 0.0;
    if premultiplied {
        encoded = encoded / texel.a;
    }
    // Colours outside the BT.2020 gamut have no PQ code value; clip them.
    let linear = max(BT709_TO_BT2020 * srgb_to_linear_extended(encoded), vec3<f32>(0.0));
    let nits = min(linear * params.sdr_white_nits, vec3<f32>(params.peak_nits));
    var pq = pq_encode(nits);
    if premultiplied {
        pq = pq * texel.a;
    }
    return vec4<f32>(pq, texel.a);
}
