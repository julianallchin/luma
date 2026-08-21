//! Business logic for waveform operations.
//!
//! The database layer stores/retrieves serialized waveform payloads only.
//! All audio decoding and DSP happens here.
//!
//! # Two resolutions, one of them not fixed
//!
//! [`get_track_waveform`] returns the *stored* envelope: [`PREVIEW_WAVEFORM_SIZE`]
//! and [`FULL_WAVEFORM_SIZE`] buckets, that many however long the track is. A
//! five-minute track is 100 buckets a second, so past 100 pixels a second a
//! renderer is stretching one bucket over several pixels and the picture is a
//! staircase — detail that was thrown away at import, not detail the screen is
//! too small to show.
//!
//! [`get_track_waveform_window`] is the other half: exactly the visible range,
//! at exactly the caller's pixel density, measured from the audio each time.
//!
//! ## One picture, two sources
//!
//! Both return the *same three band envelopes in the same units*, so a renderer
//! has one drawing routine and crossing the threshold shows more detail rather
//! than a different waveform. What makes the units shared is [`BandGains`]:
//! band peaks are normalised against a whole-track percentile, which a visible
//! range cannot measure for itself, so the three divisors are stored beside the
//! envelopes and every later measurement is compressed against them.
//!
//! ## Why on-demand aggregation and not a pyramid
//!
//! The alternative was a precomputed multi-resolution pyramid — the stored
//! envelope at 30 000 buckets, then 60 000, 120 000, … — with window queries
//! served by slicing whichever level is finest without exceeding the request.
//! Two things sink it. A pyramid's deepest level *bounds* the precision, so
//! "a bucket per pixel at every zoom" holds only until the zoom passes that
//! level and interpolation resumes; making the bound unreachable means a
//! deepest level of one bucket per frame, which is the decoded audio plus every
//! coarser copy of it. And it is a schema migration plus a recompute of every
//! already-imported track, for a stored artifact that can go stale against the
//! file it was derived from — a third thing to invalidate.
//!
//! On-demand aggregation has neither cost because it reads what is already
//! there. The decoded PCM is `audio::cache`'s business: a process-wide LRU of
//! `Arc<DecodedAudio>` over an on-disk `.pcm` cache over the decoder. The track
//! a timeline is showing is *already* in that LRU — `host_load_track` put it
//! there to play it — so a window query is a strided scan over a slice of RAM
//! the process already holds, and the worst case (a track never loaded for
//! playback) is the decode playback would have done anyway.
//!
//! The seam does not leak which of the two it is: one command shape, buckets in
//! and buckets out, and a pyramid could be slid underneath it later without a
//! caller noticing.

use realfft::RealFftPlanner;
use sqlx::SqlitePool;
use std::path::Path;
use std::time::Instant;

use crate::audio::{decode_track_samples, filter_3band, FilteredBands};
use crate::database::local;
use crate::database::local::track_access::{Operate, Read, VisibleTrackAccess};
use crate::database::local::waveforms::StoredWaveform;
use crate::models::waveforms::{BandEnvelopes, BandGains, TrackWaveform, WaveformWindow};
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
    colors: Vec<u8>,
    preview_colors: Vec<u8>,
    sample_rate: u32,
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

    let computed = compute_waveform_payload(Path::new(&file_path), track_id, analysis).await?;
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
            colors_blob: &computed.colors,
            preview_colors_blob: &computed.preview_colors,
            bands_blob: &bands_blob,
            preview_bands_blob: &preview_bands_blob,
            band_gains: computed.gains,
            sample_rate: computed.sample_rate as i64,
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
            colors: Some(computed.colors),
            preview_colors: Some(computed.preview_colors),
            sample_rate: computed.sample_rate,
            duration_seconds: computed.duration_seconds,
        },
        computed.gains,
    ))
}

