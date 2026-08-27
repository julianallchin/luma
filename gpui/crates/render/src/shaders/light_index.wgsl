// Consumer prelude for the unified light index (`light_index.rs`).
//
// Concatenated into every shader that asks "which fixtures can reach here":
// the tiled haze marcher, the surface pass, and later froxel injection. The
// group number is fixed at 1 here; a consumer whose pipeline uses a different
// slot rebinds it with a documented string replace at composition time.
//
// The index is defined in full-resolution pixel space: a pass rendering at a
// fraction of output resolution scales its fragment coordinate before calling
// in — it never gets its own grid.
//
// Ids handed out by `light_index_next` are sorted-space indices into the
// light SoA this module uploads — the SoA is reordered to match, so consumers
// never translate.

struct LightIndexParams {
    // columns, rows, big_columns, big_rows (8 px tile grid).
    grid: vec4<u32>,
    // light_count, unused ×3.
    counts: vec4<u32>,
    // near, Z_BINS / (far - near), unused ×2.
    depth: vec4<f32>,
}

// Bindings start at 8 so the prelude can share a group with a consumer's own
// low-numbered bindings (the surface pass keeps its light SoA and shadow
// resources in the same group).
@group(1) @binding(8) var<uniform> light_index_params: LightIndexParams;
@group(1) @binding(9) var<storage, read> light_index_masks: array<u32>;
@group(1) @binding(10) var<storage, read> light_index_zbins: array<u32>;

const LIGHT_INDEX_TILE: u32 = 8u;
const LIGHT_INDEX_WORDS: u32 = 16u;
const LIGHT_INDEX_ZBINS: u32 = 4096u;

struct LightCursor {
    base: u32,
    word: u32,
    bits: u32,
    // Inclusive sorted-id range from the Z-bin; lights_along leaves it open.
    min_id: u32,
    max_id: u32,
}

fn light_index_cursor(frag_xy: vec2<f32>, min_id: u32, max_id: u32) -> LightCursor {
    let tile = min(
        vec2<u32>(frag_xy) / LIGHT_INDEX_TILE,
        light_index_params.grid.xy - vec2<u32>(1u),
    );
    let base = (tile.y * light_index_params.grid.x + tile.x) * LIGHT_INDEX_WORDS;
    var cursor: LightCursor;
    cursor.base = base;
    // Start the walk at the range's first word; edge bits are rejected by the
    // id compare in `light_index_next`.
    cursor.word = min(min_id / 32u, LIGHT_INDEX_WORDS - 1u);
    cursor.bits = light_index_masks[base + cursor.word];
    cursor.min_id = min_id;
    cursor.max_id = max_id;
    return cursor;
}

/// Lights whose cone can reach anywhere along this pixel's ray — the ray
/// consumer's query. `frag_xy` in full-resolution pixels.
fn lights_along(frag_xy: vec2<f32>) -> LightCursor {
    return light_index_cursor(frag_xy, 0u, 0xFFFEu);
}

/// Lights whose cone can reach this pixel at this view depth — the point
/// consumer's query, mask ∩ Z-bin range.
fn lights_at(frag_xy: vec2<f32>, view_depth: f32) -> LightCursor {
    let bin_f = (view_depth - light_index_params.depth.x) * light_index_params.depth.y;
    let bin = min(u32(max(bin_f, 0.0)), LIGHT_INDEX_ZBINS - 1u);
    let range = light_index_zbins[bin];
    // An empty bin packs min 0xFFFF > max 0, so the compare rejects all.
    return light_index_cursor(frag_xy, range >> 16u, range & 0xFFFFu);
}

/// Advances the cursor; returns false when exhausted. `id` receives a source
/// fixture index into the light SoA.
fn light_index_next(cursor: ptr<function, LightCursor>, id: ptr<function, u32>) -> bool {
    loop {
        if (*cursor).bits == 0u {
            (*cursor).word += 1u;
            if (*cursor).word >= LIGHT_INDEX_WORDS {
                return false;
            }
            (*cursor).bits = light_index_masks[(*cursor).base + (*cursor).word];
            continue;
        }
        let bit = firstTrailingBit((*cursor).bits);
        (*cursor).bits &= (*cursor).bits - 1u;
        let sorted = (*cursor).word * 32u + bit;
        if sorted > (*cursor).max_id {
            return false;
        }
        if sorted < (*cursor).min_id {
            continue;
        }
        *id = sorted;
        return true;
    }
    // Unreachable: the loop only exits through the returns above. Present
    // because naga types a loop as able to complete.
    return false;
}
