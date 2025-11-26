use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rodio::{buffer::SamplesBuffer, OutputStream, Sink, Source};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::time::sleep;
use ts_rs::TS;

use crate::schema::{AudioCrop, BeatGrid};

const STATE_EVENT: &str = "pattern-playback://state";

#[derive(Clone)]
pub struct PatternPlaybackState {
    inner: Arc<Mutex<PlaybackInner>>,
}

impl Default for PatternPlaybackState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PlaybackInner::new())),
        }
    }
}

impl PatternPlaybackState {
    pub fn spawn_broadcaster(&self, app_handle: AppHandle) {
        let state = self.inner.clone();
        let handle = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let snapshot = {
                    let mut guard = state.lock().expect("pattern playback state poisoned");
                    guard.refresh_progress();
                    guard.snapshot()
                };

                if handle.emit(STATE_EVENT, snapshot).is_err() {
                    // Ignore event errors (likely no listeners yet)
                }

                sleep(Duration::from_millis(50)).await;
            }
        });
    }

    pub fn update_entries(&self, entries: Vec<PlaybackEntryData>) {
        let mut guard = self.inner.lock().expect("pattern playback state poisoned");
        guard.replace_entries(entries);
    }

    pub fn play_node(&self, node_id: String) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("pattern playback state poisoned");
        guard.play_node(node_id)
    }

    pub fn pause(&self) {
        let mut guard = self.inner.lock().expect("pattern playback state poisoned");
        guard.pause();
    }

    pub fn seek(&self, seconds: f32) -> Result<(), String> {
        let mut guard = self.inner.lock().expect("pattern playback state poisoned");
        guard.seek(seconds)
    }

    pub fn snapshot(&self) -> PlaybackStateSnapshot {
        let mut guard = self.inner.lock().expect("pattern playback state poisoned");
        guard.refresh_progress();
        guard.snapshot()
    }
}

#[derive(Clone)]
pub struct PlaybackEntryData {
    pub node_id: String,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub beat_grid: Option<BeatGrid>,
    pub crop: Option<AudioCrop>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct PlaybackStateSnapshot {
    pub active_node_id: Option<String>,
    pub is_playing: bool,
    pub current_time: f32,
    pub duration_seconds: f32,
}

#[derive(Clone)]
#[allow(dead_code)] // Keep additional metadata handy for upcoming playback features
struct PlaybackEntry {
    samples: Arc<Vec<f32>>,
    sample_rate: u32,
    duration: f32,
    beat_grid: Option<BeatGrid>,
    crop: Option<AudioCrop>,
}

impl PlaybackEntry {
    fn from_data(data: PlaybackEntryData) -> Self {
        let duration = if data.sample_rate == 0 {
            0.0
        } else {
            data.samples.len() as f32 / data.sample_rate as f32
        };
        Self {
            samples: Arc::new(data.samples),
            sample_rate: data.sample_rate,
            duration,
            beat_grid: data.beat_grid,
            crop: data.crop,
        }
    }
}

struct ActiveAudio {
    stop_tx: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

struct PlaybackInner {
    entries: HashMap<String, PlaybackEntry>,
    active_node_id: Option<String>,
    current_time: f32,
    duration: f32,
    is_playing: bool,
    start_offset: f32,
    start_instant: Option<Instant>,
    active_audio: Option<ActiveAudio>,
    loop_enabled: bool,
}

impl PlaybackInner {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            active_node_id: None,
            current_time: 0.0,
            duration: 0.0,
            is_playing: false,
            start_offset: 0.0,
            start_instant: None,
            active_audio: None,
            loop_enabled: false,
        }
    }

    fn replace_entries(&mut self, entries: Vec<PlaybackEntryData>) {
        let mut next = HashMap::new();
        for entry in entries {
            next.insert(entry.node_id.clone(), PlaybackEntry::from_data(entry));
        }
        self.entries = next;

        if let Some(active) = &self.active_node_id {
            if !self.entries.contains_key(active) {
                self.stop_audio();
                self.active_node_id = None;
                self.current_time = 0.0;
                self.duration = 0.0;
            } else if let Some(entry) = self.entries.get(active) {
                self.duration = entry.duration;
                self.current_time = self.current_time.min(self.duration);
            }
        } else {
            self.current_time = 0.0;
            self.duration = 0.0;
        }
    }

    fn play_node(&mut self, node_id: String) -> Result<(), String> {
        if !self.entries.contains_key(&node_id) {
            return Err(format!("Pattern entry '{}' not available", node_id));
        }

        if self.active_node_id.as_deref() != Some(node_id.as_str()) {
            self.active_node_id = Some(node_id);
            // New track, start from 0
            self.current_time = 0.0;
            self.start_offset = 0.0;
        } else {
            // Same track, resume from current time
            self.start_offset = self.current_time;
        }

        self.start_audio()
    }

