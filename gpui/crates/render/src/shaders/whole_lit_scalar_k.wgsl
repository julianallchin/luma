// External integration draft. This is concatenated after haze_cache.wgsl and
// the compact bindings in the review patch; it is not a standalone shader.

const WHOLE_COUNTER_BASE: u32 = 64u;
const WHOLE_COUNT: u32 = WHOLE_COUNTER_BASE;
const WHOLE_FAILED: u32 = WHOLE_COUNTER_BASE + 1u;
const WHOLE_REFRESHED: u32 = WHOLE_COUNTER_BASE + 2u;
const WHOLE_STALE: u32 = WHOLE_COUNTER_BASE + 3u;
const WHOLE_SUBMISSION: u32 = WHOLE_COUNTER_BASE + 4u;
const WHOLE_TEMPORAL: u32 = WHOLE_COUNTER_BASE + 5u;
const WHOLE_LOGICAL: u32 = WHOLE_COUNTER_BASE + 6u;
const WHOLE_PHYSICAL: u32 = WHOLE_COUNTER_BASE + 7u;
const WHOLE_EPOCH: u32 = WHOLE_COUNTER_BASE + 8u;
const WHOLE_GC_COUNT: u32 = WHOLE_COUNTER_BASE + 9u;
const WHOLE_SATURATED: u32 = WHOLE_COUNTER_BASE + 10u;
const WHOLE_PREFIX: u32 = WHOLE_COUNTER_BASE + 11u;
const WHOLE_GC_PHYSICAL: u32 = WHOLE_COUNTER_BASE + 12u;

const WHOLE_ARGS_BASE: u32 = 104u;
const WHOLE_ARGS_GC: u32 = WHOLE_ARGS_BASE;
const WHOLE_ARGS_REFRESH: u32 = WHOLE_ARGS_BASE + 3u;
const WHOLE_NONE: u32 = 0xFFFFFFFFu;

// CompactUniform integration:
// whole[0] = descriptor capacity, resid_rgb K-tail base in u32 words,
// resid_aux descriptor-tail base in u32 words, and packed
// period | phase<<8 | full<<16 | gc<<17 | allocation<<18.
// whole[1].x = renderer-local whole-transport submission id.
// whole[1].y = any resident transport dirty (uniform dispatch mode).

fn whole_period() -> u32 { return max(compact.whole[0].w & 0xFFu, 1u); }
fn whole_phase() -> u32 { return min((compact.whole[0].w >> 8u) & 0xFFu, whole_period() - 1u); }
fn whole_full() -> bool { return ((compact.whole[0].w >> 16u) & 1u) != 0u; }
fn whole_gc() -> bool { return ((compact.whole[0].w >> 17u) & 1u) != 0u; }
fn whole_allocates() -> bool { return ((compact.whole[0].w >> 18u) & 1u) != 0u; }
fn whole_dirty() -> bool { return compact.whole[1].y != 0u; }

fn whole_descriptor_plane(d: u32) -> u32 {
    return resid_aux[compact.whole[0].z + d];
}

fn whole_quad_index(lane: u32) -> u32 {
    let x = lane & 7u;
    let y = lane >> 3u;
    return (y >> 1u) * 4u + (x >> 1u);
}

fn whole_k_word(d: u32, lane: u32) -> u32 {
    return compact.whole[0].y + d * 8u + whole_quad_index(lane);
}

// Preserve a whole descriptor across a residual-only list rebuild only when
// both sides of the unique plane<->descriptor backpointer still agree.
fn whole_live_backpointer(plane: u32, old_mask: u32, backpointer: u32) -> u32 {
    if old_mask != 0u || backpointer == 0u || backpointer == RESID_EMPTY {
        return 0u;
    }
    let descriptor = backpointer - 1u;
    if descriptor >= compact.whole[0].x
        || descriptor >= atomicLoad(&resid_counters[WHOLE_COUNT])
        || whole_descriptor_plane(descriptor) != plane {
        return 0u;
    }
    return backpointer;
}

// Saturates rather than incrementing past capacity. WHOLE_SATURATED makes
// every later miss in this frame skip the CAS loop. CPU state keeps allocation
// disabled on later frames until topology evidence allows one GC attempt.
fn whole_allocate() -> u32 {
    if !whole_allocates() || compact.whole[0].x == 0u || atomicLoad(&resid_counters[WHOLE_SATURATED]) != 0u {
        return WHOLE_NONE;
    }
    var observed = atomicLoad(&resid_counters[WHOLE_COUNT]);
    loop {
        if observed >= compact.whole[0].x {
            atomicStore(&resid_counters[WHOLE_SATURATED], 1u);
            atomicAdd(&resid_counters[WHOLE_FAILED], 1u);
            return WHOLE_NONE;
        }
        let claimed = atomicCompareExchangeWeak(&resid_counters[WHOLE_COUNT], observed, observed + 1u);
        if claimed.exchanged { return observed; }
        observed = claimed.old_value;
    }
    return WHOLE_NONE;
}