async fn compute_waveform_payload(
    track_path: &Path,
    track_id: &str,
    analysis: &AnalysisGuard,
) -> Result<ComputedWaveform, String> {
    let t_total = Instant::now();

    eprintln!("[waveform] computing waveforms for track {}", track_id);

    // Decode audio samples (returns stereo, convert to mono for waveform analysis)
    let t0 = Instant::now();
    let path = track_path.to_path_buf();
    let (samples, sample_rate) =
        tauri::async_runtime::spawn_blocking(move || -> Result<(Vec<f32>, u32), String> {
            let audio = decode_track_samples(&path, None)?;
            // Convert stereo to mono for waveform analysis
            Ok((audio.to_mono(), audio.sample_rate))
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
    let decoded_duration = samples.len() as f64 / sample_rate as f64;

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
    let filtered = filter_3band(&samples, sample_rate as f32);

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

    // Compute legacy colors for backwards compatibility
    let t0 = Instant::now();
    let colors = compute_spectral_colors(&samples, sample_rate, FULL_WAVEFORM_SIZE);
    let preview_colors = compute_spectral_colors(&samples, sample_rate, PREVIEW_WAVEFORM_SIZE);
    analysis.checkpoint()?;
    let colors_ms = t0.elapsed().as_millis();

    eprintln!(
        "[waveform] track {} computed in {}ms (decode={}ms waveform={}ms bands={}ms colors={}ms)",
        track_id,
        t_total.elapsed().as_millis(),
        decode_ms,
        waveform_ms,
        bands_ms,
        colors_ms,
    );

    Ok(ComputedWaveform {
        preview_samples,
        full_samples,
        bands,
        preview_bands,
        gains,
        colors,
        preview_colors,
        sample_rate,
        duration_seconds: decoded_duration,
    })
}

/// Force-recompute and atomically replace waveform data for a track.
pub async fn reprocess_track_waveform(
    pool: &SqlitePool,
    tasks: &AnalysisTaskGroup,
    track_id: &str,
) -> Result<TrackWaveform, String> {
    let epoch = tasks.current_epoch()?;
    let lease = tasks.lease(epoch)?;
    Ok(ensure_track_waveform(pool, track_id, &lease.guard())
        .await?
        .0)
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
    drop(access);

    match stored {
        Some(gains) => Ok(gains),
        None => Ok(get_track_waveform(pool, tasks, track_id).await?.1),
    }
}

/// The most buckets one window may be cut into.
///
/// A bucket is a pixel, and no display asks for sixteen thousand of them across
/// one timeline. The cap is what keeps a mistyped request from turning into a
/// gigabyte of `f32`s on the wire — not a limit any caller is expected to meet.
const MAX_WINDOW_BUCKETS: usize = 16_384;

/// Measure `start_seconds..end_seconds` of a track's audio into `buckets`
/// three-band buckets — see the module docs for why this is measured rather
/// than looked up.
///
/// The bands come back in the *stored* envelope's units (see [`BandGains`]), so
/// a renderer draws this exactly as it draws [`get_track_waveform`]'s bands and
/// the two are interchangeable at the same range.
///
/// Total by construction: the range is clamped to the decoded audio and the
/// bucket count to `1..=`[`MAX_WINDOW_BUCKETS`], so a caller cannot ask a
/// question this cannot answer. The returned `start_seconds`/`end_seconds` are
/// the clamped range, and the three series always have the same length.
///
/// # Errors
///
/// If the track is not visible to the caller, its row has no audio path, or the
/// file cannot be decoded.
pub async fn get_track_waveform_window(
    pool: &SqlitePool,
    tasks: &AnalysisTaskGroup,
    track_id: &str,
    start_seconds: f64,
    end_seconds: f64,
    buckets: u32,
) -> Result<WaveformWindow, String> {
    let gains = band_gains(pool, tasks, track_id).await?;
    let mut access = VisibleTrackAccess::<Read>::read(pool, track_id).await?;
    let (file_path, track_hash): (String, String) =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(access.connection())
            .await
            .map_err(|error| format!("Failed to load waveform source: {error}"))?;
    drop(access);

    // The same `(hash, rate)` key the audio host decodes under, so the track
    // being played is the track being measured and neither pays for the other's
    // copy.
    let rate = crate::host_audio::device_sample_rate();
    let audio = tauri::async_runtime::spawn_blocking(move || {
        crate::audio::load_or_decode_audio_shared(Path::new(&file_path), &track_hash, rate)
    })
    .await
    .map_err(|error| format!("Waveform window decode task failed: {error}"))??;

    let buckets = (buckets as usize).clamp(1, MAX_WINDOW_BUCKETS);
    let channels = usize::from(audio.channels).max(1);
    let sample_rate = audio.sample_rate.max(1);
    let rate = f64::from(sample_rate);
    let frames = audio.samples.len() / channels;
    let duration = frames as f64 / rate;

    let start = start_seconds.max(0.).min(duration);
    let end = end_seconds.clamp(start, duration);
    let range = (start * rate) as usize..((end * rate).ceil() as usize).min(frames);

    let peaks = tauri::async_runtime::spawn_blocking(move || {
        window_band_peaks(&audio.samples, channels, sample_rate, range, buckets)
    })
    .await
    .map_err(|error| format!("Waveform window measurement failed: {error}"))?;

    Ok(WaveformWindow {
        track_id: track_id.to_owned(),
        start_seconds: start,
        end_seconds: end,
        bands: gains.compress(&peaks),
    })
}

// -----------------------------------------------------------------------------
// DSP helpers
// -----------------------------------------------------------------------------

/// How much audio either side of a measured range is filtered and thrown away.
///
/// [`filter_3band`] is a chain of biquads, and a biquad started mid-signal rings
/// for a few dozen samples before it settles. Starting it at the edge of the
/// visible range would put that ringing on screen — a band bar taller or
/// shorter than the stored envelope's at the same instant, which is precisely
/// the seam this is all here to remove. A quarter second is orders of magnitude
/// more than the lowest crossover needs.
const FILTER_WARMUP_SECONDS: f64 = 0.25;

/// Measure `frames` of interleaved `channels`-channel PCM into `buckets` raw
/// three-band peaks — the same quantity [`bucketize_band_peaks`] takes off a
/// whole track, over a shorter range and usually a much finer grid.
///
/// The result is *not* in envelope units; [`BandGains::compress`] puts it
/// there, and only the track's stored gains can.
fn window_band_peaks(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    frames: std::ops::Range<usize>,
    buckets: usize,
) -> BandPeaks {
    let total = samples.len() / channels.max(1);
    if frames.end <= frames.start || total == 0 {
        return BandPeaks::silent(buckets);
    }

    // Filtered over a padded range, bucketized over the asked-for one: the pad
    // is what the filters settle in, so the buckets never see the transient.
    let warmup = (f64::from(sample_rate) * FILTER_WARMUP_SECONDS) as usize;
    let padded = frames.start.saturating_sub(warmup)..(frames.end + warmup).min(total);
    let mono: Vec<f32> = padded
        .clone()
        .map(|index| {
            let base = index * channels;
            samples[base..base + channels].iter().sum::<f32>() / channels as f32
        })
        .collect();

    let filtered = filter_3band(&mono, sample_rate as f32);
    let offset = frames.start - padded.start;
    bucketize_band_peaks(
        &filtered,
        offset..offset + (frames.end - frames.start),
        buckets,
    )
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

/// Compute RGB colors based on spectral content (Legacy - kept for backwards compatibility).
/// Uses rayon to parallelize FFT computation across chunks of buckets.
pub fn compute_spectral_colors(samples: &[f32], sample_rate: u32, num_buckets: usize) -> Vec<u8> {
    use rayon::prelude::*;

    if samples.is_empty() || num_buckets == 0 {
        return vec![0; num_buckets * 3];
    }

    let fft_size = 2048;
    let bin_freq = sample_rate as f32 / fft_size as f32;
    let low_bin_end = (300.0 / bin_freq).ceil() as usize;
    let mid_bin_end = (3000.0 / bin_freq).ceil() as usize;

    // Pre-compute Hann window (shared across threads)
    let window: Vec<f32> = (0..fft_size)
        .map(|i| {
            0.5 * (1.0 - ((2.0 * std::f32::consts::PI * i as f32) / (fft_size as f32 - 1.0)).cos())
        })
        .collect();

    let total = samples.len() as f64;
    let buckets = num_buckets as f64;

    // Process buckets in parallel — each thread gets its own FFT plan + buffers
    let chunk_size = (num_buckets / rayon::current_num_threads().max(1)).max(256);

    let chunks: Vec<Vec<u8>> = (0..num_buckets)
        .collect::<Vec<_>>()
        .par_chunks(chunk_size)
        .map(|bucket_indices| {
            let mut planner = RealFftPlanner::<f32>::new();
            let r2c = planner.plan_fft_forward(fft_size);
            let mut spectrum = r2c.make_output_vec();
            let mut input_window = r2c.make_input_vec();
            let num_bins = spectrum.len();

            let mut chunk_result = Vec::with_capacity(bucket_indices.len() * 3);

            for &bucket_idx in bucket_indices {
                let start = (bucket_idx as f64 * total / buckets) as usize;
                if start + fft_size > samples.len() {
                    chunk_result.extend_from_slice(&[0, 0, 0]);
                    continue;
                }

                let slice = &samples[start..start + fft_size];
                for i in 0..fft_size {
                    input_window[i] = slice[i] * window[i];
                }

                if r2c.process(&mut input_window, &mut spectrum).is_err() {
                    chunk_result.extend_from_slice(&[0, 0, 0]);
                    continue;
                }

                let mut low_energy = 0.0f32;
                for bin in &spectrum[..low_bin_end.min(num_bins)] {
                    low_energy += (bin.re * bin.re + bin.im * bin.im).sqrt();
                }
                let mut mid_energy = 0.0f32;
                for bin in &spectrum[low_bin_end.min(num_bins)..mid_bin_end.min(num_bins)] {
                    mid_energy += (bin.re * bin.re + bin.im * bin.im).sqrt();
                }
                let mut high_energy = 0.0f32;
                for bin in &spectrum[mid_bin_end.min(num_bins)..num_bins] {
                    high_energy += (bin.re * bin.re + bin.im * bin.im).sqrt();
                }

                let l = (low_energy / 100.0).min(1.0);
                let m = (mid_energy / 100.0).min(1.0);
                let h = (high_energy / 100.0).min(1.0);

                let r = 30.0 * l + 220.0 * m + 80.0 * h;
                let g = 30.0 * l + 120.0 * m + 150.0 * h;
                let b = 220.0 * l + 20.0 * m + 150.0 * h;

                chunk_result.push(r.round().min(255.0) as u8);
                chunk_result.push(g.round().min(255.0) as u8);
                chunk_result.push(b.round().min(255.0) as u8);
            }

            chunk_result
        })
        .collect();

    // Flatten chunks into final result
    let total_bytes: usize = chunks.iter().map(|c| c.len()).sum();
    let mut result = Vec::with_capacity(total_bytes);
    for chunk in chunks {
        result.extend_from_slice(&chunk);
    }
    result
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
