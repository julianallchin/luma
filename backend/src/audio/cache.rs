use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::decoder::{decode_track_samples, SAMPLE_RATE};

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

/// A `.pcm` file's header. `len` is the interleaved sample count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmHeader {
    pub version: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub len: u64,
}

/// Parse and validate the header. Accepts format versions 1 and 2, a nonzero
/// rate and channel count, and a sample count that is whole frames.
fn parse_pcm_header(reader: &mut impl Read, path: &Path) -> Result<PcmHeader, String> {
    let mut buf = [0u8; PCM_HEADER_LEN];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("Failed to read pcm header {}: {e}", path.display()))?;
    let header = PcmHeader {
        version: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        sample_rate: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        channels: u16::from_le_bytes(buf[8..10].try_into().unwrap()),
        len: u64::from_le_bytes(buf[10..18].try_into().unwrap()),
    };
    let problem = if header.version == 0 || header.version > CACHE_VERSION {
        format!("unsupported pcm version {}", header.version)
    } else if header.sample_rate == 0 || header.channels == 0 {
        "pcm header declares a zero sample rate or channel count".into()
    } else if !header.len.is_multiple_of(u64::from(header.channels)) {
        "pcm sample count is not whole frames".into()
    } else {
        return Ok(header);
    };
    Err(format!("{problem} in {}", path.display()))
}

/// Read a `.pcm` file's header, checking the file holds exactly the samples it
/// declares. For callers that hand the payload on without loading it.
pub fn read_pcm_header(path: &Path) -> Result<PcmHeader, String> {
    let mut file =
        File::open(path).map_err(|e| format!("Failed to open pcm {}: {e}", path.display()))?;
    let header = parse_pcm_header(&mut file, path)?;
    let actual = file.metadata().map_err(|e| e.to_string())?.len();
    let expected = PCM_HEADER_LEN as u64 + header.len * 4;
    if actual != expected {
        return Err(format!(
            "pcm {} is {actual} bytes, its header declares {expected}",
            path.display()
        ));
    }
    Ok(header)
}

