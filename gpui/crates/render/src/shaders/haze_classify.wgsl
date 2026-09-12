// Conservative visibility shared by each 4x4x4 block of volume samples.
// Bounds include every cell's complete radial segment, not just its centre.
@group(2) @binding(0) var columns: texture_2d<f32>;
@group(2) @binding(1) var<storage, read_write> candidates: array<u32>;
var<workgroup> lower: array<vec3<f32>, FOG_BLOCK_LANES>;
var<workgroup> upper: array<vec3<f32>, FOG_BLOCK_LANES>;
var<workgroup> masks: array<atomic<u32>, FOG_BLOCK_WORDS>;

// 0: wholly shadowed, 1: wholly visible, 2: unresolved shadow edge.
fn block_visibility(lo: vec3<f32>, hi: vec3<f32>, li: u32) -> u32 {
    if haze.shadow.x <= 0.0 { return 1u; }
    let layer = i32(light_rest[li].shadow_slot);
    if layer < 0 { return 2u; }
    let matrix = fixture_shadow_matrices[layer].view_proj;
    var pmin = vec3<f32>(1e20);
    var pmax = vec3<f32>(-1e20);
    // Explicit corners measured faster on Metal. Keep the original order
    // and early exit so the conservative visibility test is unchanged.
    {
        let clip = matrix * vec4<f32>(vec3<f32>(lo.x, lo.y, lo.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    {
        let clip = matrix * vec4<f32>(vec3<f32>(hi.x, lo.y, lo.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    {
        let clip = matrix * vec4<f32>(vec3<f32>(lo.x, hi.y, lo.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    {
        let clip = matrix * vec4<f32>(vec3<f32>(hi.x, hi.y, lo.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    {
        let clip = matrix * vec4<f32>(vec3<f32>(lo.x, lo.y, hi.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    {
        let clip = matrix * vec4<f32>(vec3<f32>(hi.x, lo.y, hi.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    {
        let clip = matrix * vec4<f32>(vec3<f32>(lo.x, hi.y, hi.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    {
        let clip = matrix * vec4<f32>(vec3<f32>(hi.x, hi.y, hi.z), 1.0);
        if clip.w <= 0.0 { return 2u; }
        let ndc = clip.xyz / clip.w;
        pmin = min(pmin, ndc);
        pmax = max(pmax, ndc);
    }
    if pmin.z < 0.0 || pmax.z > 1.0 || any(pmin.xy < vec2<f32>(-1.0)) || any(pmax.xy > vec2<f32>(1.0)) { return 2u; }
    let dims = textureDimensions(fixture_shadow_map);
    let c0 = min(vec2<u32>((vec2<f32>(pmin.x, -pmax.y) * 0.5 + 0.5) * vec2<f32>(dims)), dims - 1u);
    let c1 = min(vec2<u32>((vec2<f32>(pmax.x, -pmin.y) * 0.5 + 0.5) * vec2<f32>(dims)), dims - 1u);
    let difference = (c0.x ^ c1.x) | (c0.y ^ c1.y);
    let level = max(i32(firstLeadingBit(difference)), 0);
    let coordinate = vec2<i32>(c0 >> vec2<u32>(u32(level + 1)));
    var depths: vec2<f32>;
    if layer < 256 { depths = textureLoad(shadow_ranges, coordinate, layer, level).rg; }
    else { depths = textureLoad(shadow_ranges_extra, coordinate, layer - 256, level).rg; }
    let planes = fixture_shadow_matrices[layer].params;
    if shadow_compare_reference(pmin.z, planes.x, planes.y, 0.02) >= depths.y { return 1u; }
    if shadow_compare_reference(pmax.z, planes.x, planes.y, 0.02) < depths.x { return 0u; }
    return 2u;
}

@compute @workgroup_size(FOG_BLOCK_SIDE, FOG_BLOCK_SIDE, FOG_BLOCK_SIDE)
fn classify_blocks(@builtin(global_invocation_id) cell: vec3<u32>,
                   @builtin(workgroup_id) block: vec3<u32>,
                   @builtin(local_invocation_index) lane: u32) {
    let size = vec3<u32>(textureDimensions(columns), FOG_SLICES);
    lower[lane] = vec3<f32>(1e20);
    upper[lane] = vec3<f32>(-1e20);
    if lane < FOG_BLOCK_WORDS { atomicStore(&masks[lane], 0u); }
    if all(cell < size) {
        let info = textureLoad(columns, vec2<i32>(cell.xy), 0);
        let span = medium_lighting_span(haze.medium, haze.camera_pos.xyz, info.xyz, haze.shadow.w);
        let a = f32(cell.z) / f32(size.z);
        let b = f32(cell.z + 1u) / f32(size.z);
        if span.y > span.x && a < info.w + 1e-6 {
            let p = haze.camera_pos.xyz + info.xyz * mix(span.x, span.y, a * a);
            let q = haze.camera_pos.xyz + info.xyz * mix(span.x, span.y, b * b);
            lower[lane] = min(p, q);
            upper[lane] = max(p, q);
        }
    }
    workgroupBarrier();
    for (var stride = FOG_BLOCK_LANES / 2u; stride > 0u; stride /= 2u) {
        if lane < stride {
            lower[lane] = min(lower[lane], lower[lane + stride]);
            upper[lane] = max(upper[lane], upper[lane + stride]);
        }
        workgroupBarrier();
    }
    // Expand by 0.1 mm to cover rounding between endpoint and midpoint FMAs.
    let lo = lower[0] - vec3<f32>(0.0001);
    let hi = upper[0] + vec3<f32>(0.0001);
    if all(lo <= hi) {
        let center = (lo + hi) * 0.5;
        let radius = length(hi - lo) * 0.5;
        for (var li = lane; li < light_index_params.counts.x; li += FOG_BLOCK_LANES) {
            let core = light_core[li];
            let rest = light_rest[li];
            if rest.wash < FOG_BROAD_WASH || rest.gobo >= 0.5 || rest.haze_gain <= 0.0 { continue; }
            let q = center - core.position;
            let distance = length(q);
            if distance > radius + core.range || distance + radius <= FOG_SOURCE_INNER { continue; }
            let axial = dot(q, rest.direction);
            if rest.cos_field >= 0.0 {
                let perpendicular = sqrt(max(dot(q,q) - axial * axial, 0.0));
                let sine = sqrt(max(1.0 - rest.cos_field * rest.cos_field, 0.0));
                if axial < -radius || rest.cos_field * perpendicular - axial * sine > radius { continue; }
            }
            let visibility = block_visibility(lo, hi, li);
            if visibility == 0u { continue; }
            atomicOr(&masks[li / 32u], 1u << (li % 32u));
            if visibility == 1u { atomicOr(&masks[LIGHT_INDEX_WORDS + li / 32u], 1u << (li % 32u)); }
        }
    }
    workgroupBarrier();
    let blocks = (size + FOG_BLOCK_SIDE - 1u) / FOG_BLOCK_SIDE;
    let index = (block.z * blocks.y + block.y) * blocks.x + block.x;
    if lane < FOG_BLOCK_WORDS { candidates[index * FOG_BLOCK_WORDS + lane] = atomicLoad(&masks[lane]); }
}
