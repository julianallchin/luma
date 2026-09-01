// The medium, its parameterisations and the in-scattering integral, shared by
// every atmosphere pass and by the composite that reads their tables.
//
// The medium *constants* are not here: they are injected ahead of this file by
// `atmosphere::prelude()`, because the CPU builds the transmittance table and
// derives the sun's colour from the same numbers. Two hand-kept copies of an
// extinction coefficient drift, and when they do the symptom is a sky that
// disagrees with the light it casts.
//
// Lengths are kilometres and heights are above the ground sphere. Radiance is
// in units of the top-of-atmosphere solar irradiance, so a value of 1 is "as
// bright as unattenuated sunlight falling on a surface facing it"; `exposure`
// is the one scalar that carries that into display range.

const PI: f32 = 3.14159265358979;
const INV_4PI: f32 = 0.0795774715459;

struct SkyUniform {
    // xyz: unit world direction from the ground toward the sun. w: 1 when the
    // sky is the frame's background, 0 when it is off.
    sun: vec4<f32>,
    // x: exposure, y: ground albedo, z: view height above ground in km,
    // w: cosine of the sun's angular radius.
    params: vec4<f32>,
};

fn sky_exposure(cfg: SkyUniform) -> f32 { return cfg.params.x; }
fn sky_ground_albedo(cfg: SkyUniform) -> f32 { return cfg.params.y; }
fn sky_view_radius(cfg: SkyUniform) -> f32 { return GROUND_RADIUS_KM + cfg.params.z; }

/// Rayleigh scattering, Mie scattering and total extinction at height `h`.
struct Medium {
    rayleigh_scattering: vec3<f32>,
    mie_scattering: f32,
    extinction: vec3<f32>,
};

fn medium_at(h: f32) -> Medium {
    let rayleigh_density = exp(-max(h, 0.0) / RAYLEIGH_SCALE_KM);
    let mie_density = exp(-max(h, 0.0) / MIE_SCALE_KM);
    let ozone_density = max(0.0, 1.0 - abs(h - OZONE_CENTER_KM) / OZONE_HALF_WIDTH_KM);
    var m: Medium;
    m.rayleigh_scattering = RAYLEIGH_SCATTERING * rayleigh_density;
    m.mie_scattering = MIE_SCATTERING * mie_density;
    m.extinction = m.rayleigh_scattering
        + vec3<f32>(MIE_EXTINCTION * mie_density)
        + OZONE_ABSORPTION * ozone_density;
    return m;
}

fn rayleigh_phase(cos_theta: f32) -> f32 {
    return 3.0 / (16.0 * PI) * (1.0 + cos_theta * cos_theta);
}

/// Cornette-Shanks, the phase function Hillaire's reference implementation uses
/// for the Mie term.
fn mie_phase(cos_theta: f32) -> f32 {
    let g = MIE_G;
    let k = 3.0 / (8.0 * PI) * (1.0 - g * g) / (2.0 + g * g);
    return k * (1.0 + cos_theta * cos_theta) / pow(1.0 + g * g - 2.0 * g * cos_theta, 1.5);
}

/// Distance along `dir` from a point `r` out from the centre with `mu = dot(up, dir)`
/// to the sphere of radius `radius`. Negative when the ray misses it.
fn ray_sphere_distance(r: f32, mu: f32, radius: f32) -> f32 {
    let discriminant = r * r * (mu * mu - 1.0) + radius * radius;
    if discriminant < 0.0 {
        return -1.0;
    }
    let sq = sqrt(discriminant);
    let near = -r * mu - sq;
    let far = -r * mu + sq;
    if far < 0.0 {
        return -1.0;
    }
    if near < 0.0 {
        return far;
    }
    return near;
}

// --- transmittance table ---------------------------------------------------
//
// Bruneton's (r, mu) parameterisation: `u` is where the ray's length falls
// between the shortest and longest a ray from this altitude can be, `v` is the
// altitude itself as a fraction of the atmosphere's chord half-length. It is
// the mapping that keeps texels dense near the horizon, where transmittance
// changes fastest.

fn transmittance_uv(r: f32, mu: f32) -> vec2<f32> {
    let h = sqrt(max(0.0, TOP_RADIUS_KM * TOP_RADIUS_KM - GROUND_RADIUS_KM * GROUND_RADIUS_KM));
    let rho = sqrt(max(0.0, r * r - GROUND_RADIUS_KM * GROUND_RADIUS_KM));
    let discriminant = r * r * (mu * mu - 1.0) + TOP_RADIUS_KM * TOP_RADIUS_KM;
    let d = max(0.0, -r * mu + sqrt(max(discriminant, 0.0)));
    let d_min = TOP_RADIUS_KM - r;
    let d_max = rho + h;
    return vec2<f32>((d - d_min) / max(d_max - d_min, 1e-6), rho / max(h, 1e-6));
}

