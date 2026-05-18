use super::*;
use crate::node_graph::oklab::{oklab_to_srgb, srgb_to_oklab};

/// Pull a scalar from a Signal at (n_idx, t_idx) with broadcast-safe indexing,
/// falling back to `default` when the signal is unavailable or out of range.
fn sample_scalar(sig: Option<&Signal>, n_idx: usize, t_idx: usize, default: f32) -> f32 {
    let Some(s) = sig else {
        return default;
    };
    if s.data.is_empty() {
        return default;
    }
    let ni = if s.n <= 1 { 0 } else { n_idx % s.n };
    let ti = if s.t <= 1 { 0 } else { t_idx % s.t };
    let idx = ni * (s.t * s.c) + ti * s.c;
    s.data.get(idx).copied().unwrap_or(default)
}

/// Splitmix-style hash for deterministic seed derivation.
fn hash64(seed: u64, v: u64) -> u64 {
    let mut x = seed ^ v.wrapping_mul(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn hash01(seed: u64, v: u64) -> f32 {
    (hash64(seed, v) as f64 / u64::MAX as f64) as f32
}

/// Physics tunables. Chosen so behavior is recognizable across bbox sizes —
/// forces scale with bbox diagonal where it matters.
const SUBSTEPS_PER_OUTPUT: usize = 4;
const WARMUP_SUBSTEPS: usize = 50;
const DAMPING_PER_SEC: f32 = 1.0;
const WALL_SPRING: f32 = 500.0;
const BROWNIAN_DRIVE: f32 = 2.0;
const REPULSION_SCALE: f32 = 0.5;

/// Per-axis Lissajous target periods, in seconds. Primes so the three
/// frequencies are mutually incommensurate; combined with per-seed random
/// phases this makes each seed's target trajectory dense in the bbox
/// (Kronecker-Weyl). Targets are scaled by wander_speed so the user knob
/// still controls overall exploration pace.
const TARGET_PERIODS_SEC: [f32; 3] = [17.0, 13.0, 11.0];
/// Spring constant pulling each seed toward its current target. Tuned so
/// the seed lags the target by a fraction of a period — the seed mostly
/// tracks but Brownian + repulsion still deflect it visibly.
const TARGET_SPRING: f32 = 1.5;

/// Cap on the chroma-rescue scale factor. With physics repulsion keeping
/// seeds well-separated, near-zero `c_now` regions are confined to thin
/// bisector slices, so we can boost much harder before flicker becomes
/// visible. A 10× cap still smooths the exact-bisector singularity (a
/// fixture passing the midplane of two complementary seeds momentarily
/// goes grey) without bottlenecking the rescue everywhere else.
const MAX_CHROMA_BOOST: f32 = 10.0;

#[inline]
fn apply_chroma_rescue(a: &mut f32, b: &mut f32, c_now: f32, c_target: f32, vibrance: f32) {
    let c_final = c_now + (c_target - c_now) * vibrance;
    if c_now > 1e-6 {
        let scale = (c_final / c_now).min(MAX_CHROMA_BOOST).max(0.0);
        *a *= scale;
        *b *= scale;
    }
}

/// Convert an sRGB stop to (L, a, b, alpha) in OKLab.
fn stop_to_lab(rgba: [f32; 4]) -> (f32, f32, f32, f32) {
    let (l, a, b) = srgb_to_oklab(rgba[0], rgba[1], rgba[2]);
    (l, a, b, rgba[3])
}

/// Sample a Stops function at u ∈ [0,1] and return OKLab with the same
/// vibrance-based chroma rescue used by the spatial blend. When num_points
/// is larger than the number of stops, interpolated seeds otherwise tend to
/// collapse to greys at complementary-color midpoints; the rescue keeps them
/// vibrant.
fn sample_stops_lab(stops: &Stops, u: f32, vibrance: f32) -> (f32, f32, f32, f32) {
    if stops.is_empty() {
        return (0.0, 0.0, 0.0, 1.0);
    }
    if stops.stops.len() == 1 {
        return stop_to_lab(stops.stops[0].1);
    }
    let u = u.clamp(0.0, 1.0);
    if u <= stops.stops[0].0 {
        return stop_to_lab(stops.stops[0].1);
    }
    let last_idx = stops.stops.len() - 1;
    if u >= stops.stops[last_idx].0 {
        return stop_to_lab(stops.stops[last_idx].1);
    }
    let mut lo_idx = 0usize;
    let mut hi_idx = last_idx;
    while hi_idx - lo_idx > 1 {
        let mid = (lo_idx + hi_idx) / 2;
        if stops.stops[mid].0 <= u {
            lo_idx = mid;
        } else {
            hi_idx = mid;
        }
    }
    let (t0, c_lo_rgba) = stops.stops[lo_idx];
    let (t1, c_hi_rgba) = stops.stops[hi_idx];
    let span = (t1 - t0).max(1e-6);
    let frac = ((u - t0) / span).clamp(0.0, 1.0);

    let (l_lo, a_lo, b_lo, alpha_lo) = stop_to_lab(c_lo_rgba);
    let (l_hi, a_hi, b_hi, alpha_hi) = stop_to_lab(c_hi_rgba);
    let c_lo = (a_lo * a_lo + b_lo * b_lo).sqrt();
    let c_hi = (a_hi * a_hi + b_hi * b_hi).sqrt();

    let l = l_lo + (l_hi - l_lo) * frac;
    let mut a = a_lo + (a_hi - a_lo) * frac;
    let mut b = b_lo + (b_hi - b_lo) * frac;
    let c_now = (a * a + b * b).sqrt();
    let c_target = c_lo + (c_hi - c_lo) * frac;
    apply_chroma_rescue(&mut a, &mut b, c_now, c_target, vibrance);
    let alpha = alpha_lo + (alpha_hi - alpha_lo) * frac;
    (l, a, b, alpha)
}

#[derive(Clone, Copy)]
struct Particle {
    pos: [f32; 3],
    vel: [f32; 3],
    /// Random per-axis phases for this seed's Lissajous target trajectory.
    target_phase: [f32; 3],
}

/// Run one velocity-Verlet-ish substep: Lissajous-target attraction (long-term
/// exploration), pairwise soft repulsion (no close approaches), bbox walls
/// (containment), stochastic Brownian forcing (chaos), and viscous damping.
/// `step_counter` is bumped so the deterministic Brownian RNG draws fresh
/// values per step. `sim_time` is the simulation clock used by the Lissajous
/// targets — independent of step_counter because the target trajectory must
/// be a continuous function of time.
#[allow(clippy::too_many_arguments)]
fn integrate_substep(
    particles: &mut [Particle],
    forces: &mut [[f32; 3]],
    step_counter: &mut u64,
    sim_time: f32,
    target_amp: [f32; 3],
    target_center: [f32; 3],
    speed: f32,
    bbox: [(f32, f32); 3],
    diag: f32,
    k_rep: f32,
    r_soft: f32,
    dt: f32,
    base_seed: u64,
) {
    let n = particles.len();
    for f in forces.iter_mut() {
        *f = [0.0, 0.0, 0.0];
    }

    // Lissajous target attraction. Each seed is pulled toward its current
    // target position, which traces a dense quasi-periodic trajectory through
    // the bbox over many seconds. This is what prevents the lattice
    // equilibrium that Brownian + repulsion alone would reach — the targets
    // are constantly moving so the seeds are constantly chasing.
    let omega = [
        std::f32::consts::TAU / TARGET_PERIODS_SEC[0],
        std::f32::consts::TAU / TARGET_PERIODS_SEC[1],
        std::f32::consts::TAU / TARGET_PERIODS_SEC[2],
    ];
    for i in 0..n {
        let p = &particles[i];
        let tx = target_center[0] + target_amp[0] * (omega[0] * sim_time + p.target_phase[0]).sin();
        let ty = target_center[1] + target_amp[1] * (omega[1] * sim_time + p.target_phase[1]).sin();
        let tz = target_center[2] + target_amp[2] * (omega[2] * sim_time + p.target_phase[2]).sin();
        forces[i][0] += TARGET_SPRING * (tx - p.pos[0]);
        forces[i][1] += TARGET_SPRING * (ty - p.pos[1]);
        forces[i][2] += TARGET_SPRING * (tz - p.pos[2]);
    }

    // Pairwise soft repulsion. Force magnitude is k_rep / r²; the (1/r³) form
    // below absorbs the unit-direction normalization, with r_soft² added to
    // the squared distance to cap the near-field force.
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = particles[i].pos[0] - particles[j].pos[0];
            let dy = particles[i].pos[1] - particles[j].pos[1];
            let dz = particles[i].pos[2] - particles[j].pos[2];
            let r2 = dx * dx + dy * dy + dz * dz + r_soft * r_soft;
            let inv_r3 = 1.0 / (r2 * r2.sqrt());
            let mag = k_rep * inv_r3;
            let fx = mag * dx;
            let fy = mag * dy;
            let fz = mag * dz;
            forces[i][0] += fx;
            forces[i][1] += fy;
            forces[i][2] += fz;
            forces[j][0] -= fx;
            forces[j][1] -= fy;
            forces[j][2] -= fz;
        }
    }

    // Walls only push back when a particle has crossed the bbox boundary.
    // Stiff spring (WALL_SPRING ≈ 500/s²) makes the bounce-back near-immediate.
    let ((min_x, max_x), (min_y, max_y), (min_z, max_z)) = (bbox[0], bbox[1], bbox[2]);
    for i in 0..n {
        let p = particles[i].pos;
        if p[0] < min_x {
            forces[i][0] += WALL_SPRING * (min_x - p[0]);
        }
        if p[0] > max_x {
            forces[i][0] -= WALL_SPRING * (p[0] - max_x);
        }
        if p[1] < min_y {
            forces[i][1] += WALL_SPRING * (min_y - p[1]);
        }
        if p[1] > max_y {
            forces[i][1] -= WALL_SPRING * (p[1] - max_y);
        }
        if p[2] < min_z {
            forces[i][2] += WALL_SPRING * (min_z - p[2]);
        }
        if p[2] > max_z {
            forces[i][2] -= WALL_SPRING * (p[2] - max_z);
        }
    }

    // Brownian forcing — only source of energy in the system. Force scaling
    // gives velocity ∝ wander_speed at steady state (after damping).
    let brownian_force_mag = BROWNIAN_DRIVE * diag * speed / dt.sqrt().max(1e-6);
    for i in 0..n {
        let h = step_counter.wrapping_mul(n as u64).wrapping_add(i as u64);
        let rx = hash01(base_seed ^ 0xB1, h) * 2.0 - 1.0;
        let ry = hash01(base_seed ^ 0xB2, h) * 2.0 - 1.0;
        let rz = hash01(base_seed ^ 0xB3, h) * 2.0 - 1.0;
        forces[i][0] += rx * brownian_force_mag;
        forces[i][1] += ry * brownian_force_mag;
        forces[i][2] += rz * brownian_force_mag;
    }

    // Semi-implicit Euler with viscous damping on velocity.
    let damp = (1.0 - DAMPING_PER_SEC * dt).max(0.0);
    for i in 0..n {
        particles[i].vel[0] = particles[i].vel[0] * damp + forces[i][0] * dt;
        particles[i].vel[1] = particles[i].vel[1] * damp + forces[i][1] * dt;
        particles[i].vel[2] = particles[i].vel[2] * damp + forces[i][2] * dt;
        particles[i].pos[0] += particles[i].vel[0] * dt;
        particles[i].pos[1] += particles[i].vel[1] * dt;
        particles[i].pos[2] += particles[i].vel[2] * dt;
    }

    *step_counter = step_counter.wrapping_add(1);
}

