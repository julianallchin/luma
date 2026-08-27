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
