// A bounded rotating window retains the one-writer-per-block invariant while
// limiting discovery independently of candidate density. Payload allocation is
// one immutable base plus the bounded per-frame fill ticket.
@group(2) @binding(0) var<storage, read> cache_candidates: array<u32>;
@group(2) @binding(1) var<storage, read_write> cache_words: array<atomic<u32>>;
@group(2) @binding(2) var<storage, read_write> cache_args: array<atomic<u32>>;

const CACHE_PAYLOAD_USED: u32 = 0u;
const CACHE_FILL_COUNT: u32 = 1u;
const CACHE_STOPPED_BLOCKS: u32 = 2u;
const CACHE_ACTIVE: u32 = 16u;
const CACHE_BLOCK_COUNT: u32 = 17u;
const CACHE_DIRECTORY_OFFSET: u32 = 21u;
const CACHE_FILL_OFFSET: u32 = 22u;
const CACHE_PAYLOAD_CAPACITY: u32 = 23u;
const CACHE_FILL_BUDGET: u32 = 24u;
const CACHE_PLAN_CURSOR: u32 = 25u;
const PLAN_BLOCK_WINDOW: u32 = 2048u;

@compute @workgroup_size(64, 1, 1)
fn plan_visibility(@builtin(global_invocation_id) id: vec3<u32>) {
    if atomicLoad(&cache_words[CACHE_ACTIVE]) == 0u { return; }
    let block_count = atomicLoad(&cache_words[CACHE_BLOCK_COUNT]);
    let cursor = min(atomicLoad(&cache_words[CACHE_PLAN_CURSOR]), block_count - 1u);
    let window_count = min(PLAN_BLOCK_WINDOW, block_count - cursor);
    if id.x >= window_count { return; }

    let payload_base = atomicLoad(&cache_words[CACHE_PAYLOAD_USED]);
    let payload_capacity = atomicLoad(&cache_words[CACHE_PAYLOAD_CAPACITY]);
    let limit = min(
        atomicLoad(&cache_words[CACHE_FILL_BUDGET]),
        payload_capacity - min(payload_base, payload_capacity),
    );
    if limit == 0u || atomicLoad(&cache_words[CACHE_FILL_COUNT]) >= limit { return; }

    let block_index = cursor + id.x;
    let directory_offset = atomicLoad(&cache_words[CACHE_DIRECTORY_OFFSET]);
    let fill_offset = atomicLoad(&cache_words[CACHE_FILL_OFFSET]);
    let words = (light_index_params.counts.x + 31u) / 32u;
    for (var word = 0u; word < words; word += 1u) {
        // Wholly visible candidates retain the current no-shadow-call bypass.
        var bits = cache_candidates[block_index * FOG_BLOCK_WORDS + word]
            & ~cache_candidates[block_index * FOG_BLOCK_WORDS + LIGHT_INDEX_WORDS + word];
        while bits != 0u {
            let bit = firstTrailingBit(bits);
            bits &= bits - 1u;
            let light = word * 32u + bit;
            let slot_i = i32(light_rest[light].shadow_slot);
            if slot_i < 0 { continue; }
            let entry = directory_offset + u32(slot_i) * block_count + block_index;
            if atomicLoad(&cache_words[entry]) != 0u { continue; }
            // Recheck before the contended operation. Once another invocation
            // saturates the frame, this owner stops scanning the rest of its block.
            if atomicLoad(&cache_words[CACHE_FILL_COUNT]) >= limit {
                atomicAdd(&cache_words[CACHE_STOPPED_BLOCKS], 1u);
                return;
            }
            let fill = atomicAdd(&cache_words[CACHE_FILL_COUNT], 1u);
            if fill >= limit {
                atomicAdd(&cache_words[CACHE_STOPPED_BLOCKS], 1u);
                return;
            }
            let payload = payload_base + fill;
            atomicStore(&cache_words[fill_offset + fill], entry);
            atomicStore(&cache_words[entry], payload + 1u);
        }
    }
}

@compute @workgroup_size(1, 1, 1)
fn finalize_visibility_plan(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id != vec3<u32>(0u)) { return; }
    let block_count = atomicLoad(&cache_words[CACHE_BLOCK_COUNT]);
    let cursor = min(atomicLoad(&cache_words[CACHE_PLAN_CURSOR]), block_count - 1u);
    let window_count = min(PLAN_BLOCK_WINDOW, block_count - cursor);
    let next = cursor + window_count;
    atomicStore(
        &cache_words[CACHE_PLAN_CURSOR],
        select(next, 0u, next == block_count),
    );

    let payload_base = atomicLoad(&cache_words[CACHE_PAYLOAD_USED]);
    let payload_capacity = atomicLoad(&cache_words[CACHE_PAYLOAD_CAPACITY]);
    let limit = min(
        atomicLoad(&cache_words[CACHE_FILL_BUDGET]),
        payload_capacity - min(payload_base, payload_capacity),
    );
    let count = min(atomicLoad(&cache_words[CACHE_FILL_COUNT]), limit);
    atomicStore(&cache_words[CACHE_FILL_COUNT], count);
    atomicStore(&cache_words[CACHE_PAYLOAD_USED], payload_base + count);
    atomicStore(&cache_args[0], count);
    atomicStore(&cache_args[1], 1u);
    atomicStore(&cache_args[2], 1u);
}
