use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    probe::Hint,
};
use symphonia::default::{get_codecs, get_probe};

pub fn decode_track_samples(
    path: &Path,
    max_samples: Option<usize>,
) -> Result<(Vec<f32>, u32), String> {
    // Try ffmpeg first (Hybrid Approach)
    if let Ok((mut samples, sample_rate)) = decode_ffmpeg(path) {
        if let Some(limit) = max_samples {
            if samples.len() > limit {
                samples.truncate(limit);
            }
        }
        return Ok((samples, sample_rate));
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

    let mut samples = Vec::new();

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

                let channels = spec.channels.count();
                let total_samples = sample_buffer.samples().len();
                let frames = if channels == 0 {
                    0
                } else {
                    total_samples / channels
                };
                if frames == 0 || channels == 0 {
                    continue;
                }

                let interleaved = sample_buffer.samples();
                for frame_index in 0..frames {
                    let mut sum = 0.0f32;
                    for channel in 0..channels {
                        sum += interleaved[frame_index * channels + channel];
                    }
                    samples.push(sum / channels as f32);
                    if let Some(limit) = max_samples {
                        if samples.len() >= limit {
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

    if let Some(limit) = max_samples {
        if samples.len() > limit {
            samples.truncate(limit);
        }
    }

    Ok((samples, sample_rate))
}

fn decode_ffmpeg(path: &Path) -> Result<(Vec<f32>, u32), String> {
    // 1. Get sample rate using ffprobe
    let output = Command::new("ffprobe")
        .args(&[
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            path.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("Failed to run ffprobe: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let sample_rate_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let sample_rate: u32 = sample_rate_str
        .parse()
        .map_err(|_| "Failed to parse sample rate")?;

    // 2. Decode using ffmpeg
    let output = Command::new("ffmpeg")
        .args(&[
            "-i",
            path.to_str().unwrap(),
            "-f",
            "f32le",
            "-ac",
            "1", // Mono
            "-acodec",
            "pcm_f32le",
            "-ar",
            &sample_rate.to_string(), // Keep original rate
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
    let samples: Vec<f32> = data
        .chunks_exact(4)
        .map(|chunk| {
            let arr: [u8; 4] = chunk.try_into().unwrap();
            f32::from_le_bytes(arr)
        })
        .collect();

    Ok((samples, sample_rate))
}
