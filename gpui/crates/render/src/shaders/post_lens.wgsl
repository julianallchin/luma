// Lens glow (`post.rs`): each lit fixture's lens as a small emitting disc,
// added to the scene-linear frame before metering and glare.
//
// A lens seen from inside its beam is the light source itself: as bright as
// the beam is concentrated, and far above anything lit by it. Seen from the
// side it is a faint glow in the glass. The disc faces the camera, is
// centred on the lens, and is hidden by nearer geometry.

struct LensFrame {
    view_proj: mat4x4<f32>,
    // xyz: camera position.
    camera: vec4<f32>,
    // xy: target size in pixels, z: pixels per radian at the centre (focal
    // length), w: glow gain.
    viewport: vec4<f32>,
    // x: near plane, y: far plane, z: occlusion slack in metres.
    depth: vec4<f32>,
    // Camera right and up, world space.
    right: vec4<f32>,
    up: vec4<f32>,
    // Diffraction sprites. x: half-size as a fraction of the frame height,
    // y: pattern texels across, z: gain.
    psf: vec4<f32>,
};

struct Lens {
    // xyz: lens centre, w: lens radius in metres.
    position: vec4<f32>,
    // xyz: beam axis, w: cosine of the beam half-angle (50% point).
    direction: vec4<f32>,
    // rgb: emitted colour, w: dimmer times cone gain.
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cfg: LensFrame;
@group(0) @binding(1) var<storage, read> lenses: array<Lens>;
@group(0) @binding(2) var depth_tex: texture_depth_2d;

/// Fraction of the lens's glass lit from the side.
const SIDE: f32 = 0.02;
/// Small broadband leak, as the beam transport has: a white source behind a
/// filter. At full a lens goes white-hot through any tone curve.
const WHITE_LEAK: f32 = 0.03;
/// Smallest drawn radius in pixels. A smaller lens is drawn this size with
/// the same total energy, so a distant fixture does not shimmer.
const MIN_RADIUS_PX: f32 = 1.5;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) disc: vec2<f32>,
    @location(1) @interpolate(flat) radiance: vec3<f32>,
    @location(2) @interpolate(flat) view_depth: f32,
};

/// How much of the beam's peak the lens shows toward `to_camera`: all of it
/// within half the beam's half-angle, fading to nothing at one and a half
/// times it (three quarters of the full beam angle).
fn view_term(axis: vec3<f32>, cos_beam: f32, to_camera: vec3<f32>) -> f32 {
    let half = acos(clamp(cos_beam, -1.0, 1.0));
    let c = dot(axis, to_camera);
    let inner = cos(min(0.5 * half, 3.1));
    let outer = cos(min(1.5 * half, 3.1));
    let on_axis = smoothstep(outer, inner, c);
    // Squared so the brightening is sharp: the lens flashes as the beam
    // sweeps across the camera.
    return on_axis * on_axis + SIDE * max(c, 0.0);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    var out: VsOut;
    // Degenerate unless proven visible.
    out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    out.disc = vec2<f32>(0.0);
    out.radiance = vec3<f32>(0.0);
    out.view_depth = 0.0;

    let lens = lenses[ii];
    let to_eye = cfg.camera.xyz - lens.position.xyz;
    let distance = length(to_eye);
    if distance < cfg.depth.x * 2.0 {
        return out;
    }
    let to_camera = to_eye / distance;
    let term = view_term(lens.direction.xyz, lens.direction.w, to_camera);
    if term <= 0.0 {
        return out;
    }
    let radius_px = lens.position.w / distance * cfg.viewport.z;
    let drawn_px = max(radius_px, MIN_RADIUS_PX);
    let energy = (radius_px * radius_px) / (drawn_px * drawn_px);

    let corner = vec2<f32>(
        select(-1.0, 1.0, vi == 1u || vi == 2u || vi == 4u),
        select(-1.0, 1.0, vi == 2u || vi == 4u || vi == 5u),
    );
    let centre = cfg.view_proj * vec4<f32>(lens.position.xyz, 1.0);
    if centre.w <= cfg.depth.x {
        return out;
    }
    // Offsets in pixels, then to clip space at the centre's depth, so the
    // disc stays round and at least MIN_RADIUS_PX whatever the projection.
    let ndc_offset = corner * drawn_px * 2.0 / cfg.viewport.xy;
    out.position = vec4<f32>(centre.xy + ndc_offset * centre.w, 0.0, centre.w);
    out.disc = corner;
    let tint = mix(lens.color.rgb, vec3<f32>(1.0), WHITE_LEAK);
    out.radiance = tint * lens.color.w * term * energy * cfg.viewport.w;
    out.view_depth = centre.w;
    return out;
}