// Called only after the classifier's mask==0/backpointer!=EMPTY test and after
// the exact shared scalar has been computed by all 32 lanes. One hot workgroup
// owns this plane, and the allocating frame consumes shared_sum directly.
fn whole_publish(plane: u32, shared_sum: f32) -> u32 {
    var descriptor = WHOLE_NONE;
    if cache_lane == 0u { descriptor = whole_allocate(); }
    descriptor = subgroupBroadcast(descriptor, 0u);
    if descriptor == WHOLE_NONE { return WHOLE_NONE; }
    if (cache_lane & 9u) == 0u {
        resid_rgb[whole_k_word(descriptor, cache_lane)] = shared_sum;
    }
    if cache_lane == 0u {
        resid_aux[compact.whole[0].z + descriptor] = plane;
        resid_planes[plane + 1u] = descriptor + 1u;
    }
    // No same-dispatch reader exists. The record becomes reusable only at the
    // next ordered dispatch boundary; see the ownership proof in README.md.
    return descriptor;
}

fn whole_descriptor_for_physical_group(g: u32) -> u32 {
    if whole_full() || whole_period() == 1u || whole_dirty() { return g; }
    return g * whole_period() + whole_phase();
}

fn whole_flat_group(group: vec3<u32>) -> u32 {
    return group.y * compact.point_target.z + group.x;
}

fn whole_full_block(block: u32, blocks_per_row: u32, output: vec2<u32>) -> bool {
    if blocks_per_row == 0u { return false; }
    let bx = block % blocks_per_row;
    let by = block / blocks_per_row;
    return bx < 0x1FFFFFFFu && by < 0x3FFFFFFFu
        && (bx + 1u) * 8u <= output.x
        && (by + 1u) * 4u <= output.y;
}

@compute @workgroup_size(32)
fn refresh_whole_scalar_k(@builtin(workgroup_id) group: vec3<u32>,
                          @builtin(local_invocation_index) lane: u32) {
    let physical = whole_flat_group(group);
    let physical_count = atomicLoad(&resid_counters[WHOLE_PHYSICAL]);
    if physical >= physical_count { return; }
    let descriptor = whole_descriptor_for_physical_group(physical);
    let prefix = atomicLoad(&resid_counters[WHOLE_PREFIX]);
    if descriptor >= prefix { return; }

    var plane = 0u;
    var valid = 0u;
    if lane == 0u {
        plane = whole_descriptor_plane(descriptor);
        valid = select(0u, 1u, (plane & 1u) == 0u
            && plane < arrayLength(&resid_planes) - 1u
            && resid_planes[plane] == 0u
            && resid_planes[plane + 1u] == descriptor + 1u);
    }
    plane = subgroupBroadcast(plane, 0u);
    valid = subgroupBroadcast(valid, 0u);
    if valid == 0u {
        if lane == 0u { atomicAdd(&resid_counters[WHOLE_STALE], 1u); }
        return;
    }

    let plane_id = plane >> 1u;
    let dense_stride = compact.params.x;
    if dense_stride == 0u { return; }
    let block = plane_id / dense_stride;
    let dense = plane_id % dense_stride;
    let output = textureDimensions(haze_exact_out);
    // These tests are subgroup-uniform and precede all light SoA reads and all
    // calls that contain subgroup shuffles.
    if dense >= 512u
        || !whole_full_block(block, compact.params.z, output) {
        if lane == 0u { atomicAdd(&resid_counters[WHOLE_STALE], 1u); }
        return;
    }
    let selected = whole_full()
        || whole_period() == 1u
        || descriptor % whole_period() == whole_phase();
    if !compact_dense_dirty(dense) && !selected { return; }
    let li = compact_light(dense);
    if li == 0xFFFFFFFFu {
        if lane == 0u { atomicAdd(&resid_counters[WHOLE_STALE], 1u); }
        return;
    }
    let block_xy = vec2<u32>(block % compact.params.z, block / compact.params.z);
    let pixel = block_xy * vec2<u32>(8u, 4u) + vec2<u32>(lane & 7u, lane >> 3u);
    cache_locate(pixel, lane);
    let frag = vec2<f32>(pixel) + 0.5;
    lazy_frag = frag;
    lazy_ray_ready = false;
    lazy_quad_ready = false;
    let ray = lazy_scene_ray();
    lazy_quad_prepare();
    let span = beam_span(li, ray);
    let shared_sum = quad_shared_scalar(li, ray, span);
    if (lane & 9u) == 0u {
        resid_rgb[whole_k_word(descriptor, lane)] = shared_sum;
    }
    if lane == 0u { atomicAdd(&resid_counters[WHOLE_REFRESHED], 1u); }
}