/// Read a `.pcm` cache file. The one reader for this format — the eval engine,
/// the golden harness, the agent and the decode cache all go through here.
///
/// A header [`parse_pcm_header`] rejects or a payload shorter than the declared
/// length is an error (callers treat a failed read as "cache miss, re-derive").
pub fn read_pcm_file(path: &Path) -> Result<PcmData, String> {
    let mut reader = BufReader::new(
        File::open(path).map_err(|e| format!("Failed to open pcm {}: {e}", path.display()))?,
    );
    let PcmHeader {
        version,
        sample_rate,
        channels,
        len,
    } = parse_pcm_header(&mut reader, path)?;

    let mut bytes = vec![0u8; len as usize * 4];
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

/// Process-wide RAM cache of fully-decoded tracks, keyed by hash, so a track's
/// expensive decode/disk-read happens **once** and every later consumer
/// (playback *and* analysis) reuses it. Without
/// this, opening a track for playback and then compositing it decodes the same
/// audio twice. Bounded LRU; entries are `Arc` so a hit is an O(1) clone.
#[derive(Default)]
struct SharedDecodeState {
    cache: Vec<(String, Arc<Vec<f32>>)>,
    in_flight: HashMap<String, Arc<DecodeFlight>>,
}

#[derive(Default)]
struct DecodeFlight {
    result: Mutex<Option<Result<Arc<Vec<f32>>, String>>>,
    ready: Condvar,
}

impl DecodeFlight {
    fn wait(&self) -> Result<Arc<Vec<f32>>, String> {
        let mut result = lock(&self.result);
        while result.is_none() {
            result = self
                .ready
                .wait(result)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        result.as_ref().expect("decode flight was signaled").clone()
    }

    fn publish(&self, result: Result<Arc<Vec<f32>>, String>) {
        *lock(&self.result) = Some(result);
        self.ready.notify_all();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

static SHARED_DECODES: Lazy<Mutex<SharedDecodeState>> =
    Lazy::new(|| Mutex::new(SharedDecodeState::default()));
const DECODE_RAM_CACHE_MAX: usize = 3;

/// [`load_or_decode_audio`] with a shared RAM cache (returns an `Arc`). Use this
/// from both the playback and the analysis paths so the decode is not duplicated.
pub fn load_or_decode_audio_shared(
    track_path: &Path,
    track_hash: &str,
) -> Result<Arc<Vec<f32>>, String> {
    load_or_decode_audio_shared_by_key(track_hash.to_owned(), || {
        load_or_decode_audio(track_path, track_hash)
    })
}

fn load_or_decode_audio_shared_by_key(
    key: String,
    decode: impl FnOnce() -> Result<Vec<f32>, String>,
) -> Result<Arc<Vec<f32>>, String> {
    let (flight, leader) = {
        let mut shared = lock(&SHARED_DECODES);
        if let Some(index) = shared.cache.iter().position(|(cached, _)| cached == &key) {
            let hit = shared.cache.remove(index);
            let decoded = hit.1.clone();
            shared.cache.push(hit);
            return Ok(decoded);
        }
        if let Some(flight) = shared.in_flight.get(&key) {
            (flight.clone(), false)
        } else {
            let flight = Arc::new(DecodeFlight::default());
            shared.in_flight.insert(key.clone(), flight.clone());
            (flight, true)
        }
    };

    if !leader {
        return flight.wait();
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode))
        .map_err(|panic| {
            let message = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown panic");
            format!("Audio decode panicked: {message}")
        })
        .and_then(|decoded| decoded)
        .map(Arc::new);
    flight.publish(result.clone());

    let mut shared = lock(&SHARED_DECODES);
    shared.in_flight.remove(&key);
    if let Ok(decoded) = &result {
        if shared.cache.len() >= DECODE_RAM_CACHE_MAX {
            shared.cache.remove(0);
        }
        shared.cache.push((key, decoded.clone()));
    }
    result
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

/// Load audio from cache or decode from file, as stereo interleaved samples at
/// [`SAMPLE_RATE`].
///
/// The cache is `<dir of the audio file>/cache/<hash>.pcm`. Only a current-format
/// stereo file at [`SAMPLE_RATE`] is a hit; anything else — an older version
/// (v1 at this path is the old mono format), another rate, a corrupt file — is
/// re-decoded and overwritten.
fn load_or_decode_audio(track_path: &Path, track_hash: &str) -> Result<Vec<f32>, String> {
    let Ok(cache_dir) = cache_dir_for_track(track_path) else {
        return decode_track_samples(track_path);
    };
    let cache_file = cache_dir.join(format!("{track_hash}.pcm"));
    if let Ok(cached) = read_pcm_file(&cache_file) {
        if (cached.version, cached.sample_rate, cached.channels) == (CACHE_VERSION, SAMPLE_RATE, 2)
        {
            return Ok(cached.samples);
        }
    }

    let decoded = decode_track_samples(track_path)?;
    if let Err(err) = write_pcm_file(&cache_file, &decoded, SAMPLE_RATE, 2) {
        eprintln!(
            "[audio-cache] failed to write cache {}: {}",
            cache_file.display(),
            err
        );
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use std::time::Duration;

    static TEST_KEY: AtomicUsize = AtomicUsize::new(0);

    fn unique_key(label: &str) -> String {
        format!(
            "test-{label}-{}-{}",
            std::process::id(),
            TEST_KEY.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn decoded(sample: f32) -> Vec<f32> {
        vec![sample, sample]
    }

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
        assert!(err.contains("unsupported pcm version 99"), "{err}");
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
        let err = read_pcm_header(&path).unwrap_err();
        assert!(err.contains("its header declares 30"), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn header_rejects_degenerate_fields_and_trailing_bytes() {
        let path = tmp("header.pcm");
        write_pcm_file(&path, &[0.0; 4], 48_000, 2).unwrap();
        let header = read_pcm_header(&path).unwrap();
        assert_eq!(
            (header.sample_rate, header.channels, header.len),
            (48_000, 2, 4)
        );

        for (rate, channels, samples, problem) in [
            (0, 2, 4, "zero sample rate"),
            (48_000, 0, 4, "zero sample rate or channel count"),
            (48_000, 3, 4, "not whole frames"),
        ] {
            write_pcm_file(&path, &vec![0.0; samples], rate, channels).unwrap();
            let err = read_pcm_header(&path).unwrap_err();
            assert!(err.contains(problem), "{err}");
        }

        write_pcm_file(&path, &[0.0; 2], 48_000, 1).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(0);
        std::fs::write(&path, bytes).unwrap();
        assert!(read_pcm_header(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    /// One second of 8 kHz mono 16-bit silence.
    fn write_wav(path: &Path) {
        let data_len = 8_000_u32 * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&8_000_u32.to_le_bytes());
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.resize((44 + data_len) as usize, 0);
        std::fs::write(path, bytes).unwrap();
    }

    /// A cache left at another rate (playback used to write the device's) is a
    /// miss: the track is re-decoded and the file rewritten at [`SAMPLE_RATE`].
    #[test]
    fn an_off_rate_cache_is_redecoded_and_rewritten_at_the_one_rate() {
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("track.wav");
        write_wav(&track);
        let cache = dir.path().join("cache").join("hash.pcm");
        write_pcm_file(&cache, &[0.5; 8], 44_100, 2).unwrap();

        let audio = load_or_decode_audio(&track, "hash").unwrap();
        let frames = audio.len() / 2;
        assert!(frames.abs_diff(SAMPLE_RATE as usize) < 100, "{frames}");

        let rewritten = read_pcm_file(&cache).unwrap();
        assert_eq!(
            (rewritten.version, rewritten.sample_rate, rewritten.channels),
            (CACHE_VERSION, SAMPLE_RATE, 2)
        );
        assert_eq!(rewritten.samples, audio);
    }

    #[test]
    fn concurrent_same_key_misses_share_one_decode() {
        let key = unique_key("same-key");
        let callers = 8;
        let start = Arc::new(Barrier::new(callers));
        let decodes = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..callers)
            .map(|_| {
                let key = key.clone();
                let start = start.clone();
                let decodes = decodes.clone();
                std::thread::spawn(move || {
                    start.wait();
                    load_or_decode_audio_shared_by_key(key, || {
                        decodes.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(decoded(0.25))
                    })
                    .unwrap()
                })
            })
            .collect();

        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(decodes.load(Ordering::SeqCst), 1);
        assert!(results
            .iter()
            .skip(1)
            .all(|result| Arc::ptr_eq(&results[0], result)));
    }

    #[test]
    fn different_keys_decode_concurrently() {
        let keys = [unique_key("key-a"), unique_key("key-b")];
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(2));

        let threads: Vec<_> = keys
            .into_iter()
            .map(|key| {
                let active = active.clone();
                let peak = peak.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    load_or_decode_audio_shared_by_key(key, || {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(decoded(0.5))
                    })
                    .unwrap()
                })
            })
            .collect();

        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_panicking_leader_releases_waiters_and_allows_retry() {
        let key = unique_key("panic");
        let (leader_started_tx, leader_started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let leader_key = key.clone();
        let leader = std::thread::spawn(move || {
            load_or_decode_audio_shared_by_key(leader_key, || {
                leader_started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                panic!("broken decoder")
            })
        });
        leader_started_rx.recv().unwrap();

        let waiter_key = key.clone();
        let waiter = std::thread::spawn(move || {
            load_or_decode_audio_shared_by_key(waiter_key, || {
                panic!("waiter must not become the leader")
            })
        });
        std::thread::sleep(Duration::from_millis(10));
        release_tx.send(()).unwrap();

        let leader_error = match leader.join().unwrap() {
            Ok(_) => panic!("panicking leader unexpectedly decoded audio"),
            Err(error) => error,
        };
        let waiter_error = match waiter.join().unwrap() {
            Ok(_) => panic!("waiter unexpectedly decoded audio"),
            Err(error) => error,
        };
        assert!(leader_error.contains("broken decoder"), "{leader_error}");
        assert_eq!(waiter_error, leader_error);

        let retry = load_or_decode_audio_shared_by_key(key, || Ok(decoded(0.75))).unwrap();
        assert_eq!(*retry, vec![0.75, 0.75]);
    }
}
