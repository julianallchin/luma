//! Business logic for waveform operations.
//!
//! The database layer stores/retrieves serialized waveform payloads only.
//! All audio decoding and DSP happens here.
//!
//! Stored envelopes serve library previews. Native timelines load filtered
//! sample-rate bands once and aggregate their GPU hierarchy at draw time.

use sqlx::SqlitePool;
use std::path::Path;
use std::time::Instant;

use crate::audio::{filter_3band, stereo_to_mono, FilteredBands, SAMPLE_RATE};
use crate::database::local;
use crate::database::local::track_access::{Operate, Read, VisibleTrackAccess};
use crate::database::local::waveforms::StoredWaveform;
use crate::models::waveforms::{BandEnvelopes, BandGains, TrackWaveform, WaveformSignal};
use crate::preprocessing::{AnalysisGuard, AnalysisTaskGroup};

/// Number of samples in preview waveform (low resolution for overview/minimap)
pub const PREVIEW_WAVEFORM_SIZE: usize = 1000;

/// Number of samples in full waveform (high resolution for zoomed view)
pub const FULL_WAVEFORM_SIZE: usize = 30000;

struct ComputedWaveform {
    preview_samples: Vec<f32>,
    full_samples: Vec<f32>,
    bands: BandEnvelopes,
    preview_bands: BandEnvelopes,
    /// The units `bands` and `preview_bands` are both in.
    gains: BandGains,
    duration_seconds: f64,
}

