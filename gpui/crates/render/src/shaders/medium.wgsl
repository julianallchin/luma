// Shared procedural density and optical-depth integration. A bounded volume
// keeps venue haze from attenuating kilometres of outdoor sky.
struct ProceduralMedium {
    // cloudiness, cloud size (m), turbulence, light-cache angular resolution
    shape: vec4<f32>,
    // world XY velocity, unused, time
    wind: vec4<f32>,
    // XYZ bounds; min.w is mean extinction (1/m)
    min: vec4<f32>,
    max: vec4<f32>,
};

// Keep a full-density interior, then dissolve over several metres. The box
// is only integration support; it must never appear as a visible wall of fog.
fn medium_envelope(m: ProceduralMedium, p: vec3<f32>) -> f32 {
    let width = min((m.max.xyz - m.min.xyz) / 3.0, vec3<f32>(12.0));
    let edge = min(p - m.min.xyz, m.max.xyz - p);
    let fade = smoothstep(vec3<f32>(0.0), width, edge);
    return fade.x * fade.y * fade.z;
}

fn medium_interior(m: ProceduralMedium, p: vec3<f32>) -> bool {
    let width = min((m.max.xyz - m.min.xyz) / 3.0, vec3<f32>(12.0));
    return all(p >= m.min.xyz + width) && all(p <= m.max.xyz - width);
}

fn medium_density(m: ProceduralMedium, p: vec3<f32>) -> f32 {
    let envelope = medium_envelope(m, p);
    if envelope <= 0.0 || m.shape.x <= 0.0 { return envelope; }
    var q = (p - vec3<f32>(m.wind.xy, 0.0) * m.wind.w) / m.shape.y;
    // Smooth deformation, not random per-frame flicker or a fluid solver.
    let phase = q.yzx * 1.7 + m.wind.w * 0.23;
    q += m.shape.z * vec3<f32>(sin(phase.x), sin(phase.y + 2.1), sin(phase.z + 4.2));
    // Four octaves are already baked into this field. Wind and deformation
    // move the shared field without rebuilding it or fetching each octave.
    let layered = textureSampleLevel(haze_noise_field, haze_noise_sampler,
        q * 2.0 * FIELD_INV_CELLS, 0.0).x;
    // Symmetric remapping preserves statistical mean density in the interior.
    let contrast = 2.0 * smoothstep(-0.22, 0.22, layered) - 1.0;
    return envelope * (1.0 + m.shape.x * contrast);
}

fn medium_span(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, distance: f32) -> vec2<f32> {
    let safe = select(vec3<f32>(-1e-6), vec3<f32>(1e-6), direction >= vec3<f32>(0.0));
    let inverse = 1.0 / select(safe, direction, abs(direction) > vec3<f32>(1e-6));
    let a = (m.min.xyz - origin) * inverse;
    let b = (m.max.xyz - origin) * inverse;
    let lo = min(a, b);
    let hi = max(a, b);
    let start = max(0.0, max(lo.x, max(lo.y, lo.z)));
    let end = min(distance, min(hi.x, min(hi.y, hi.z)));
    return vec2<f32>(start, max(start, end));
}

// Fixed 32 strata over the part of the ray actually inside the venue.
// Used once per camera ray, independently of the number of lights.
struct MediumRay {
    span: vec2<f32>,
    optical: array<f32, 33>,
};
fn medium_ray(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, distance: f32) -> MediumRay {
    var result: MediumRay;
    result.span = medium_span(m, origin, direction, distance);
    if m.min.w <= 0.0 || result.span.y <= result.span.x { return result; }
    let step = (result.span.y - result.span.x) / 32.0;
    result.optical[0] = 0.0;
    for (var i = 0u; i < 32u; i += 1u) {
        let p = origin + direction * (result.span.x + (f32(i) + 0.5) * step);
        result.optical[i + 1u] = result.optical[i] + m.min.w * step * medium_density(m, p);
    }
    return result;
}
fn medium_depth(ray: MediumRay, distance: f32) -> f32 {
    let z = clamp((distance - ray.span.x) / max(ray.span.y - ray.span.x, 1e-5) * 32.0, 0.0, 32.0);
    let lo = min(u32(z), 31u);
    return mix(ray.optical[lo], ray.optical[lo + 1u], z - f32(lo));
}
