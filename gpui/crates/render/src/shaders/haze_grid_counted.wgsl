// Diagnostic copy of `haze_grid.wgsl` (`LUMA_FOG_GRID_COUNTS=1`): the same
// cell loop, plus outcome counters for every `segment_shadow_visibility`
// decision and for the proof a 4-cell column-block union segment would give.
// Never built into a production pipeline; its timings are ineligible.
@group(2) @binding(0) var fog_grid: texture_storage_3d<rgba16float, write>;
@group(2) @binding(1) var fog_columns: texture_2d<f32>;
@group(2) @binding(2) var<storage, read> candidates: array<u32>;
@group(2) @binding(3) var<storage, read_write> grid_counts: array<atomic<u32>>;
override BLOCK_VISIBILITY: bool = true;
override PROFILE_SKIP_GRID_SHADOW_TESTS: bool = false;

var<private> gc: array<u32, 16>;

fn gc_flush() {
    for (var i = 0u; i < 16u; i += 1u) {
        if gc[i] != 0u { atomicAdd(&grid_counts[i], gc[i]); }
    }
}

// The hierarchy proof of `segment_shadow_visibility` between two shadow-space
// points: 0 proven lit, 1 proven shadowed, 2 unproven.
fn proof_outcome(first: vec4<f32>, last: vec4<f32>, layer: i32) -> u32 {
    let p = first.xyz / first.w;
    let q = last.xyz / last.w;
    if first.w > 0.0 && last.w > 0.0 && min(p.z, q.z) >= 0.0 && max(p.z, q.z) <= 1.0
        && all(abs(p.xy) <= vec2<f32>(1.0)) && all(abs(q.xy) <= vec2<f32>(1.0)) {
        let dims = textureDimensions(fixture_shadow_map);
        let c0 = min(vec2<u32>((p.xy * vec2<f32>(0.5, -0.5) + 0.5) * vec2<f32>(dims)), dims - 1u);
        let c1 = min(vec2<u32>((q.xy * vec2<f32>(0.5, -0.5) + 0.5) * vec2<f32>(dims)), dims - 1u);
        let difference = (c0.x ^ c1.x) | (c0.y ^ c1.y);
        let level = max(i32(firstLeadingBit(difference)), 0);
        let coordinate = vec2<i32>(c0 >> vec2<u32>(u32(level + 1)));
        var depths: vec2<f32>;
        if layer < 256 { depths = textureLoad(shadow_ranges, coordinate, layer, level).rg; }
        else { depths = textureLoad(shadow_ranges_extra, coordinate, layer - 256, level).rg; }
        let planes = fixture_shadow_matrices[layer].params;
        let ref0 = shadow_compare_reference(p.z, planes.x, planes.y, 0.02);
        let ref1 = shadow_compare_reference(q.z, planes.x, planes.y, 0.02);
        if min(ref0, ref1) >= depths.y { return 0u; }
        if max(ref0, ref1) < depths.x { return 1u; }
    }
    return 2u;
}

@compute @workgroup_size(FOG_BLOCK_SIDE, FOG_BLOCK_SIDE, FOG_BLOCK_SIDE)
fn light_grid(@builtin(global_invocation_id) cell: vec3<u32>) {
    for (var i = 0u; i < 16u; i += 1u) { gc[i] = 0u; }
    let size = textureDimensions(fog_grid);
    if any(cell >= size) { return; }
    let pixel = (vec2<f32>(cell.xy) + 0.5) / vec2<f32>(size.xy)
        * vec2<f32>(haze.transport.w, haze.transport.z);
    let column = textureLoad(fog_columns, vec2<i32>(cell.xy), 0);
    if f32(cell.z) / f32(size.z) >= column.w + 1e-6 {
        textureStore(fog_grid, vec3<i32>(cell), vec4<f32>(0.0));
        gc[13] = 1u; gc_flush();
        return;
    }
    let ray_dir = column.xyz;
    let a = f32(cell.z) / f32(size.z);
    let b = f32(cell.z + 1u) / f32(size.z);
    let span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, ray_dir, haze.shadow.w);
    if span.y <= span.x {
        textureStore(fog_grid, vec3<i32>(cell), vec4<f32>(0.0));
        gc[14] = 1u; gc_flush();
        return;
    }
    gc[0] = 1u;
    let start = mix(span.x, span.y, a * a);
    let end = mix(span.x, span.y, b * b);
    // The column block's 4-cell union: cell 0's first tap to cell 3's last.
    let z0 = f32((cell.z / FOG_BLOCK_SIDE) * FOG_BLOCK_SIDE);
    let a0 = z0 / f32(size.z);
    let b0 = (z0 + 1.0) / f32(size.z);
    let a3 = (z0 + 3.0) / f32(size.z);
    let b3 = (z0 + 4.0) / f32(size.z);
    let t_first = mix(mix(span.x, span.y, a0 * a0), mix(span.x, span.y, b0 * b0), 0.125);
    let t_last = mix(mix(span.x, span.y, a3 * a3), mix(span.x, span.y, b3 * b3), 0.875);
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
            gc[11] += 1u;
            var visibility = 1.0;
            if haze.shadow.x > 0.0 && (visible & (1u << bit)) == 0u && !PROFILE_SKIP_GRID_SHADOW_TESTS {
                gc[1] += 1u;
                let layer = i32(light_rest[li].shadow_slot);
                if layer < 0 {
                    gc[2] += 1u;
                } else {
                    let matrix = fixture_shadow_matrices[layer].view_proj;
                    let origin = matrix * vec4<f32>(haze.camera_pos.xyz, 1.0);
                    let direction = matrix * vec4<f32>(ray_dir, 0.0);
                    let cell_out = proof_outcome(origin + direction * mix(start, end, 0.125),
                                                 origin + direction * mix(start, end, 0.875), layer);
                    let union_out = proof_outcome(origin + direction * t_first,
                                                  origin + direction * t_last, layer);
                    gc[3u + cell_out] += 1u;
                    gc[6u + union_out] += 1u;
                    if union_out == 2u && cell_out != 2u { gc[9] += 1u; }
                    if union_out != 2u && cell_out == 2u { gc[10] += 1u; }
                    if (cell.z % FOG_BLOCK_SIDE) == 0u { gc[15] += 1u; }
                }
                visibility = segment_shadow_visibility(ray_dir, start, end, li);
            } else if haze.shadow.x > 0.0 && !PROFILE_SKIP_GRID_SHADOW_TESTS {
                gc[12] += 1u;
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
    gc_flush();
}
