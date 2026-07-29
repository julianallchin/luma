use once_cell::sync::Lazy;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::decoder::DecodedAudio;
use super::resample::resample_stereo_to_target;

/// Cache file format version - increment when format changes.
/// Every writer emits this; [`read_pcm_file`] accepts 1 or 2 (both describe the
/// identical byte layout — v1 files are the eval engine's mono caches).
pub const CACHE_VERSION: u32 = 2;

/// Byte length of the `.pcm` header: `version u32 | sample_rate u32 |
/// channels u16 | len u64`, all little-endian, then `len` × f32 LE samples.
/// Public because the agent data plane hands this offset straight to NumPy.
pub const PCM_HEADER_LEN: usize = 18;

/// A PCM buffer exactly as it lives in a `.pcm` cache file: `samples` is
/// channel-interleaved, so `frames = samples.len() / channels`.
#[derive(Debug, Clone)]
pub struct PcmData {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Format version as found on disk. Callers that care which generation of
    /// file they got (the stereo decode cache does — see [`load_or_decode_audio`])
    /// inspect it. [`write_pcm_file`] ignores it and always writes
    /// [`CACHE_VERSION`], so a read→write round trip upgrades in place.
    pub version: u32,
}

impl From<PcmData> for DecodedAudio {
    fn from(p: PcmData) -> Self {
        let PcmData {
            samples,
            sample_rate,
            channels,
            ..
        } = p;
        Self {
            samples,
            sample_rate,
            channels,
        }
    }
}

/// Read a `.pcm` cache file. The one reader for this format — the eval engine,
/// the golden harness and the decode cache all go through here.
///
/// Accepts format versions 1 and 2. Anything else, a truncated header, or a
/// payload shorter than the declared length is an error (callers treat a failed
/// read as "cache miss, re-derive").
pub fn read_pcm_file(path: &Path) -> Result<PcmData, String> {
    let mut reader = BufReader::new(
        File::open(path).map_err(|e| format!("Failed to open pcm {}: {e}", path.display()))?,
    );

    let mut header = [0u8; PCM_HEADER_LEN];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("Failed to read pcm header {}: {e}", path.display()))?;

    let version = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if version == 0 || version > CACHE_VERSION {
        return Err(format!(
            "Unsupported pcm version {version} in {} (expected 1..={CACHE_VERSION})",
            path.display()
        ));
    }
    let sample_rate = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let channels = u16::from_le_bytes(header[8..10].try_into().unwrap());
    let len = u64::from_le_bytes(header[10..18].try_into().unwrap()) as usize;

    let mut bytes = vec![0u8; len * 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| format!("Failed to read pcm samples {}: {e}", path.display()))?;
    let samples = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();

    Ok(PcmData {
        samples,
        sample_rate,
        channels,
        version,
    })
}

/// Write a `.pcm` cache file at [`CACHE_VERSION`], creating the parent directory.
/// The one writer for this format. Takes the fields rather than a [`PcmData`] so
/// callers holding a large buffer don't have to clone it.
pub fn write_pcm_file(
    path: &Path,
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create pcm dir {}: {e}", parent.display()))?;
    }
    let file =
        File::create(path).map_err(|e| format!("Failed to create pcm {}: {e}", path.display()))?;
    let mut writer = BufWriter::new(file);

    let mut header = [0u8; PCM_HEADER_LEN];
    header[0..4].copy_from_slice(&CACHE_VERSION.to_le_bytes());
    header[4..8].copy_from_slice(&sample_rate.to_le_bytes());
    header[8..10].copy_from_slice(&channels.to_le_bytes());
    header[10..18].copy_from_slice(&(samples.len() as u64).to_le_bytes());
    writer
        .write_all(&header)
        .map_err(|e| format!("Failed to write pcm header {}: {e}", path.display()))?;

    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    writer
        .write_all(&bytes)
        .map_err(|e| format!("Failed to write pcm samples {}: {e}", path.display()))?;
    writer
        .flush()
        .map_err(|e| format!("Failed to flush pcm {}: {e}", path.display()))
}

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

