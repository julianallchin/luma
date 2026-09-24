// The cloud layer: its density and its light, shared by every pass that
// marches it (`atmosphere/clouds.rs`): the per-frame view trace, the probe's
// panorama and the shadow map.
//
// Positions are kilometres in the venue's world frame, Z up, the venue at the
// origin and the planet's centre `GROUND_RADIUS_KM` below it. Heights are
// above the ground sphere, so the layer curves away toward the horizon.
//
// Shape follows Schneider (2015, 2017): a weather map says how much cloud of
// which kind stands over each point; a Perlin-Worley volume, cut by a height
// profile for that kind, is the body; a Worley volume erodes its edge, wispy
// at the base and billowed at the top. Light follows Hillaire (2016): an
// energy-conserving step, a two-lobe Henyey-Greenstein phase with a strong
// forward lobe (the silver lining), a second Wrenninge octave, and for the
// rest of the multiple scattering a two-stream diffusion estimate that keeps
// a thick cloud white where the sun reaches it; the sky above and the
// ground below are the ambient.

@group(0) @binding(0) var transmittance_lut: texture_2d<f32>;
@group(0) @binding(1) var lut_sampler: sampler;
@group(0) @binding(2) var skyview_lut: texture_2d<f32>;
@group(0) @binding(3) var weather_tex: texture_2d<f32>;
@group(0) @binding(4) var wrap_sampler: sampler;
@group(0) @binding(5) var shape_tex: texture_3d<f32>;
@group(0) @binding(6) var detail_tex: texture_3d<f32>;
@group(0) @binding(7) var<uniform> cfg: SkyUniform;
@group(0) @binding(8) var<uniform> layer: CloudLayer;
@group(0) @binding(15) var multiscatter_lut: texture_2d<f32>;

struct CloudLayer {
    // x: base above the ground, km. y: thickness, km. z: extinction at full
    // density, 1/km. w: ground span of one weather tile, km.
    shape: vec4<f32>,
    // x: erosion. y: shape noise period, km. z: detail noise period, km.
    // w: fraction of the sky left clear.
    detail: vec4<f32>,
    // x: view steps. y: light steps. z: length of the light march, km.
    // w: ambient strength.
    march: vec4<f32>,
    // xy: weather drift, km. z: shape noise drift, km. w: detail noise drift.
    wind: vec4<f32>,
    // x: powder strength. y: edge sharpness, how fast density rises inside a
    // cloud's outline. zw unused.
    light: vec4<f32>,
    // The cirrus sheet: x its height, km, y optical depth straight through
    // its densest streak, z the fraction of sky it covers (zero for none),
    // w a streak's length along the wind, km.
    cirrus: vec4<f32>,
    // x: a streak's width across the wind, km. y: the wind's drift at that
    // height, km. zw unused.
    cirrus_shape: vec4<f32>,
};

/// Forward and back lobes of the phase function, and the forward weight.
/// The forward lobe is what makes a cloud in front of the sun glow.
const CLOUD_G_FORWARD: f32 = 0.85;
const CLOUD_G_BACK: f32 = -0.25;
const CLOUD_FORWARD_WEIGHT: f32 = 0.6;
/// Wrenninge's octave factors: scattering, extinction and eccentricity each
/// halve per octave (the HDRP and Unreal defaults).
const CLOUD_OCTAVES: u32 = 3u;
const CLOUD_MS_A: f32 = 0.5;
const CLOUD_MS_B: f32 = 0.5;
const CLOUD_MS_C: f32 = 0.5;
/// Mean cosine of light that has scattered many times, for the diffusion
/// estimate of how much sunlight is still diffusing at a given depth.
const CLOUD_G_DIFFUSE: f32 = 0.85;
/// Albedo-like gain on that diffuse light. An optically thick cloud sends
/// back most of the sunlight it takes in, which is why a sunlit cumulus is
/// the whitest thing in the sky; a few octaves of single scattering alone
/// leave it grey.
const CLOUD_MS_GAIN: f32 = 0.75;
/// Scale on the optical depth toward the sun in the single-scattering
/// octaves: the light march's own absorption, set apart from the density
/// the view sees (Schneider's light absorption; HDRP and Unreal expose the
/// same split). At the density a cumulus has, sunlight dies within tens of
/// metres of its surface, and the forward glow of a cloud in front of the
/// sun is a line one pixel wide; real clouds pass forward-scattered light
/// deeper than a single-scattering estimate says.
const CLOUD_LIGHT_ABSORPTION: f32 = 0.35;
/// Growth of each light step over the one before (Schneider's cone).
const CLOUD_LIGHT_GROWTH: f32 = 1.7;
/// How much hazier than the clean-air model the air under a cloud layer is.
const CLOUD_AIR_HAZE: f32 = 3.0;

