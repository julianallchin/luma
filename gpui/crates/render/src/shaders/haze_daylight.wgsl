// The diffuse probe stores E/pi. Averaging its six lobes approximates the
// sky's mean radiance, including ground bounce. The sun is absent from the
// probe and enters separately through the haze's phase (`haze_phase.wgsl`),
// divided by 4pi to be a normalised density.
// Shared by surfaces and background: transparent layers need their own path.
fn outdoor_haze_light(direction: vec3<f32>, sun_dir: vec3<f32>, sunlight: vec4<f32>) -> vec3<f32> {
    // The lobe average is frame-constant, so `environment_ambient.wgsl` takes
    // the six samples once per frame in the same order, from the same probe,
    // sampler and mip, and this reads the result.
    return outdoor_ambient_mean.rgb + outdoor_haze_sun(direction, sun_dir, sunlight);
}

/// The sun's part of `outdoor_haze_light`, which the scene pass and the
/// composite scale by how much of the haze the sun really reaches
/// (`sun_shafts.wgsl`).
fn outdoor_haze_sun(direction: vec3<f32>, sun_dir: vec3<f32>, sunlight: vec4<f32>) -> vec3<f32> {
    return sunlight.rgb * haze_phase(dot(direction, sun_dir)) / 12.566370614359;
}
