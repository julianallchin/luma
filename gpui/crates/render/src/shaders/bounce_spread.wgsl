// Bounced light, every frame after the discs are lit (`bounce_relight.wgsl`):
// each patch and group takes its discs' mean, then every receiver adds up
// what its links bring. Prefixed by `bounce_common.wgsl`.
//
// `radiosity` holds what each emitter sends out, W/m2: discs, then patches,
// then groups. `gathered` is the bounced light landing on each disc, which
// the next frame's relight sends out again: a bounce more each frame, at no
// cost.

struct BouncePatch { start: u32, count: u32, group: u32, area: f32 };
struct BounceGroup { start: u32, count: u32, area: f32, pad: f32 };
struct BounceLink { node: u32, weight: f32, size: f32, dir: u32 };

@group(0) @binding(0) var<uniform> field: BounceField;
@group(0) @binding(1) var<storage, read> discs: array<BounceDisc>;
@group(0) @binding(2) var<storage, read> patches: array<BouncePatch>;
@group(0) @binding(3) var<storage, read> groups: array<BounceGroup>;
@group(0) @binding(4) var<storage, read_write> radiosity: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read> starts: array<u32>;
@group(0) @binding(6) var<storage, read> links: array<BounceLink>;
@group(0) @binding(7) var<storage, read_write> gathered: array<vec4<f32>>;
@group(0) @binding(8) var<storage, read_write> mirrored: array<vec4<f32>>;
@group(0) @binding(9) var air_out: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(64)
fn pull_patches(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= field.counts.y { return; }
    let run = patches[id.x];
    var sum = vec3<f32>(0.0);
    for (var k = run.start; k < run.start + run.count; k++) {
        sum += discs[k].area * radiosity[k].rgb;
    }
    radiosity[field.counts.x + id.x] = vec4<f32>(sum / max(run.area, 1e-9), 0.0);
}

@compute @workgroup_size(64)
fn pull_groups(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= field.counts.z { return; }
    let group = groups[id.x];
    let first = field.counts.x;
    var sum = vec3<f32>(0.0);
    for (var k = group.start; k < group.start + group.count; k++) {
        sum += patches[k].area * radiosity[first + k].rgb;
    }
    radiosity[first + field.counts.y + id.x] = vec4<f32>(sum / max(group.area, 1e-9), 0.0);
}

// Each disc: the bounced light on its face, and the bounced light it
// mirrors toward the eye. Each link is a small light in the disc's sky; its
// shine is the surface's own highlight of it, widened by the link's size so
// neighbouring links join up.
@compute @workgroup_size(64)
fn gather_discs(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= field.counts.x { return; }
    let disc = discs[id.x];
    let v = normalize(field.eye.xyz - disc.pos);
    let nov = max(dot(disc.normal, v), 1e-3);
    let a = disc.roughness * disc.roughness;
    var sum = vec3<f32>(0.0);
    var shine = vec3<f32>(0.0);
    for (var k = starts[id.x]; k < starts[id.x + 1u]; k++) {
        let link = links[k];
        let sent = radiosity[link.node].rgb;
        sum += link.weight * sent;
        let l = bounce_unpack_direction(link.dir);
        let nol = dot(disc.normal, l);
        if nol <= 0.0 { continue; }
        let wide = min(a + field.eye.w * sqrt(link.size), 1.0);
        let h = normalize(v + l);
        let noh = max(dot(disc.normal, h), 0.0);
        let lobe = noh * noh * (wide * wide - 1.0) + 1.0;
        let d = wide * wide / (3.14159265 * lobe * lobe);
        let k2 = wide * 0.5;
        let visible = 1.0 / (4.0 * (nov * (1.0 - k2) + k2) * (nol * (1.0 - k2) + k2));
        let fresnel = disc.f0 + (vec3<f32>(1.0) - disc.f0) * pow(1.0 - max(dot(v, h), 0.0), 5.0);
        shine += sent * (link.size * nol * d * visible) * fresnel;
    }
    gathered[id.x] = vec4<f32>(sum, 0.0);
    mirrored[id.x] = vec4<f32>(shine, 0.0);
}

// Each point in the air: the bounced light on six faces, +x -x +y -y +z -z,
// each a slab of the air texture along z. A point has one list of links,
// and each face takes of a link what comes from its side.
@compute @workgroup_size(64)
fn gather_air(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= field.counts.w { return; }
    let receiver = field.counts.x + id.x;
    var plus = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    var minus = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    for (var k = starts[receiver]; k < starts[receiver + 1u]; k++) {
        let link = links[k];
        let sent = link.size * radiosity[link.node].rgb;
        let l = bounce_unpack_direction(link.dir);
        plus[0] += sent * max(l.x, 0.0);
        plus[1] += sent * max(l.y, 0.0);
        plus[2] += sent * max(l.z, 0.0);
        minus[0] += sent * max(-l.x, 0.0);
        minus[1] += sent * max(-l.y, 0.0);
        minus[2] += sent * max(-l.z, 0.0);
    }
    let dims = vec3<u32>(field.air_dims.xyz);
    let at = vec3<u32>(id.x % dims.x, (id.x / dims.x) % dims.y, id.x / (dims.x * dims.y));
    for (var axis = 0u; axis < 3u; axis++) {
        textureStore(air_out, vec3<u32>(at.xy, at.z + (axis * 2u) * dims.z), vec4<f32>(plus[axis], 1.0));
        textureStore(air_out, vec3<u32>(at.xy, at.z + (axis * 2u + 1u) * dims.z), vec4<f32>(minus[axis], 1.0));
    }
}
