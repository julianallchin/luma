use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

fn cache_dir_for_track(track_path: &Path) -> Result<PathBuf, String> {
    let parent = track_path
        .parent()
        .ok_or_else(|| format!("Track path {} has no parent", track_path.display()))?;
    let cache_dir = parent.join("cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir {}: {}", cache_dir.display(), e))?;
    Ok(cache_dir)
}

pub fn read_cache_file(cache_file: &Path) -> Result<(u32, Vec<f32>), String> {
    let mut reader = BufReader::new(
        File::open(cache_file)
            .map_err(|e| format!("Failed to open cache {}: {}", cache_file.display(), e))?,
    );

    let mut rate_buf = [0u8; 4];
    reader
        .read_exact(&mut rate_buf)
        .map_err(|e| format!("Failed to read cache header: {}", e))?;
    let sample_rate = u32::from_le_bytes(rate_buf);

    let mut len_buf = [0u8; 8];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| format!("Failed to read cache length: {}", e))?;
    let len = u64::from_le_bytes(len_buf) as usize;

    let mut samples = vec![0f32; len];
    for sample in &mut samples {
        let mut buf = [0u8; 4];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("Failed to read cached samples: {}", e))?;
        *sample = f32::from_le_bytes(buf);
    }

    Ok((sample_rate, samples))
}

pub fn write_cache_file(
    cache_file: &Path,
    sample_rate: u32,
    samples: &[f32],
) -> Result<(), String> {
    let file = File::create(cache_file)
        .map_err(|e| format!("Failed to create cache {}: {}", cache_file.display(), e))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(&sample_rate.to_le_bytes())
        .map_err(|e| format!("Failed to write cache header: {}", e))?;
    writer
        .write_all(&(samples.len() as u64).to_le_bytes())
        .map_err(|e| format!("Failed to write cache length: {}", e))?;
    for sample in samples {
        writer
            .write_all(&sample.to_le_bytes())
            .map_err(|e| format!("Failed to write cache samples: {}", e))?;
    }
    writer
        .flush()
        .map_err(|e| format!("Failed to flush cache file: {}", e))
}

pub fn load_or_decode_audio(
    track_path: &Path,
    track_hash: &str,
    target_rate: u32,
) -> Result<(Vec<f32>, u32), String> {
    use super::decoder::decode_track_samples;
    use super::resample::resample_to_target;

    if let Ok(cache_dir) = cache_dir_for_track(track_path) {
        let cache_file = cache_dir.join(format!("{}.pcm", track_hash));
        if cache_file.exists() {
            if let Ok((cached_rate, cached_samples)) = read_cache_file(&cache_file) {
                if cached_rate == target_rate || target_rate == 0 {
                    return Ok((cached_samples, cached_rate));
                }
                let resampled = resample_to_target(&cached_samples, cached_rate, target_rate);
                return Ok((resampled, target_rate));
            }
        }

        let (decoded, src_rate) = decode_track_samples(track_path, None)?;
        let (samples, final_rate) = if target_rate > 0 && src_rate > target_rate {
            (
                resample_to_target(&decoded, src_rate, target_rate),
                target_rate,
            )
        } else {
            (decoded, src_rate)
        };

        if let Err(err) = write_cache_file(&cache_file, final_rate, &samples) {
            eprintln!(
                "[audio-cache] failed to write cache {}: {}",
                cache_file.display(),
                err
            );
        }

        return Ok((samples, final_rate));
    }

    decode_track_samples(track_path, None)
}