pub async fn run_node(
    node: &NodeInstance,
    ctx: &NodeExecutionContext<'_>,
    state: &mut ExecutionState,
) -> Result<bool, String> {
    let incoming_edges = ctx.incoming_edges;
    let context = ctx.graph_context;
    if node.type_id != "soft_voronoi" {
        return Ok(false);
    }

    let input_edges = incoming_edges
        .get(node.id.as_str())
        .cloned()
        .unwrap_or_default();
    let selection_edge = input_edges.iter().find(|e| e.to_port == "selection");
    let stops_edge = input_edges.iter().find(|e| e.to_port == "stops");
    let num_points_edge = input_edges.iter().find(|e| e.to_port == "num_points");
    let softness_edge = input_edges.iter().find(|e| e.to_port == "softness");
    let vibrance_edge = input_edges.iter().find(|e| e.to_port == "vibrance");
    let speed_edge = input_edges.iter().find(|e| e.to_port == "wander_speed");

    let Some(selection_edge) = selection_edge else {
        return Ok(true);
    };
    let Some(selections) = state.selections.get(&(
        selection_edge.from_node.clone(),
        selection_edge.from_port.clone(),
    )) else {
        return Ok(true);
    };

    let Some(stops_edge) = stops_edge else {
        return Ok(true);
    };
    let Some(stops) = state
        .stops_outputs
        .get(&(stops_edge.from_node.clone(), stops_edge.from_port.clone()))
    else {
        return Ok(true);
    };
    if stops.is_empty() {
        return Ok(true);
    }

    let num_points_signal = num_points_edge.and_then(|e| {
        state
            .signal_outputs
            .get(&(e.from_node.clone(), e.from_port.clone()))
    });
    let softness_signal = softness_edge.and_then(|e| {
        state
            .signal_outputs
            .get(&(e.from_node.clone(), e.from_port.clone()))
    });
    let vibrance_signal = vibrance_edge.and_then(|e| {
        state
            .signal_outputs
            .get(&(e.from_node.clone(), e.from_port.clone()))
    });
    let speed_signal = speed_edge.and_then(|e| {
        state
            .signal_outputs
            .get(&(e.from_node.clone(), e.from_port.clone()))
    });

    let num_points_param = node
        .params
        .get("num_points")
        .and_then(|v| v.as_f64())
        .unwrap_or(6.0) as f32;
    let num_points = (sample_scalar(num_points_signal, 0, 0, num_points_param)
        .clamp(1.0, 64.0)
        .round()) as usize;
    let softness_param = node
        .params
        .get("softness")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.3) as f32;
    let vibrance_param = node
        .params
        .get("vibrance")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.6) as f32;
    let speed_param = node
        .params
        .get("wander_speed")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.3) as f32;
    let seed_offset = node
        .params
        .get("seed_offset")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;

    let duration = (context.end_time - context.start_time).max(0.001);
    let t_steps = ((duration * SIMULATION_RATE).ceil() as usize).max(PREVIEW_LENGTH);

    // Collect every selected fixture across all groups into one flat list.
    let items: Vec<&SelectableItem> = selections.iter().flat_map(|sel| sel.items.iter()).collect();
    let n = items.len();
    if n == 0 {
        state.signal_outputs.insert(
            (node.id.clone(), "out".into()),
            Signal {
                n: 0,
                t: t_steps,
                c: 4,
                data: Vec::new(),
            },
        );
        return Ok(true);
    }

    // Bounding box + diagonal.
    let (mut min_x, mut max_x) = (items[0].pos.0, items[0].pos.0);
    let (mut min_y, mut max_y) = (items[0].pos.1, items[0].pos.1);
    let (mut min_z, mut max_z) = (items[0].pos.2, items[0].pos.2);
    for it in &items {
        min_x = min_x.min(it.pos.0);
        max_x = max_x.max(it.pos.0);
        min_y = min_y.min(it.pos.1);
        max_y = max_y.max(it.pos.1);
        min_z = min_z.min(it.pos.2);
        max_z = max_z.max(it.pos.2);
    }
    let range_x = (max_x - min_x).max(1e-3);
    let range_y = (max_y - min_y).max(1e-3);
    let range_z = (max_z - min_z).max(1e-3);
    // Softmin temperature uses the true fixture-span diagonal so the user's
    // `softness` value keeps its perceptual meaning regardless of inflation.
    let diag = (range_x * range_x + range_y * range_y + range_z * range_z).sqrt();
    let cx = (min_x + max_x) * 0.5;
    let cy = (min_y + max_y) * 0.5;
    let cz = (min_z + max_z) * 0.5;

    // Inflated *physics* bbox: any axis much smaller than the longest gets
    // padded so seeds have orthogonal room to deflect past each other. A 1D
    // fixture line (long X, ~0 Y/Z) otherwise traps repelling particles into
    // a 1D crowd that can't permute. Fixtures stay at their true positions;
    // seeds wander through the inflated volume and the softmin still uses
    // real distances, so off-axis excursions just reduce a seed's influence
    // for a beat.
    const MIN_AXIS_RATIO: f32 = 0.3;
    let max_range = range_x.max(range_y).max(range_z);
    let phys_range_x = range_x.max(max_range * MIN_AXIS_RATIO);
    let phys_range_y = range_y.max(max_range * MIN_AXIS_RATIO);
    let phys_range_z = range_z.max(max_range * MIN_AXIS_RATIO);
    let phys_min_x = cx - phys_range_x * 0.5;
    let phys_max_x = cx + phys_range_x * 0.5;
    let phys_min_y = cy - phys_range_y * 0.5;
    let phys_max_y = cy + phys_range_y * 0.5;
    let phys_min_z = cz - phys_range_z * 0.5;
    let phys_max_z = cz + phys_range_z * 0.5;
    let phys_diag =
        (phys_range_x * phys_range_x + phys_range_y * phys_range_y + phys_range_z * phys_range_z)
            .sqrt();

    // Characteristic minimum spacing for K seeds well-distributed in the
    // inflated volume — that's the space the seeds actually live in.
    let min_spacing = phys_diag / (num_points as f32 + 1.0).cbrt();
    let r_soft = min_spacing * 0.25;
    let k_rep = REPULSION_SCALE * min_spacing.powi(3);

    // Derive a deterministic base seed from instance_seed (when supplied) and
    // node id, mixed with the per-node seed_offset param.
    let mut node_hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&node.id, &mut node_hasher);
    let node_id_hash = std::hash::Hasher::finish(&node_hasher);
    let instance_seed = context.instance_seed.unwrap_or(0);
    let base_seed = hash64(node_id_hash ^ seed_offset, instance_seed);

    // Pre-bake seed colors in OKLab. K = num_points.
    //  - If K matches stops.len() exactly, use the stops 1:1 (each seed gets
    //    one stop's color). No interpolation, no rescue needed.
    //  - Otherwise sample the Stops function at K evenly-spaced positions
    //    with the chroma rescue so interpolated seeds don't collapse to grey
    //    at complementary-color midpoints. Uses the static `vibrance` param;
    //    the per-time vibrance signal only modulates the spatial blend layer.
    let lab_palette: Vec<(f32, f32, f32, f32)> = if num_points == stops.stops.len() {
        stops
            .stops
            .iter()
            .map(|(_, rgba)| stop_to_lab(*rgba))
            .collect()
    } else {
        (0..num_points)
            .map(|k| {
                let u = if num_points == 1 {
                    0.0
                } else {
                    k as f32 / (num_points - 1) as f32
                };
                sample_stops_lab(stops, u, vibrance_param)
            })
            .collect()
    };
    let lab_chroma: Vec<f32> = lab_palette
        .iter()
        .map(|(_, a, b, _)| (a * a + b * b).sqrt())
        .collect();

    // Init particles uniformly within the inflated physics volume, with a
    // random Lissajous phase per axis drawn from the deterministic base seed.
    let mut particles: Vec<Particle> = (0..num_points)
        .map(|k| {
            let rx = hash01(base_seed, (k as u64) * 6);
            let ry = hash01(base_seed, (k as u64) * 6 + 1);
            let rz = hash01(base_seed, (k as u64) * 6 + 2);
            let px = hash01(base_seed, (k as u64) * 6 + 3) * std::f32::consts::TAU;
            let py = hash01(base_seed, (k as u64) * 6 + 4) * std::f32::consts::TAU;
            let pz = hash01(base_seed, (k as u64) * 6 + 5) * std::f32::consts::TAU;
            Particle {
                pos: [
                    phys_min_x + rx * phys_range_x,
                    phys_min_y + ry * phys_range_y,
                    phys_min_z + rz * phys_range_z,
                ],
                vel: [0.0, 0.0, 0.0],
                target_phase: [px, py, pz],
            }
        })
        .collect();

    // Lissajous targets live in the *real* fixture bbox so seeds spend most
    // of their time near actual fixtures, not in the inflated periphery.
    let target_center = [cx, cy, cz];
    let target_amp = [range_x * 0.5, range_y * 0.5, range_z * 0.5];

    let bbox = [
        (phys_min_x, phys_max_x),
        (phys_min_y, phys_max_y),
        (phys_min_z, phys_max_z),
    ];
    let dt_output = duration / ((t_steps - 1).max(1) as f32);
    let dt = dt_output / SUBSTEPS_PER_OUTPUT as f32;
    let mut forces = vec![[0.0f32; 3]; num_points];
    let mut step_counter: u64 = 0;

    // Warmup runs backwards in sim_time so the seeds reach ti=0 already
    // tracking their targets rather than coasting in from rest.
    let warmup_speed = speed_param.max(0.5);
    for w in 0..WARMUP_SUBSTEPS {
        let sim_time = -((WARMUP_SUBSTEPS - w) as f32) * dt;
        integrate_substep(
            &mut particles,
            &mut forces,
            &mut step_counter,
            sim_time,
            target_amp,
            target_center,
            warmup_speed,
            bbox,
            phys_diag,
            k_rep,
            r_soft,
            dt,
            base_seed,
        );
    }

    // Record t=0, then run SUBSTEPS_PER_OUTPUT substeps before each subsequent
    // recording. Stored layout: [ti * K + k] * 3.
    let mut seed_pos = vec![0.0f32; num_points * t_steps * 3];
    let record = |seed_pos: &mut [f32], particles: &[Particle], ti: usize| {
        for (k, p) in particles.iter().enumerate() {
            let base = (ti * particles.len() + k) * 3;
            seed_pos[base] = p.pos[0];
            seed_pos[base + 1] = p.pos[1];
            seed_pos[base + 2] = p.pos[2];
        }
    };
    record(&mut seed_pos, &particles, 0);
    for ti in 1..t_steps {
        let speed = sample_scalar(speed_signal, 0, ti, speed_param).max(0.0);
        for sub in 0..SUBSTEPS_PER_OUTPUT {
            let sim_time = (ti - 1) as f32 * dt_output + (sub as f32 + 1.0) * dt;
            integrate_substep(
                &mut particles,
                &mut forces,
                &mut step_counter,
                sim_time,
                target_amp,
                target_center,
                speed,
                bbox,
                phys_diag,
                k_rep,
                r_soft,
                dt,
                base_seed,
            );
        }
        record(&mut seed_pos, &particles, ti);
    }

    // Main per-fixture, per-time blend loop.
    let mut data = vec![0.0f32; n * t_steps * 4];
    let mut weights = vec![0.0f32; num_points];
    for ti in 0..t_steps {
        let softness_raw = sample_scalar(softness_signal, 0, ti, softness_param).max(0.001);
        // Allow heavy over-saturation; the chroma-rescue cap inside
        // apply_chroma_rescue prevents flicker if vibrance gets cranked.
        let vibrance = sample_scalar(vibrance_signal, 0, ti, vibrance_param).clamp(0.0, 10.0);
        // softness param is a fraction of the bbox diagonal — turn it into an
        // absolute temperature in world distance units.
        let temperature = (softness_raw * diag).max(1e-4);

        for (ni, it) in items.iter().enumerate() {
            // Soft-min weights via -d/T → softmax.
            // Subtract min for numeric stability before exponentiating.
            let mut min_d = f32::INFINITY;
            for k in 0..num_points {
                let base = (ti * num_points + k) * 3;
                let dx = it.pos.0 - seed_pos[base];
                let dy = it.pos.1 - seed_pos[base + 1];
                let dz = it.pos.2 - seed_pos[base + 2];
                let d = (dx * dx + dy * dy + dz * dz).sqrt();
                if d < min_d {
                    min_d = d;
                }
            }
            let mut wsum = 0.0f32;
            for k in 0..num_points {
                let base = (ti * num_points + k) * 3;
                let dx = it.pos.0 - seed_pos[base];
                let dy = it.pos.1 - seed_pos[base + 1];
                let dz = it.pos.2 - seed_pos[base + 2];
                let d = (dx * dx + dy * dy + dz * dz).sqrt();
                let w = (-(d - min_d) / temperature).exp();
                weights[k] = w;
                wsum += w;
            }
            if wsum > 0.0 {
                for w in &mut weights {
                    *w /= wsum;
                }
            } else {
                weights[0] = 1.0;
                for w in weights.iter_mut().skip(1) {
                    *w = 0.0;
                }
            }

            // Weighted OKLab blend.
            let mut l_out = 0.0f32;
            let mut a_out = 0.0f32;
            let mut b_out = 0.0f32;
            let mut alpha_out = 0.0f32;
            let mut chroma_target = 0.0f32;
            for k in 0..num_points {
                let (l, a, b, alpha) = lab_palette[k];
                let w = weights[k];
                l_out += w * l;
                a_out += w * a;
                b_out += w * b;
                alpha_out += w * alpha;
                chroma_target += w * lab_chroma[k];
            }

            // Chroma rescue: scale (a,b) so the blended chroma magnitude is
            // lerped between the natural blend (vibrance=0) and the weight-
            // averaged input chroma (vibrance=1). The cap inside
            // apply_chroma_rescue prevents the divide-by-near-zero flicker at
            // bisectors between complementary seeds — without it tiny weight
            // perturbations would flip colors hard.
            let chroma_now = (a_out * a_out + b_out * b_out).sqrt();
            apply_chroma_rescue(&mut a_out, &mut b_out, chroma_now, chroma_target, vibrance);

            let (r, g, b) = oklab_to_srgb(l_out, a_out, b_out);
            let base = (ni * t_steps + ti) * 4;
            data[base] = r.clamp(0.0, 1.0);
            data[base + 1] = g.clamp(0.0, 1.0);
            data[base + 2] = b.clamp(0.0, 1.0);
            data[base + 3] = alpha_out.clamp(0.0, 1.0);
        }
    }

    state.signal_outputs.insert(
        (node.id.clone(), "out".into()),
        Signal {
            n,
            t: t_steps,
            c: 4,
            data,
        },
    );
    Ok(true)
}

