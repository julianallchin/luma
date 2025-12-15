//! Host Audio State
//!
//! This module manages audio playback at the Host level (Editor/Renderer/Live Engine).
//! It is completely decoupled from graph execution - graphs are pure functions that
//! produce visualization/lighting data, while the Host owns audio playback.
//!
//! The Host loads a track segment, plays it, and broadcasts the current playhead
//! position. UI components use this position to render playhead overlays on
//! visualizations (mel specs, waveforms, etc.).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleRate, StreamConfig};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::sleep;
use ts_rs::TS;

use crate::audio::load_or_decode_audio;
use crate::database::Db;
use crate::engine::render_frame;
use crate::models::schema::LayerTimeSeries;
use crate::schema::BeatGrid;
use crate::tracks::TARGET_SAMPLE_RATE;

const STATE_EVENT: &str = "host-audio://state";
const UNIVERSE_EVENT: &str = "universe-state-update";

/// The Host Audio State - manages playback independently of graph execution
#[derive(Clone)]
pub struct HostAudioState {
    inner: Arc<Mutex<HostAudioInner>>,
}

impl Default for HostAudioState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HostAudioInner::new())),
        }
    }
}

impl HostAudioState {
    /// Spawn a background task that broadcasts playback state every 50ms
    pub fn spawn_broadcaster(&self, app_handle: AppHandle) {
        let state = self.inner.clone();
        let handle = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let mut frame_counter: u64 = 0;
            loop {
                let (snapshot, universe_state) = {
                    let mut guard = state.lock().expect("host audio state poisoned");
                    guard.refresh_progress();
                    let snap = guard.snapshot();

                    // Render Universe State if layer exists
                    let uni_state = if let Some(layer) = &guard.active_layer {
                        // Current time is relative to segment start (0.0)
                        // LayerTimeSeries assumes absolute time from the GraphContext
                        // When we load_segment, we pass startTime/endTime.
                        // We need to know the absolute start time of the segment to map playback time to layer time.

                        let abs_time = guard.segment_start_abs + snap.current_time;
                        Some(render_frame(layer, abs_time))
                    } else {
                        None
                    };

                    (snap, uni_state)
                };

                // Broadcast Audio State (Throttle to ~15fps to save UI thread)
                if frame_counter % 4 == 0 {
                    if handle.emit(STATE_EVENT, &snapshot).is_err() {
                        // Ignore event errors
                    }
                }

                // Broadcast Universe State (Full 60fps for smooth lights)
                if let Some(u_state) = universe_state {
                    let _ = handle.emit(UNIVERSE_EVENT, &u_state);

                    // Send ArtNet
                    if let Some(artnet) = handle.try_state::<crate::artnet::ArtNetManager>() {
                        artnet.broadcast(&u_state);
                    }
                }

                frame_counter += 1;
                sleep(Duration::from_millis(16)).await; // ~60fps
            }
        });
    }

    pub fn set_active_layer(&self, layer: Option<LayerTimeSeries>) {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.active_layer = layer;
    }

    pub fn load_segment(
        &self,
        samples: Vec<f32>,
        sample_rate: u32,
        beat_grid: Option<BeatGrid>,
        start_time_abs: f32,
    ) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.load_segment(samples, sample_rate, beat_grid, start_time_abs)
    }

    pub fn play(&self) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.play()
    }

    pub fn pause(&self) {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.pause();
    }

    pub fn seek(&self, seconds: f32) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.seek(seconds)
    }

    pub fn set_loop(&self, enabled: bool) {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.set_loop(enabled);
    }

    pub fn set_audio_output_enabled(&self, enabled: bool) {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.set_audio_output_enabled(enabled);
    }

    pub fn snapshot(&self) -> HostAudioSnapshot {
        let mut guard = self.inner.lock().expect("host audio state poisoned");
        guard.refresh_progress();
        guard.snapshot()
    }
}

/// Snapshot of playback state sent to frontend
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct HostAudioSnapshot {
    /// Whether audio is currently loaded
    pub is_loaded: bool,
    /// Whether playback is active
    pub is_playing: bool,
    /// Current playhead position in seconds (relative to segment start, 0 to duration)
    pub current_time: f32,
    /// Duration of the loaded segment in seconds
    pub duration_seconds: f32,
    /// Whether looping is enabled
    pub loop_enabled: bool,
}

