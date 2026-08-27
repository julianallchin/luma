// Bakes the volumetric density field into a wrapping 3D texture, once per
// device (`haze_field.rs`).
//
// This shader is the field's only definition: nothing on the CPU evaluates it
// in the shipping path, and the sampling side (`beam_transport.wgsl`'s
// `haze_noise`) is one `textureSampleLevel`. `FIELD_SIZE`, `FIELD_TEXELS` and
// `FIELD_CELLS` are injected at pipeline creation, the way `light_index.rs`
// injects `NARROW_PHASE`.
//
// Output is a storage buffer of packed f16 pairs rather than a storage texture
// because `r16float` is not a WebGPU storage-capable format; the buffer is
// copied into the texture and dropped.

@group(0) @binding(0) var<storage, read_write> packed: array<u32>;

/// Lattice gradient from an integer hash of the *wrapped* cell coordinate.
///
/// Integer rather than the `sin`-based hash this field used when it was
/// evaluated per sample, and that is a correctness requirement, not a speed
/// one: `sin` of a large argument depends on the device's range reduction, so
/// a `sin`-hashed field bakes differently on different GPUs — and once the
/// field is a baked resource, that is a different texture and different golden
/// images per machine. Integer arithmetic is exact everywhere, so the field is
/// bit-reproducible across devices and on the CPU, which is what lets the bake
/// be tested at all.
///
/// Wrapping the cell coordinate before hashing is what makes the field
/// periodic rather than merely truncated, and therefore what makes the texture
/// tile seamlessly.
fn wrapped_gradient(cell: vec3<f32>) -> vec3<f32> {
    let wrapped = vec3<u32>(cell - floor(cell / FIELD_CELLS) * FIELD_CELLS);
    let h = wrapped * vec3<u32>(1597334673u, 3812015801u, 2798796415u);
    let m = h.x ^ h.y ^ h.z;
    var s = vec3<u32>(m, m * 1597334677u, m * 3812015801u);
    s = s ^ (s >> vec3<u32>(15u));
    s = s * vec3<u32>(2246822519u);
    s = s ^ (s >> vec3<u32>(13u));
    // 23 bits over 2^23 lands on exactly representable f32s, so the mapping to
    // [-1, 1] introduces no rounding of its own.
    return -1.0 + 2.0 * (vec3<f32>(s >> vec3<u32>(9u)) * (1.0 / 8388608.0));
}

/// Gradient noise on the unit lattice — the same interpolation the per-sample
/// `noise3d` used, so the field's spectrum is unchanged and only its gradient
/// source and its periodicity differ.
fn lattice_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let g000 = dot(wrapped_gradient(i + vec3<f32>(0.0, 0.0, 0.0)), f - vec3<f32>(0.0, 0.0, 0.0));
    let g100 = dot(wrapped_gradient(i + vec3<f32>(1.0, 0.0, 0.0)), f - vec3<f32>(1.0, 0.0, 0.0));
    let g010 = dot(wrapped_gradient(i + vec3<f32>(0.0, 1.0, 0.0)), f - vec3<f32>(0.0, 1.0, 0.0));
    let g110 = dot(wrapped_gradient(i + vec3<f32>(1.0, 1.0, 0.0)), f - vec3<f32>(1.0, 1.0, 0.0));
    let g001 = dot(wrapped_gradient(i + vec3<f32>(0.0, 0.0, 1.0)), f - vec3<f32>(0.0, 0.0, 1.0));
    let g101 = dot(wrapped_gradient(i + vec3<f32>(1.0, 0.0, 1.0)), f - vec3<f32>(1.0, 0.0, 1.0));
    let g011 = dot(wrapped_gradient(i + vec3<f32>(0.0, 1.0, 1.0)), f - vec3<f32>(0.0, 1.0, 1.0));
    let g111 = dot(wrapped_gradient(i + vec3<f32>(1.0, 1.0, 1.0)), f - vec3<f32>(1.0, 1.0, 1.0));
    let x00 = mix(g000, g100, u.x);
    let x10 = mix(g010, g110, u.x);
    let x01 = mix(g001, g101, u.x);
    let x11 = mix(g011, g111, u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

/// Lattice coordinate of a texel's centre. Baking at centres — not corners —
/// is what lets the sampling side map `q` to uv with a plain divide and no
/// half-texel correction, and it stays seamless across the wrap because the
/// field's period is exactly `FIELD_CELLS`.
fn texel_centre(texel: vec3<u32>) -> vec3<f32> {
    return (vec3<f32>(texel) + 0.5) / FIELD_TEXELS;
}

fn texel_of(index: u32) -> vec3<u32> {
    return vec3<u32>(
        index % FIELD_SIZE,
        (index / FIELD_SIZE) % FIELD_SIZE,
        index / (FIELD_SIZE * FIELD_SIZE),
    );
}

// One invocation per *pair* of texels along x, because two f16 share a u32.
//
// Dispatched 2D — x over one slice's pairs, y over slices — because a 256³
// field is 8.4 M pairs and a 1D dispatch would need 131072 workgroups against
// a 65535-per-dimension limit.
@compute @workgroup_size(64)
fn bake_field(@builtin(global_invocation_id) gid: vec3<u32>) {
    let pairs_per_slice = FIELD_SIZE * FIELD_SIZE / 2u;
    if gid.x >= pairs_per_slice || gid.y >= FIELD_SIZE {
        return;
    }
    let pair = gid.y * pairs_per_slice + gid.x;
    let lo = lattice_noise(texel_centre(texel_of(pair * 2u)));
    let hi = lattice_noise(texel_centre(texel_of(pair * 2u + 1u)));
    packed[pair] = pack2x16float(vec2<f32>(lo, hi));
}
