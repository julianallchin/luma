// A top-down height field of the stage and how much sky a point under it sees.
//
// Two orthographic depth maps looked at the venue from straight above and from
// straight below, with the ground plane left out. Together they store one slab
// per texel: the lowest and the highest surface over that point of the ground.
// A deck on legs is a thin slab with open air under it; a block or a leg is a
// slab that reaches the ground. That is enough to tell an overhang from a wall,
// which a single height map cannot: under a deck the sky is still visible
// sideways, between the deck's underside and whatever stands on the ground.
//
// `sky_visibility` marches that field along a fixed set of azimuths: standing
// geometry raises the horizon, overhangs hide a band of elevations above it.
// What is left is weighted by the receiver's cosine lobe.

struct HeightParams {
    // xy: world XY of the map's minimum corner. z: texel size in metres.
    // w: top of the depth range; the bottom is the ground plane, z = 0.
    origin: vec4<f32>,
    // xy: resolution in texels. z: ground-map texel size in metres. w: ground
    // map mip count.
    size: vec4<f32>,
    // xyz: unit direction toward the sun. w: 1 when there is a sun.
    sun: vec4<f32>,
    // x: first march radius, y: growth per step, z: surface bias in metres.
    march: vec4<f32>,
};

const SKY_AZIMUTHS: u32 = 8u;
const SKY_STEPS: u32 = 12u;
const HALF_PI: f32 = 1.5707963;
const TAU: f32 = 6.2831853;
// Bottom of an empty column: no overhang anywhere above the ground.
const NO_SLAB: f32 = 1.0e9;
// Height margin for slab compares on a receiver that is already lifted.
const SLAB_EPSILON: f32 = 0.005;

@group(0) @binding(0) var<uniform> height: HeightParams;
@group(0) @binding(1) var heights: texture_2d<f32>;

/// (top, bottom) of the stage slab over world `xy`. Off the map there is only
/// ground: nothing stands up and nothing hangs over.
fn slab_at(xy: vec2<f32>) -> vec2<f32> {
    let texel = (xy - height.origin.xy) / height.origin.z;
    if any(texel < vec2<f32>(0.0)) || any(texel >= height.size.xy) {
        return vec2<f32>(0.0, NO_SLAB);
    }
    return textureLoad(heights, vec2<i32>(texel), 0).xy;
}

/// Primitive of the cosine lobe `max(0, a cos(t) + b sin(t)) cos(t)` over
/// elevation `t`, the solid-angle weight of one azimuth slice for a receiver
/// whose normal has horizontal component `a` along the slice and vertical `b`.
fn lobe_primitive(a: f32, b: f32, t: f32) -> f32 {
    let s = sin(t);
    return a * (0.5 * t + 0.25 * sin(2.0 * t)) + 0.5 * b * s * s;
}

/// The lobe's weight between elevations `low` and `high`, restricted to the
/// part of the slice the receiver faces.
fn lobe(a: f32, b: f32, low: f32, high: f32) -> f32 {
    // a cos t + b sin t = r cos(t - alpha): positive within half a turn of alpha.
    let alpha = atan2(b, a);
    let lo = max(low, alpha - HALF_PI);
    let hi = min(high, alpha + HALF_PI);
    if hi <= lo {
        return 0.0;
    }
    return lobe_primitive(a, b, hi) - lobe_primitive(a, b, lo);
}

/// Fraction of the upper hemisphere's cosine-weighted sky a receiver at `p`
/// with normal `n` sees past the stage. `rotation` (0..1) turns the azimuth
/// set and stretches the radii; per-pixel values are denoised afterwards.
///
/// Per azimuth, what stands on the ground below the point hides everything
/// under its top: one horizon, as in horizon-based occlusion. An overhang
/// hides a band: from the underside of its far edge to the top of its near
/// edge, or to the zenith when the point is under it. Bands are summed, not
/// merged, so two overhangs that overlap in one direction count twice; the
/// sum is clamped to the whole lobe.
///
/// `p` is already off its own surface (the caller lifts it along the
/// normal), so the height compares only absorb depth rounding, and they lean
/// toward "under": a wall that meets an overhang is covered right up to it.
/// A margin the other way counted the top few centimetres of every beam
/// under a deck as open sky, a bright band that stepped with the texels.
fn sky_visibility(p: vec3<f32>, n: vec3<f32>, rotation: f32) -> f32 {
    let bias = SLAB_EPSILON;
    let own = slab_at(p.xy);
    let covered = own.y > p.z - bias && own.y < NO_SLAB;
    var visible = 0.0;
    var total = 0.0;
    for (var i = 0u; i < SKY_AZIMUTHS; i = i + 1u) {
        let phi = (f32(i) + rotation) * (TAU / f32(SKY_AZIMUTHS));
        let d = vec2<f32>(cos(phi), sin(phi));
        let a = dot(n.xy, d);
        let full = lobe(a, n.z, 0.0, HALF_PI);
        var low = 0.0;
        // The overhang run being crossed: the top of its near edge and the
        // underside of the furthest sample so far.
        var in_run = covered;
        var run_top = HALF_PI;
        var run_bottom = HALF_PI;
        var bands = 0.0;
        var r = height.march.x * pow(height.march.y, rotation - 0.5);
        for (var s = 0u; s < SKY_STEPS; s = s + 1u) {
            let slab = slab_at(p.xy + d * r);
            let overhang = slab.y > p.z - bias && slab.y < NO_SLAB;
            if overhang {
                let top = atan2(slab.x - p.z, r);
                if !in_run {
                    run_top = top;
                }
                in_run = true;
                run_bottom = atan2(max(slab.y - p.z, 0.0), r);
            } else {
                if in_run {
                    bands += lobe(a, n.z, run_bottom, run_top);
                    in_run = false;
                }
                if slab.x > p.z + bias {
                    low = max(low, atan2(slab.x - p.z, r));
                }
            }
            r *= height.march.y;
        }
        if in_run {
            bands += lobe(a, n.z, run_bottom, run_top);
        }
        total += full;
        visible += max(full - lobe(a, n.z, 0.0, low) - bands, 0.0);
    }
    // A receiver facing (nearly) straight down has almost no upper-hemisphere
    // lobe and no sky term to scale. The floor on `total` takes its value
    // smoothly to zero there: a ratio of two tiny numbers is noise, and the
    // unweighted opening this used to return lit a bright line along every
    // crease under a deck once the denoiser spread it onto the wall beside
    // it. An open floor's `total` is 4, so the floor never touches it.
    return saturate(visible / max(total, 0.02));
}