struct LoadedSegment {
    samples: Arc<Vec<f32>>,
    sample_rate: u32,
    duration: f32,
    #[allow(dead_code)]
    beat_grid: Option<BeatGrid>,
}

/// Shared state between the audio thread and the main thread
struct SharedAudioState {
    /// Current sample index in the buffer
    sample_idx: std::sync::atomic::AtomicUsize,
    /// Whether we're actively outputting audio (vs silence)
    is_outputting: AtomicBool,
    /// Whether looping is enabled
    loop_flag: AtomicBool,
    /// The audio samples
    samples: Vec<f32>,
    /// Sample rate (kept for potential future use)
    #[allow(dead_code)]
    sample_rate: u32,
}

struct PersistentStream {
    /// Channel to signal the audio thread to stop
    stop_tx: mpsc::Sender<()>,
    /// Handle to the audio thread
    handle: Option<thread::JoinHandle<()>>,
    /// Shared state with the audio callback
    shared: Arc<SharedAudioState>,
}

struct HostAudioInner {
    segment: Option<LoadedSegment>,
    current_time: f32,
    is_playing: bool,
    start_offset: f32,
    start_instant: Option<Instant>,
    /// Persistent audio stream - created on load, kept alive until new segment
    stream: Option<PersistentStream>,
    loop_enabled: bool,
    audio_output_enabled: bool,

    // New fields
    active_layer: Option<LayerTimeSeries>,
    segment_start_abs: f32, // Absolute start time of the loaded segment
}

impl HostAudioInner {
    fn new() -> Self {
        Self {
            segment: None,
            current_time: 0.0,
            is_playing: false,
            start_offset: 0.0,
            start_instant: None,
            stream: None,
            loop_enabled: false,
            audio_output_enabled: true,
            active_layer: None,
            segment_start_abs: 0.0,
        }
    }

    fn load_segment(
        &mut self,
        samples: Vec<f32>,
        sample_rate: u32,
        beat_grid: Option<BeatGrid>,
        start_time_abs: f32,
    ) -> Result<(), String> {
        // Stop any current playback and stream
        self.stop_audio();
        self.stop_stream();

        if samples.is_empty() || sample_rate == 0 {
            return Err("Cannot load empty audio segment".into());
        }

        let duration = samples.len() as f32 / sample_rate as f32;

        self.segment = Some(LoadedSegment {
            samples: Arc::new(samples.clone()),
            sample_rate,
            duration,
            beat_grid,
        });
        self.current_time = 0.0;
        self.start_offset = 0.0;
        self.segment_start_abs = start_time_abs;

        // Create persistent stream if audio output is enabled
        if self.audio_output_enabled {
            self.stream = Some(Self::spawn_persistent_stream(
                samples,
                sample_rate,
                self.loop_enabled,
            )?);
        }

        Ok(())
    }

    fn play(&mut self) -> Result<(), String> {
        let segment = self
            .segment
            .as_ref()
            .ok_or("No audio segment loaded")?;

        let duration = segment.duration;
        let sample_rate = segment.sample_rate;

        let start_seconds = self.current_time.clamp(0.0, duration);
        self.current_time = start_seconds;
        self.start_offset = start_seconds;

        if duration <= 0.0 || start_seconds >= duration {
            self.current_time = duration;
            return Ok(());
        }

        self.is_playing = true;
        self.start_instant = Some(Instant::now());

        // Update the stream's sample index and start outputting
        if let Some(stream) = &self.stream {
            let start_sample = (start_seconds * sample_rate as f32).floor() as usize;
            stream
                .shared
                .sample_idx
                .store(start_sample, Ordering::SeqCst);
            stream.shared.is_outputting.store(true, Ordering::SeqCst);
        }

        Ok(())
    }

