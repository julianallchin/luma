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

// The haze is stirred air: the cloud size is its largest eddy L, and
// turbulence how hard it is stirred, ε = STIRRING · turbulence³ (m²/s³), so
// eddy speeds are linear in the slider. By Kolmogorov's law an eddy r
// across moves at (ε·r)^⅓ and turns over in r over that, and the haze's
// contrast at r is the clouds' times (r/L)^⅓. Every size, speed and
// strength in the field follows from those, so no setting gives haze that
// air can't make.
//
// At full turbulence, a fan on the haze.
const STIRRING: f32 = 0.1;

// Speed of eddies `size` metres across, metres a second.
fn medium_eddy_speed(m: ProceduralMedium, size: f32) -> f32 {
    return m.shape.z * pow(STIRRING * size, 1.0 / 3.0);
}

// Turns an eddy `size` metres across has made by now.
fn medium_turns(m: ProceduralMedium, size: f32) -> f32 {
    return m.wind.w * medium_eddy_speed(m, size) / size;
}

// The haze's contrast at `size`, as a share of the clouds'.
fn medium_contrast_at(m: ProceduralMedium, size: f32) -> f32 {
    return pow(min(size / m.shape.y, 1.0), 1.0 / 3.0);
}

// Eddies turn into new eddies where they are rather than slide by. Each
// half turn the haze moves on to the next place in the baked field, one
// `FIELD_TURN` on, fading out the place it was at and in the next. A texel
// holds both (`haze_field.rs`): x the place fading out, y the one fading in.
// So one read at `at` gives both, and after each step the place fading in
// reads as x exactly what it read as y before it: nothing jumps.
struct Turning {
    // Where in the field, base units.
    at: vec3<f32>,
    // The incoming place's share, sin²(π/2 · the way through the half
    // turn): the shares sum to one, and their square roots keep the
    // contrast steady between.
    share: f32,
};

fn medium_turning(turns: f32) -> Turning {
    let half_turns = 2.0 * turns;
    let s = sin(1.5707963 * fract(half_turns));
    // Whole texels, wrapped: exact however long the show runs.
    let steps = (u32(half_turns) % FIELD_SIZE) * FIELD_TURN % FIELD_SIZE;
    return Turning(vec3<f32>(steps) / FIELD_TEXELS, s * s);
}

fn medium_cloud(m: ProceduralMedium, p: vec3<f32>) -> f32 {
    let q = (p - vec3<f32>(m.wind.xy, 0.0) * m.wind.w) / m.shape.y * 2.0;
    // Four octaves are already baked into this field. Wind and turnover
    // move the shared field without rebuilding it or fetching each octave.
    let turning = medium_turning(medium_turns(m, m.shape.y));
    let both = textureSampleLevel(haze_noise_field, haze_noise_sampler,
        (q + turning.at) * FIELD_INV_CELLS, 0.0).xy;
    let layered = sqrt(1.0 - turning.share) * both.x + sqrt(turning.share) * both.y;
    // Symmetric remapping preserves statistical mean density in the interior.
    let contrast = 2.0 * smoothstep(-0.22, 0.22, layered) - 1.0;
    return 1.0 + m.shape.x * contrast;
}

fn medium_density(m: ProceduralMedium, p: vec3<f32>) -> f32 {
    let envelope = medium_envelope(m, p);
    if envelope <= 0.0 || m.shape.x <= 0.0 { return envelope; }
    return envelope * medium_cloud(m, p);
}

// The fine end of the field, the wisps a laser beam is thin enough to
// show, metres across. How strong they are, how fast they turn over and
// what carries them all follow from their size and the stirring.
const WISP_M: f32 = 0.5;
// How far the largest eddies carry a wisp, as a share of their size: an
// eddy moves air about its own size a turn.
const SWIRL_REACH: f32 = 0.25;
// A strand is where the fine field is within this of zero; the share of
// the air a strand fills on average, measured over the baked field, so the
// contrast keeps mean 1.
const STRAND_WIDTH: f32 = 0.15;
const STRAND_MEAN: f32 = 0.55;
// At this scale the noise repeats every 1.2 m along each axis, and a beam
// along an axis would show the same few eddies over and over. Turned off
// the axes, a line through the field never retraces itself.
const WISP_TURN: mat3x3<f32> = mat3x3<f32>(
    0.6849, -0.1424, 0.7146,
    0.5182, 0.7846, -0.3403,
    -0.5122, 0.6034, 0.6111,
);

// One read of the baked field at `p`, in metres, for features `size` wide,
// moved `jump` base units: two independent values (`Turning`), each about
// −0.5 to 0.5, mean 0. Turned off the axes, so a beam along one never
// retraces the field's repeats.
fn medium_turned_field(p: vec3<f32>, size: f32, jump: vec3<f32>) -> vec2<f32> {
    // The baked field's broadest octave is four base units across.
    let q = WISP_TURN * p * (4.0 / size) + jump;
    return textureSampleLevel(haze_noise_field, haze_noise_sampler, q * FIELD_INV_CELLS, 0.0).xy;
}

// Bright where the fine field crosses zero: the shapes are its level lines
// drawn out by the swirl, not its blobs.
fn medium_strand(w: f32) -> f32 {
    return 1.0 - smoothstep(0.0, STRAND_WIDTH, abs(w));
}

// The wisps' share of the haze's density at `world`, mean 1: the fine end
// of the field, which a laser beam is thin enough to show.
//
// Real eddies turn over where they are, get drawn out into wisps by the
// largest eddies carrying them, and come in patches. So the fine field is
// warped by the largest eddies, moving at their speed; turns over as the
// clouds do, at its own size's rate; is shaped into strands rather than
// blobs; and is strong only where the largest eddies are churning. Its
// strength is the clouds' at its size. Where a pixel spans `footprint`
// metres, as wide as a wisp, the pixel averages them away.
fn medium_wisps(m: ProceduralMedium, world: vec3<f32>, footprint: f32) -> f32 {
    let largest = m.shape.y;
    let carried = world - vec3<f32>(m.wind.xy, 0.0) * m.wind.w;
    // The largest eddies, carrying the wisps: their field moves at their
    // speed, which reads as churning, not sliding, through the warp.
    let travel = vec3<f32>(0.0, 0.0, m.wind.w * medium_eddy_speed(m, largest));
    let ahead = medium_turned_field(carried + travel, largest, vec3<f32>(0.0));
    let behind = medium_turned_field(carried - travel, largest, vec3<f32>(17.3, 0.0, 0.0));
    let swirl = vec3<f32>(ahead, behind.x);
    let warped = carried + swirl * (2.0 * SWIRL_REACH * largest);
    // Fading the strands rather than the fields keeps them as fine mid-turn.
    let turning = medium_turning(medium_turns(m, WISP_M));
    let both = medium_turned_field(warped, WISP_M, turning.at);
    let strands = mix(medium_strand(both.x), medium_strand(both.y), turning.share);
    // Patches: churning air here, calm air there, as the largest eddies go.
    let churning = smoothstep(-0.25, 0.15, behind.y);
    let resolved = 1.0 - smoothstep(0.25 * WISP_M, WISP_M, footprint);
    let contrast = (strands - STRAND_MEAN) / STRAND_MEAN;
    let strength = m.shape.x * medium_contrast_at(m, WISP_M);
    return 1.0 + strength * resolved * churning * contrast;
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
