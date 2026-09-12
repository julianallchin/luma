// The cache plan publishes only bounded positive-slot records. One 4x4x4
// workgroup writes one immutable 32-byte payload before the grid reads it.
@group(2) @binding(0) var cache_columns: texture_2d<f32>;
@group(2) @binding(1) var<storage, read_write> cache_words: array<atomic<u32>>;
@group(2) @binding(2) var<storage, read_write> cache_payload: array<atomic<u32>>;

const CACHE_FILL_COUNT: u32 = 1u;
const CACHE_COLD_CALLS: u32 = 6u;
const CACHE_BLOCK_COUNT: u32 = 17u;
const CACHE_BLOCKS_X: u32 = 18u;
const CACHE_BLOCKS_Y: u32 = 19u;
const CACHE_DIRECTORY_OFFSET: u32 = 21u;
const CACHE_FILL_OFFSET: u32 = 22u;
const CACHE_INVALID: u32 = 0xffffffffu;
var<workgroup> fill_cold_calls: atomic<u32>;

@compute @workgroup_size(FOG_BLOCK_SIDE, FOG_BLOCK_SIDE, FOG_BLOCK_SIDE)
fn fill_visibility(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
) {
    let fill = group.x;
    if fill >= atomicLoad(&cache_words[CACHE_FILL_COUNT]) { return; }
    let record = atomicLoad(&cache_words[atomicLoad(&cache_words[CACHE_FILL_OFFSET]) + fill]);
    if record == CACHE_INVALID { return; }
    let handle = atomicLoad(&cache_words[record]);
    if handle == 0u { return; }
    let payload = handle - 1u;
    let lane = (local.z * FOG_BLOCK_SIDE + local.y) * FOG_BLOCK_SIDE + local.x;
    if lane == 0u { atomicStore(&fill_cold_calls, 0u); }
    if lane < 8u { atomicStore(&cache_payload[payload * 8u + lane], 0u); }
    storageBarrier();
    workgroupBarrier();

    let block_count = atomicLoad(&cache_words[CACHE_BLOCK_COUNT]);
    let relative = record - atomicLoad(&cache_words[CACHE_DIRECTORY_OFFSET]);
    let slot = relative / block_count;
    let block_index = relative % block_count;
    let blocks_x = atomicLoad(&cache_words[CACHE_BLOCKS_X]);
    let blocks_y = atomicLoad(&cache_words[CACHE_BLOCKS_Y]);
    let block_z = block_index / (blocks_x * blocks_y);
    let block_xy = block_index % (blocks_x * blocks_y);
    let block = vec3<u32>(block_xy % blocks_x, block_xy / blocks_x, block_z);
    let cell = block * FOG_BLOCK_SIDE + local;
    let size = vec3<u32>(textureDimensions(cache_columns), FOG_SLICES);
    var raw = 4u;
    if all(cell < size) {
        let ray_dir = textureLoad(cache_columns, vec2<i32>(cell.xy), 0).xyz;
        let span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, ray_dir, haze.shadow.w);
        if span.y > span.x {
            let a = f32(cell.z) / f32(size.z);
            let b = f32(cell.z + 1u) / f32(size.z);
            let start = mix(span.x, span.y, a * a);
            let end = mix(span.x, span.y, b * b);
            raw = u32(round(segment_shadow_visibility_layer(ray_dir, start, end, i32(slot)) * 4.0));
            atomicAdd(&fill_cold_calls, 1u);
        }
    }
    atomicOr(&cache_payload[payload * 8u + lane / 8u], raw << ((lane & 7u) * 4u));
    workgroupBarrier();
    if lane == 0u {
        atomicAdd(&cache_words[CACHE_COLD_CALLS], atomicLoad(&fill_cold_calls));
    }
}
