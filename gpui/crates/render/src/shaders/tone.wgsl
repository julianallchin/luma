// Tone curves shared by the composite pass and the post chain's tonemap.
// Scene-linear in, display-linear out.

const LINEAR_SRGB_TO_LINEAR_REC2020 = mat3x3<f32>(
    vec3<f32>(0.6274, 0.0691, 0.0164),
    vec3<f32>(0.3293, 0.9195, 0.0880),
    vec3<f32>(0.0433, 0.0113, 0.8956),
);
const LINEAR_REC2020_TO_LINEAR_SRGB = mat3x3<f32>(
    vec3<f32>(1.6605, -0.1246, -0.0182),
    vec3<f32>(-0.5876, 1.1329, -0.1006),
    vec3<f32>(-0.0728, -0.0083, 1.1187),
);
const AGX_INSET = mat3x3<f32>(
    vec3<f32>(0.856627153315983, 0.137318972929847, 0.11189821299995),
    vec3<f32>(0.0951212405381588, 0.761241990602591, 0.0767994186031903),
    vec3<f32>(0.0482516061458583, 0.101439036467562, 0.811302368396859),
);
const AGX_OUTSET = mat3x3<f32>(
    vec3<f32>(1.1271005818144368, -0.1413297634984383, -0.14132976349843826),
    vec3<f32>(-0.11060664309660323, 1.157823702216272, -0.11060664309660294),
    vec3<f32>(-0.016493938717834573, -0.016493938717834257, 1.2519364065950405),
);

fn agx_contrast(x: vec3<f32>) -> vec3<f32> {
    let x2 = x * x;
    let x4 = x2 * x2;
    return 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232;
}

/// three's `AgXToneMapping` at exposure 1, term for term. This is the pairing
/// the haze's HDR core was designed against: white-hot is the display
/// transform's answer, not a shader gate.
fn agx(color_in: vec3<f32>) -> vec3<f32> {
    let min_ev = -12.47393;
    let max_ev = 4.026069;
    var color = LINEAR_SRGB_TO_LINEAR_REC2020 * color_in;
    color = AGX_INSET * color;
    color = log2(max(color, vec3<f32>(1e-10)));
    color = clamp((color - min_ev) / (max_ev - min_ev), vec3<f32>(0.0), vec3<f32>(1.0));
    color = agx_contrast(color);
    color = AGX_OUTSET * color;
    color = pow(max(color, vec3<f32>(0.0)), vec3<f32>(2.2));
    color = LINEAR_REC2020_TO_LINEAR_SRGB * color;
    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
}

/// Display value where the HDR expansion starts. Every curve here puts
/// scene-linear 1.0 (diffuse white) at about 0.6, so everything up to diffuse
/// white keeps exactly its SDR value; only the shoulder above it changes.
const HDR_KNEE: f32 = 0.6;

/// A curve's SDR answer `sdr` for a display with `headroom` times SDR white
/// to spare.
///
/// The SDR picture below the knee is kept, so the UI around it keeps its
/// meaning. The curve's shoulder holds everything from diffuse white up to
/// its white point in the last 0.4 of the range; that span is re-expanded
/// into [knee, headroom]:
///
///   m' = m + (headroom - 1) * t^2,   t = (m - knee) / (1 - knee)
///
/// on the pixel's largest channel m, and the pixel is scaled by m' / m. The
/// result meets the SDR one at the knee with the same value and slope (no
/// visible seam in a gradient), rises monotonically (slope >= 1), and sends
/// the curve's white to the headroom. Scaling all three channels by one
/// factor keeps the curve's hue and its highlight desaturation. With
/// headroom 1 it is the SDR answer exactly.
fn hdr_expand(sdr: vec3<f32>, headroom: f32) -> vec3<f32> {
    let peak = max(max(sdr.r, sdr.g), sdr.b);
    if headroom <= 1.0 || peak <= HDR_KNEE {
        return sdr;
    }
    let t = (peak - HDR_KNEE) / (1.0 - HDR_KNEE);
    return sdr * ((peak + (headroom - 1.0) * t * t) / peak);
}

/// AgX for a display with `headroom` times SDR white to spare.
fn agx_hdr(color_in: vec3<f32>, headroom: f32) -> vec3<f32> {
    return hdr_expand(agx(color_in), headroom);
}

/// AgX with Blender's "Punchy" look (power 1.35, saturation 1.4), applied
/// between the contrast sigmoid and the outset as in Benjamin Wrensch's
/// minimal AgX. Kept apart from [`agx`] so that function's arithmetic, and
/// every image captured through it, stays exactly as it was.
fn agx_punchy(color_in: vec3<f32>) -> vec3<f32> {
    let min_ev = -12.47393;
    let max_ev = 4.026069;
    var color = LINEAR_SRGB_TO_LINEAR_REC2020 * color_in;
    color = AGX_INSET * color;
    color = log2(max(color, vec3<f32>(1e-10)));
    color = clamp((color - min_ev) / (max_ev - min_ev), vec3<f32>(0.0), vec3<f32>(1.0));
    color = agx_contrast(color);
    let lw = vec3<f32>(0.2126, 0.7152, 0.0722);
    color = pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.35));
    let luma = dot(color, lw);
    color = luma + 1.4 * (color - luma);
    color = AGX_OUTSET * color;
    color = pow(max(color, vec3<f32>(0.0)), vec3<f32>(2.2));
    color = LINEAR_REC2020_TO_LINEAR_SRGB * color;
    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
}

/// Krzysztof Narkowicz's fit of the ACES RRT+ODT, per channel, including
/// its 0.6 input scale. Display-linear out; the target encodes sRGB.
fn aces_fit(color_in: vec3<f32>) -> vec3<f32> {
    let x = max(color_in, vec3<f32>(0.0)) * 0.6;
    return clamp(
        (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

/// The tone curve `curve` selects (`ToneCurve::shader_code`).
fn tone_curve(color: vec3<f32>, curve: u32) -> vec3<f32> {
    switch curve {
        case 1u: { return agx_punchy(color); }
        case 2u: { return aces_fit(color); }
        default: { return agx(color); }
    }
}

/// One least-significant bit of triangular noise for an 8-bit sRGB target.
/// An HDR frame takes the same noise. One 8-bit sRGB step is about one 10-bit
/// PQ step near SDR white and several in the shadows, so it also breaks up
/// the steps of an HDR10 swapchain.
///
/// The amplitude is in *display* code values, not in the linear ones the
/// shader returns. The target is sRGB-encoded, so a fixed linear step is a
/// dozen code values in the shadows and a third of one in the highlights —
/// grain at one end and banding still at the other. Dividing by the encoder's
/// slope makes it one code value everywhere.
fn display_dither(display: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    // Two decorrelated hashes make a triangular distribution, which has no DC
    // term: a uniform one would lift the whole frame by half a bit.
    let a = fract(sin(dot(frag, vec2<f32>(12.9898, 78.233))) * 43758.5453);
    let b = fract(sin(dot(frag, vec2<f32>(63.7264, 10.873))) * 32361.4771);
    // Inverse slope of the sRGB transfer curve, 2.4 / 1.055 * L^(1 - 1/2.4).
    let step = 2.2749 * pow(max(display, vec3<f32>(1e-4)), vec3<f32>(0.58333)) / 255.0;
    return (a - b) * step;
}
