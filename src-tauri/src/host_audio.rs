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

use rodio::{OutputStream, Sink, Source};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
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

struct ActiveAudio {
    stop_tx: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
    loop_flag: Arc<AtomicBool>,
}

struct HostAudioInner {
    segment: Option<LoadedSegment>,
    current_time: f32,
    is_playing: bool,
    start_offset: f32,
    start_instant: Option<Instant>,
    active_audio: Option<ActiveAudio>,
    loop_enabled: bool,
    
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
            active_audio: None,
            loop_enabled: false,
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
        // Stop any current playback
        self.stop_audio();

        if samples.is_empty() || sample_rate == 0 {
            return Err("Cannot load empty audio segment".into());
        }

        let duration = samples.len() as f32 / sample_rate as f32;

        self.segment = Some(LoadedSegment {
            samples: Arc::new(samples),
            sample_rate,
            duration,
            beat_grid,
        });
        self.current_time = 0.0;
        self.start_offset = 0.0;
        self.segment_start_abs = start_time_abs;

        Ok(())
    }
    
    fn play(&mut self) -> Result<(), String> {
        let segment = self
            .segment
            .as_ref()
            .ok_or("No audio segment loaded")?
            .clone();

        self.stop_audio();

        let start_seconds = self.current_time.clamp(0.0, segment.duration);
        self.current_time = start_seconds;
        self.start_offset = start_seconds;

        if segment.duration <= 0.0 || start_seconds >= segment.duration {
            self.current_time = segment.duration;
            return Ok(());
        }

        let (stop_tx, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let sample_rate = segment.sample_rate;
        let buffer_samples = segment.samples.clone();
        let loop_flag = Arc::new(AtomicBool::new(self.loop_enabled));
        let loop_flag_for_thread = loop_flag.clone();
        let start_sample = (start_seconds * sample_rate as f32).floor() as usize;

        let handle = thread::spawn(move || {
            let playback = (|| -> Result<(), String> {
                let (stream, stream_handle) = OutputStream::try_default()
                    .map_err(|e| format!("Failed to access output stream: {}", e))?;
                let sink = Sink::try_new(&stream_handle)
                    .map_err(|e| format!("Failed to create output sink: {}", e))?;
                let source: Box<dyn Source<Item = f32> + Send> = Box::new(LoopingSamples {
                    samples: buffer_samples,
                    idx: start_sample,
                    sample_rate,
                    loop_flag: loop_flag_for_thread,
                });
                sink.append(source);
                sink.play();
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv();
                sink.stop();
                drop(sink);
                drop(stream);
                Ok(())
            })();
            if let Err(err) = playback {
                let _ = ready_tx.send(Err(err));
            }
        });

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.is_playing = true;
                self.start_instant = Some(Instant::now());
                self.active_audio = Some(ActiveAudio {
                    stop_tx,
                    handle: Some(handle),
                    loop_flag,
                });
                Ok(())
            }
            Ok(Err(err)) => {
                let _ = stop_tx.send(());
                let _ = handle.join();
                Err(err)
            }
            Err(_) => Err("Playback worker failed to start".into()),
        }
    }

    fn pause(&mut self) {
        if self.is_playing {
            if let Some(start) = self.start_instant.take() {
                let elapsed = start.elapsed().as_secs_f32();
                let duration = self.segment.as_ref().map(|s| s.duration).unwrap_or(0.0);
                self.current_time = (self.start_offset + elapsed).min(duration);
            }
        }
        self.stop_audio();
        self.start_offset = self.current_time;
    }

    fn seek(&mut self, seconds: f32) -> Result<(), String> {
        let duration = self.segment.as_ref().map(|s| s.duration).unwrap_or(0.0);

        if duration <= 0.0 {
            self.current_time = 0.0;
            return Ok(());
        }

        let clamped = seconds.clamp(0.0, duration);
        self.current_time = clamped;
        self.start_offset = clamped;

        if self.is_playing {
            self.play()?;
        }
        Ok(())
    }

    fn set_loop(&mut self, enabled: bool) {
        let changed = self.loop_enabled != enabled;
        self.loop_enabled = enabled;

        if changed {
            if let Some(active) = &self.active_audio {
                active.loop_flag.store(enabled, Ordering::SeqCst);
            }
            if self.is_playing {
                self.refresh_progress();
                self.start_offset = self.current_time;
                self.start_instant = Some(Instant::now());
            }
        }
    }

    fn stop_audio(&mut self) {
        if let Some(mut active) = self.active_audio.take() {
            let _ = active.stop_tx.send(());
            if let Some(handle) = active.handle.take() {
                let _ = handle.join();
            }
        }
        self.is_playing = false;
        self.start_instant = None;
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

/// Source that can toggle looping live without rebuilding the sink
struct LoopingSamples {
    samples: Arc<Vec<f32>>,
    idx: usize,
    sample_rate: u32,
    loop_flag: Arc<AtomicBool>,
}

impl Iterator for LoopingSamples {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.samples.is_empty() {
            return None;
        }
        if self.idx >= self.samples.len() {
            if self.loop_flag.load(Ordering::SeqCst) {
                self.idx = 0;
            } else {
                return None;
            }
        }
        let sample = *self.samples.get(self.idx)?;
        self.idx += 1;
        Some(sample)
    }
}

impl Source for LoopingSamples {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        1
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
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
