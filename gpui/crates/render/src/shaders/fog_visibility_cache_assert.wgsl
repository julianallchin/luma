// Assertion mode uses the same two buffers and is timing-ineligible.
@group(2) @binding(3) var<storage, read_write> cache_atomic: array<atomic<u32>>;
@group(2) @binding(4) var<storage, read> cache_payload: array<u32>;

const CACHE_GRID_HITS: u32 = 4u;
const CACHE_GRID_FALLBACKS: u32 = 5u;
const CACHE_MISMATCHES: u32 = 7u;
const CACHE_ACTIVE: u32 = 16u;
const CACHE_BLOCK_COUNT: u32 = 17u;
const CACHE_DIRECTORY_OFFSET: u32 = 21u;
fn cache_word(index: u32) -> u32 { return atomicLoad(&cache_atomic[index]); }
fn cache_record_hit() { atomicAdd(&cache_atomic[CACHE_GRID_HITS], 1u); }
fn cache_record_fallback() { atomicAdd(&cache_atomic[CACHE_GRID_FALLBACKS], 1u); }
fn cache_assert_raw(cached: u32, reference: u32) {
    if cached != reference { atomicAdd(&cache_atomic[CACHE_MISMATCHES], 1u); }
}
