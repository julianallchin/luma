// Shared density and optical-depth integration. Indoor haze occupies a room;
// outdoor haze is a global height field, independent of the rig's bounds.
struct ProceduralMedium {
    // cloudiness, cloud size (m), turbulence, light-cache angular resolution
    shape: vec4<f32>,
    // world XY velocity, unused, time
    wind: vec4<f32>,
    // XYZ lighting-work bounds; also the physical room boundary indoors.
    // min.w: mean extinction at ground level (1/m).
    // max.w: outdoor scale height (m), or zero for an indoor room.
    min: vec4<f32>,
    max: vec4<f32>,
};

// Keep a full-density interior, then dissolve over several metres. The box
// is only integration support; it must never appear as a visible wall of fog.
fn medium_envelope(m: ProceduralMedium, p: vec3<f32>) -> f32 {
    if m.max.w > 0.0 {
        return select(exp(-max(p.z, 0.0) / m.max.w), 0.0, p.z < 0.0);
    }
    let width = min((m.max.xyz - m.min.xyz) / 3.0, vec3<f32>(12.0));
    let edge = min(p - m.min.xyz, m.max.xyz - p);
    let fade = smoothstep(vec3<f32>(0.0), width, edge);
    return fade.x * fade.y * fade.z;
}

fn medium_interior(m: ProceduralMedium, p: vec3<f32>) -> bool {
    // The outdoor field changes with height even inside the lighting bounds.
    if m.max.w > 0.0 { return false; }
    let width = min((m.max.xyz - m.min.xyz) / 3.0, vec3<f32>(12.0));
    return all(p >= m.min.xyz + width) && all(p <= m.max.xyz - width);
}

fn medium_cloud(m: ProceduralMedium, p: vec3<f32>) -> f32 {
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
    return 1.0 + m.shape.x * contrast;
}

fn medium_density(m: ProceduralMedium, p: vec3<f32>) -> f32 {
    let envelope = medium_envelope(m, p);
    if envelope <= 0.0 || m.shape.x <= 0.0 { return envelope; }
    return envelope * medium_cloud(m, p);
}

