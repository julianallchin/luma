//! Render Engine
//!
//! Owns all rendering state (layers, universe generation, ArtNet output).
//! Decoupled from audio playback — reads time from HostAudioState only in
//! edit mode. In perform mode it renders per-deck layers and blends by volume.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::sleep;

use crate::engine::render_frame;
use crate::host_audio::HostAudioState;
use crate::models::node_graph::LayerTimeSeries;
use crate::models::universe::{PrimitiveState, UniverseState};

/// Per-deck render input from the Perform page.
#[derive(Deserialize, Clone, Debug)]
pub struct PerformDeckInput {
    pub deck_id: u8,
    pub time: f32,
    pub volume: f32, // effective volume = fader * crossfader weight
}

const UNIVERSE_EVENT: &str = "universe-state-update";

#[derive(Clone)]
pub struct RenderEngine {
    inner: Arc<Mutex<RenderEngineInner>>,
}

struct RenderEngineInner {
    /// Active layer for track editor / pattern editor
    active_layer: Option<LayerTimeSeries>,
    /// Per-deck layers for perform mode
    perform_layers: HashMap<u8, LayerTimeSeries>,
    /// Per-deck time + volume from frontend each frame
    perform_deck_states: Vec<PerformDeckInput>,
}

impl Default for RenderEngine {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RenderEngineInner {
                active_layer: None,
                perform_layers: HashMap::new(),
                perform_deck_states: Vec::new(),
            })),
        }
    }
}

impl RenderEngine {
    pub fn set_active_layer(&self, layer: Option<LayerTimeSeries>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.active_layer = layer;
    }

    pub fn set_perform_deck_states(&self, states: Vec<PerformDeckInput>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.perform_deck_states = states;
    }

    /// Move the current active_layer into a perform deck slot.
    /// Called after composite_track to redirect the result to a specific deck.
    pub fn promote_active_layer_to_deck(&self, deck_id: u8) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if let Some(layer) = guard.active_layer.take() {
            guard.perform_layers.insert(deck_id, layer);
        }
    }

    pub fn clear_perform(&self) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.perform_layers.clear();
        guard.perform_deck_states.clear();
    }

    /// Spawn the ~60fps render loop that emits universe-state-update + ArtNet.
    pub fn spawn_render_loop(&self, app_handle: AppHandle) {
        let state = self.inner.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let universe_state = {
                    let guard = state.lock().expect("render engine poisoned");

                    if !guard.perform_deck_states.is_empty() {
                        // Perform mode: render each deck's layer and blend
                        Some(render_perform_mix(
                            &guard.perform_layers,
                            &guard.perform_deck_states,
                        ))
                    } else if let Some(layer) = &guard.active_layer {
                        // Track editor mode: read time from host audio
                        if let Some(host) = app_handle.try_state::<HostAudioState>() {
                            let abs_time = host.render_time();
                            Some(render_frame(layer, abs_time))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                if let Some(u_state) = universe_state {
                    let _ = app_handle.emit(UNIVERSE_EVENT, &u_state);

                    // Send ArtNet
                    if let Some(artnet) = app_handle.try_state::<crate::artnet::ArtNetManager>() {
                        artnet.broadcast(&u_state);
                    }
                }

                sleep(Duration::from_millis(16)).await; // ~60fps
            }
        });
    }
}

/// Render each deck's layer at its current time and blend by volume.
fn render_perform_mix(
    layers: &HashMap<u8, LayerTimeSeries>,
    deck_states: &[PerformDeckInput],
) -> UniverseState {
    // Collect (rendered state, volume) for decks that have a layer
    let mut frames: Vec<(UniverseState, f32)> = Vec::new();
    for ds in deck_states {
        if ds.volume <= 0.0 {
            continue;
        }
        if let Some(layer) = layers.get(&ds.deck_id) {
            frames.push((render_frame(layer, ds.time), ds.volume));
        }
    }

    if frames.is_empty() {
        return UniverseState {
            primitives: HashMap::new(),
        };
    }

    if frames.len() == 1 {
        return frames.into_iter().next().unwrap().0;
    }

    // Blend: weighted average by volume across all contributing decks
    let total_volume: f32 = frames.iter().map(|(_, v)| *v).sum();
    if total_volume <= 0.0 {
        return UniverseState {
            primitives: HashMap::new(),
        };
    }

    // Collect all fixture keys across all frames
    let mut all_keys = std::collections::HashSet::new();
    for (state, _) in &frames {
        all_keys.extend(state.primitives.keys().cloned());
    }

    let mut blended = HashMap::with_capacity(all_keys.len());
    for key in all_keys {
        let mut dimmer = 0.0f32;
        let mut color = [0.0f32; 3];
        let mut strobe = 0.0f32;
        let mut position = [0.0f32; 2];
        let mut speed = 0.0f32;

        for (state, vol) in &frames {
            let w = vol / total_volume;
            if let Some(prim) = state.primitives.get(&key) {
                dimmer += prim.dimmer * w;
                color[0] += prim.color[0] * w;
                color[1] += prim.color[1] * w;
                color[2] += prim.color[2] * w;
                strobe += prim.strobe * w;
                position[0] += prim.position[0] * w;
                position[1] += prim.position[1] * w;
                speed += prim.speed * w;
            }
            // If a fixture isn't in this frame, it contributes zeros (dark)
        }

        blended.insert(
            key,
            PrimitiveState {
                dimmer: dimmer.clamp(0.0, 1.0),
                color,
                strobe: strobe.clamp(0.0, 1.0),
                position,
                speed: if speed > 0.5 { 1.0 } else { 0.0 },
            },
        );
    }

    UniverseState {
        primitives: blended,
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Batch-update per-deck render states (time + volume) from the Perform page.
/// Called every StateChanged frame to drive real-time crossfade blending.
#[tauri::command]
pub fn render_set_deck_states(
    render_engine: State<'_, RenderEngine>,
    states: Vec<PerformDeckInput>,
) {
    render_engine.set_perform_deck_states(states);
}

/// Clear all perform state (layers + deck states). Called on disconnect/unmount.
#[tauri::command]
pub fn render_clear_perform(render_engine: State<'_, RenderEngine>) {
    render_engine.clear_perform();
}