    fn pause(&mut self) {
        if self.is_playing {
            if let Some(start) = self.start_instant.take() {
                let elapsed = start.elapsed().as_secs_f32();
                self.current_time = (self.start_offset + elapsed).min(self.duration);
            }
        }
        self.stop_audio();
        self.start_offset = self.current_time;
    }

    fn seek(&mut self, seconds: f32) -> Result<(), String> {
        if self.active_node_id.is_none() {
            return Err("No active pattern entry".into());
        }
        if self.duration <= 0.0 {
            self.current_time = 0.0;
            return Ok(());
        }
        let clamped = seconds.clamp(0.0, self.duration);
        self.current_time = clamped;
        self.start_offset = clamped;
        if self.is_playing {
            self.start_audio()?;
        }
        Ok(())
    }

    fn set_loop(&mut self, enabled: bool) -> Result<(), String> {
        self.loop_enabled = enabled;
        // Apply on next wrap; do not reset playback position
        Ok(())
    }

    fn start_audio(&mut self) -> Result<(), String> {
        let node_id = match &self.active_node_id {
            Some(id) => id.clone(),
            None => return Err("No active pattern entry".into()),
        };

        let entry = self
            .entries
            .get(&node_id)
            .cloned()
            .ok_or_else(|| format!("Pattern entry '{}' not available", node_id))?;

        if entry.samples.is_empty() || entry.sample_rate == 0 {
            return Err("Pattern entry is missing audio data".into());
        }

        self.stop_audio();

        self.duration = entry.duration;
        let start_seconds = self.current_time.clamp(0.0, self.duration);
        self.current_time = start_seconds;
        self.start_offset = start_seconds;

        let total_samples = entry.samples.len();
        let start_sample = ((start_seconds * entry.sample_rate as f32).floor() as usize)
            .min(total_samples.saturating_sub(1));

        if start_sample >= total_samples {
            self.current_time = self.duration;
            self.is_playing = false;
            return Ok(());
        }

        let slice = entry.samples[start_sample..].to_vec();
        if slice.is_empty() {
            self.current_time = self.duration;
            self.is_playing = false;
            return Ok(());
        }

        let (stop_tx, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let sample_rate = entry.sample_rate;
        let buffer_samples = slice.clone();
        let handle = thread::spawn(move || {
            let playback = (|| -> Result<(), String> {
                let (stream, stream_handle) = OutputStream::try_default()
                    .map_err(|e| format!("Failed to access output stream: {}", e))?;
                let sink = Sink::try_new(&stream_handle)
                    .map_err(|e| format!("Failed to create output sink: {}", e))?;
                let source = SamplesBuffer::new(1, sample_rate, buffer_samples);
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
        if !self.is_playing || self.duration <= 0.0 {
            return;
        }

        if let Some(start) = self.start_instant {
            let elapsed = start.elapsed().as_secs_f32();
            let position = self.start_offset + elapsed;
            if self.loop_enabled && position >= self.duration {
                let wrapped = (position - self.duration).max(0.0);
                self.stop_audio();
                self.current_time = wrapped;
                self.start_offset = wrapped;
                let _ = self.start_audio();
            } else if position >= self.duration {
                self.current_time = self.duration;
                self.stop_audio();
                self.start_offset = self.current_time;
            } else {
                self.current_time = position;
            }
        }
    }

    fn snapshot(&self) -> PlaybackStateSnapshot {
        PlaybackStateSnapshot {
            active_node_id: self.active_node_id.clone(),
            is_playing: self.is_playing,
            current_time: self.current_time,
            duration_seconds: self.duration,
        }
    }
}

#[tauri::command]
pub fn playback_play_node(
    state: State<'_, PatternPlaybackState>,
    node_id: String,
) -> Result<(), String> {
    state.play_node(node_id)
}

#[tauri::command]
pub fn playback_pause(state: State<'_, PatternPlaybackState>) {
    state.pause();
}

#[tauri::command]
pub fn playback_seek(state: State<'_, PatternPlaybackState>, seconds: f32) -> Result<(), String> {
    state.seek(seconds)
}

#[tauri::command]
pub fn playback_set_loop(
    state: State<'_, PatternPlaybackState>,
    enabled: bool,
) -> Result<(), String> {
    let mut guard = state.inner.lock().expect("pattern playback state poisoned");
    guard.set_loop(enabled)
}

#[tauri::command]
pub fn playback_snapshot(state: State<'_, PatternPlaybackState>) -> PlaybackStateSnapshot {
    state.snapshot()
}
