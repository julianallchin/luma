//! Encoding a rendered frame as a PNG.
//!
//! [`Renderer::render`](crate::Renderer::render) hands back tightly packed
//! RGBA8. Turning that into a file is the same three lines everywhere it is
//! done — the tracked goldens, an agent's offscreen venue frame — so it is
//! spelled once here rather than once per caller.

use std::io::Write;
use std::path::Path;

/// `rgba` as PNG bytes.
///
/// # Errors
/// Fails if `rgba` is not exactly `width * height * 4` bytes, or if the encoder
/// rejects the dimensions.
pub fn encode(rgba: &[u8], width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let expected = width as usize * height as usize * 4;
    anyhow::ensure!(
        rgba.len() == expected,
        "expected {expected} RGBA bytes for {width}x{height}, got {}",
        rgba.len()
    );
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(out)
}

/// [`encode`], written to `path`.
///
/// # Errors
/// Fails if the frame cannot be encoded or the file cannot be written.
pub fn write(path: &Path, rgba: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
    let bytes = encode(rgba, width, height)?;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

/// A presented stage frame as RGBA8, for writing with [`write`].
///
/// A presented frame is BGRA8 sRGB-encoded (4 bytes a texel) or, on an HDR
/// display, RGBA half-float linear light (8 bytes a texel); the length says
/// which. HDR values above SDR white clip to white here.
///
/// # Errors
/// Fails if `bytes` is neither size for `width * height`.
pub fn rgba8_from_presented(bytes: &[u8], width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let texels = width as usize * height as usize;
    if bytes.len() == texels * 4 {
        return Ok(bytes
            .chunks_exact(4)
            .flat_map(|bgra| [bgra[2], bgra[1], bgra[0], bgra[3]])
            .collect());
    }
    anyhow::ensure!(
        bytes.len() == texels * 8,
        "expected {} or {} bytes for {width}x{height}, got {}",
        texels * 4,
        texels * 8,
        bytes.len()
    );
    let encode = |linear: f32| {
        let c = linear.clamp(0.0, 1.0);
        let srgb = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round() as u8
    };
    Ok(bytes
        .chunks_exact(8)
        .flat_map(|texel| {
            let channel =
                |i: usize| half::f16::from_le_bytes([texel[2 * i], texel[2 * i + 1]]).to_f32();
            [
                encode(channel(0)),
                encode(channel(1)),
                encode(channel(2)),
                (channel(3).clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_a_png_header() {
        let png = super::encode(&[255, 0, 0, 255, 0, 255, 0, 255], 2, 1).expect("encodes");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn rejects_a_short_buffer() {
        assert!(super::encode(&[0; 4], 2, 1).is_err());
    }
}