fn remap(v: f32, lo: f32, hi: f32, new_lo: f32, new_hi: f32) -> f32 {
    return new_lo + (v - lo) / max(hi - lo, 1e-5) * (new_hi - new_lo);
}

fn henyey_greenstein(g: f32, cos_theta: f32) -> f32 {
    let denominator = 1.0 + g * g - 2.0 * g * cos_theta;
    return (1.0 - g * g) * INV_4PI / (denominator * sqrt(max(denominator, 1e-6)));
}

fn cloud_phase(cos_theta: f32, eccentricity: f32) -> f32 {
    return mix(
        henyey_greenstein(CLOUD_G_BACK * eccentricity, cos_theta),
        henyey_greenstein(CLOUD_G_FORWARD * eccentricity, cos_theta),
        CLOUD_FORWARD_WEIGHT,
    );
}

/// Fraction of the flux a slab of optical depth `tau` passes, scattered or
/// not: the two-stream diffusion estimate.
fn cloud_diffusion(tau: f32) -> f32 {
    return 1.0 / (1.0 + 0.75 * (1.0 - CLOUD_G_DIFFUSE) * tau);
}

/// Height above the ground sphere of a point in the venue frame.
fn cloud_altitude(p: vec3<f32>) -> f32 {
    return length(p + vec3<f32>(0.0, 0.0, GROUND_RADIUS_KM)) - GROUND_RADIUS_KM;
}

/// Vertical profile of a cloud of `kind` at height fraction `hf`, for a
/// column whose coverage is `coverage`: 0 is stratus, a thin sheet low in
/// the layer; a half is stratocumulus, wide and flattened; 1 is cumulus.
///
/// A column is only as tall as its coverage allows, and its base lifts a
/// little where the coverage thins. The weather map's coverage falls off
/// slowly toward a heap's edge, so a heap rounds into a dome over a base
/// that narrows a little, instead of standing as a slab with walls for
/// sides and a skirt at its foot.
fn cloud_profile(hf: f32, kind: f32, coverage: f32) -> f32 {
    let k = clamp(kind, 0.0, 1.0);
    let c = clamp(coverage, 0.0, 1.0);
    let top = mix(0.3, 1.0, k) * mix(0.2, 1.0, sqrt(c));
    let bottom = 0.15 * k * (1.0 - c);
    if hf <= bottom || hf >= top {
        return 0.0;
    }
    let x = (hf - bottom) / (top - bottom);
    let base = smoothstep(0.0, mix(0.08, 0.2, k), x);
    let crown = 1.0 - smoothstep(mix(0.6, 0.2, k), 1.0, x);
    return base * crown;
}

/// Coverage, kind and density at a ground position, km.
fn cloud_weather(xy: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(weather_tex, wrap_sampler, (xy + layer.wind.xy) / layer.shape.w, 0.0);
}

