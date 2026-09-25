use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    probe::Hint,
};
use symphonia::default::{get_codecs, get_probe};

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
    // Try ffmpeg first (Hybrid Approach)
    if let Ok(audio) = decode_ffmpeg(path) {
        return Ok(audio);
    }

    // Fallback to Symphonia (Original Implementation)
    let file = File::open(path).map_err(|e| format!("Failed to open track for decoding: {}", e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }

    let probed = get_probe()
        .format(&hint, mss, &FormatOptions::default(), &Default::default())
        .map_err(|e| format!("Failed to probe audio file: {}", e))?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| "Audio file contains no default track".to_string())?;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| "Track missing sample rate".to_string())?;

    let mut decoder = get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create decoder: {}", e))?;

    // Output is always stereo interleaved
    let mut samples = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(format!("Failed to read audio packet: {}", err)),
        };

        match decoder.decode(&packet) {
            Ok(audio_buffer) => {
                let spec = *audio_buffer.spec();
                let mut sample_buffer =
                    SampleBuffer::<f32>::new(audio_buffer.capacity() as u64, spec);
                sample_buffer.copy_interleaved_ref(audio_buffer);

                let src_channels = spec.channels.count();
                let total_samples = sample_buffer.samples().len();
                let frames = if src_channels == 0 {
                    0
                } else {
                    total_samples / src_channels
                };
                if frames == 0 || src_channels == 0 {
                    continue;
                }

                let interleaved = sample_buffer.samples();
                for frame_idx in 0..frames {
                    let base = frame_idx * src_channels;

                    // Convert to stereo: duplicate mono, take first 2 channels of multi-channel
                    let (left, right) = if src_channels == 1 {
                        let s = interleaved[base];
                        (s, s)
                    } else {
                        (interleaved[base], interleaved[base + 1])
                    };

                    samples.push(left);
                    samples.push(right);
                }
            }
            Err(err) => {
                return Err(format!("Failed to decode audio packet: {}", err));
            }
        }
    }

    if samples.is_empty() {
        return Err("Audio file produced no samples".into());
    }

    Ok(resample(samples, sample_rate))
}

fn decode_ffmpeg(path: &Path) -> Result<Vec<f32>, String> {
    let ffmpeg = crate::ffmpeg_env::ffmpeg_path();
    let mut cmd = Command::new(&ffmpeg);
    crate::cmd_util::no_window(&mut cmd);
    let output = cmd
        .args([
            "-i",
            path.to_str().unwrap(),
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

/// Resample stereo interleaved audio [L0, R0, L1, R1, ...] from `src_rate` to
/// [`SAMPLE_RATE`] using linear interpolation, each channel independently.
fn resample(samples: Vec<f32>, src_rate: u32) -> Vec<f32> {
    if src_rate == 0 || src_rate == SAMPLE_RATE {
        return samples;
    }

    // Number of stereo frames
    let src_frames = samples.len() / 2;
    if src_frames == 0 {
        return Vec::new();
    }

    let ratio = SAMPLE_RATE as f64 / src_rate as f64;
    let new_frames = ((src_frames as f64) * ratio).ceil() as usize;
    let mut output = Vec::with_capacity(new_frames * 2);

    for i in 0..new_frames {
        let src_pos = (i as f64) / ratio;
        let lower_frame = src_pos.floor() as usize;
        let frac = (src_pos - lower_frame as f64) as f32;

        if lower_frame >= src_frames - 1 {
            // At or past the end - use last frame
            let last_idx = (src_frames - 1) * 2;
            output.push(samples[last_idx]); // L
            output.push(samples[last_idx + 1]); // R
        } else {
            // Linear interpolation for each channel independently
            let lower_idx = lower_frame * 2;
            let upper_idx = (lower_frame + 1) * 2;

            // Left channel
            let left = samples[lower_idx] * (1.0 - frac) + samples[upper_idx] * frac;
            // Right channel
            let right = samples[lower_idx + 1] * (1.0 - frac) + samples[upper_idx + 1] * frac;

            output.push(left);
            output.push(right);
        }
    }

    output
}