/// Compute and atomically publish waveform data for the exact visible track
/// snapshot admitted at the start of this analysis generation.
///
/// Returns the payload and the [`BandGains`] its envelopes are in — the two are
/// one measurement, and separating them is how a range ends up drawn in units
/// nothing else shares.
pub(crate) async fn ensure_track_waveform(
    pool: &SqlitePool,
    track_id: &str,
    analysis: &AnalysisGuard,
) -> Result<(TrackWaveform, BandGains), String> {
    analysis.checkpoint()?;
    let mut initial = VisibleTrackAccess::<Read>::read(pool, track_id).await?;
    let initial_principal = initial.principal().map(str::to_owned);
    let (file_path, track_hash): (String, String) =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(initial.connection())
            .await
            .map_err(|error| format!("Failed to load waveform source: {error}"))?;
    drop(initial);

    let computed = compute_waveform_payload(file_path, &track_hash, track_id, analysis).await?;
    analysis.checkpoint()?;

    // Publication and the identity transition use the same SQLite write lock.
    // Once this guard is admitted, the payload either commits for the exact
    // principal/source hash or the transition wins and nothing is written.
    let mut publication = VisibleTrackAccess::<Operate>::operate(pool, track_id).await?;
    if publication.principal() != initial_principal.as_deref() {
        return Err("Authenticated identity changed while processing waveform".into());
    }
    let (uid, current_hash): (Option<String>, String) =
        sqlx::query_as("SELECT uid, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(publication.connection())
            .await
            .map_err(|error| format!("Failed to verify waveform source: {error}"))?;
    if current_hash != track_hash {
        return Err("Track audio changed while processing waveform".into());
    }
    analysis.checkpoint()?;

    let preview_samples_blob = f32_slice_to_bytes(&computed.preview_samples);
    let full_samples_blob = f32_slice_to_bytes(&computed.full_samples);
    let bands_blob = band_envelopes_to_bytes(&computed.bands);
    let preview_bands_blob = band_envelopes_to_bytes(&computed.preview_bands);
    let db_started = Instant::now();
    local::waveforms::upsert_track_waveform_for_connection(
        publication.connection(),
        track_id,
        uid.as_deref(),
        &StoredWaveform {
            preview_samples_blob: &preview_samples_blob,
            full_samples_blob: &full_samples_blob,
            bands_blob: &bands_blob,
            preview_bands_blob: &preview_bands_blob,
            band_gains: computed.gains,
            decoded_duration: computed.duration_seconds,
        },
    )
    .await?;
    let db_ms = db_started.elapsed().as_millis();
    publication.commit().await?;

    eprintln!("[waveform] track {track_id} published in {db_ms}ms");
    Ok((
        TrackWaveform {
            track_id: track_id.to_owned(),
            uid,
            preview_samples: computed.preview_samples,
            full_samples: Some(computed.full_samples),
            bands: Some(computed.bands),
            preview_bands: Some(computed.preview_bands),
            duration_seconds: computed.duration_seconds,
        },
        computed.gains,
    ))
}

async fn compute_waveform_payload(
    file_path: String,
    track_hash: &str,
    track_id: &str,
    analysis: &AnalysisGuard,
) -> Result<ComputedWaveform, String> {
    let t_total = Instant::now();

    eprintln!("[waveform] computing waveforms for track {}", track_id);

    // Decode through the shared cache (stereo), then downmix for analysis.
    let t0 = Instant::now();
    let track_hash = track_hash.to_owned();
    let samples = tokio::task::spawn_blocking(move || -> Result<Vec<f32>, String> {
        let audio = crate::audio::load_or_decode_audio_shared(Path::new(&file_path), &track_hash)?;
        Ok(stereo_to_mono(&audio))
    })
    .await
    .map_err(|e| format!("Waveform decode task failed: {}", e))??;
    analysis.checkpoint()?;
    let decode_ms = t0.elapsed().as_millis();

    if samples.is_empty() {
        return Err("Cannot compute waveform for empty audio".into());
    }

    // Use the actual decoded sample count for duration — metadata can differ
    // due to encoder padding, VBR headers, etc.
    let decoded_duration = samples.len() as f64 / SAMPLE_RATE as f64;

    let t0 = Instant::now();

    // Compute both preview and full waveforms
    let preview_samples = compute_waveform(&samples, PREVIEW_WAVEFORM_SIZE);
    let full_samples = compute_waveform(&samples, FULL_WAVEFORM_SIZE);
    analysis.checkpoint()?;
    let waveform_ms = t0.elapsed().as_millis();

    // Filter once, reuse for both resolutions. The gains come from the full
    // resolution and are then applied to *both*, so the preview and the full
    // envelope are one unit system rather than two percentiles of two different
    // bucketizations.
    let t0 = Instant::now();
    let filtered = filter_3band(&samples, SAMPLE_RATE as f32);

    let full_peaks = bucketize_band_peaks(&filtered, 0..samples.len(), FULL_WAVEFORM_SIZE);
    let gains = BandGains::from_peaks(&full_peaks);
    let bands = gains.compress(&full_peaks);
    let preview_bands = gains.compress(&bucketize_band_peaks(
        &filtered,
        0..samples.len(),
        PREVIEW_WAVEFORM_SIZE,
    ));
    analysis.checkpoint()?;
    let bands_ms = t0.elapsed().as_millis();

    eprintln!(
        "[waveform] track {} computed in {}ms (decode={}ms waveform={}ms bands={}ms)",
        track_id,
        t_total.elapsed().as_millis(),
        decode_ms,
        waveform_ms,
        bands_ms,
    );

    Ok(ComputedWaveform {
        preview_samples,
        full_samples,
        bands,
        preview_bands,
        gains,
        duration_seconds: decoded_duration,
    })
}

/// Get waveform for a track, computing if missing.
///
/// A row written before the band gains were stored counts as missing: the
/// envelopes without their units are not a waveform anything else can be drawn
/// against, and recomputing is how such a row is backfilled — one measurement
/// of one decode produces both, so they cannot disagree.
pub async fn get_track_waveform(
    pool: &SqlitePool,
    tasks: &AnalysisTaskGroup,
    track_id: &str,
) -> Result<(TrackWaveform, BandGains), String> {
    let epoch = tasks.current_epoch()?;
    let lease = tasks.lease(epoch)?;
    let analysis = lease.guard();
    let mut access = VisibleTrackAccess::<Read>::read(pool, track_id).await?;
    let duration_seconds: Option<f64> =
        sqlx::query_scalar("SELECT duration_seconds FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(access.connection())
            .await
            .map_err(|error| format!("Failed to load track duration: {error}"))?;
    let duration_seconds = duration_seconds.unwrap_or(0.0);

    // Try cached waveform
    let row = local::waveforms::fetch_track_waveform_for_connection(access.connection(), track_id)
        .await?;
    let gains = local::waveforms::fetch_band_gains(access.connection(), track_id).await?;

    if let (Some(row), Some(gains)) = (row, gains) {
        return Ok((build_waveform(track_id, duration_seconds, row)?, gains));
    }
    drop(access);

    ensure_track_waveform(pool, track_id, &analysis).await
}

/// The units a track's band envelopes are in.
///
/// Total wherever [`get_track_waveform`] is: the fast path is the stored
/// scalars, and a row that predates them is materialised through the one path
/// that computes them rather than re-derived here — a second derivation would
/// be a second unit system, which is exactly what these exist to prevent.
async fn band_gains(
    pool: &SqlitePool,
    tasks: &AnalysisTaskGroup,
    track_id: &str,
) -> Result<BandGains, String> {
    let mut access = VisibleTrackAccess::<Read>::read(pool, track_id).await?;
    let stored = local::waveforms::fetch_band_gains(access.connection(), track_id).await?;
    access.finish().await?;

    match stored {
        Some(gains) => Ok(gains),
        None => Ok(get_track_waveform(pool, tasks, track_id).await?.1),
    }
}

/// Decode and filter once for the GPU waveform. Visibility and stored band
/// gains are resolved through the same guarded path as library previews.
pub async fn get_track_waveform_signal(
    pool: &SqlitePool,
    tasks: &AnalysisTaskGroup,
    track_id: &str,
) -> Result<WaveformSignal, String> {
    let gains = band_gains(pool, tasks, track_id).await?;
    let mut access = VisibleTrackAccess::<Read>::read(pool, track_id).await?;
    let (file_path, track_hash): (String, String) =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(access.connection())
            .await
            .map_err(|error| format!("Failed to load waveform source: {error}"))?;
    drop(access);

    // The same key the audio host decodes under, so the track being played is
    // the track being measured and neither pays for the other's copy.
    let audio = tokio::task::spawn_blocking(move || {
        crate::audio::load_or_decode_audio_shared(Path::new(&file_path), &track_hash)
    })
    .await
    .map_err(|error| format!("Waveform decode task failed: {error}"))??;

    tokio::task::spawn_blocking(move || {
        let mono = stereo_to_mono(&audio);
        let filtered = filter_3band(&mono, SAMPLE_RATE as f32);
        WaveformSignal {
            bands: [filtered.low, filtered.mid, filtered.high],
            gains,
            ceilings: BandGains::ceilings(),
        }
    })
    .await
    .map_err(|error| format!("Waveform filtering failed: {error}"))
}

/// The half-open sample range bucket `index` of `buckets` covers within
/// `range`.
///
/// Every bucket covers at least one sample: past one bucket per sample the
/// buckets repeat rather than coming back empty, which is what makes a request
/// finer than the audio a flat answer instead of an error.
fn bucket_range(
    range: &std::ops::Range<usize>,
    buckets: usize,
    index: usize,
) -> std::ops::Range<usize> {
    let span = range.end - range.start;
    let edge = |bucket: usize| range.start + (bucket * span) / buckets;
    let from = edge(index);
    from..edge(index + 1).max(from + 1).min(range.end)
}

fn build_waveform(
    _track_id: &str,
    metadata_duration: f64,
    mut waveform: TrackWaveform,
) -> Result<TrackWaveform, String> {
    // Use decoded_duration if available (already set from DB row), otherwise fall back to metadata
    if waveform.duration_seconds <= 0.0 {
        waveform.duration_seconds = metadata_duration;
    }
    Ok(waveform)
}

/// Compute waveform data from audio samples
/// Returns min/max pairs for each bucket (interleaved: [min0, max0, min1, max1, ...])
pub fn compute_waveform(samples: &[f32], num_buckets: usize) -> Vec<f32> {
    if samples.is_empty() || num_buckets == 0 {
        return vec![0.0; num_buckets * 2];
    }

    if samples.len() < num_buckets {
        let mut result = Vec::with_capacity(num_buckets * 2);
        for i in 0..num_buckets {
            let sample = samples.get(i).copied().unwrap_or(0.0);
            result.push(sample.min(0.0));
            result.push(sample.max(0.0));
        }
        return result;
    }

    let total = samples.len() as f64;
    let buckets = num_buckets as f64;

    let mut result = Vec::with_capacity(num_buckets * 2);
    for bucket_idx in 0..num_buckets {
        let start = (bucket_idx as f64 * total / buckets) as usize;
        let end = (((bucket_idx + 1) as f64 * total / buckets) as usize).min(samples.len());

        let bucket = &samples[start..end];
        let (min_val, max_val) = bucket
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), &sample| {
                (min.min(sample), max.max(sample))
            });

        result.push(if min_val.is_finite() { min_val } else { 0.0 });
        result.push(if max_val.is_finite() { max_val } else { 0.0 });
    }

    result
}