/// Extinction at `p`, 1/km. `detail` erodes the edge with the fine volume;
/// the light march and the shadow map go without it.
fn cloud_extinction(p: vec3<f32>, detail: bool) -> f32 {
    let h = cloud_altitude(p);
    let hf = (h - layer.shape.x) / layer.shape.y;
    if hf <= 0.0 || hf >= 1.0 {
        return 0.0;
    }
    let weather = cloud_weather(p.xy);
    if weather.r <= 0.004 {
        return 0.0;
    }
    let q = vec3<f32>(p.xy + layer.wind.xy + vec2<f32>(layer.wind.z, 0.0), h) / layer.detail.y;
    let low = textureSampleLevel(shape_tex, wrap_sampler, q, 0.0);
    let cells = low.g * 0.625 + low.b * 0.25 + low.a * 0.125;
    let noise = remap(low.r, cells - 1.0, 1.0, 0.0, 1.0);
    // Nubis: cloud where the noise rises past what the column allows. The
    // allowance is the profile times the coverage, so a thin column keeps
    // only the noise's peaks and a column falls off to rounded lobes at its
    // top and its base rather than ending in a wall.
    let allowance = cloud_profile(hf, weather.g, weather.r) * weather.r;
    // Scaled by the allowance, a thin column's body stays shallow, so the
    // erosion below bites deep into it and its outline breaks into lobes.
    var body = clamp(remap(noise, 1.0 - allowance, 1.0, 0.0, 1.0), 0.0, 1.0) * allowance;
    if body <= 0.0 {
        return 0.0;
    }
    if detail {
        let d = textureSampleLevel(
            detail_tex,
            wrap_sampler,
            vec3<f32>(p.xy + layer.wind.xy + vec2<f32>(layer.wind.w, 0.0), h) / layer.detail.z,
            0.0,
        ).rgb;
        let fine = d.r * 0.625 + d.g * 0.25 + d.b * 0.125;
        let modifier = mix(fine, 1.0 - fine, clamp(hf * 10.0, 0.0, 1.0));
        body = clamp(remap(body, modifier * layer.detail.x, 1.0, 0.0, 1.0), 0.0, 1.0);
    } else {
        // The erosion at its mean, so a coarse sample sees the same amount
        // of cloud as a fine one.
        body = clamp(remap(body, 0.5 * layer.detail.x, 1.0, 0.0, 1.0), 0.0, 1.0);
    }
    // A cumulus's outline is crisp: density rises within tens of metres of
    // it. Without the gain the rise takes hundreds and every lobe blurs.
    body = clamp(body * layer.light.y, 0.0, 1.0);
    return body * layer.shape.z * mix(0.4, 1.6, weather.b);
}

/// Distance along a ray from `origin` to where it enters and leaves the
/// shell, capped at `CLOUD_MAX_KM`; x >= y when it never enters.
fn cloud_span(origin: vec3<f32>, dir: vec3<f32>) -> vec2<f32> {
    let centred = origin + vec3<f32>(0.0, 0.0, GROUND_RADIUS_KM);
    let r = length(centred);
    let mu = dot(centred / r, dir);
    let bottom = GROUND_RADIUS_KM + layer.shape.x;
    let top = bottom + layer.shape.y;
    let ground = ray_sphere_distance(r, mu, GROUND_RADIUS_KM);
    // Entry: at the base from below, at the top from above, here from inside.
    var near = 0.0;
    var far = ray_sphere_distance(r, mu, top);
    if r < bottom {
        near = ray_sphere_distance(r, mu, bottom);
        // A ray leaving the ground upward cannot meet it again before the
        // layer; the test is only for a downward ray from above the ground.
        if near < 0.0 || (mu < 0.0 && ground > 0.0 && ground < near) {
            return vec2<f32>(1.0, 0.0);
        }
    } else if r > top {
        let discriminant = r * r * (mu * mu - 1.0) + top * top;
        if discriminant < 0.0 || mu > 0.0 {
            return vec2<f32>(1.0, 0.0);
        }
        near = -r * mu - sqrt(discriminant);
        far = -r * mu + sqrt(discriminant);
        let inner = ray_sphere_distance(r, mu, bottom);
        if inner > 0.0 {
            far = inner;
        }
    } else {
        let inner = ray_sphere_distance(r, mu, bottom);
        if inner > 0.0 && mu < 0.0 {
            far = inner;
        }
    }
    if far < 0.0 || near >= CLOUD_MAX_KM {
        return vec2<f32>(1.0, 0.0);
    }
    return vec2<f32>(near, min(far, CLOUD_MAX_KM));
}

/// Optical depth toward the sun from `p`, in steps that grow away from it,
/// each sampled a little off the axis (Schneider's cone), and one long
/// sample past the cone for the cloud beyond it.
fn cloud_light_depth(p: vec3<f32>, sun: vec3<f32>, steps: u32) -> f32 {
    let first = layer.march.z * (CLOUD_LIGHT_GROWTH - 1.0)
        / (pow(CLOUD_LIGHT_GROWTH, f32(steps)) - 1.0);
    let side = normalize(cross(sun, vec3<f32>(0.0, 1.0, 0.001)));
    let up = cross(side, sun);
    var depth = 0.0;
    var t = 0.0;
    var step = first;
    for (var i = 0u; i < steps; i = i + 1u) {
        let s = t + 0.5 * step;
        let angle = f32(i) * 2.39996;
        let offset = (side * cos(angle) + up * sin(angle)) * s * 0.15;
        depth += cloud_extinction(p + sun * s + offset, i < 2u) * step;
        t += step;
        step *= CLOUD_LIGHT_GROWTH;
    }
    let far = t + layer.march.z;
    depth += cloud_extinction(p + sun * far, false) * layer.march.z;
    return depth;
}

