// Auto-exposure: a log-luminance histogram of the frame, then one small
// dispatch that meters it and adapts the exposure over time (`post.rs`).
//
// The histogram is of scene-linear light before exposure, so the meter reads
// the scene, not its own previous answer.

const BINS: u32 = 128u;
/// log2 luminance of the lowest bin's lower edge. Anything darker is black:
/// it is counted, in the last slot, but never metered.
const LOG_MIN: f32 = -14.0;
/// Stops the bins cover, from LOG_MIN up.
const LOG_RANGE: f32 = 24.0;
/// Centre weight at the middle of the frame, relative to 1 at its edges.
/// Integer weights keep the histogram in atomics.
const CENTRE_WEIGHT: f32 = 4.0;

struct Meter {
    // x: auto (1) or manual (0), y: ev (compensation when auto, exposure
    // when manual), z: min ev, w: max ev.
    exposure: vec4<f32>,
    // x: seconds since the last adaptation, y: 1 to snap to the target,
    // z: brightening speed (1/s), w: darkening speed (1/s).
    adapt: vec4<f32>,
    // x: low percentile, y: high percentile of the metered band, z: log2 of
    // the key the band's mean is exposed to, w: centre weighting (0 or 1).
    meter: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cfg: Meter;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
// BINS bins, then the black weight.
@group(0) @binding(2) var<storage, read_write> histogram: array<atomic<u32>, 129>;
// x: current ev, y: target ev, z: 1 once valid, w: exposure multiplier.
@group(0) @binding(3) var<storage, read_write> state: vec4<f32>;

var<workgroup> local_bins: array<atomic<u32>, 129>;

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

/// One thread per 2×2 block, reading its top-left pixel: a quarter of the
/// pixels is plenty for a histogram and a quarter of the cost.
@compute @workgroup_size(16, 16)
fn histogram_main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) lindex: u32,
) {
    if lindex < 129u {
        atomicStore(&local_bins[lindex], 0u);
    }
    workgroupBarrier();
    let size = textureDimensions(scene_tex);
    let pixel = gid.xy * 2u;
    if pixel.x < size.x && pixel.y < size.y {
        let l = luminance(textureLoad(scene_tex, vec2<i32>(pixel), 0).rgb);
        var weight = 1.0;
        if cfg.meter.w > 0.5 {
            let uv = (vec2<f32>(pixel) + 0.5) / vec2<f32>(size) - 0.5;
            // Wider than tall: a stage is.
            let r2 = dot(uv * vec2<f32>(2.2, 2.8), uv * vec2<f32>(2.2, 2.8));
            weight = 1.0 + (CENTRE_WEIGHT - 1.0) * exp(-r2);
        }
        let w = u32(round(weight * 4.0));
        let lg = log2(max(l, 1e-20));
        if lg < LOG_MIN || l != l {
            atomicAdd(&local_bins[BINS], w);
        } else {
            let bin = min(u32((lg - LOG_MIN) / LOG_RANGE * f32(BINS)), BINS - 1u);
            atomicAdd(&local_bins[bin], w);
        }
    }
    workgroupBarrier();
    if lindex < 129u {
        let v = atomicLoad(&local_bins[lindex]);
        if v > 0u {
            atomicAdd(&histogram[lindex], v);
        }
    }
}

var<workgroup> bins: array<f32, 129>;

/// Meter the histogram, move the exposure toward it and clear the histogram
/// for the next frame.
@compute @workgroup_size(128)
fn adapt_main(@builtin(local_invocation_index) lindex: u32) {
    bins[lindex] = f32(atomicLoad(&histogram[lindex]));
    atomicStore(&histogram[lindex], 0u);
    if lindex == 0u {
        bins[BINS] = f32(atomicLoad(&histogram[BINS]));
        atomicStore(&histogram[BINS], 0u);
    }
    workgroupBarrier();
    if lindex != 0u {
        return;
    }

    let previous = state;
    let valid = previous.z > 0.5;
    let snap = cfg.adapt.y > 0.5 || !valid;
    var target_ev = cfg.exposure.y;
    var current = target_ev;

    if cfg.exposure.x > 0.5 {
        var total = 0.0;
        for (var i = 0u; i < BINS; i++) {
            total += bins[i];
        }
        // Held rather than metered when almost nothing is lit: a blackout
        // must not open the iris so the next cue arrives blinding.
        let lit = total > 0.002 * (total + bins[BINS]);
        if lit {
            let lo = cfg.meter.x * total;
            let hi = cfg.meter.y * total;
            var below = 0.0;
            var sum = 0.0;
            var weight = 0.0;
            for (var i = 0u; i < BINS; i++) {
                // The part of this bin's weight inside [lo, hi].
                let a = max(below, lo);
                let b = min(below + bins[i], hi);
                below += bins[i];
                if b > a {
                    let centre = LOG_MIN + (f32(i) + 0.5) / f32(BINS) * LOG_RANGE;
                    sum += (b - a) * centre;
                    weight += b - a;
                }
            }
            let mean = sum / max(weight, 1e-6);
            target_ev = clamp(cfg.meter.z - mean, cfg.exposure.z, cfg.exposure.w)
                + cfg.exposure.y;
        } else if valid {
            target_ev = previous.y;
        } else {
            target_ev = clamp(0.0, cfg.exposure.z, cfg.exposure.w) + cfg.exposure.y;
        }
        current = target_ev;
        if !snap {
            let delta = target_ev - previous.x;
            // A scene that got brighter closes the iris fast; one that got
            // darker opens it slowly, the way an eye does.
            let speed = select(cfg.adapt.w, cfg.adapt.z, delta > 0.0);
            current = previous.x + delta * (1.0 - exp(-cfg.adapt.x * speed));
        }
    }
    state = vec4<f32>(current, target_ev, 1.0, exp2(current));
}