/// Compute 3-band envelopes (low, mid, high) for rekordbox-style waveform,
/// normalised against this call's own audio.
///
/// Standalone: it derives its own [`BandGains`], so two calls over different
/// audio are in different units. The pipeline instead derives gains once per
/// track and compresses every resolution against them — that is what makes the
/// stored envelope and a measured window the same picture.
pub fn compute_band_envelopes(
    samples: &[f32],
    sample_rate: u32,
    num_buckets: usize,
) -> BandEnvelopes {
    if samples.is_empty() || num_buckets == 0 {
        return BandPeaks::silent(num_buckets).into_envelopes();
    }

    let filtered = filter_3band(samples, sample_rate as f32);
    let peaks = bucketize_band_peaks(&filtered, 0..samples.len(), num_buckets);
    BandGains::from_peaks(&peaks).compress(&peaks)
}

/// Per-bucket band peaks in the audio's own units, before any normalisation.
///
/// Distinct from [`BandEnvelopes`] on purpose: these two are the same three
/// vectors in different units, and the type is what stops one being drawn where
/// the other belongs.
pub struct BandPeaks {
    low: Vec<f32>,
    mid: Vec<f32>,
    high: Vec<f32>,
}

impl BandPeaks {
    fn silent(buckets: usize) -> Self {
        BandPeaks {
            low: vec![0.0; buckets],
            mid: vec![0.0; buckets],
            high: vec![0.0; buckets],
        }
    }