// Finite support for expensive fixture lighting, never an outdoor density edge.
fn medium_lighting_span(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, distance: f32) -> vec2<f32> {
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

fn medium_span(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, distance: f32) -> vec2<f32> {
    if m.max.w <= 0.0 { return medium_lighting_span(m, origin, direction, distance); }
    // The ground is the only outdoor boundary. No haze exists below it.
    var start = 0.0;
    var end = max(distance, 0.0);
    if origin.z < 0.0 {
        if direction.z <= 0.0 { return vec2<f32>(0.0); }
        start = -origin.z / direction.z;
    } else if direction.z < 0.0 {
        end = min(end, -origin.z / direction.z);
    }
    return vec2<f32>(start, max(start, end));
}

// Exact integral of the outdoor mean density. A midpoint expansion near the
// horizontal avoids subtracting two almost equal exponentials.
fn medium_height_depth(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, span: vec2<f32>) -> f32 {
    let length = max(span.y - span.x, 0.0);
    let slope = direction.z / m.max.w;
    let h0 = max(origin.z + direction.z * span.x, 0.0) / m.max.w;
    let h1 = max(origin.z + direction.z * span.y, 0.0) / m.max.w;
    if abs(slope * length) < 0.001 {
        return m.min.w * length * exp(-0.5 * (h0 + h1));
    }
    return max(m.min.w * (exp(-h0) - exp(-h1)) / slope, 0.0);
}

// Beyond optical depth 16 less than one ten-millionth of the background
// survives. Stop numerical detail there, rather than sampling miles of noise.
fn medium_detail_span(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, span: vec2<f32>) -> vec2<f32> {
    if m.max.w <= 0.0 || medium_height_depth(m, origin, direction, span) <= 16.0 { return span; }
    let h0 = max(origin.z + direction.z * span.x, 0.0);
    if abs(direction.z) < 1e-5 {
        return vec2<f32>(span.x, min(span.y, span.x + 16.0 / (m.min.w * exp(-h0 / m.max.w))));
    }
    let density_end = max(exp(-h0 / m.max.w) - 16.0 * direction.z / (m.max.w * m.min.w), 1e-30);
    let height_end = -m.max.w * log(density_end);
    let end = (height_end - origin.z) / direction.z;
    return vec2<f32>(span.x, clamp(end, span.x, span.y));
}

// Both total-depth and prefix consumers use the same outdoor quadrature.
fn medium_outdoor_segment(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>,
    a: f32, b: f32, detail: f32) -> f32 {
    var cloud = 1.0;
    if detail > 0.0 && m.shape.x > 0.0 {
        cloud = mix(1.0, medium_cloud(m, origin + direction * (0.5 * (a + b))), detail);
    }
    return medium_height_depth(m, origin, direction, vec2<f32>(a, b)) * cloud;
}

// Fixed 32 strata for resolved cloud detail; global mean density is analytic.
// Used once per camera ray, independently of the number of lights.
struct MediumRay {
    span: vec2<f32>,
    optical: array<f32, 33>,
};
fn medium_ray(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, distance: f32) -> MediumRay {
    var result: MediumRay;
    result.span = medium_span(m, origin, direction, distance);
    if m.min.w <= 0.0 || result.span.y <= result.span.x { return result; }
    if m.max.w > 0.0 {
        result.span = medium_detail_span(m, origin, direction, result.span);
        let step = (result.span.y - result.span.x) / 32.0;
        // Unresolved cloud fluctuations converge to their statistical mean.
        // This keeps long outdoor paths stable as the camera moves.
        let detail = 1.0 - smoothstep(m.shape.y, 2.0 * m.shape.y, step);
        for (var i = 0u; i < 32u; i += 1u) {
            let a = result.span.x + f32(i) * step;
            let b = a + step;
            result.optical[i + 1u] = result.optical[i] + medium_outdoor_segment(m, origin, direction, a, b, detail);
        }
        return result;
    }
    let step = (result.span.y - result.span.x) / 32.0;
    result.optical[0] = 0.0;
    for (var i = 0u; i < 32u; i += 1u) {
        let p = origin + direction * (result.span.x + (f32(i) + 0.5) * step);
        result.optical[i + 1u] = result.optical[i] + m.min.w * step * medium_density(m, p);
    }
    return result;
}

// Total-depth consumers do not need the 33-element prefix or its indexed
// reads/writes. Preserve the same strata and running addition order.
fn medium_optical_depth(m: ProceduralMedium, origin: vec3<f32>, direction: vec3<f32>, distance: f32) -> f32 {
    if m.min.w <= 0.0 { return 0.0; }
    var span = medium_span(m, origin, direction, distance);
    if m.max.w > 0.0 {
        span = medium_detail_span(m, origin, direction, span);
        if m.shape.x <= 0.0 || (span.y - span.x) / 32.0 >= 2.0 * m.shape.y {
            return medium_height_depth(m, origin, direction, span);
        }
    }
    if span.y <= span.x { return 0.0; }
    let step = (span.y - span.x) / 32.0;
    var optical = 0.0;
    if m.max.w > 0.0 {
        let detail = 1.0 - smoothstep(m.shape.y, 2.0 * m.shape.y, step);
        for (var i = 0u; i < 32u; i += 1u) {
            let a = span.x + f32(i) * step;
            let b = a + step;
            optical += medium_outdoor_segment(m, origin, direction, a, b, detail);
        }
    } else {
        for (var i = 0u; i < 32u; i += 1u) {
            let p = origin + direction * (span.x + (f32(i) + 0.5) * step);
            optical += m.min.w * step * medium_density(m, p);
        }
    }
    return optical;
}
fn medium_depth(ray: MediumRay, distance: f32) -> f32 {
    let z = clamp((distance - ray.span.x) / max(ray.span.y - ray.span.x, 1e-5) * 32.0, 0.0, 32.0);
    let lo = min(u32(z), 31u);
    return mix(ray.optical[lo], ray.optical[lo + 1u], z - f32(lo));
}
