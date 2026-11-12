use std::path::Path;

/// The one sample rate of decoded audio. Every decode, every `.pcm` cache and
/// every analysis consumer is at this rate; playback converts to the device's
/// rate in its output stream and nowhere else.
pub const SAMPLE_RATE: u32 = 48_000;

/// Convert stereo interleaved samples to mono by averaging L and R channels.
pub fn stereo_to_mono(stereo_samples: &[f32]) -> Vec<f32> {
    stereo_samples
        .chunks_exact(2)
        .map(|pair| (pair[0] + pair[1]) * 0.5)
        .collect()
}

/// Decode audio file to stereo interleaved samples [L0, R0, L1, R1, ...] at
/// [`SAMPLE_RATE`]. Mono sources are duplicated to both channels. Only the
/// decode cache calls this; everything else goes through it.
pub(super) fn decode_track_samples(path: &Path) -> Result<Vec<f32>, String> {
    let ffmpeg = crate::runtime::ffmpeg::ffmpeg_path();
    let output = crate::runtime::command::command(&ffmpeg)
        .arg("-i")
        .arg(path)
        .args([
            "-f",
            "f32le",
            "-ac",
            "2", // Stereo
            "-acodec",
            "pcm_f32le",
            "-ar",
            SAMPLE_RATE.to_string().as_str(),
            "pipe:1",
        ])
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(output
        .stdout
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect())
}