fn cloud_sky(sun: vec3<f32>, radius: f32, dir: vec3<f32>) -> vec3<f32> {
    let coords = skyview_coords(sun, dir);
    return textureSampleLevel(skyview_lut, lut_sampler, skyview_uv(radius, coords.y, coords.x), 0.0).rgb;
}

/// Cirrus density, 0 to 1, at a ground position under the sheet, km.
///
/// Streaks are the shape noise stretched along the wind (+X), long and
/// narrow, with a warp across the wind that bends them into hooks and
/// fans. A second, coarse lookup says where there is cirrus at all, so it
/// comes in patches and bands, not an even veil.
fn cirrus_density(xy: vec2<f32>) -> f32 {
    let coverage = layer.cirrus.z;
    if coverage <= 0.0 {
        return 0.0;
    }
    let q = xy + vec2<f32>(layer.cirrus_shape.y, 0.0);
    let region = textureSampleLevel(shape_tex, wrap_sampler, vec3<f32>(q / 50.0, 0.13), 0.0).r;
    let patches = smoothstep(1.0 - coverage - 0.08, 1.0 - coverage + 0.08, region);
    if patches <= 0.0 {
        return 0.0;
    }
    var s = vec2<f32>(q.x / layer.cirrus.w, q.y / layer.cirrus_shape.x);
    let warp = textureSampleLevel(shape_tex, wrap_sampler, vec3<f32>(s * vec2<f32>(0.15, 0.05), 0.37), 0.0).r;
    s.y += (warp - 0.5) * 6.0;
    let streak = textureSampleLevel(shape_tex, wrap_sampler, vec3<f32>(s.x * 0.25, s.y * 0.25, 0.61), 0.0);
    // Fibres: the fine volume stretched harder still.
    let fibre = textureSampleLevel(detail_tex, wrap_sampler, vec3<f32>(s.x * 0.5, s.y * 3.0, 0.29), 0.0).r;
    let body = clamp(remap(streak.r * 0.7 + streak.g * 0.3, 0.5, 0.9, 0.0, 1.0), 0.0, 1.0);
    return patches * body * mix(0.3, 1.0, fibre);
}

/// Optical depth of the sheet straight up at a ground position, km.
fn cirrus_optical_depth(xy: vec2<f32>) -> f32 {
    return cirrus_density(xy) * layer.cirrus.y;
}

/// What the sheet shows along a ray from `origin`: `rgb` in-scattered
/// light, `a` transmittance, and the distance to it in `depth`.
fn cirrus_sample(origin: vec3<f32>, dir: vec3<f32>, sun: vec3<f32>, sky_top: vec3<f32>) -> CloudSample {
    var result: CloudSample;
    result.light = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    result.depth = CLOUD_MAX_KM;
    if layer.cirrus.z <= 0.0 {
        return result;
    }
    let centred = origin + vec3<f32>(0.0, 0.0, GROUND_RADIUS_KM);
    let r = length(centred);
    let mu = dot(centred / r, dir);
    let radius = GROUND_RADIUS_KM + layer.cirrus.x;
    if r >= radius {
        return result;
    }
    let t = ray_sphere_distance(r, mu, radius);
    if t <= 0.0 || t > 4.0 * CLOUD_MAX_KM {
        return result;
    }
    let p = origin + dir * t;
    let density = cirrus_density(p.xy);
    if density <= 0.0 {
        return result;
    }
    // Slant path through a sheet a few hundred metres thick.
    let up = normalize(p + vec3<f32>(0.0, 0.0, GROUND_RADIUS_KM));
    let slant = 1.0 / max(dot(up, dir), 0.08);
    let tau = density * layer.cirrus.y * slant;
    let alpha = 1.0 - exp(-tau);
    let sun_mu = dot(up, sun);
    let sunlight = transmittance_to_top(transmittance_lut, lut_sampler, radius, sun_mu);
    // Ice: a strong forward lobe, so the sheet glows round the sun, and a
    // thin sheet passes most of its light on, so it stays bright white.
    let cos_theta = dot(dir, sun);
    let phase = mix(henyey_greenstein(0.2, cos_theta), henyey_greenstein(0.8, cos_theta), 0.5);
    let light = sunlight * phase + sky_top * 0.6;
    // The air in front, as for the cumulus, over the low part of the path.
    let low = min(t, 3.0 / max(dir.z, 0.03));
    let air = exp(-medium_at(0.0).extinction.g * CLOUD_AIR_HAZE * low);
    result.light = vec4<f32>(light * alpha * air, 1.0 - alpha * air);
    result.depth = t;
    return result;
}