@compute @workgroup_size(128)
fn validate_whole_scalar_k_gc(@builtin(workgroup_id) group: vec3<u32>,
                              @builtin(local_invocation_index) lane: u32) {
    let physical = whole_flat_group(group);
    if physical >= atomicLoad(&resid_counters[WHOLE_GC_PHYSICAL]) { return; }
    let descriptor = physical * 128u + lane;
    let prefix = atomicLoad(&resid_counters[WHOLE_PREFIX]);
    if descriptor >= prefix { return; }
    let plane = whole_descriptor_plane(descriptor);
    // Validation reads plane words but only writes this invocation's unique
    // descriptor slot. No workgroup writes a plane during this pass.
    let valid = (plane & 1u) == 0u
        && plane < arrayLength(&resid_planes) - 1u
        && resid_planes[plane] == 0u
        && resid_planes[plane + 1u] == descriptor + 1u;
    if !valid {
        resid_aux[compact.whole[0].z + descriptor] = WHOLE_NONE;
        atomicAdd(&resid_counters[WHOLE_STALE], 1u);
    }
}

@compute @workgroup_size(128)
fn clear_whole_scalar_k_gc(@builtin(workgroup_id) group: vec3<u32>,
                           @builtin(local_invocation_index) lane: u32) {
    let physical = whole_flat_group(group);
    if physical >= atomicLoad(&resid_counters[WHOLE_GC_PHYSICAL]) { return; }
    let descriptor = physical * 128u + lane;
    let prefix = atomicLoad(&resid_counters[WHOLE_PREFIX]);
    if descriptor >= prefix { return; }
    let plane = whole_descriptor_plane(descriptor);
    if plane == WHOLE_NONE || (plane & 1u) != 0u
        || plane >= arrayLength(&resid_planes) - 1u {
        return;
    }
    // The validation pass left at most the unique descriptor named by this
    // backpointer. This pass performs no plane read, so duplicate stale
    // descriptors cannot race a read against this ordinary write.
    resid_planes[plane + 1u] = 0u;
}

@compute @workgroup_size(1)
fn prepare_whole_scalar_k() {
    let count = min(atomicLoad(&resid_counters[WHOLE_COUNT]), compact.whole[0].x);
    atomicStore(&resid_counters[WHOLE_PREFIX], count);
    atomicStore(&resid_counters[WHOLE_REFRESHED], 0u);
    atomicStore(&resid_counters[WHOLE_STALE], 0u);
    atomicStore(&resid_counters[WHOLE_SUBMISSION], compact.whole[1].x);
    atomicStore(&resid_counters[WHOLE_TEMPORAL], compact.whole[0].w & 0x3FFFFu);
    let refresh_groups = select(
        select(0u, (count + whole_period() - 1u - whole_phase()) / whole_period(), count > whole_phase()),
        count,
        whole_full() || whole_period() == 1u || whole_dirty(),
    );
    atomicStore(&resid_counters[WHOLE_LOGICAL], count);
    let columns = compact.point_target.z;
    let active_refresh = select(refresh_groups, 0u, whole_gc());
    atomicStore(&resid_counters[WHOLE_PHYSICAL], active_refresh);
    atomicStore(&resid_args[WHOLE_ARGS_REFRESH], min(active_refresh, columns));
    atomicStore(&resid_args[WHOLE_ARGS_REFRESH + 1u], (active_refresh + columns - 1u) / columns);
    atomicStore(&resid_args[WHOLE_ARGS_REFRESH + 2u], 1u);
    let gc_groups = (count + 127u) / 128u;
    let active_gc = select(0u, gc_groups, whole_gc());
    atomicStore(&resid_counters[WHOLE_GC_PHYSICAL], active_gc);
    atomicStore(&resid_args[WHOLE_ARGS_GC], min(active_gc, columns));
    atomicStore(&resid_args[WHOLE_ARGS_GC + 1u], (active_gc + columns - 1u) / columns);
    atomicStore(&resid_args[WHOLE_ARGS_GC + 2u], 1u);
}

@compute @workgroup_size(1)
fn reset_whole_after_gc() {
    if !whole_gc() { return; }
    atomicStore(&resid_counters[WHOLE_COUNT], 0u);
    atomicStore(&resid_counters[WHOLE_FAILED], 0u);
    atomicStore(&resid_counters[WHOLE_PREFIX], 0u);
    atomicStore(&resid_counters[WHOLE_SATURATED], 0u);
    atomicAdd(&resid_counters[WHOLE_EPOCH], 1u);
    atomicAdd(&resid_counters[WHOLE_GC_COUNT], 1u);
}
