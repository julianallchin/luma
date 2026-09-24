// Glare (`post.rs`): the exposed frame's hot pixels convolved with the glare
// kernel (`psf.rs`) by FFT, on a padded grid of ROW × COL texels. `post.rs`
// prepends the two sizes:
//
//   const ROW: u32 = …;  // texels across, a power of two up to 1024
//   const COL: u32 = …;  // texels down, a power of two up to 512
//
// The frame covers the grid's top-left quarter or less; the rest is zero, so
// the kernel can reach across the whole frame without the cyclic convolution
// folding it back. Both inputs are real, so a row transform keeps only the
// ROW / 2 + 1 non-negative frequencies, as three planes (red, green, blue)
// of complex numbers. Red and green share one complex transform (red real,
// green imaginary) and are separated afterwards; blue takes the other half
// of each texel.
//
// A frame is three dispatches: `rows_forward` (hot pixels, then the row
// transform of each frame row), `columns_convolve` (column transform,
// multiply by the kernel's spectrum, inverse column transform) and
// `rows_inverse` (inverse row transform into the glare texture). The
// kernel's spectrum is made by `rows_kernel` and `columns_forward` when the
// kernel changes.

struct Fft {
    // x, y: frame region in grid texels.
    region: vec4<u32>,
    // x: frame pixels per grid texel, y: threshold, z: soft knee, both in
    // exposed scene light.
    scene: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cfg: Fft;
@group(0) @binding(1) var scene_tex: texture_2d<f32>;
// The exposure state `post_exposure.wgsl` writes; w is the multiplier.
@group(0) @binding(2) var<storage, read> exposure: vec4<f32>;
// The frame's spectrum, three planes of COL × HALF.
@group(0) @binding(3) var<storage, read_write> spectrum: array<vec2<f32>>;
// The kernel's spectrum, laid out the same.
@group(0) @binding(4) var<storage, read_write> kernel: array<vec2<f32>>;
// The kernel itself, ROW × COL texels of RGB.
@group(0) @binding(5) var<storage, read> kernel_texels: array<vec4<f32>>;
@group(0) @binding(6) var glare_out: texture_storage_2d<rgba16float, write>;

const HALF: u32 = ROW / 2u + 1u;
const THREADS: u32 = 256u;
const TAU: f32 = 6.283185307179586;

var<workgroup> row: array<vec4<f32>, ROW>;
var<workgroup> column: array<vec2<f32>, 3u * COL>;

fn index(plane: u32, y: u32, u: u32) -> u32 {
    return (plane * COL + y) * HALF + u;
}

fn reverse(i: u32, n: u32) -> u32 {
    return reverseBits(i) >> (32u - firstTrailingBit(n));
}

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn conj(a: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x, -a.y);
}

/// Radix-2 decimation in time over `row`, already in bit-reversed order.
/// `sign` is -1 forward, +1 inverse (unscaled). Each texel carries two
/// complex numbers.
fn fft_row(t: u32, sign: f32) {
    var half = 1u;
    loop {
        if half >= ROW {
            break;
        }
        for (var b = t; b < ROW / 2u; b += THREADS) {
            let k = b & (half - 1u);
            let a = (b - k) * 2u + k;
            let c = a + half;
            let angle = sign * TAU * f32(k) / f32(2u * half);
            let w = vec2<f32>(cos(angle), sin(angle));
            let x = row[a];
            let y = row[c];
            let yw = vec4<f32>(cmul(y.xy, w), cmul(y.zw, w));
            row[a] = x + yw;
            row[c] = x - yw;
        }
        workgroupBarrier();
        half *= 2u;
    }
}

/// The same over the three planes of `column`.
fn fft_column(t: u32, sign: f32) {
    var half = 1u;
    loop {
        if half >= COL {
            break;
        }
        for (var b = t; b < COL / 2u; b += THREADS) {
            let k = b & (half - 1u);
            let a = (b - k) * 2u + k;
            let c = a + half;
            let angle = sign * TAU * f32(k) / f32(2u * half);
            let w = vec2<f32>(cos(angle), sin(angle));
            for (var p = 0u; p < 3u; p++) {
                let x = column[p * COL + a];
                let y = cmul(column[p * COL + c], w);
                column[p * COL + a] = x + y;
                column[p * COL + c] = x - y;
            }
        }
        workgroupBarrier();
        half *= 2u;
    }
}

/// The part of `c` above the threshold, with a quadratic knee so the glare
/// fades in rather than switching on.
fn hot(c: vec3<f32>) -> vec3<f32> {
    let peak = max(max(c.r, c.g), c.b);
    let threshold = cfg.scene.y;
    let knee = cfg.scene.z;
    var soft = clamp(peak - threshold + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 1e-5);
    return c * (max(soft, peak - threshold) / max(peak, 1e-5));
}

/// Mean hot light over grid texel (`x`, `y`): every frame pixel it covers,
/// weighted by the area it covers, so a small source keeps its energy
/// wherever it falls.
fn hot_texel(x: u32, y: u32) -> vec3<f32> {
    let s = cfg.scene.x;
    let size = textureDimensions(scene_tex);
    let lo = vec2<f32>(f32(x), f32(y)) * s;
    let hi = lo + s;
    let first = vec2<u32>(floor(lo));
    let last = min(vec2<u32>(ceil(hi)), size);
    let e = exposure.w;
    var sum = vec3<f32>(0.0);
    for (var py = first.y; py < last.y; py++) {
        let wy = min(f32(py + 1u), hi.y) - max(f32(py), lo.y);
        for (var px = first.x; px < last.x; px++) {
            let wx = min(f32(px + 1u), hi.x) - max(f32(px), lo.x);
            let c = textureLoad(scene_tex, vec2<u32>(px, py), 0).rgb * e;
            sum += hot(c) * (wx * wy);
        }
    }
    return sum / (s * s);
}