    fn pause(&mut self) {
        if self.is_playing {
            if let Some(start) = self.start_instant.take() {
                let elapsed = start.elapsed().as_secs_f32();
                let duration = self.segment.as_ref().map(|s| s.duration).unwrap_or(0.0);
                self.current_time = (self.start_offset + elapsed).min(duration);
            }
        }

        self.is_playing = false;
        self.start_instant = None;
        self.start_offset = self.current_time;

        // Stop outputting audio (but keep stream alive)
        if let Some(stream) = &self.stream {
            stream.shared.is_outputting.store(false, Ordering::SeqCst);
        }
    }

    fn set_audio_output_enabled(&mut self, enabled: bool) {
        if self.audio_output_enabled == enabled {
            return;
        }

        self.audio_output_enabled = enabled;

        if !enabled {
            // Stop and destroy the stream
            self.stop_stream();
            return;
        }

        // Create stream if we have a segment loaded
        if let Some(segment) = &self.segment {
            let samples: Vec<f32> = (*segment.samples).clone();
            let sample_rate = segment.sample_rate;

            if let Ok(stream) =
                Self::spawn_persistent_stream(samples, sample_rate, self.loop_enabled)
            {
                // If currently playing, set up the stream state
                if self.is_playing {
                    self.refresh_progress();
                    let start_sample =
                        (self.current_time * sample_rate as f32).floor() as usize;
                    stream
                        .shared
                        .sample_idx
                        .store(start_sample, Ordering::SeqCst);
                    stream.shared.is_outputting.store(true, Ordering::SeqCst);
                }
                self.stream = Some(stream);
            }
        }
    }

    fn seek(&mut self, seconds: f32) -> Result<(), String> {
        let segment = match &self.segment {
            Some(s) => s,
            None => {
                self.current_time = 0.0;
                return Ok(());
            }
        };

        let duration = segment.duration;
        let sample_rate = segment.sample_rate;

        if duration <= 0.0 {
            self.current_time = 0.0;
            return Ok(());
        }

        let clamped = seconds.clamp(0.0, duration);
        self.current_time = clamped;
        self.start_offset = clamped;

        // Update sample index in the stream
        if let Some(stream) = &self.stream {
            let sample_idx = (clamped * sample_rate as f32).floor() as usize;
            stream.shared.sample_idx.store(sample_idx, Ordering::SeqCst);
        }

        // Reset the timer if playing
        if self.is_playing {
            self.start_instant = Some(Instant::now());
        }

        Ok(())
    }

    fn set_loop(&mut self, enabled: bool) {
        let changed = self.loop_enabled != enabled;
        self.loop_enabled = enabled;

        if changed {
            if let Some(stream) = &self.stream {
                stream.shared.loop_flag.store(enabled, Ordering::SeqCst);
            }
            if self.is_playing {
                self.refresh_progress();
                self.start_offset = self.current_time;
                self.start_instant = Some(Instant::now());
            }
        }
    }

    fn stop_audio(&mut self) {
        self.is_playing = false;
        self.start_instant = None;

        // Stop outputting but keep stream alive
        if let Some(stream) = &self.stream {
            stream.shared.is_outputting.store(false, Ordering::SeqCst);
        }
    }

