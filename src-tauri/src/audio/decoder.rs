use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    probe::Hint,
};
use symphonia::default::{get_codecs, get_probe};

use super::resample::resample_stereo_to_target;

/// Decoded audio data with channel information
pub struct DecodedAudio {
    /// Interleaved stereo samples [L0, R0, L1, R1, ...]
    pub samples: Vec<f32>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels (always 2 for stereo output)
    pub channels: u16,
}

impl DecodedAudio {
    /// Convert stereo to mono by averaging L and R channels.
    /// Useful for analysis functions (waveforms, mel specs, beat detection).
    pub fn to_mono(&self) -> Vec<f32> {
        stereo_to_mono(&self.samples)
    }
}

/// Convert stereo interleaved samples to mono by averaging L and R channels.
pub fn stereo_to_mono(stereo_samples: &[f32]) -> Vec<f32> {
    stereo_samples
        .chunks_exact(2)
        .map(|pair| (pair[0] + pair[1]) * 0.5)
        .collect()
}

/// Decode audio file to stereo interleaved samples at 48kHz.
/// All audio is output as stereo - mono sources are duplicated to both channels.
pub fn decode_track_samples(
    path: &Path,
    max_frames: Option<usize>,
) -> Result<DecodedAudio, String> {
    // Try ffmpeg first (Hybrid Approach)
    if let Ok(audio) = decode_ffmpeg(path, max_frames) {
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
    let mut frame_count = 0usize;

    'outer: loop {
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
                    frame_count += 1;

                    if let Some(limit) = max_frames {
                        if frame_count >= limit {
                            break 'outer;
                        }
                    }
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

    // Resample to 48kHz if needed (stereo-aware)
    let (final_samples, final_rate) = if sample_rate != 48000 {
        (
            resample_stereo_to_target(&samples, sample_rate, 48000),
            48000,
        )
    } else {
        (samples, sample_rate)
    };

    // Truncate to max_frames if specified (in stereo samples = frames * 2)
    let final_samples = if let Some(limit) = max_frames {
        let max_samples = limit * 2;
        if final_samples.len() > max_samples {
            final_samples[..max_samples].to_vec()
        } else {
            final_samples
        }
    } else {
        final_samples
    };

    Ok(DecodedAudio {
        samples: final_samples,
        sample_rate: final_rate,
        channels: 2,
    })
}

fn decode_ffmpeg(path: &Path, max_frames: Option<usize>) -> Result<DecodedAudio, String> {
    // Decode using ffmpeg - output stereo at 48kHz
    let output = Command::new("ffmpeg")
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
            "48000", // Force 48k
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

    let data = output.stdout;
    let mut samples: Vec<f32> = data
        .chunks_exact(4)
        .map(|chunk| {
            let arr: [u8; 4] = chunk.try_into().unwrap();
            f32::from_le_bytes(arr)
        })
        .collect();

    // Truncate to max_frames if specified (stereo = 2 samples per frame)
    if let Some(limit) = max_frames {
        let max_samples = limit * 2;
        if samples.len() > max_samples {
            samples.truncate(max_samples);
        }
    }

    Ok(DecodedAudio {
        samples,
        sample_rate: 48000,
        channels: 2,
    })
}