/// Transform `row` forward and store its non-negative frequencies as the
/// three planes' row `y`.
fn store_row_spectrum(t: u32, y: u32) {
    workgroupBarrier();
    fft_row(t, -1.0);
    for (var u = t; u < HALF; u += THREADS) {
        let z = row[u];
        let mirror = conj(row[(ROW - u) % ROW].xy);
        // Red was real and green imaginary: their spectra are the even and
        // odd parts under conjugate mirroring.
        let red = 0.5 * (z.xy + mirror);
        let odd = 0.5 * (z.xy - mirror);
        let green = vec2<f32>(odd.y, -odd.x);
        spectrum[index(0u, y, u)] = red;
        spectrum[index(1u, y, u)] = green;
        spectrum[index(2u, y, u)] = z.zw;
    }
}

@compute @workgroup_size(256)
fn rows_forward(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let y = group.x;
    for (var i = t; i < ROW; i += THREADS) {
        var c = vec3<f32>(0.0);
        if i < cfg.region.x {
            c = hot_texel(i, y);
        }
        row[reverse(i, ROW)] = vec4<f32>(c.r, c.g, c.b, 0.0);
    }
    store_row_spectrum(t, y);
}

@compute @workgroup_size(256)
fn rows_kernel(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let y = group.x;
    for (var i = t; i < ROW; i += THREADS) {
        let c = kernel_texels[y * ROW + i];
        row[reverse(i, ROW)] = vec4<f32>(c.r, c.g, c.b, 0.0);
    }
    store_row_spectrum(t, y);
}

/// Load column `u` of the spectrum's first `rows` rows (zero below) into
/// `column`, bit-reversed, and transform it forward.
fn load_column(t: u32, u: u32, rows: u32) {
    for (var i = t; i < COL; i += THREADS) {
        let r = reverse(i, COL);
        for (var p = 0u; p < 3u; p++) {
            var v = vec2<f32>(0.0);
            if i < rows {
                v = spectrum[index(p, i, u)];
            }
            column[p * COL + r] = v;
        }
    }
    workgroupBarrier();
    fft_column(t, -1.0);
}

@compute @workgroup_size(256)
fn columns_forward(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let u = group.x;
    load_column(t, u, COL);
    for (var v = t; v < COL; v += THREADS) {
        for (var p = 0u; p < 3u; p++) {
            kernel[index(p, v, u)] = column[p * COL + v];
        }
    }
}

@compute @workgroup_size(256)
fn columns_convolve(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let u = group.x;
    load_column(t, u, cfg.region.y);
    // Multiply by the kernel's spectrum and put the result back in
    // bit-reversed order for the inverse transform.
    for (var v = t; v < COL; v += THREADS) {
        for (var p = 0u; p < 3u; p++) {
            column[p * COL + v] = cmul(column[p * COL + v], kernel[index(p, v, u)]);
        }
    }
    workgroupBarrier();
    for (var v = t; v < COL; v += THREADS) {
        let r = reverse(v, COL);
        if v < r {
            for (var p = 0u; p < 3u; p++) {
                let a = column[p * COL + v];
                column[p * COL + v] = column[p * COL + r];
                column[p * COL + r] = a;
            }
        }
    }
    workgroupBarrier();
    fft_column(t, 1.0);
    for (var y = t; y < cfg.region.y; y += THREADS) {
        for (var p = 0u; p < 3u; p++) {
            spectrum[index(p, y, u)] = column[p * COL + y] / f32(COL);
        }
    }
}

@compute @workgroup_size(256)
fn rows_inverse(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let y = group.x;
    for (var u = t; u < ROW; u += THREADS) {
        // A real row's spectrum mirrors: the negative frequencies are the
        // conjugates of the positive ones.
        var red: vec2<f32>;
        var green: vec2<f32>;
        var blue: vec2<f32>;
        if u < HALF {
            red = spectrum[index(0u, y, u)];
            green = spectrum[index(1u, y, u)];
            blue = spectrum[index(2u, y, u)];
        } else {
            red = conj(spectrum[index(0u, y, ROW - u)]);
            green = conj(spectrum[index(1u, y, ROW - u)]);
            blue = conj(spectrum[index(2u, y, ROW - u)]);
        }
        // Red + i·green, so the inverse puts red in the real part and
        // green in the imaginary.
        let pair = red + vec2<f32>(-green.y, green.x);
        row[reverse(u, ROW)] = vec4<f32>(pair, blue);
    }
    workgroupBarrier();
    fft_row(t, 1.0);
    for (var x = t; x < cfg.region.x; x += THREADS) {
        let v = row[x] / f32(ROW);
        // Rounding leaves the far field a hair either side of zero.
        textureStore(glare_out, vec2<u32>(x, y), vec4<f32>(max(v.xyz, vec3<f32>(0.0)), 1.0));
    }
}
