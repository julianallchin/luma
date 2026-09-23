// LUMA LOCAL EDIT: not upstream. The sRGB transfer function extended past
// [0, 1]: odd-symmetric below zero and unclamped above one. An HDR frame
// (see `hdr.rs`) keeps its values in this encoding, so a value <= 1 is the
// same number an SDR frame holds and blends the same way, and a value > 1 is
// brighter than SDR white.

fn srgb_to_linear_extended(encoded: vec3<f32>) -> vec3<f32> {
    let magnitude = abs(encoded);
    let higher = pow((magnitude + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    let lower = magnitude / vec3<f32>(12.92);
    return sign(encoded) * select(higher, lower, magnitude <= vec3<f32>(0.04045));
}

fn linear_to_srgb_extended(linear: vec3<f32>) -> vec3<f32> {
    let magnitude = abs(linear);
    let higher = vec3<f32>(1.055) * pow(magnitude, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    let lower = magnitude * vec3<f32>(12.92);
    return sign(linear) * select(higher, lower, magnitude <= vec3<f32>(0.0031308));
}