/// Directory for a `.pcm` cache derived from the audio file's **own** location:
/// `<parent of the audio file>/cache/`.
///
/// Deliberately file-relative rather than going through
/// [`crate::storage::StorageRoot`]: this same function serves imported tracks
/// (under `<root>/tracks/`, where it lands on exactly `mix_pcm_path`), separated
/// stems (under `<root>/tracks/stems/<hash>/`, i.e. `stem_pcm_path`), *and*
/// arbitrary paths handed in by dev binaries and the importer before a file has
/// moved into the library. `StorageRoot` names the two well-known results so
/// readers don't re-derive them; this keeps the arbitrary-path case working.
fn cache_dir_for_track(track_path: &Path) -> Result<PathBuf, String> {
    let parent = track_path
        .parent()
        .ok_or_else(|| format!("Track path {} has no parent", track_path.display()))?;
    let cache_dir = parent.join("cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir {}: {}", cache_dir.display(), e))?;
    Ok(cache_dir)
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
            // `read_pcm_file` accepts v1 for the eval engine's mono caches, but
            // the *stereo decode* cache treats v1 as stale: a v1 file at this
            // path is the old mono format and must be re-decoded, not
            // reinterpreted as interleaved stereo.
            match read_pcm_file(&cache_file).and_then(|p| {
                if p.version == CACHE_VERSION {
                    Ok(DecodedAudio::from(p))
                } else {
                    Err(format!("stale decode cache (version {})", p.version))
                }
            }) {
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
        if let Err(err) = write_pcm_file(
            &cache_file,
            &final_audio.samples,
            final_audio.sample_rate,
            final_audio.channels,
        ) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("luma-pcm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// Overwrite the 4-byte version field of an already-written file.
    fn stamp_version(path: &Path, version: u32) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes[0..4].copy_from_slice(&version.to_le_bytes());
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn round_trips_samples_rate_and_channels() {
        let path = tmp("roundtrip.pcm");
        let samples: Vec<f32> = vec![0.0, -1.0, 0.5, 0.25, 1.0, -0.125];
        write_pcm_file(&path, &samples, 48_000, 2).unwrap();

        let read = read_pcm_file(&path).unwrap();
        assert_eq!(read.samples, samples);
        assert_eq!(read.sample_rate, 48_000);
        assert_eq!(read.channels, 2);
        assert_eq!(read.version, CACHE_VERSION);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len() as usize,
            PCM_HEADER_LEN + samples.len() * 4
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn writes_current_version_and_creates_parent_dirs() {
        let path = tmp("nested/deeper/v.pcm");
        write_pcm_file(&path, &[1.0], 44_100, 1).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2);
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).ok();
    }

    /// v1 files are the eval engine's older mono caches — still readable.
    #[test]
    fn reads_version_1_files() {
        let path = tmp("v1.pcm");
        write_pcm_file(&path, &[0.5, -0.5], 22_050, 1).unwrap();
        stamp_version(&path, 1);

        let read = read_pcm_file(&path).unwrap();
        assert_eq!(read.version, 1);
        assert_eq!(read.samples, vec![0.5, -0.5]);
        assert_eq!(read.sample_rate, 22_050);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_unknown_version() {
        let path = tmp("v99.pcm");
        write_pcm_file(&path, &[0.0], 48_000, 1).unwrap();
        stamp_version(&path, 99);

        let err = read_pcm_file(&path).unwrap_err();
        assert!(err.contains("Unsupported pcm version 99"), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_short_header() {
        let path = tmp("short.pcm");
        std::fs::write(&path, [0u8; 10]).unwrap();
        let err = read_pcm_file(&path).unwrap_err();
        assert!(err.contains("Failed to read pcm header"), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_truncated_payload() {
        let path = tmp("truncated.pcm");
        write_pcm_file(&path, &[0.0, 1.0, 2.0], 48_000, 1).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 5]).unwrap();

        let err = read_pcm_file(&path).unwrap_err();
        assert!(err.contains("Failed to read pcm samples"), "{err}");
        std::fs::remove_file(&path).ok();
    }
}
