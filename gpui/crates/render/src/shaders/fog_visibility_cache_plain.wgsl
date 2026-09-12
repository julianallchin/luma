@group(2) @binding(3) var<storage, read> cache_words: array<u32>;
@group(2) @binding(4) var<storage, read> cache_payload: array<u32>;

const CACHE_ACTIVE: u32 = 16u;
const CACHE_BLOCK_COUNT: u32 = 17u;
const CACHE_DIRECTORY_OFFSET: u32 = 21u;
fn cache_word(index: u32) -> u32 { return cache_words[index]; }
fn cache_record_hit() {}
fn cache_record_fallback() {}
fn cache_assert_raw(cached: u32, reference: u32) {}