fn transmittance_to_top(lut: texture_2d<f32>, samp: sampler, r: f32, mu: f32) -> vec3<f32> {
    return textureSampleLevel(lut, samp, transmittance_uv(r, mu), 0.0).rgb;
}

// --- sky-view table --------------------------------------------------------
//
// A whole sky in 192x108 texels. `u` is the azimuth away from the sun, folded
// on the sun's meridian because the sky is symmetric about it — that buys twice
// the angular density for the same texels and lets the table be sampled with a
// clamped sampler. `v` splits at the horizon and takes a square root on each
// side, so the band the eye actually reads is where the texels are.

fn horizon_zenith_angle(r: f32) -> f32 {
    let cos_horizon = sqrt(max(0.0, r * r - GROUND_RADIUS_KM * GROUND_RADIUS_KM)) / max(r, 1e-6);
    return PI - acos(clamp(cos_horizon, -1.0, 1.0));
}

/// (azimuth away from the sun, zenith angle of the view) to a table coordinate.
fn skyview_uv(r: f32, view_zenith: f32, sun_azimuth: f32) -> vec2<f32> {
    let u = clamp(abs(sun_azimuth) / PI, 0.0, 1.0);
    let horizon = horizon_zenith_angle(r);
    var v: f32;
    if view_zenith < horizon {
        v = 0.5 * (1.0 - sqrt(max(0.0, 1.0 - view_zenith / max(horizon, 1e-6))));
    } else {
        v = 0.5 + 0.5 * sqrt(clamp((view_zenith - horizon) / max(PI - horizon, 1e-6), 0.0, 1.0));
    }
    return vec2<f32>(u, v);
}

/// The inverse of [`skyview_uv`], for the pass that fills the table.
fn skyview_angles(r: f32, uv: vec2<f32>) -> vec2<f32> {
    let horizon = horizon_zenith_angle(r);
    var view_zenith: f32;
    if uv.y < 0.5 {
        let t = 1.0 - 2.0 * uv.y;
        view_zenith = horizon * (1.0 - t * t);
    } else {
        let t = 2.0 * uv.y - 1.0;
        view_zenith = horizon + (PI - horizon) * t * t;
    }
    return vec2<f32>(uv.x * PI, view_zenith);
}

/// World direction for a view zenith angle and an azimuth measured away from
/// the sun's meridian. `up` is world +Z; the sun's horizontal bearing is the
/// azimuth origin.
fn skyview_direction(sun: vec3<f32>, azimuth: f32, view_zenith: f32) -> vec3<f32> {
    var sun_flat = vec2<f32>(sun.x, sun.y);
    let flat_length = length(sun_flat);
    // The sun straight overhead has no bearing; any meridian will do, and the
    // sky is a function of the zenith angle alone there.
    if flat_length < 1e-4 {
        sun_flat = vec2<f32>(1.0, 0.0);
    } else {
        sun_flat = sun_flat / flat_length;
    }
    let tangent = vec2<f32>(-sun_flat.y, sun_flat.x);
    let horizontal = sun_flat * cos(azimuth) + tangent * sin(azimuth);
    let s = sin(view_zenith);
    return vec3<f32>(horizontal * s, cos(view_zenith));
}

/// The signed azimuth of `dir` away from the sun's meridian, and its zenith
/// angle — the inverse of [`skyview_direction`].
fn skyview_coords(sun: vec3<f32>, dir: vec3<f32>) -> vec2<f32> {
    var sun_flat = vec2<f32>(sun.x, sun.y);
    let flat_length = length(sun_flat);
    if flat_length < 1e-4 {
        sun_flat = vec2<f32>(1.0, 0.0);
    } else {
        sun_flat = sun_flat / flat_length;
    }
    let tangent = vec2<f32>(-sun_flat.y, sun_flat.x);
    let horizontal = vec2<f32>(dir.x, dir.y);
    let azimuth = atan2(dot(horizontal, tangent), dot(horizontal, sun_flat));
    return vec2<f32>(azimuth, acos(clamp(dir.z, -1.0, 1.0)));
}

// --- the integral ----------------------------------------------------------

struct Scattered {
    /// In-scattered radiance reaching the ray's origin.
    luminance: vec3<f32>,
    /// The second-order transfer function of Hillaire §4: what one scattering
    /// event returns when the whole sphere around it is unit radiance. Only the
    /// multiple-scattering pass reads it.
    transfer: vec3<f32>,
};