pub fn get_node_types() -> Vec<NodeTypeDef> {
    vec![NodeTypeDef {
        id: "soft_voronoi".into(),
        name: "Soft Voronoi".into(),
        description: Some(
            "K wandering seed points within the selection's bounding volume, blended in OKLab to a per-fixture color. Softness controls the softmin temperature (fraction of bbox diagonal). Vibrance lerps the blended OKLab chroma magnitude toward the weight-averaged input chroma so blends don't go muddy."
                .into(),
        ),
        category: Some("Spatial".into()),
        inputs: vec![
            PortDef {
                id: "selection".into(),
                name: "Selection".into(),
                port_type: PortType::Selection,
            },
            PortDef {
                id: "stops".into(),
                name: "Stops".into(),
                port_type: PortType::Stops,
            },
            PortDef {
                id: "num_points".into(),
                name: "Points".into(),
                port_type: PortType::Signal,
            },
            PortDef {
                id: "softness".into(),
                name: "Softness".into(),
                port_type: PortType::Signal,
            },
            PortDef {
                id: "vibrance".into(),
                name: "Vibrance".into(),
                port_type: PortType::Signal,
            },
            PortDef {
                id: "wander_speed".into(),
                name: "Wander Speed".into(),
                port_type: PortType::Signal,
            },
        ],
        outputs: vec![PortDef {
            id: "out".into(),
            name: "Color".into(),
            port_type: PortType::Signal,
        }],
        params: vec![
            ParamDef {
                id: "num_points".into(),
                name: "Points".into(),
                param_type: ParamType::Number,
                default_number: Some(6.0),
                default_text: None,
            },
            ParamDef {
                id: "wander_speed".into(),
                name: "Wander Speed".into(),
                param_type: ParamType::Number,
                default_number: Some(0.3),
                default_text: None,
            },
            ParamDef {
                id: "softness".into(),
                name: "Softness".into(),
                param_type: ParamType::Number,
                default_number: Some(0.3),
                default_text: None,
            },
            ParamDef {
                id: "vibrance".into(),
                name: "Vibrance".into(),
                param_type: ParamType::Number,
                default_number: Some(0.6),
                default_text: None,
            },
            ParamDef {
                id: "seed_offset".into(),
                name: "Seed Offset".into(),
                param_type: ParamType::Number,
                default_number: Some(0.0),
                default_text: None,
            },
        ],
    }]
}
