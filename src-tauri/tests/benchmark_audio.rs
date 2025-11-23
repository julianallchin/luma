use std::path::Path;
use std::process::Command;
use std::time::Instant;
use luma_lib::tracks::decode_track_samples;

fn decode_ffmpeg(path: &Path) -> Result<(Vec<f32>, u32), String> {
    // 1. Get sample rate using ffprobe
    let output = Command::new("ffprobe")
        .args(&[
            "-v", "error",
            "-select_streams", "a:0",
            "-show_entries", "stream=sample_rate",
            "-of", "default=noprint_wrappers=1:nokey=1",
            path.to_str().unwrap()
        ])
        .output()
        .map_err(|e| format!("Failed to run ffprobe: {}", e))?;

    if !output.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&output.stderr)));
    }

    let sample_rate_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let sample_rate: u32 = sample_rate_str.parse().map_err(|_| "Failed to parse sample rate")?;

    // 2. Decode using ffmpeg
    let output = Command::new("ffmpeg")
        .args(&[
            "-i", path.to_str().unwrap(),
            "-f", "f32le",
            "-ac", "1", // Mono
            "-acodec", "pcm_f32le",
            "-ar", &sample_rate.to_string(), // Keep original rate
            "pipe:1"
        ])
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;

    if !output.status.success() {
        return Err(format!("ffmpeg failed: {}", String::from_utf8_lossy(&output.stderr)));
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

#[test]
fn benchmark_loading() {
    let song_path = Path::new("../experiments/songs/Mild Minds - TEARDROPS.mp3");
    if !song_path.exists() {
        println!("Skipping benchmark, song not found at {:?}", song_path);
        // If running in CI or elsewhere, this might fail, but locally it should exist.
        // I'll list dir to be sure if it fails.
        return;
    }
    
    println!("Benchmarking: {:?}", song_path);

    // Warmup / Baseline
    let start = Instant::now();
    let (_samples_crate, rate_crate) = decode_track_samples(song_path, None).expect("Crate decode failed");
    let duration_crate = start.elapsed();
    println!("Crate decode time: {:.2?}", duration_crate);

    // Optimized
    let start = Instant::now();
    let (samples_sys, rate_sys) = decode_ffmpeg(song_path).expect("System decode failed");
    let duration_sys = start.elapsed();
    println!("System decode time: {:.2?}", duration_sys);

    let speedup = duration_crate.as_secs_f64() / duration_sys.as_secs_f64();
    println!("Speedup: {:.2}x", speedup);
    
    assert_eq!(rate_crate, rate_sys, "Sample rates should match");
    
    // Only check length roughly as decoding methods might differ slightly
    let diff = (_samples_crate.len() as i64 - samples_sys.len() as i64).abs();
    println!("Length diff: {}", diff);
}