/// March `dir` from a point `view_radius` out from the planet centre, gathering
/// sunlight scattered into the ray.
///
/// `psi_ms` is the converged multiple-scattering radiance for this altitude and
/// sun angle; pass zero while building it, or the table would feed itself.
/// `isotropic` swaps the Rayleigh/Mie phase functions for a uniform one, which
/// is what the multiple-scattering estimate integrates.
fn integrate_scattered(
    transmittance_lut: texture_2d<f32>,
    samp: sampler,
    view_radius: f32,
    dir: vec3<f32>,
    sun: vec3<f32>,
    ground_albedo: f32,
    steps: u32,
    psi_ms: vec3<f32>,
    isotropic: bool,
) -> Scattered {
    var result: Scattered;
    result.luminance = vec3<f32>(0.0);
    result.transfer = vec3<f32>(0.0);

    // The view is on the +Z axis of the local frame, so `mu` is the ray's Z.
    let mu = dir.z;
    let to_top = ray_sphere_distance(view_radius, mu, TOP_RADIUS_KM);
    if to_top < 0.0 {
        return result;
    }
    let to_ground = ray_sphere_distance(view_radius, mu, GROUND_RADIUS_KM);
    let hits_ground = to_ground > 0.0;
    let length_km = select(to_top, to_ground, hits_ground);

    let cos_theta = dot(dir, sun);
    let phase_r = select(rayleigh_phase(cos_theta), INV_4PI, isotropic);
    let phase_m = select(mie_phase(cos_theta), INV_4PI, isotropic);

    var throughput = vec3<f32>(1.0);
    let dt = length_km / f32(steps);
    for (var i = 0u; i < steps; i = i + 1u) {
        // Midpoint of the segment: a sample at the near edge biases every
        // exponential density downward and lightens the whole table.
        let t = (f32(i) + 0.5) * dt;
        // The sample point in the local frame, whose +Z is the view's up.
        let p = vec3<f32>(dir.xy * t, view_radius + dir.z * t);
        let r = length(p);
        let up = p / r;
        let m = medium_at(r - GROUND_RADIUS_KM);
        let step_transmittance = exp(-m.extinction * dt);

        let mu_sun = dot(up, sun);
        // The planet's own shadow: a sample whose line to the sun passes
        // through the ground gets no direct sunlight, which is the whole of
        // twilight.
        let lit = select(1.0, 0.0, ray_sphere_distance(r, mu_sun, GROUND_RADIUS_KM) > 0.0);
        let sun_transmittance = transmittance_to_top(transmittance_lut, samp, r, mu_sun);

        let direct = (m.rayleigh_scattering * phase_r + m.mie_scattering * phase_m)
            * sun_transmittance * lit;
        let multiple = (m.rayleigh_scattering + vec3<f32>(m.mie_scattering)) * psi_ms;
        let source = direct + multiple;

        // Analytic integration of the source term across the segment, rather
        // than source * dt: at the step counts a real-time table can afford,
        // the rectangle rule bands visibly where extinction is high.
        let integrated = (source - source * step_transmittance) / max(m.extinction, vec3<f32>(1e-9));
        result.luminance += throughput * integrated;

        let uniform_source = m.rayleigh_scattering + vec3<f32>(m.mie_scattering);
        let uniform_integrated =
            (uniform_source - uniform_source * step_transmittance) / max(m.extinction, vec3<f32>(1e-9));
        result.transfer += throughput * uniform_integrated;

        throughput *= step_transmittance;
    }

    if hits_ground && ground_albedo > 0.0 {
        let p = vec3<f32>(dir.xy * length_km, view_radius + dir.z * length_km);
        let up = p / length(p);
        let mu_sun = dot(up, sun);
        var irradiance = vec3<f32>(0.0);
        if mu_sun > 0.0 {
            irradiance += transmittance_to_top(transmittance_lut, samp, GROUND_RADIUS_KM, mu_sun)
                * mu_sun;
        }
        // Skylight on the ground. Hillaire's ground term is direct sun only,
        // which leaves every surface pitch black the moment the sun sets — and
        // twilight is exactly when a venue is looking at the ground. `psi_ms`
        // is the isotropic radiance the sky has converged to at this altitude,
        // so pi times it is the irradiance a flat ground receives from the
        // whole hemisphere, to the same order the term itself is good to.
        irradiance += psi_ms * PI;
        result.luminance += throughput * irradiance * ground_albedo / PI;
    }
    return result;
}
