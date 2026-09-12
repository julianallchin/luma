@group(2) @binding(0) var fog_grid: texture_3d<f32>;

// Ungoboed beams use deterministic visible-interval integration; gobos use MIS.
// Shadowed live rigs share distant broad-wash lighting in a 3D grid;
// only their four-metre source regions remain in the per-ray candidate list.
// The complementary smooth source/far windows sum to one. Captures with
// several subframes retain the full per-ray path for converged spatial detail.

// Independent integer stream for light selection: the integration jitter must
// not also choose the light, or the combined estimator becomes biased.
fn light_sample_hash(input: u32) -> u32 {
    var x = input;
    x = (x ^ (x >> 16u)) * 0x7feb352du;
    x = (x ^ (x >> 15u)) * 0x846ca68bu;
    return x ^ (x >> 16u);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle; no vertex buffer.
    let xy = vec2<f32>(f32((vi << 1u) & 2u) * 2.0 - 1.0, f32(vi & 2u) * 2.0 - 1.0);
    return vec4<f32>(xy, 0.0, 1.0);
}

struct HazeOutput {
    @location(0) exact: vec4<f32>,
    @location(1) sampled: vec4<f32>,
};

fn haze_at(frag: vec4<f32>) -> HazeOutput {
    var ray = scene_ray(frag.xy);
    let weight = haze.tuning.y;
    let density = haze.params.y;

    if density < 0.001 {
        return HazeOutput(vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * weight), vec4<f32>(0.0));
    }

    // The same local density governs scattering and optical depth on both
    // the camera path and the cached light path.
    let sigma = haze.depth.z;

    var scattered = vec3<f32>(0.0);
    var deterministic = vec3<f32>(0.0);
    let include_shared = GRID_FOG && haze.tiles.z > 1.5;
    var sampled = vec3<f32>(0.0);

    if haze.tiles.z > 0.5 {
    if !GRID_FOG { ray.medium = medium_ray(haze.medium, haze.camera_pos.xyz, ray.dir, ray.hit_dist); }

    // This pass renders at a fraction of output resolution; the light index
    // is defined in full-resolution pixels, so scale the fragment coordinate
    // rather than rebuilding the index per consumer resolution.
    var cursor = lights_along(frag.xy * haze.tiles.xy);
    // The second mask plane shares the same tiles and sorted light ids.
    // Rejections are computed once per tile instead of once per haze pixel.
    if GRID_FOG {
        cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
        cursor.bits = light_index_masks[cursor.base + cursor.word];
    }
    var li = 0u;
    let group_size = select(max(u32(haze.depth.w), 1u), 1u, GRID_FOG);
    let seed = light_sample_hash(u32(frag.x) + u32(frag.y) * 65537u
        + u32(haze.tuning.x + haze.tiles.w) * 747796405u);
    var grouped = 0u;
    var selected = 0u;
    var total_importance = 0.0;
    var selected_importance = 1.0;
    while light_index_next(&cursor, &li) {
        if NATIVE_DETERMINISTIC {
            deterministic += beam_scatter(li, ray, sigma);
            continue;
        }
        let rest = light_rest[li];
        if GRID_FOG && rest.gobo < 0.5 {
            if include_shared { deterministic += beam_scatter(li, ray, sigma); }
            continue;
        }
        if group_size > 1u && rest.wash >= FOG_BROAD_WASH && rest.gobo < 0.5 && rest.haze_gain > 0.0 {
            // Weighted reservoir in small groups. The inverse selection
            // probability preserves each emitter's expected radiance. Narrow
            // beams and gobos retain exhaustive integration.
            grouped += 1u;
            let u = f32(light_sample_hash(seed ^ (li * 2891336453u)) >> 8u) / 16777216.0;
            let importance = beam_importance(li, ray, sigma);
            total_importance += importance;
            if u * total_importance < importance {
                selected = li;
                selected_importance = importance;
            }
            if grouped == group_size {
                if total_importance > 0.0 {
                    sampled += beam_scatter(selected, ray, sigma) * (total_importance / selected_importance);
                }
                grouped = 0u;
                total_importance = 0.0;
            }
        } else {
            scattered += beam_scatter(li, ray, sigma);
        }
    }
    if total_importance > 0.0 {
        sampled += beam_scatter(selected, ray, sigma) * (total_importance / selected_importance);
    }

    }

    if include_shared {
        let size = vec3<f32>(textureDimensions(fog_grid));
        let uv = clamp(frag.xy / vec2<f32>(haze.transport.w, haze.transport.z), 0.5 / size.xy, 1.0 - 0.5 / size.xy);
        let span = ray.fog_span;
        let radial = sqrt(clamp((ray.hit_dist - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
        let z = (radial * (size.z - 1.0) + 0.5) / size.z;
        deterministic += textureSampleLevel(fog_grid, haze_noise_sampler, vec3<f32>(uv, z), 0.0).rgb * 256.0;
    }

    // Alpha carries linear view depth in metres so temporal rejection and the
    // composite's bilateral upsample have a distance-independent threshold.
    // Stochastic work is weighted by its sample count; deterministic work is
    // emitted once, including the full depth needed by the composite.
    let depth_weight = select(weight, select(0.0, 1.0, include_shared), GRID_FOG);
    return HazeOutput(vec4<f32>(scattered * weight + deterministic, ray.view_depth * depth_weight), vec4<f32>(sampled * weight, 0.0));
}

// `haze_at` restricted to what the deterministic compute kernel executes, so
// the cache bindings are reachable only from the compute entry points.
fn haze_at_native(frag: vec4<f32>) -> HazeOutput {
    var ray = scene_ray(frag.xy);
    quad_prepare(frag.xy, ray);
    let weight = haze.tuning.y;
    let density = haze.params.y;

    if density < 0.001 {
        return HazeOutput(vec4<f32>(0.0, 0.0, 0.0, ray.view_depth * weight), vec4<f32>(0.0));
    }

    let sigma = haze.depth.z;
    var deterministic = vec3<f32>(0.0);
    let include_shared = GRID_FOG && haze.tiles.z > 1.5;

    if haze.tiles.z > 0.5 {
        var cursor = lights_along(frag.xy * haze.tiles.xy);
        if GRID_FOG {
            cursor.base += light_index_params.grid.x * light_index_params.grid.y * LIGHT_INDEX_WORDS;
            cursor.bits = light_index_masks[cursor.base + cursor.word];
        }
        var li = 0u;
        while light_index_next(&cursor, &li) {
            deterministic += beam_scatter_cached(li, ray, sigma);
        }
    }

    if include_shared {
        let size = vec3<f32>(textureDimensions(fog_grid));
        let uv = clamp(frag.xy / vec2<f32>(haze.transport.w, haze.transport.z), 0.5 / size.xy, 1.0 - 0.5 / size.xy);
        let span = ray.fog_span;
        let radial = sqrt(clamp((ray.hit_dist - span.x) / max(span.y - span.x, 1e-5), 0.0, 1.0));
        let z = (radial * (size.z - 1.0) + 0.5) / size.z;
        deterministic += textureSampleLevel(fog_grid, haze_noise_sampler, vec3<f32>(uv, z), 0.0).rgb * 256.0;
    }

    let depth_weight = select(weight, select(0.0, 1.0, include_shared), GRID_FOG);
    return HazeOutput(vec4<f32>(deterministic, ray.view_depth * depth_weight), vec4<f32>(0.0));
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> HazeOutput {
    return haze_at(frag);
}

@group(2) @binding(1) var haze_exact_out: texture_storage_2d<rgba16float, write>;
@group(2) @binding(2) var haze_sampled_out: texture_storage_2d<rgba16float, write>;

override HAZE_GROUP_X: u32 = 8u;
override HAZE_GROUP_Y: u32 = 4u;

@compute @workgroup_size(HAZE_GROUP_X, HAZE_GROUP_Y)
fn compute_haze(@builtin(global_invocation_id) pixel: vec3<u32>,
                @builtin(local_invocation_index) lane: u32) {
    if any(pixel.xy >= textureDimensions(haze_exact_out)) { return; }
    cache_locate(pixel.xy, lane);
    let result = haze_at_native(vec4<f32>(vec2<f32>(pixel.xy) + 0.5, 0.0, 1.0));
    textureStore(haze_exact_out, vec2<i32>(pixel.xy), result.exact);
    textureStore(haze_sampled_out, vec2<i32>(pixel.xy), result.sampled);
}

@group(2) @binding(3) var<storage, read_write> work_counts: array<u32>;
var<workgroup> work_sum_a: array<vec4<u32>, HAZE_GROUP_X * HAZE_GROUP_Y>;
var<workgroup> work_sum_b: array<vec4<u32>, HAZE_GROUP_X * HAZE_GROUP_Y>;
var<workgroup> work_max_a: array<vec4<u32>, HAZE_GROUP_X * HAZE_GROUP_Y>;
var<workgroup> work_max_b: array<vec4<u32>, HAZE_GROUP_X * HAZE_GROUP_Y>;

// Histogram mode: the eight "sums" become the per-pair lit-interval call
// histogram and the eight "maxima" become a second set of sums, binning each
// pixel's shadowed-pair count (0, 1-2, 3-4, 5-6, 7-8, 9-12, 13-16, 17+).
var<private> pixel_hist: array<u32, 8>;

fn record_haze_work(group: vec3<u32>, lane: u32) {
    if HAZE_WORK_HIST {
        work_sum_a[lane] = vec4<u32>(haze_hist[0], haze_hist[1], haze_hist[2], haze_hist[3]);
        work_sum_b[lane] = vec4<u32>(haze_hist[4], haze_hist[5], haze_hist[6], haze_hist[7]);
        work_max_a[lane] = vec4<u32>(pixel_hist[0], pixel_hist[1], pixel_hist[2], pixel_hist[3]);
        work_max_b[lane] = vec4<u32>(pixel_hist[4], pixel_hist[5], pixel_hist[6], pixel_hist[7]);
    } else {
        work_sum_a[lane] = vec4<u32>(haze_work[0], haze_work[1], haze_work[2], haze_work[3]);
        work_sum_b[lane] = vec4<u32>(haze_work[4], haze_work[5], haze_work[6], haze_work[7]);
        work_max_a[lane] = work_sum_a[lane];
        work_max_b[lane] = work_sum_b[lane];
    }
    workgroupBarrier();
    for (var stride = HAZE_GROUP_X * HAZE_GROUP_Y / 2u; stride > 0u; stride /= 2u) {
        if lane < stride {
            work_sum_a[lane] += work_sum_a[lane + stride];
            work_sum_b[lane] += work_sum_b[lane + stride];
            if HAZE_WORK_HIST {
                work_max_a[lane] += work_max_a[lane + stride];
                work_max_b[lane] += work_max_b[lane + stride];
            } else {
                work_max_a[lane] = max(work_max_a[lane], work_max_a[lane + stride]);
                work_max_b[lane] = max(work_max_b[lane], work_max_b[lane + stride]);
            }
        }
        workgroupBarrier();
    }
    if lane == 0u {
        let columns = (textureDimensions(haze_exact_out).x + HAZE_GROUP_X - 1u) / HAZE_GROUP_X;
        let base = (group.y * columns + group.x) * 16u;
        for (var component = 0u; component < 4u; component += 1u) {
            work_counts[base + component] = work_sum_a[0][component];
            work_counts[base + 4u + component] = work_sum_b[0][component];
            work_counts[base + 8u + component] = work_max_a[0][component];
            work_counts[base + 12u + component] = work_max_b[0][component];
        }
    }
}

@compute @workgroup_size(HAZE_GROUP_X, HAZE_GROUP_Y)
fn compute_haze_counted(@builtin(global_invocation_id) pixel: vec3<u32>,
                @builtin(workgroup_id) group: vec3<u32>,
                @builtin(local_invocation_index) lane: u32) {
    // Out-of-bounds lanes still join the diagnostic reduction, with zero work.
    if all(pixel.xy < textureDimensions(haze_exact_out)) {
        cache_locate(pixel.xy, lane);
        let result = haze_at_native(vec4<f32>(vec2<f32>(pixel.xy) + 0.5, 0.0, 1.0));
        textureStore(haze_exact_out, vec2<i32>(pixel.xy), result.exact);
        textureStore(haze_sampled_out, vec2<i32>(pixel.xy), result.sampled);
        if HAZE_WORK_HIST {
            let pairs = haze_work[6];
            var bin = 7u;
            if pairs == 0u { bin = 0u; }
            else if pairs <= 2u { bin = 1u; }
            else if pairs <= 4u { bin = 2u; }
            else if pairs <= 6u { bin = 3u; }
            else if pairs <= 8u { bin = 4u; }
            else if pairs <= 12u { bin = 5u; }
            else if pairs <= 16u { bin = 6u; }
            pixel_hist[bin] += 1u;
        }
    }
    record_haze_work(group, lane);
}
