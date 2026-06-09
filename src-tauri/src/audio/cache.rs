use once_cell::sync::Lazy;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::decoder::DecodedAudio;
use super::resample::resample_stereo_to_target;

/// Cache file format version - increment when format changes
const CACHE_VERSION: u32 = 2;

/// Process-wide RAM cache of fully-decoded tracks, keyed by `"{hash}@{rate}"`, so
/// a track's expensive decode/disk-read happens **once** and every later consumer
/// (playback *and* analysis — they hit the same `(hash, rate)`) reuses it. Without
/// this, opening a track for playback and then compositing it decodes the same
/// audio twice. Bounded LRU; entries are `Arc` so a hit is an O(1) clone.
static DECODE_RAM_CACHE: Lazy<Mutex<Vec<(String, Arc<DecodedAudio>)>>> =
    Lazy::new(|| Mutex::new(Vec::new()));
const DECODE_RAM_CACHE_MAX: usize = 3;

/// [`load_or_decode_audio`] with a shared RAM cache (returns an `Arc`). Use this
/// from both the playback and the analysis paths so the decode is not duplicated.
pub fn load_or_decode_audio_shared(
    track_path: &Path,
    track_hash: &str,
    target_rate: u32,
) -> Result<Arc<DecodedAudio>, String> {
    let key = format!("{track_hash}@{target_rate}");
    if let Ok(cache) = DECODE_RAM_CACHE.lock() {
        if let Some((_, a)) = cache.iter().find(|(k, _)| *k == key) {
            return Ok(a.clone());
        }
    }
    let decoded = Arc::new(load_or_decode_audio(track_path, track_hash, target_rate)?);
    if let Ok(mut cache) = DECODE_RAM_CACHE.lock() {
        if cache.len() >= DECODE_RAM_CACHE_MAX {
            cache.remove(0); // evict oldest
        }
        cache.push((key, decoded.clone()));
    }
    Ok(decoded)
}

fn cache_dir_for_track(track_path: &Path) -> Result<PathBuf, String> {
    let parent = track_path
        .parent()
        .ok_or_else(|| format!("Track path {} has no parent", track_path.display()))?;
    let cache_dir = parent.join("cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir {}: {}", cache_dir.display(), e))?;
    Ok(cache_dir)
}

/// Read cache file with version, sample rate, channels, and samples
fn read_cache_file(cache_file: &Path) -> Result<DecodedAudio, String> {
    let mut reader = BufReader::new(
        File::open(cache_file)
            .map_err(|e| format!("Failed to open cache {}: {}", cache_file.display(), e))?,
    );

    // Read version
    let mut version_buf = [0u8; 4];
    reader
        .read_exact(&mut version_buf)
        .map_err(|e| format!("Failed to read cache version: {}", e))?;
    let version = u32::from_le_bytes(version_buf);

    // Only accept current version (invalidates old mono caches)
    if version != CACHE_VERSION {
        return Err(format!(
            "Cache version mismatch: expected {}, got {}",
            CACHE_VERSION, version
        ));
    }

    // Read sample rate
    let mut rate_buf = [0u8; 4];
    reader
        .read_exact(&mut rate_buf)
        .map_err(|e| format!("Failed to read cache header: {}", e))?;
    let sample_rate = u32::from_le_bytes(rate_buf);

    // Read channel count
    let mut channels_buf = [0u8; 2];
    reader
        .read_exact(&mut channels_buf)
        .map_err(|e| format!("Failed to read cache channels: {}", e))?;
    let channels = u16::from_le_bytes(channels_buf);

    // Read sample count
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

    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels,
    })
}

/// Write cache file with version, sample rate, channels, and samples
fn write_cache_file(cache_file: &Path, audio: &DecodedAudio) -> Result<(), String> {
    let file = File::create(cache_file)
        .map_err(|e| format!("Failed to create cache {}: {}", cache_file.display(), e))?;
    let mut writer = BufWriter::new(file);

    // Write version
    writer
        .write_all(&CACHE_VERSION.to_le_bytes())
        .map_err(|e| format!("Failed to write cache version: {}", e))?;

    // Write sample rate
    writer
        .write_all(&audio.sample_rate.to_le_bytes())
        .map_err(|e| format!("Failed to write cache header: {}", e))?;

    // Write channel count
    writer
        .write_all(&audio.channels.to_le_bytes())
        .map_err(|e| format!("Failed to write cache channels: {}", e))?;

    // Write sample count and samples
    writer
        .write_all(&(audio.samples.len() as u64).to_le_bytes())
        .map_err(|e| format!("Failed to write cache length: {}", e))?;
    for sample in &audio.samples {
        writer
            .write_all(&sample.to_le_bytes())
            .map_err(|e| format!("Failed to write cache samples: {}", e))?;
    }
    writer
        .flush()
        .map_err(|e| format!("Failed to flush cache file: {}", e))
}

/// Load audio from cache or decode from file.
/// Returns stereo interleaved samples at the target sample rate.
pub fn load_or_decode_audio(
    track_path: &Path,
    track_hash: &str,
    target_rate: u32,
) -> Result<DecodedAudio, String> {
    use super::decoder::decode_track_samples;

    if let Ok(cache_dir) = cache_dir_for_track(track_path) {
        let cache_file = cache_dir.join(format!("{}.pcm", track_hash));
        if cache_file.exists() {
            match read_cache_file(&cache_file) {
                Ok(cached) => {
                    if cached.sample_rate == target_rate || target_rate == 0 {
                        return Ok(cached);
                    }
                    // Resample cached audio (stereo-aware)
                    let resampled =
                        resample_stereo_to_target(&cached.samples, cached.sample_rate, target_rate);
                    return Ok(DecodedAudio {
                        samples: resampled,
                        sample_rate: target_rate,
                        channels: cached.channels,
                    });
                }
                Err(_) => {
                    // Cache is stale or corrupt - delete it
                    let _ = std::fs::remove_file(&cache_file);
                }
            }
        }

        // Decode fresh
        let decoded = decode_track_samples(track_path, None)?;

        // Optionally resample if needed
        let final_audio = if target_rate > 0 && decoded.sample_rate != target_rate {
            let resampled =
                resample_stereo_to_target(&decoded.samples, decoded.sample_rate, target_rate);
            DecodedAudio {
                samples: resampled,
                sample_rate: target_rate,
                channels: decoded.channels,
            }
        } else {
            decoded
        };

        // Cache the result
        if let Err(err) = write_cache_file(&cache_file, &final_audio) {
            eprintln!(
                "[audio-cache] failed to write cache {}: {}",
                cache_file.display(),
                err
            );
        }

        return Ok(final_audio);
    }

    decode_track_samples(track_path, None)
}
