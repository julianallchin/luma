// The diffuse probe stores E/pi. Averaging its six lobes approximates the
// sky's mean radiance, including ground bounce. The sun is absent from the
// probe and enters separately with a normalized Henyey-Greenstein phase.
// Shared by surfaces and background: transparent layers need their own path.
fn outdoor_haze_light(direction: vec3<f32>, sun_dir: vec3<f32>, sunlight: vec4<f32>) -> vec3<f32> {
    let axes = array<vec3<f32>, 6>(
        vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0),
        vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
        vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0),
    );
    var ambient = vec3<f32>(0.0);
    for (var i = 0u; i < 6u; i += 1u) {
        ambient += textureSampleLevel(environment_irradiance, environment_sampler, axes[i], 0.0).rgb / 6.0;
    }
    let g = sunlight.w;
    let denominator = max(1.0 + g * g - 2.0 * g * dot(direction, sun_dir), 1e-4);
    let phase = (1.0 - g * g) / (12.566370614359 * denominator * sqrt(denominator));
    return ambient + sunlight.rgb * phase;
}
