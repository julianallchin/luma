// Concatenate either fog_visibility_cache_plain.wgsl or
// fog_visibility_cache_assert.wgsl before this file.
@group(2) @binding(0) var fog_grid: texture_storage_3d<rgba16float, write>;
@group(2) @binding(1) var fog_columns: texture_2d<f32>;
@group(2) @binding(2) var<storage, read> candidates: array<u32>;
override BLOCK_VISIBILITY: bool = true;
override PROFILE_SKIP_GRID_SHADOW_TESTS: bool = false;
override FOG_VISIBILITY_ASSERT: bool = false;

fn cached_segment_visibility(ray_dir: vec3<f32>, start: f32, end: f32,
                             light: u32, block: u32, lane: u32) -> f32 {
    let slot_i = i32(light_rest[light].shadow_slot);
    if cache_word(CACHE_ACTIVE) == 0u || slot_i < 0 {
        cache_record_fallback();
        return segment_shadow_visibility(ray_dir, start, end, light);
    }
    let entry = cache_word(CACHE_DIRECTORY_OFFSET)
        + u32(slot_i) * cache_word(CACHE_BLOCK_COUNT) + block;
    let handle = cache_word(entry);
    if handle == 0u {
        cache_record_fallback();
        return segment_shadow_visibility(ray_dir, start, end, light);
    }
    let packed = cache_payload[(handle - 1u) * 8u + lane / 8u];
    let raw = (packed >> ((lane & 7u) * 4u)) & 0xfu;
    cache_record_hit();
    if FOG_VISIBILITY_ASSERT {
        let reference = u32(round(segment_shadow_visibility(ray_dir, start, end, light) * 4.0));
        cache_assert_raw(raw, reference);
    }
    return f32(raw) * 0.25;
}

@compute @workgroup_size(FOG_BLOCK_SIDE, FOG_BLOCK_SIDE, FOG_BLOCK_SIDE)
fn light_grid(
    @builtin(global_invocation_id) cell: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let size = textureDimensions(fog_grid);
    if any(cell >= size) { return; }
    let pixel = (vec2<f32>(cell.xy) + 0.5) / vec2<f32>(size.xy)
        * vec2<f32>(haze.transport.w, haze.transport.z);
    let column = textureLoad(fog_columns, vec2<i32>(cell.xy), 0);
    if f32(cell.z) / f32(size.z) >= column.w + 1e-6 {
        textureStore(fog_grid, vec3<i32>(cell), vec4<f32>(0.0));
        return;
    }
    let ray_dir = column.xyz;
    let a = f32(cell.z) / f32(size.z);
    let b = f32(cell.z + 1u) / f32(size.z);
    let span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, ray_dir, haze.shadow.w);
    if span.y <= span.x {
        textureStore(fog_grid, vec3<i32>(cell), vec4<f32>(0.0));
        return;
    }
    let start = mix(span.x, span.y, a * a);
    let end = mix(span.x, span.y, b * b);
    let radius = mix(start, end, 0.5);
    let world = haze.camera_pos.xyz + radius * ray_dir;
    let block = cell / FOG_BLOCK_SIDE;
    let blocks = (size + FOG_BLOCK_SIDE - 1u) / FOG_BLOCK_SIDE;
    let index = (block.z * blocks.y + block.y) * blocks.x + block.x;
    var radiance = vec3<f32>(0.0);
    for (var word = 0u; word < (light_index_params.counts.x + 31u) / 32u; word += 1u) {
        var bits = candidates[index * FOG_BLOCK_WORDS + word];
        var visible = candidates[index * FOG_BLOCK_WORDS + LIGHT_INDEX_WORDS + word];
        if !BLOCK_VISIBILITY {
            let cursor = lights_along(pixel * haze.tiles.xy);
            bits = light_index_masks[cursor.base + word];
            visible = 0u;
        }
        while bits != 0u {
            let bit = firstTrailingBit(bits);
            bits &= bits - 1u;
            let li = word * 32u + bit;
            let rest = light_rest[li];
            if !BLOCK_VISIBILITY && (rest.wash < FOG_BROAD_WASH || rest.gobo >= 0.5 || rest.haze_gain <= 0.0) { continue; }
            let core = light_core[li];
            let q = world - core.position;
            let d2 = dot(q, q);
            let dist = sqrt(d2);
            if dist >= core.range || dist <= FOG_SOURCE_INNER { continue; }
            let angular = angular_profile(dot(q, rest.direction) / max(dist, 1e-4), rest.cos_beam, rest.cos_field);
            if angular <= 0.0 { continue; }
            var visibility = 1.0;
            if haze.shadow.x > 0.0 && (visible & (1u << bit)) == 0u && !PROFILE_SKIP_GRID_SHADOW_TESTS {
                visibility = cached_segment_visibility(ray_dir, start, end, li, index, lane);
            }
            if visibility <= 0.0 { continue; }
            let phase = henyey_greenstein(-dot(q, ray_dir) / max(dist, 1e-4), haze.transport.y);
            let tint = mix(rest.color, vec3<f32>(1.0), haze.transport.x);
            radiance += tint * (rest.intensity * rest.haze_gain * haze.tuning.w
                * smoothstep(FOG_SOURCE_INNER, FOG_SOURCE_OUTER, dist) * angular * beam_range_falloff(dist, core.range) * phase * visibility
                * exp(-light_optical_depth(li, world)) / max(d2, haze.tuning.z));
        }
    }
    textureStore(fog_grid, vec3<i32>(cell), vec4<f32>(radiance / 256.0, 0.0));
}