fn linear_view_depth(raw_depth: f32) -> f32 {
    let near = cfg.depth.x;
    let far = cfg.depth.y;
    return near * far / max(near + raw_depth * (far - near), 1e-5);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r2 = dot(in.disc, in.disc);
    if r2 >= 1.0 {
        discard;
    }
    // Hidden behind anything nearer than the lens, less the slack its own
    // glass and bezel need.
    let scene = linear_view_depth(textureLoad(depth_tex, vec2<i32>(in.position.xy), 0));
    if scene < in.view_depth - cfg.depth.z {
        discard;
    }
    // A slightly soft rim, so a disc a few pixels wide is not a square.
    let rim = 1.0 - smoothstep(0.6, 1.0, r2);
    return vec4<f32>(in.radiance * rim, 1.0);
}

// --- diffraction sprites ------------------------------------------------------
// The lens's point-spread function (`psf.rs`) as a sprite into the glare
// target, which the tonemap adds after the tone curve. Same visibility as
// the disc, decided once at the lens's centre so the pattern is never cut
// by the geometry it spreads over.

// The exposure state `post_exposure.wgsl` writes; w is the multiplier.
@group(0) @binding(3) var<storage, read> exposure: vec4<f32>;
@group(0) @binding(4) var psf_tex: texture_2d<f32>;
@group(0) @binding(5) var psf_sampler: sampler;

struct PsfOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) energy: vec3<f32>,
};

@vertex
fn vs_psf(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> PsfOut {
    var out: PsfOut;
    out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    out.uv = vec2<f32>(0.0);
    out.energy = vec3<f32>(0.0);

    let lens = lenses[ii];
    let to_eye = cfg.camera.xyz - lens.position.xyz;
    let distance = length(to_eye);
    if distance < cfg.depth.x * 2.0 {
        return out;
    }
    let term = view_term(lens.direction.xyz, lens.direction.w, to_eye / distance);
    if term <= 0.0 {
        return out;
    }
    let centre = cfg.view_proj * vec4<f32>(lens.position.xyz, 1.0);
    if centre.w <= cfg.depth.x {
        return out;
    }
    let ndc = centre.xy / centre.w;
    if any(abs(ndc) > vec2<f32>(1.0)) {
        return out;
    }
    let size = vec2<f32>(textureDimensions(depth_tex));
    let pixel = vec2<i32>((vec2<f32>(ndc.x, -ndc.y) * 0.5 + 0.5) * size);
    let scene = linear_view_depth(textureLoad(depth_tex, clamp(pixel, vec2<i32>(0), vec2<i32>(size) - 1), 0));
    if scene < centre.w - cfg.depth.z {
        return out;
    }
    // The lens's light, in full-resolution pixels times radiance: what the
    // disc draws, before its minimum size.
    let radius_px = lens.position.w / distance * cfg.viewport.z;
    let tint = mix(lens.color.rgb, vec3<f32>(1.0), WHITE_LEAK);
    let radiance = tint * lens.color.w * term * cfg.viewport.w;
    let energy = radiance * 3.14159265 * radius_px * radius_px * exposure.w;
    // The sprite's half-size is a fixed angle, as a diffraction pattern's
    // is: `psf.x` of the frame's height.
    let half_px = cfg.psf.x * cfg.viewport.y;
    let corner = vec2<f32>(
        select(-1.0, 1.0, vi == 1u || vi == 2u || vi == 4u),
        select(-1.0, 1.0, vi == 2u || vi == 4u || vi == 5u),
    );
    out.position = vec4<f32>(ndc + corner * half_px * 2.0 / cfg.viewport.xy, 0.0, 1.0);
    out.uv = corner * vec2<f32>(0.5, -0.5) + 0.5;
    // Texels of the pattern per full-resolution pixel of the sprite: the
    // texture holds energy per texel.
    let texels = cfg.psf.y * cfg.psf.y / (4.0 * half_px * half_px);
    out.energy = energy * texels * cfg.psf.z;
    return out;
}

@fragment
fn fs_psf(in: PsfOut) -> @location(0) vec4<f32> {
    let p = textureSampleLevel(psf_tex, psf_sampler, in.uv, 0.0).rgb;
    return vec4<f32>(p * in.energy, 1.0);
}