    fn stop_stream(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.stop_tx.send(());
            if let Some(handle) = stream.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn refresh_progress(&mut self) {
        let duration = match &self.segment {
            Some(s) => s.duration,
            None => return,
        };

        if !self.is_playing || duration <= 0.0 {
            return;
        }

        if let Some(start) = self.start_instant {
            let elapsed = start.elapsed().as_secs_f32();
            let position = self.start_offset + elapsed;

            if self.loop_enabled && position >= duration {
                let wrapped = position % duration;
                self.current_time = wrapped;
                self.start_offset = wrapped;
                self.start_instant = Some(Instant::now());
            } else if position >= duration {
                self.current_time = duration;
                self.stop_audio();
                self.start_offset = self.current_time;
            } else {
                self.current_time = position;
            }
        }
    }

    fn snapshot(&self) -> HostAudioSnapshot {
        let (is_loaded, duration) = match &self.segment {
            Some(s) => (true, s.duration),
            None => (false, 0.0),
        };

        HostAudioSnapshot {
            is_loaded,
            is_playing: self.is_playing,
            current_time: self.current_time,
            duration_seconds: duration,
            loop_enabled: self.loop_enabled,
        }
    }

    /// Spawn a persistent audio stream that stays alive until explicitly stopped.
    /// The stream outputs silence when `is_outputting` is false, and audio when true.
    fn spawn_persistent_stream(
        samples: Vec<f32>,
        sample_rate: u32,
        loop_enabled: bool,
    ) -> Result<PersistentStream, String> {
        let (stop_tx, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<Arc<SharedAudioState>, String>>();

        let handle = thread::spawn(move || {
            let result = (|| -> Result<Arc<SharedAudioState>, String> {
                let host = cpal::default_host();
                let device = host
                    .default_output_device()
                    .ok_or("No output device available")?;

                // Get supported config to determine channel count
                let supported_config = device
                    .default_output_config()
                    .map_err(|e| format!("Failed to get output config: {}", e))?;

                let channels = supported_config.channels();

                // Use device's default buffer size for compatibility
                let config = StreamConfig {
                    channels,
                    sample_rate: SampleRate(sample_rate),
                    buffer_size: BufferSize::Default,
                };

                // Create shared state
                let shared = Arc::new(SharedAudioState {
                    sample_idx: std::sync::atomic::AtomicUsize::new(0),
                    is_outputting: AtomicBool::new(false),
                    loop_flag: AtomicBool::new(loop_enabled),
                    samples,
                    sample_rate,
                });

                let shared_for_callback = shared.clone();
                let samples_len = shared.samples.len();

                let stream = device
                    .build_output_stream(
                        &config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            let ch = channels as usize;
                            let is_outputting =
                                shared_for_callback.is_outputting.load(Ordering::Relaxed);

                            for frame in data.chunks_mut(ch) {
                                let sample = if is_outputting {
                                    let current_idx =
                                        shared_for_callback.sample_idx.load(Ordering::Relaxed);

                                    if current_idx < samples_len {
                                        let s = shared_for_callback.samples[current_idx];
                                        shared_for_callback
                                            .sample_idx
                                            .fetch_add(1, Ordering::Relaxed);
                                        s
                                    } else if shared_for_callback.loop_flag.load(Ordering::Relaxed)
                                    {
                                        shared_for_callback.sample_idx.store(0, Ordering::Relaxed);
                                        shared_for_callback.samples.first().copied().unwrap_or(0.0)
                                    } else {
                                        0.0
                                    }
                                } else {
                                    // Output silence when paused
                                    0.0
                                };

                                // Write sample to all channels
                                for ch_sample in frame.iter_mut() {
                                    *ch_sample = sample;
                                }
                            }
                        },
                        |err| {
                            eprintln!("Audio stream error: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| format!("Failed to build output stream: {}", e))?;

                stream
                    .play()
                    .map_err(|e| format!("Failed to start playback: {}", e))?;

                let _ = ready_tx.send(Ok(shared.clone()));

                // Keep stream alive until stop signal
                let _ = stop_rx.recv();
                drop(stream);
                Ok(shared)
            })();

            if let Err(err) = result {
                let _ = ready_tx.send(Err(err));
            }
        });

        match ready_rx.recv() {
            Ok(Ok(shared)) => Ok(PersistentStream {
                stop_tx,
                handle: Some(handle),
                shared,
            }),
            Ok(Err(err)) => {
                let _ = stop_tx.send(());
                let _ = handle.join();
                Err(err)
            }
            Err(_) => Err("Audio stream worker failed to start".into()),
        }
    }
}

impl Clone for LoadedSegment {
    fn clone(&self) -> Self {
        Self {
            samples: self.samples.clone(),
            sample_rate: self.sample_rate,
            duration: self.duration,
            beat_grid: self.beat_grid.clone(),
        }
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Load a track segment for playback
#[tauri::command]
pub async fn host_load_segment(
    db: State<'_, Db>,
    host: State<'_, HostAudioState>,
    track_id: i64,
    start_time: f32,
    end_time: f32,
    beat_grid: Option<BeatGrid>,
) -> Result<(), String> {
    // Fetch track path from DB
    let track_row: Option<(String, String)> =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&db.0)
            .await
            .map_err(|e| format!("Failed to fetch track: {}", e))?;

    let (file_path, track_hash) =
        track_row.ok_or_else(|| format!("Track {} not found", track_id))?;

    // Load and decode audio
    let path = Path::new(&file_path);
    let (full_samples, sample_rate) = load_or_decode_audio(path, &track_hash, TARGET_SAMPLE_RATE)
        .map_err(|e| format!("Failed to decode track: {}", e))?;

    if full_samples.is_empty() || sample_rate == 0 {
        return Err("Track has no audio data".into());
    }

    // Slice to segment
    let start_sample = (start_time * sample_rate as f32).floor().max(0.0) as usize;
    let end_sample = if end_time > 0.0 {
        (end_time * sample_rate as f32).ceil() as usize
    } else {
        full_samples.len()
    };

    let samples = if start_sample >= full_samples.len() {
        Vec::new()
    } else {
        let capped_end = end_sample.min(full_samples.len());
        full_samples[start_sample..capped_end].to_vec()
    };

    if samples.is_empty() {
        return Err("Segment time range produced empty audio".into());
    }

    // PASS start_time as absolute time
    host.load_segment(samples, sample_rate, beat_grid, start_time)
}

/// Start playback
#[tauri::command]
pub fn host_play(host: State<'_, HostAudioState>) -> Result<(), String> {
    host.play()
}

/// Pause playback
#[tauri::command]
pub fn host_pause(host: State<'_, HostAudioState>) {
    host.pause();
}

/// Seek to position (seconds relative to segment start)
#[tauri::command]
pub fn host_seek(host: State<'_, HostAudioState>, seconds: f32) -> Result<(), String> {
    host.seek(seconds)
}

/// Enable/disable looping
#[tauri::command]
pub fn host_set_loop(host: State<'_, HostAudioState>, enabled: bool) {
    host.set_loop(enabled);
}

/// Get current playback state
#[tauri::command]
pub fn host_snapshot(host: State<'_, HostAudioState>) -> HostAudioSnapshot {
    host.snapshot()
}

pub async fn reload_settings(app: &AppHandle) -> Result<(), String> {
    let settings = crate::settings::get_all_settings(app).await?;
    if let Some(host) = app.try_state::<HostAudioState>() {
        host.set_audio_output_enabled(settings.audio_output_enabled);
    }
    Ok(())
}

/// Load a full track for playback (convenience for track editor)
#[tauri::command]
pub async fn host_load_track(
    db: State<'_, Db>,
    host: State<'_, HostAudioState>,
    track_id: i64,
) -> Result<(), String> {
    // Fetch track path from DB
    let track_row: Option<(String, String)> =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&db.0)
            .await
            .map_err(|e| format!("Failed to fetch track: {}", e))?;

    let (file_path, track_hash) =
        track_row.ok_or_else(|| format!("Track {} not found", track_id))?;

    // Load and decode full audio
    let path = Path::new(&file_path);
    let (samples, sample_rate) = load_or_decode_audio(path, &track_hash, TARGET_SAMPLE_RATE)
        .map_err(|e| format!("Failed to decode track: {}", e))?;

    if samples.is_empty() || sample_rate == 0 {
        return Err("Track has no audio data".into());
    }

    // Load beat grid if available
    let beat_grid = sqlx::query_as::<_, (String, String, Option<f64>, Option<f64>, Option<i64>)>(
        "SELECT beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar FROM track_beats WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(&db.0)
    .await
    .ok()
    .flatten()
    .and_then(|(beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)| {
        let beats: Vec<f32> = serde_json::from_str(&beats_json).ok()?;
        let downbeats: Vec<f32> = serde_json::from_str(&downbeats_json).ok()?;
        let (fallback_bpm, fallback_offset, fallback_bpb) =
            crate::tracks::infer_grid_metadata(&beats, &downbeats);
        Some(BeatGrid {
            beats,
            downbeats,
            bpm: bpm.unwrap_or(fallback_bpm as f64) as f32,
            downbeat_offset: downbeat_offset.unwrap_or(fallback_offset as f64) as f32,
            beats_per_bar: beats_per_bar.unwrap_or(fallback_bpb as i64) as i32,
        })
    });

    // Start time 0.0 for full track
    host.load_segment(samples, sample_rate, beat_grid, 0.0)
}