/// What the layer shows along one ray: `rgb` in-scattered light before
/// exposure, `a` how much of the sky behind shows through. `depth` is the
/// transmittance-weighted distance to the cloud, km.
struct CloudSample {
    light: vec4<f32>,
    depth: f32,
};

/// March `dir` from `origin` through the layer. `jitter` in [0, 1) offsets
/// the steps; `steps` is the budget for a ray that crosses the layer
/// straight up, and a slanting ray takes up to twice as many.
fn cloud_march(origin: vec3<f32>, dir: vec3<f32>, jitter: f32, steps_budget: u32) -> CloudSample {
    let sun = normalize(cfg.sun.xyz);
    let radius = GROUND_RADIUS_KM + max(cloud_altitude(origin), 0.001);

    // The sky's mean radiance over the layer and the ground's under it: the
    // ambient every sample shares.
    let flat_sun = normalize(vec3<f32>(sun.xy + vec2<f32>(1e-5, 0.0), 0.0));
    let side = vec3<f32>(-flat_sun.y, flat_sun.x, 0.0);
    let sky_top = 0.4 * cloud_sky(sun, radius, vec3<f32>(0.0, 0.0, 1.0))
        + 0.2 * cloud_sky(sun, radius, flat_sun * 0.866 + vec3<f32>(0.0, 0.0, 0.5))
        + 0.2 * cloud_sky(sun, radius, -flat_sun * 0.866 + vec3<f32>(0.0, 0.0, 0.5))
        + 0.2 * cloud_sky(sun, radius, side * 0.866 + vec3<f32>(0.0, 0.0, 0.5));
    let mu_sun = sun.z;
    let ground_sun = transmittance_to_top(transmittance_lut, lut_sampler, GROUND_RADIUS_KM, max(mu_sun, 0.0))
        * max(mu_sun, 0.0);
    let clear = layer.detail.w;
    let deck_tau = layer.shape.z * layer.shape.y * 0.3;
    // Diffuse light leaves a thick deck at much the same rate whatever the
    // sun's angle; the angle is already in `ground_sun`.
    let lit_ground = mix(cloud_diffusion(deck_tau), 1.0, clear);
    let ground = sky_ground_albedo(cfg) * INV_PI
        * (ground_sun * lit_ground + PI * sky_top * mix(cloud_diffusion(deck_tau), 1.0, clear));
    // The air under the deck, lit by the deck: what the horizon fades to
    // where the sky is not clear.
    // The same light the aerial march fills the air with
    // (`atmosphere_aerial.wgsl`), so the deck at the horizon and the far
    // ground under it fade to one colour.
    // A long path through that air shows its source over its extinction:
    // the multiple scattering of the sky under the layer's grey, and the
    // sunlight the cloud diffused down to it.
    let low_air = medium_at(0.0);
    let ms = textureSampleLevel(multiscatter_lut, lut_sampler, vec2<f32>(mu_sun * 0.5 + 0.5, 0.0), 0.0).rgb;
    let albedo = (low_air.rayleigh_scattering + vec3<f32>(low_air.mie_scattering))
        / max(low_air.extinction, vec3<f32>(1e-9));
    let base_light = albedo * (ms * cfg.clouds.y + ground_sun * INV_PI * 0.5 * cfg.clouds.z);

    var scattered = vec3<f32>(0.0);
    var transmittance = 1.0;
    var weight = 0.0;
    var depth_sum = 0.0;
    let span = cloud_span(origin, dir);
    if span.x < span.y && layer.shape.z > 0.0 {
        let slant = (span.y - span.x) / (2.0 * layer.shape.y);
        let steps = u32(f32(steps_budget) * clamp(slant, 1.0, 2.0));
        let dt = (span.y - span.x) / f32(steps);
        let cos_theta = dot(dir, sun);
        var phases: array<f32, CLOUD_OCTAVES>;
        var eccentricity = 1.0;
        for (var o = 0u; o < CLOUD_OCTAVES; o = o + 1u) {
            phases[o] = cloud_phase(cos_theta, eccentricity);
            eccentricity *= CLOUD_MS_C;
        }
        // Powder (HDRP's form): a thin edge has had no depth in which to
        // gather scattered light, so it is darker — except toward the sun,
        // where the forward lobe dominates.
        let powder_weight = layer.light.x * smoothstep(0.5, -0.5, cos_theta);
        let light_steps = u32(layer.march.y);
        for (var i = 0u; i < steps; i = i + 1u) {
            let t = span.x + (f32(i) + jitter) * dt;
            let p = origin + dir * t;
            let sigma = cloud_extinction(p, true);
            if sigma <= 0.0 {
                continue;
            }
            let h = cloud_altitude(p);
            let up = normalize(p + vec3<f32>(0.0, 0.0, GROUND_RADIUS_KM));
            let r = GROUND_RADIUS_KM + h;
            let sun_mu = dot(up, sun);
            let lit = select(1.0, 0.0, ray_sphere_distance(r, sun_mu, GROUND_RADIUS_KM) > 0.0);
            let sunlight = transmittance_to_top(transmittance_lut, lut_sampler, r, sun_mu) * lit;
            let tau = cloud_light_depth(p, sun, light_steps);
            var octaves = 0.0;
            var a = 1.0;
            var b = 1.0;
            for (var o = 0u; o < CLOUD_OCTAVES; o = o + 1u) {
                octaves += a * phases[o] * exp(-b * tau * CLOUD_LIGHT_ABSORPTION);
                a *= CLOUD_MS_A;
                b *= CLOUD_MS_B;
            }
            // Multiply scattered sunlight: near isotropic, as from a
            // Lambertian surface, fading with the depth it has to diffuse
            // through. This is what makes a sunlit side white and a side
            // turned from the sun grey.
            let diffuse = cloud_diffusion(tau) * INV_PI * CLOUD_MS_GAIN;
            let powder = mix(1.0, 1.0 - exp(-sigma / layer.shape.z * 6.0), powder_weight);
            let hf = clamp((h - layer.shape.x) / layer.shape.y, 0.0, 1.0);
            // The sky reaches the top of a cloud and the ground its base; a
            // deep sample sees each through the cloud between.
            let above = mix(0.4, 1.0, hf);
            let below = (1.0 - hf) * 0.6;
            let source = sunlight * (octaves + diffuse) * powder
                + (sky_top * above + ground * below) * layer.march.w;
            let step_t = exp(-sigma * dt);
            let absorbed = transmittance * (1.0 - step_t);
            scattered += source * absorbed;
            weight += absorbed;
            depth_sum += absorbed * t;
            transmittance *= step_t;
            if transmittance < 0.003 {
                transmittance = 0.0;
                break;
            }
        }
    }

    // The air in front of the cloud. Far cloud fades into what that air
    // shows: under broken cloud, the hazy clear sky behind it; under a deck,
    // the light under the deck. Fading broken cloud toward the deck's light
    // instead draws a dark band along the horizon.
    var distance = CLOUD_MAX_KM;
    if weight > 1e-4 {
        distance = depth_sum / weight;
    } else if span.x < span.y {
        distance = span.x;
    }
    let air = exp(-medium_at(0.0).extinction.g * CLOUD_AIR_HAZE * distance);
    let deck = 1.0 - smoothstep(0.0, 0.2, clear);
    var result: CloudSample;
    result.light = vec4<f32>(
        scattered * air + (1.0 - air) * deck * base_light,
        transmittance * air + (1.0 - air) * (1.0 - deck),
    );
    result.depth = distance;
    // The cirrus sheet is behind every cumulus.
    let cirrus = cirrus_sample(origin, dir, sun, sky_top);
    result.light = vec4<f32>(result.light.rgb + result.light.a * cirrus.light.rgb, result.light.a * cirrus.light.a);
    if weight <= 1e-4 && cirrus.light.a < 0.999 {
        result.depth = cirrus.depth;
    }
    return result;
}
