use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use rayon::prelude::*;
use realfft::{num_complex::Complex32, RealFftPlanner};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use std::f32::consts::PI;
use std::fs::File;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    probe::Hint,
};
use symphonia::default::{get_codecs, get_probe};
use tauri::{AppHandle, Manager, State};
use ts_rs::TS;
use uuid::Uuid;

use crate::beat_worker::{self, BeatAnalysis};
use crate::database::Db;
use crate::schema::BeatGrid;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackSummary {
    #[ts(type = "number")]
    pub id: i64,
    pub track_hash: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[ts(type = "number | null")]
    pub track_number: Option<i64>,
    #[ts(type = "number | null")]
    pub disc_number: Option<i64>,
    #[ts(type = "number | null")]
    pub duration_seconds: Option<f64>,
    pub file_path: String,
    pub album_art_path: Option<String>,
    pub album_art_mime: Option<String>,
    pub album_art_data: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl TrackSummary {
    fn from_row(row: TrackRow, album_art_data: Option<String>) -> Self {
        TrackSummary {
            id: row.id,
            track_hash: row.track_hash,
            title: row.title,
            artist: row.artist,
            album: row.album,
            track_number: row.track_number,
            disc_number: row.disc_number,
            duration_seconds: row.duration_seconds,
            file_path: row.file_path,
            album_art_path: row.album_art_path,
            album_art_mime: row.album_art_mime,
            album_art_data,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct MelSpec {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub beat_grid: Option<BeatGrid>,
}

#[derive(FromRow)]
struct TrackRow {
    id: i64,
    track_hash: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    duration_seconds: Option<f64>,
    file_path: String,
    album_art_path: Option<String>,
    album_art_mime: Option<String>,
    created_at: String,
    updated_at: String,
}

fn album_art_for_row(row: &TrackRow) -> Option<String> {
    match (&row.album_art_path, &row.album_art_mime) {
        (Some(path), Some(mime)) => read_album_art(path, mime),
        _ => None,
    }
}

pub fn ensure_storage(app: &AppHandle) -> Result<(), String> {
    let (tracks_dir, art_dir) = storage_dirs(app)?;
    std::fs::create_dir_all(&tracks_dir)
        .map_err(|e| format!("Failed to create tracks directory: {}", e))?;
    std::fs::create_dir_all(&art_dir)
        .map_err(|e| format!("Failed to create album art directory: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<TrackSummary>, String> {
    let rows = sqlx::query_as::<_, TrackRow>(
        "SELECT id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks ORDER BY created_at DESC",
    )
    .fetch_all(&db.0)
    .await
    .map_err(|e| format!("Failed to list tracks: {}", e))?;

    let mut tracks = Vec::with_capacity(rows.len());
    for row in rows {
        let album_art_data = album_art_for_row(&row);
        tracks.push(TrackSummary::from_row(row, album_art_data));
    }

    Ok(tracks)
}

#[tauri::command]
pub async fn import_track(
    db: State<'_, Db>,
    app_handle: AppHandle,
    file_path: String,
) -> Result<TrackSummary, String> {
    ensure_storage(&app_handle)?;
    let (tracks_dir, art_dir) = storage_dirs(&app_handle)?;

    let source_path = Path::new(&file_path);
    if !source_path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }

    let track_hash = compute_track_hash(source_path)?;
    if let Some(existing) = sqlx::query_as::<_, TrackRow>(
        "SELECT id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks WHERE track_hash = ?",
    )
    .bind(&track_hash)
    .fetch_optional(&db.0)
    .await
    .map_err(|e| format!("Failed to inspect existing tracks: {}", e))?
    {
        ensure_track_beats_for_path(
            &db.0,
            existing.id,
            Path::new(&existing.file_path),
            &app_handle,
        )
        .await?;
        let album_art_data = album_art_for_row(&existing);
        return Ok(TrackSummary::from_row(existing, album_art_data));
    }

    let extension = source_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("bin");
    let dest_file_name = format!("{}.{}", Uuid::new_v4(), extension);
    let dest_path = tracks_dir.join(&dest_file_name);
    std::fs::copy(&source_path, &dest_path)
        .map_err(|e| format!("Failed to copy track file: {}", e))?;

    let tagged_file = Probe::open(&dest_path)
        .map_err(|e| format!("Failed to probe track file: {}", e))?
        .read()
        .map_err(|e| format!("Failed to read track metadata: {}", e))?;

    let primary_tag = tagged_file.primary_tag();
    let title = primary_tag.and_then(|tag| tag.title().map(|s| s.to_string()));
    let artist = primary_tag.and_then(|tag| tag.artist().map(|s| s.to_string()));
    let album = primary_tag.and_then(|tag| tag.album().map(|s| s.to_string()));
    let track_number = primary_tag.and_then(|tag| tag.track()).map(|n| n as i64);
    let disc_number = primary_tag.and_then(|tag| tag.disk()).map(|n| n as i64);

    let duration_seconds = Some(tagged_file.properties().duration().as_secs_f64());
    let (album_art_path, album_art_mime, album_art_data) = match primary_tag.and_then(|tag| {
        tag.pictures()
            .iter()
            .find(|pic| {
                matches!(
                    pic.pic_type(),
                    PictureType::CoverFront | PictureType::CoverBack | PictureType::Other
                )
            })
            .cloned()
    }) {
        Some(picture) => {
            let mime = picture
                .mime_type()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "application/octet-stream".into());
            let art_extension = match mime.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/bmp" => "bmp",
                _ => "bin",
            };
            let art_file_name = format!("{}.{}", Uuid::new_v4(), art_extension);
            let art_path = art_dir.join(&art_file_name);
            std::fs::write(&art_path, picture.data())
                .map_err(|e| format!("Failed to write album art: {}", e))?;
            let data = format!("data:{};base64,{}", mime, STANDARD.encode(picture.data()));
            (
                Some(art_path.to_string_lossy().into_owned()),
                Some(mime),
                Some(data),
            )
        }
        None => (None, None, None),
    };

    let query = sqlx::query(
        "INSERT INTO tracks (track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&track_hash)
    .bind(&title)
    .bind(&artist)
    .bind(&album)
    .bind(track_number)
    .bind(disc_number)
    .bind(duration_seconds)
    .bind(dest_path.to_string_lossy().into_owned())
    .bind(album_art_path.clone())
    .bind(album_art_mime.clone())
    .execute(&db.0)
    .await
    .map_err(|e| format!("Failed to insert track: {}", e))?;

    let id = query.last_insert_rowid();

    let row = sqlx::query_as::<_, TrackRow>(
        "SELECT id, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, album_art_path, album_art_mime, created_at, updated_at FROM tracks WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch imported track: {}", e))?;

    ensure_track_beats_for_path(&db.0, id, &dest_path, &app_handle).await?;

    let album_data = album_art_data.or_else(|| album_art_for_row(&row));

    Ok(TrackSummary::from_row(row, album_data))
}

#[tauri::command]
pub async fn get_melspec(db: State<'_, Db>, track_id: i64) -> Result<MelSpec, String> {
    let (file_path, track_hash): (String, String) =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_one(&db.0)
            .await
            .map_err(|e| format!("Failed to load track path: {}", e))?;

    let path = PathBuf::from(&file_path);
    let width = MEL_SPEC_WIDTH;
    let height = MEL_SPEC_HEIGHT;

    let data = tauri::async_runtime::spawn_blocking(move || {
        let (samples, sample_rate) = load_or_decode_audio(&path, &track_hash, TARGET_SAMPLE_RATE)?;
        Ok::<_, String>(generate_melspec(&samples, sample_rate, width, height))
    })
    .await
    .map_err(|e| format!("Mel spec worker failed: {}", e))??;

    Ok(MelSpec {
        width,
        height,
        data,
        beat_grid: None,
    })
}

#[tauri::command]
pub async fn wipe_tracks(db: State<'_, Db>, app_handle: AppHandle) -> Result<(), String> {
    sqlx::query("DELETE FROM track_beats")
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to clear track beats: {}", e))?;

    sqlx::query("DELETE FROM tracks")
        .execute(&db.0)
        .await
        .map_err(|e| format!("Failed to clear tracks: {}", e))?;

    let (tracks_dir, _) = storage_dirs(&app_handle)?;
    if tracks_dir.exists() {
        std::fs::remove_dir_all(&tracks_dir).map_err(|e| {
            format!(
                "Failed to remove tracks directory {}: {}",
                tracks_dir.display(),
                e
            )
        })?;
    }
    ensure_storage(&app_handle)?;
    Ok(())
}

async fn ensure_track_beats_for_path(
    pool: &SqlitePool,
    track_id: i64,
    track_path: &Path,
    app_handle: &AppHandle,
) -> Result<(), String> {
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM track_beats WHERE track_id = ? LIMIT 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("Failed to inspect beat cache: {}", e))?;

    if existing.is_some() {
        return Ok(());
    }

    let handle = app_handle.clone();
    let path = track_path.to_path_buf();
    let beat_data =
        tauri::async_runtime::spawn_blocking(move || beat_worker::compute_beats(&handle, &path))
            .await
            .map_err(|e| format!("Beat worker task failed: {}", e))??;

    persist_track_beats(pool, track_id, &beat_data).await
}

async fn persist_track_beats(
    pool: &SqlitePool,
    track_id: i64,
    beat_data: &BeatAnalysis,
) -> Result<(), String> {
    let beats_json = serde_json::to_string(&beat_data.beats)
        .map_err(|e| format!("Failed to serialize beats: {}", e))?;
    let downbeats_json = serde_json::to_string(&beat_data.downbeats)
        .map_err(|e| format!("Failed to serialize downbeats: {}", e))?;

    sqlx::query(
        "INSERT INTO track_beats (track_id, beats_json, downbeats_json)
         VALUES (?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            beats_json = excluded.beats_json,
            downbeats_json = excluded.downbeats_json,
            updated_at = datetime('now')",
    )
    .bind(track_id)
    .bind(beats_json)
    .bind(downbeats_json)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to persist beat data: {}", e))?;

    Ok(())
}

fn storage_dirs(app: &AppHandle) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let app_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to locate app config dir: {}", e))?;
    let tracks_dir = app_dir.join("tracks");
    let art_dir = tracks_dir.join("art");
    Ok((tracks_dir, art_dir))
}

fn compute_track_hash(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|e| format!("Failed to open track for hashing: {}", e))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|e| format!("Failed to hash track: {}", e))?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_album_art(path: &str, mime: &str) -> Option<String> {
    std::fs::read(path).ok().and_then(|data| {
        if mime.is_empty() {
            None
        } else {
            Some(format!("data:{};base64,{}", mime, STANDARD.encode(data)))
        }
    })
}

pub const MEL_SPEC_WIDTH: usize = 512;
pub const MEL_SPEC_HEIGHT: usize = 128;
const FFT_SIZE: usize = 2048;
const HOP_SIZE: usize = 512;

pub fn decode_track_samples(
    path: &Path,
    max_samples: Option<usize>,
) -> Result<(Vec<f32>, u32), String> {
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

pub fn generate_melspec(
    samples: &[f32],
    sample_rate: u32,
    width: usize,
    height: usize,
) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; width * height];
    }

    let filters = build_mel_filters(height, FFT_SIZE, sample_rate);
    let window = hann_window(FFT_SIZE);

    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let frame_count = if samples.len() <= FFT_SIZE {
        1
    } else {
        (samples.len() - FFT_SIZE) / HOP_SIZE + 1
    };

    let mut mel_frames = vec![vec![0.0f32; height]; frame_count];

    mel_frames.par_iter_mut().enumerate().for_each_init(
        || StftWorkspace {
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
        },
        |workspace, (frame_index, mel_row)| {
            let start = frame_index * HOP_SIZE;
            for i in 0..FFT_SIZE {
                let sample = samples.get(start + i).copied().unwrap_or(0.0);
                workspace.input[i] = sample * window[i];
            }

            if fft
                .process(&mut workspace.input, &mut workspace.spectrum)
                .is_err()
            {
                return;
            }

            for (mel_idx, filter) in filters.iter().enumerate() {
                let mut energy = 0.0f32;
                for (bin, weight) in filter.iter().enumerate() {
                    if *weight == 0.0 {
                        continue;
                    }
                    energy += weight * workspace.spectrum[bin].norm();
                }
                mel_row[mel_idx] = energy;
            }
        },
    );

    aggregate_mel_frames(&mel_frames, width, height)
}