    /// Reinterpret as envelopes without compressing — only valid for all-zero
    /// peaks, where every gain maps to zero anyway.
    fn into_envelopes(self) -> BandEnvelopes {
        BandEnvelopes {
            low: self.low,
            mid: self.mid,
            high: self.high,
        }
    }
}

/// Where a band sits in the stack, and how tall it is allowed to draw.
///
/// The three scales are the rekordbox look: the quieter bands paint over the
/// louder ones and read as an outline around them, which only works while the
/// ceiling of each is fixed and known on both sides of the seam.
#[derive(Clone, Copy)]
enum Band {
    Low,
    Mid,
    High,
}

impl Band {
    fn scale(self) -> f32 {
        match self {
            Band::Low => 0.95,
            Band::Mid => 0.8,
            Band::High => 0.6,
        }
    }
}

impl BandGains {
    /// The display ceiling of each band, also supplied to GPU renderers.
    pub fn ceilings() -> [f32; 3] {
        [Band::Low.scale(), Band::Mid.scale(), Band::High.scale()]
    }

    /// The divisor a band normalises against: the 99th percentile of its peaks,
    /// so a handful of transients cannot flatten the rest of the track.
    ///
    /// Floored well above zero, which is what makes [`BandGains::compress`]
    /// total — silence normalises to silence rather than to a division by it.
    fn of(peaks: &[f32]) -> f32 {
        let mut sorted = peaks.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let index = (sorted.len() as f32 * 0.99) as usize;
        sorted.get(index).copied().unwrap_or(1.0).max(0.0001)
    }

