// The diffuse probe stores E/pi. Averaging its six lobes approximates the
// sky's mean radiance, including ground bounce. The sun is absent from the
// probe and enters separately with a normalized Henyey-Greenstein phase.
// Shared by surfaces and background: transparent layers need their own path.
fn outdoor_haze_light(direction: vec3<f32>, sun_dir: vec3<f32>, sunlight: vec4<f32>) -> vec3<f32> {
    // The lobe average is frame-constant, so `environment_ambient.wgsl` takes
    // the six samples once per frame in the same order, from the same probe,
    // sampler and mip, and this reads the result.
    let ambient = outdoor_ambient_mean.rgb;
    let g = sunlight.w;
    let denominator = max(1.0 + g * g - 2.0 * g * dot(direction, sun_dir), 1e-4);
    let phase = (1.0 - g * g) / (12.566370614359 * denominator * sqrt(denominator));
    return ambient + sunlight.rgb * phase;
}