struct StftWorkspace {
    input: Vec<f32>,
    spectrum: Vec<Complex32>,
}

fn aggregate_mel_frames(frames: &[Vec<f32>], width: usize, height: usize) -> Vec<f32> {
    if frames.is_empty() {
        return vec![0.0; width * height];
    }

    let frame_count = frames.len();
    let mut aggregated = vec![0.0f32; width * height];

    for col in 0..width {
        let mut start = (col * frame_count) / width;
        let mut end = ((col + 1) * frame_count) / width;
        if end <= start {
            end = start + 1;
        }
        if start >= frame_count {
            start = frame_count.saturating_sub(1);
            end = frame_count;
        } else if end > frame_count {
            end = frame_count;
        }
        let count = (end - start).max(1);

        for bin in 0..height {
            let mut sum = 0.0f32;
            for frame in &frames[start..end] {
                sum += frame[bin];
            }
            aggregated[col * height + bin] = sum / count as f32;
        }
    }

    normalize_spectrogram(&mut aggregated);
    aggregated
}

fn normalize_spectrogram(data: &mut [f32]) {
    if data.is_empty() {
        return;
    }

    let eps = 1e-8;
    let mut log_values = Vec::with_capacity(data.len());
    for &value in data.iter() {
        log_values.push((value + eps).log10());
    }

    let (min_log, max_log) = log_values
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), &val| {
            (min.min(val), max.max(val))
        });
    let range = (max_log - min_log).max(1e-3);

    for (dst, &log_value) in data.iter_mut().zip(log_values.iter()) {
        *dst = ((log_value - min_log) / range).clamp(0.0, 1.0);
    }
}