    /// Derive the units of a track from its full-resolution band peaks.
    pub fn from_peaks(peaks: &BandPeaks) -> Self {
        BandGains {
            low: Self::of(&peaks.low),
            mid: Self::of(&peaks.mid),
            high: Self::of(&peaks.high),
        }
    }

    fn gain(&self, band: Band) -> f32 {
        match band {
            Band::Low => self.low,
            Band::Mid => self.mid,
            Band::High => self.high,
        }
    }

    /// Raw band peaks to stored envelope units: normalise, log-compress to lift
    /// the quiet detail, then scale to the band's ceiling.
    ///
    /// The only place that mapping exists, so anything compressed with the same
    /// gains is directly comparable however it was bucketized.
    pub fn compress(&self, peaks: &BandPeaks) -> BandEnvelopes {
        let band = |values: &[f32], band: Band| {
            let (gain, scale) = (self.gain(band), band.scale());
            values
                .iter()
                .map(|peak| (1.0 + 9.0 * (peak / gain).clamp(0.0, 1.0)).log10() * scale)
                .collect()
        };
        BandEnvelopes {
            low: band(&peaks.low, Band::Low),
            mid: band(&peaks.mid, Band::Mid),
            high: band(&peaks.high, Band::High),
        }
    }
}

/// Fold `range` of pre-filtered 3-band audio into `num_buckets` per-band peaks.
pub fn bucketize_band_peaks(
    filtered: &FilteredBands,
    range: std::ops::Range<usize>,
    num_buckets: usize,
) -> BandPeaks {
    if num_buckets == 0 || range.end <= range.start {
        return BandPeaks::silent(num_buckets);
    }

    let peak = |values: &[f32], bucket: std::ops::Range<usize>| {
        values[bucket]
            .iter()
            .fold(0.0f32, |max, &s| max.max(s.abs()))
    };
    let mut peaks = BandPeaks {
        low: Vec::with_capacity(num_buckets),
        mid: Vec::with_capacity(num_buckets),
        high: Vec::with_capacity(num_buckets),
    };
    for index in 0..num_buckets {
        let bucket = bucket_range(&range, num_buckets, index);
        peaks.low.push(peak(&filtered.low, bucket.clone()));
        peaks.mid.push(peak(&filtered.mid, bucket.clone()));
        peaks.high.push(peak(&filtered.high, bucket));
    }
    peaks
}

// -----------------------------------------------------------------------------
// Binary blob serialization helpers
// -----------------------------------------------------------------------------

/// Serialize a slice of f32 values to raw little-endian bytes
pub fn f32_slice_to_bytes(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for &val in data {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize raw little-endian bytes back to Vec<f32>
pub fn bytes_to_f32_vec(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Serialize BandEnvelopes to a single blob: [low..., mid..., high...]
/// Each band has the same length, so we can split evenly on decode.
fn band_envelopes_to_bytes(bands: &BandEnvelopes) -> Vec<u8> {
    let total = (bands.low.len() + bands.mid.len() + bands.high.len()) * 4;
    let mut bytes = Vec::with_capacity(total);
    for &val in bands
        .low
        .iter()
        .chain(bands.mid.iter())
        .chain(bands.high.iter())
    {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize a blob back to BandEnvelopes (3 equal-length bands)
pub fn bytes_to_band_envelopes(data: &[u8]) -> Option<BandEnvelopes> {
    let floats = bytes_to_f32_vec(data);
    if !floats.len().is_multiple_of(3) {
        return None;
    }
    let band_len = floats.len() / 3;
    Some(BandEnvelopes {
        low: floats[..band_len].to_vec(),
        mid: floats[band_len..band_len * 2].to_vec(),
        high: floats[band_len * 2..].to_vec(),
    })
}

#[cfg(test)]
#[path = "waveforms_bench.rs"]
mod bench;
