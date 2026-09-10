// Angular coordinates relative to the sun, with logarithmic metric distance.
// Endpoints are actual texel centres: distance zero is exactly (L=0, T=1).
const AERIAL_PI: f32 = 3.14159265358979;

fn aerial_distance(u: f32) -> f32 {
    return AERIAL_SCALE_M * (exp(u * log(1.0 + AERIAL_MAX_M / AERIAL_SCALE_M)) - 1.0);
}

fn aerial_distance_coord(distance: f32) -> f32 {
    return log(1.0 + clamp(distance, 0.0, AERIAL_MAX_M) / AERIAL_SCALE_M)
        / log(1.0 + AERIAL_MAX_M / AERIAL_SCALE_M);
}

fn aerial_sun_horizontal(sun: vec3<f32>) -> vec2<f32> {
    let reach = length(sun.xy);
    return select(vec2<f32>(1.0, 0.0), sun.xy / max(reach, 1e-6), reach > 1e-5);
}

fn aerial_direction(sun: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let flat = aerial_sun_horizontal(sun);
    let tangent = vec2<f32>(-flat.y, flat.x);
    let azimuth = uv.x * AERIAL_PI;
    // Concentrate angular samples at the horizon, where a small change of
    // elevation changes the height (and density) of a long path the most.
    let vertical = 1.0 - 2.0 * uv.y;
    let elevation = 0.5 * AERIAL_PI * vertical * abs(vertical);
    return vec3<f32>((flat * cos(azimuth) + tangent * sin(azimuth)) * cos(elevation), sin(elevation));
}

fn aerial_coords(sun: vec3<f32>, delta: vec3<f32>) -> vec3<f32> {
    let distance = length(delta);
    let dir = delta / max(distance, 1e-6);
    let flat = aerial_sun_horizontal(sun);
    let tangent = vec2<f32>(-flat.y, flat.x);
    let azimuth = abs(atan2(dot(dir.xy, tangent), dot(dir.xy, flat)));
    let elevation = asin(clamp(dir.z, -1.0, 1.0));
    let vertical = sign(elevation) * sqrt(abs(elevation) / (0.5 * AERIAL_PI));
    return vec3<f32>(azimuth / AERIAL_PI, 0.5 - 0.5 * vertical, aerial_distance_coord(distance));
}