fn build_mel_filters(mel_bins: usize, fft_size: usize, sample_rate: u32) -> Vec<Vec<f32>> {
    let freq_bins = fft_size / 2 + 1;
    let mel_min = hz_to_mel(0.0);
    let mel_max = hz_to_mel(sample_rate as f32 / 2.0);
    let mut mel_points = Vec::with_capacity(mel_bins + 2);
    for i in 0..(mel_bins + 2) {
        mel_points.push(mel_min + (mel_max - mel_min) * i as f32 / (mel_bins + 1) as f32);
    }

    let hz_points: Vec<f32> = mel_points.iter().map(|&mel| mel_to_hz(mel)).collect();
    let bin_points: Vec<usize> = hz_points
        .iter()
        .map(|&hz| {
            let bin = ((hz / sample_rate as f32) * fft_size as f32).floor() as usize;
            bin.min(freq_bins - 1)
        })
        .collect();

    let mut filters = vec![vec![0.0f32; freq_bins]; mel_bins];
    for m in 1..=mel_bins {
        let left = bin_points[m - 1];
        let center = bin_points[m];
        let right = bin_points[m + 1];

        if center > left {
            for k in left..center {
                filters[m - 1][k] = (k - left) as f32 / (center - left) as f32;
            }
        }
        if right > center {
            for k in center..right {
                filters[m - 1][k] = (right - k) as f32 / (right - center) as f32;
            }
        }
    }

    filters
}

fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let angle = 2.0 * PI * i as f32 / (size as f32 - 1.0);
            0.5 * (1.0 - angle.cos())
        })
        .collect()
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
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

fn read_cache_file(cache_file: &Path) -> Result<(u32, Vec<f32>), String> {
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

fn write_cache_file(cache_file: &Path, sample_rate: u32, samples: &[f32]) -> Result<(), String> {
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

fn resample_to_target(samples: &[f32], src_rate: u32, target_rate: u32) -> Vec<f32> {
    if src_rate == 0 || target_rate == 0 || src_rate <= target_rate {
        return samples.to_vec();
    }

    let ratio = target_rate as f64 / src_rate as f64;
    let new_len = ((samples.len() as f64) * ratio).ceil() as usize;
    let mut output = Vec::with_capacity(new_len.max(1));

    for i in 0..new_len {
        let src_pos = (i as f64) / ratio;
        let lower = src_pos.floor() as usize;
        if lower >= samples.len() - 1 {
            output.push(*samples.last().unwrap_or(&0.0));
        } else {
            let frac = src_pos - lower as f64;
            let lower_val = samples[lower];
            let upper_val = samples[lower + 1];
            let val = lower_val * (1.0 - frac as f32) + upper_val * frac as f32;
            output.push(val);
        }
    }

    output
}
