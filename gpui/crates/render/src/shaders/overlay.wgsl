// Unlit editor affordances (spec §2.3 `Unlit`): selection cages and gizmo
// handles. three's `MeshBasicMaterial` / `LineBasicMaterial` — the colour is
// the output, with no lighting term and no texture.
//
// This runs after the display transform directly into an sRGB output target.
// CPU-authored colours are linear values; the attachment's transfer function
// restores their authored sRGB bytes without lighting or AgX changing them.

// A prefix of the scene pass's `Globals`; only the view-projection is read.
struct Globals {
    view_proj: mat4x4<f32>,
};

struct Instance {
    model: mat4x4<f32>,
    // rgb: linear colour, a: opacity.
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) instance: u32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @builtin(instance_index) instance: u32,
) -> VsOut {
    var out: VsOut;
    out.clip = globals.view_proj * instances[instance].model * vec4<f32>(position, 1.0);
    out.instance = instance;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return instances[in.instance].color;
}
