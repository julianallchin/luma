//! Render Engine
//!
//! Owns all rendering state (layers, universe generation, ArtNet output).
//! Decoupled from audio playback — reads time from HostAudioState only in
//! edit mode. In perform mode it renders per-deck layers and blends by volume.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::sleep;

use crate::engine::render_frame_max;
use crate::host_audio::HostAudioState;
use crate::models::node_graph::{BlendMode, LayerTimeSeries};
use crate::models::universe::{PrimitiveState, UniverseState};

/// Per-deck render input from the Perform page.
#[derive(Deserialize, Clone, Debug)]
pub struct PerformDeckInput {
    pub deck_id: u8,
    pub time: f32,
    pub volume: f32, // effective volume = fader * crossfader weight
}

const UNIVERSE_EVENT: &str = "universe-state-update";

/// deck_id reserved for the always-running simulated deck (no real track required).
pub const SIM_DECK_ID: u8 = 99;
/// Duration of the simulated deck's virtual track in seconds.
/// 120 BPM / 4/4 → 0.5s per beat → 2s per bar → 600s = 5 bars × 60 (10 minutes).
const SIM_DECK_DURATION: f32 = 600.0;

// ============================================================================
// Manual Layer State — live cue state driven by MIDI
// ============================================================================

/// A cue that has been triggered (latched or flashed) by the LD.
#[derive(Clone, Debug)]
pub struct CueInstance {
    pub cue_id: String,
    pub triggered_at: Instant,
    pub resolved_target: ResolvedTarget,
}

#[derive(Clone, Debug)]
pub enum ResolvedTarget {
    All,
    Groups(Vec<String>),
}

/// Per-group intensity + active cues.
#[derive(Default, Clone, Debug)]
pub struct ManualGroupState {
    pub intensity: f32,
    /// Latched (toggle-on) cue instances
    pub active_cues: HashMap<String, CueInstance>,
    /// Held (flash) cue instances
    pub flash_cues: HashMap<String, CueInstance>,
}

impl ManualGroupState {
    pub fn new() -> Self {
        Self {
            intensity: 1.0,
            ..Default::default()
        }
    }
}

/// Live LD state — modified by MIDI callback, read by 60fps render loop.
#[derive(Clone, Debug)]
pub struct ManualLayerState {
    /// Whether the manual layer is composited on top of the score
    pub active: bool,
    /// Modifier names currently held (used for target resolution)
    pub held_modifiers: HashSet<String>,
    /// binding_id → press Instant, used for TapToggleHoldFlash timing
    pub tap_timestamps: HashMap<String, Instant>,
    /// Master intensity multiplier (0.0–1.0)
    pub master_intensity: f32,
    /// Per-group state. Key = group_id.
    pub per_group: HashMap<String, ManualGroupState>,
    /// State for Target::All cues (not group-targeted)
    pub global: ManualGroupState,
}

impl Default for ManualLayerState {
    fn default() -> Self {
        Self {
            active: false,
            held_modifiers: HashSet::new(),
            tap_timestamps: HashMap::new(),
            master_intensity: 1.0,
            per_group: HashMap::new(),
            global: ManualGroupState::new(),
        }
    }
}

impl ManualLayerState {
    /// True if any cue (active or flash) is queued, regardless of the `active` flag.
    pub fn has_any_cues(&self) -> bool {
        !self.global.active_cues.is_empty()
            || !self.global.flash_cues.is_empty()
            || self
                .per_group
                .values()
                .any(|gs| !gs.active_cues.is_empty() || !gs.flash_cues.is_empty())
    }
}

// ============================================================================
// Compiled cue buffer
// ============================================================================

#[derive(Clone, Debug)]
pub enum CompiledCueMode {
    Loop,
    TrackTime,
}

/// A pre-compiled cue ready for sampling in the render loop.
/// Always compiled for the full track duration — patterns repeat naturally in
/// the buffer. Sample at deck_time directly for all modes.
#[derive(Clone)]
pub struct CompiledCue {
    pub layer: LayerTimeSeries,
    pub execution_mode: CompiledCueMode,
    /// z_index from the Cue definition (copied here so render loop doesn't need DB)
    pub z_index: i8,
    pub blend_mode: BlendMode,
}

// ============================================================================
// RenderEngine
// ============================================================================

#[derive(Clone)]
pub struct RenderEngine {
    inner: Arc<Mutex<RenderEngineInner>>,
}

/// Blink-twice identify sequence for a single fixture.
struct IdentifyState {
    fixture_id: String,
    start: Instant,
}

/// Two blinks over 0.6s: ON 0–0.15, OFF 0.15–0.3, ON 0.3–0.45, OFF 0.45–0.6
const IDENTIFY_DURATION: f32 = 0.6;

fn identify_dimmer(elapsed: f32) -> f32 {
    if (elapsed < 0.15) || (elapsed >= 0.3 && elapsed < 0.45) {
        1.0
    } else {
        0.0
    }
}

pub(crate) struct RenderEngineInner {
    /// Active layer for track editor / pattern editor
    active_layer: Option<LayerTimeSeries>,
    /// Per-deck layers for perform mode (score composites)
    perform_layers: HashMap<u8, LayerTimeSeries>,
    /// Per-deck time + volume from frontend each frame
    perform_deck_states: Vec<PerformDeckInput>,
    /// Fixture identify blink (highest priority)
    identify: Option<IdentifyState>,

    // --- Live controller layer ---
    /// Pre-compiled cue buffers. Key = (deck_id, cue_id).
    pub cue_buffers: HashMap<(u8, String), CompiledCue>,
    /// Live LD state (modified by MIDI callback thread)
    pub manual_layer: ManualLayerState,
    /// group_id → [fixture_id, ...]. Built at cue-compile time. Used for target filtering.
    pub group_fixture_map: HashMap<String, Vec<String>>,
    /// Wall-clock start for the always-running simulated deck (deck_id=99).
    simulated_deck_start: Instant,
}

impl Default for RenderEngine {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RenderEngineInner {
                active_layer: None,
                perform_layers: HashMap::new(),
                perform_deck_states: Vec::new(),
                identify: None,
                cue_buffers: HashMap::new(),
                manual_layer: ManualLayerState::default(),
                group_fixture_map: HashMap::new(),
                simulated_deck_start: Instant::now(),
            })),
        }
    }
}

impl RenderEngine {
    pub fn set_active_layer(&self, layer: Option<LayerTimeSeries>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.active_layer = layer;
    }

    /// Clone the current active layer. Used by the video export pipeline to
    /// capture the composite produced by `composite_track` for offline sampling.
    pub fn get_active_layer(&self) -> Option<LayerTimeSeries> {
        let guard = self.inner.lock().expect("render engine poisoned");
        guard.active_layer.clone()
    }

    pub fn set_perform_deck_states(&self, states: Vec<PerformDeckInput>) {
        log::debug!("[render] set_perform_deck_states: {} decks", states.len());
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.perform_deck_states = states;
    }

    /// Move the current active_layer into a perform deck slot.
    /// Called after composite_track to redirect the result to a specific deck.
    pub fn promote_active_layer_to_deck(&self, deck_id: u8) {
        log::info!("[render] promoting active_layer to deck {deck_id}");
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if let Some(layer) = guard.active_layer.take() {
            guard.perform_layers.insert(deck_id, layer);
        }
    }

    pub fn clear_perform(&self) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        log::warn!(
            "[render] clear_perform called — clearing {} deck layers and {} deck states",
            guard.perform_layers.len(),
            guard.perform_deck_states.len()
        );
        guard.perform_layers.clear();
        guard.perform_deck_states.clear();
        // Clear cue buffers for all decks; keep manual_layer state
        guard.cue_buffers.clear();
    }

    pub fn identify_fixture(&self, fixture_id: String) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.identify = Some(IdentifyState {
            fixture_id,
            start: Instant::now(),
        });
    }

    // --- MIDI live layer methods ---

    /// Store a compiled cue buffer for a deck.
    pub fn set_cue_buffer(&self, deck_id: u8, cue_id: &str, compiled: CompiledCue) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard
            .cue_buffers
            .insert((deck_id, cue_id.to_string()), compiled);
    }

    /// Remove cue buffers for a single cue across all decks.
    pub fn remove_cue_buffers(&self, cue_id: &str) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.cue_buffers.retain(|(_, id), _| id != cue_id);
    }

    /// Update the group→fixture map used for target filtering.
    pub fn set_group_fixture_map(&self, map: HashMap<String, Vec<String>>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.group_fixture_map = map;
    }

    /// Toggle whether the manual layer is active.
    pub fn set_manual_active(&self, active: bool) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.manual_layer.active = active;
    }

    /// Set master intensity (0.0–1.0).
    pub fn set_master_intensity(&self, intensity: f32) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.manual_layer.master_intensity = intensity.clamp(0.0, 1.0);
    }

    /// Set per-group intensity (0.0–1.0). None = master.
    pub fn set_group_intensity(&self, group_id: Option<String>, intensity: f32) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        match group_id {
            None => guard.manual_layer.master_intensity = intensity.clamp(0.0, 1.0),
            Some(gid) => {
                guard
                    .manual_layer
                    .per_group
                    .entry(gid)
                    .or_insert_with(ManualGroupState::new)
                    .intensity = intensity.clamp(0.0, 1.0);
            }
        }
    }

    /// Latch a cue on (toggle). Also enforces radio-button exclusivity at same z_index.
    pub fn latch_cue_on(&self, cue_id: &str, resolved_target: ResolvedTarget, z_index: i8) {
        let mut guard = self.inner.lock().expect("render engine poisoned");

        // Collect cue IDs at the same z_index from cue_buffers FIRST (avoids borrow conflict).
        let cue_ids_at_z: HashSet<String> = guard
            .cue_buffers
            .iter()
            .filter(|(_, c)| c.z_index == z_index)
            .map(|((_, cid), _)| cid.clone())
            .collect();

        // Enforce radio-button exclusivity: remove other active cues at same z_index.
        guard
            .manual_layer
            .global
            .active_cues
            .retain(|id, _| id == cue_id || !cue_ids_at_z.contains(id));
        for gs in guard.manual_layer.per_group.values_mut() {
            gs.active_cues
                .retain(|id, _| id == cue_id || !cue_ids_at_z.contains(id));
        }

        let instance = CueInstance {
            cue_id: cue_id.to_string(),
            triggered_at: Instant::now(),
            resolved_target: resolved_target.clone(),
        };

        match &resolved_target {
            ResolvedTarget::All => {
                guard
                    .manual_layer
                    .global
                    .active_cues
                    .insert(cue_id.to_string(), instance);
            }
            ResolvedTarget::Groups(groups) => {
                for gid in groups {
                    guard
                        .manual_layer
                        .per_group
                        .entry(gid.clone())
                        .or_insert_with(ManualGroupState::new)
                        .active_cues
                        .insert(cue_id.to_string(), instance.clone());
                }
            }
        }
    }

    /// Latch a cue off.
    pub fn latch_cue_off(&self, cue_id: &str) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.manual_layer.global.active_cues.remove(cue_id);
        for gs in guard.manual_layer.per_group.values_mut() {
            gs.active_cues.remove(cue_id);
        }
    }

    /// Toggle a cue's latch state. Returns the new state (true = on).
    pub fn toggle_cue(&self, cue_id: &str, resolved_target: ResolvedTarget, z_index: i8) -> bool {
        let is_on = {
            let guard = self.inner.lock().expect("render engine poisoned");
            guard.manual_layer.global.active_cues.contains_key(cue_id)
                || guard
                    .manual_layer
                    .per_group
                    .values()
                    .any(|gs| gs.active_cues.contains_key(cue_id))
        };
        if is_on {
            self.latch_cue_off(cue_id);
            false
        } else {
            self.latch_cue_on(cue_id, resolved_target, z_index);
            true
        }
    }

    /// Start a flash (held momentary).
    pub fn flash_cue_on(&self, cue_id: &str, resolved_target: ResolvedTarget) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        let instance = CueInstance {
            cue_id: cue_id.to_string(),
            triggered_at: Instant::now(),
            resolved_target: resolved_target.clone(),
        };
        match &resolved_target {
            ResolvedTarget::All => {
                guard
                    .manual_layer
                    .global
                    .flash_cues
                    .insert(cue_id.to_string(), instance);
            }
            ResolvedTarget::Groups(groups) => {
                for gid in groups {
                    guard
                        .manual_layer
                        .per_group
                        .entry(gid.clone())
                        .or_insert_with(ManualGroupState::new)
                        .flash_cues
                        .insert(cue_id.to_string(), instance.clone());
                }
            }
        }
    }

    /// Clear all active and flash cues (blackout).
    pub fn clear_all_cues(&self) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.manual_layer.global.active_cues.clear();
        guard.manual_layer.global.flash_cues.clear();
        for gs in guard.manual_layer.per_group.values_mut() {
            gs.active_cues.clear();
            gs.flash_cues.clear();
        }
    }

    /// End a flash.
    pub fn flash_cue_off(&self, cue_id: &str) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.manual_layer.global.flash_cues.remove(cue_id);
        for gs in guard.manual_layer.per_group.values_mut() {
            gs.flash_cues.remove(cue_id);
        }
    }

    /// Record a tap timestamp for TapToggleHoldFlash.
    pub fn record_tap(&self, binding_id: &str) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard
            .manual_layer
            .tap_timestamps
            .insert(binding_id.to_string(), Instant::now());
    }

    /// Return elapsed ms since tap, removing the entry.
    pub fn consume_tap_elapsed_ms(&self, binding_id: &str) -> Option<u64> {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard
            .manual_layer
            .tap_timestamps
            .remove(binding_id)
            .map(|t| t.elapsed().as_millis() as u64)
    }

    /// Hold modifier pressed.
    pub fn modifier_on(&self, name: &str) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.manual_layer.held_modifiers.insert(name.to_string());
    }

    /// Hold modifier released.
    pub fn modifier_off(&self, name: &str) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.manual_layer.held_modifiers.remove(name);
    }

    /// Snapshot of held modifiers for UI display.
    pub fn get_manual_state_snapshot(&self) -> crate::models::midi::ControllerState {
        let guard = self.inner.lock().expect("render engine poisoned");
        let ml = &guard.manual_layer;

        let mut active_ids: Vec<String> = ml.global.active_cues.keys().cloned().collect();
        let mut flash_ids: Vec<String> = ml.global.flash_cues.keys().cloned().collect();
        for gs in ml.per_group.values() {
            for id in gs.active_cues.keys() {
                if !active_ids.contains(id) {
                    active_ids.push(id.clone());
                }
            }
            for id in gs.flash_cues.keys() {
                if !flash_ids.contains(id) {
                    flash_ids.push(id.clone());
                }
            }
        }

        let group_intensities = ml
            .per_group
            .iter()
            .map(|(gid, gs)| (gid.clone(), gs.intensity))
            .collect();

        crate::models::midi::ControllerState {
            active: ml.active,
            master_intensity: ml.master_intensity,
            active_cue_ids: active_ids,
            flash_cue_ids: flash_ids,
            held_modifiers: ml.held_modifiers.iter().cloned().collect(),
            group_intensities,
        }
    }

    /// Expose inner Arc so MidiManager can share state without cloning.
    pub fn inner_arc(&self) -> Arc<Mutex<RenderEngineInner>> {
        self.inner.clone()
    }

    /// Spawn the ~60fps render loop that emits universe-state-update + ArtNet.
    pub fn spawn_render_loop(&self, app_handle: AppHandle) {
        let state = self.inner.clone();
        tauri::async_runtime::spawn(async move {
            let mut last_had_output: bool = false;
            let mut frame_count: u64 = 0;
            let mut last_frame_instant = std::time::Instant::now();
            loop {
                let frame_dt = last_frame_instant.elapsed().as_secs_f32().min(0.1);
                last_frame_instant = std::time::Instant::now();
                let universe_state = {
                    let mut guard = match state.lock() {
                        Ok(g) => g,
                        Err(e) => {
                            log::error!("[RenderEngine] mutex recovered from poison");
                            e.into_inner()
                        }
                    };

                    // Identify blink takes highest priority
                    let u_state = if let Some(ref id) = guard.identify {
                        let elapsed = id.start.elapsed().as_secs_f32();
                        if elapsed >= IDENTIFY_DURATION {
                            guard.identify = None;
                            None
                        } else {
                            let dimmer = identify_dimmer(elapsed);
                            let mut primitives = HashMap::new();
                            // Emit for head indices 0–15 to cover multi-head fixtures
                            for head in 0..16 {
                                primitives.insert(
                                    format!("{}:{}", id.fixture_id, head),
                                    PrimitiveState {
                                        dimmer,
                                        color: [1.0, 1.0, 1.0],
                                        strobe: 0.0,
                                        position: [0.0, 0.0],
                                        speed: 0.0,
                                    },
                                );
                            }
                            Some(UniverseState { primitives })
                        }
                    } else if !guard.perform_deck_states.is_empty()
                        || guard.manual_layer.active
                        || guard.manual_layer.has_any_cues()
                    {
                        // Perform mode: blend deck layers + manual layer.
                        // Entered whenever real decks are present, output is enabled,
                        // OR any cue is active (so the visualizer always reflects live state).
                        Some(render_perform_mix(&mut guard, frame_dt))
                    } else if let Some(layer) = &guard.active_layer {
                        // Track editor mode: read time from host audio
                        if let Some(host) = app_handle.try_state::<HostAudioState>() {
                            let abs_time = host.render_time();
                            Some(render_frame_max(
                                layer,
                                (abs_time - frame_dt).max(0.0),
                                abs_time,
                            ))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    u_state
                };

                let has_output = universe_state.is_some();
                if has_output != last_had_output {
                    if has_output {
                        log::info!("[render] output RESUMED");
                    } else {
                        // Log exactly why we have no output
                        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        log::warn!(
                            "[render] output STOPPED — deck_states={}, active_layer={}, manual_active={}, manual_cues={}",
                            guard.perform_deck_states.len(),
                            guard.active_layer.is_some(),
                            guard.manual_layer.active,
                            guard.manual_layer.has_any_cues(),
                        );
                    }
                    last_had_output = has_output;
                }

                frame_count += 1;
                if frame_count % 300 == 0 {
                    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    log::debug!(
                        "[render] heartbeat — deck_states={}, perform_layers={}, active_layer={}, emitting={}",
                        guard.perform_deck_states.len(),
                        guard.perform_layers.len(),
                        guard.active_layer.is_some(),
                        has_output,
                    );
                }

                if let Some(u_state) = universe_state {
                    let _ = app_handle.emit(UNIVERSE_EVENT, &u_state);

                    if let Some(artnet) = app_handle.try_state::<crate::artnet::ArtNetManager>() {
                        artnet.broadcast(&u_state);
                    }
                }

                sleep(Duration::from_millis(16)).await; // ~60fps
            }
        });
    }
}

// ============================================================================
// Blend helpers (used both here and exported for compositor)
// ============================================================================

fn blend_primitive(base: &PrimitiveState, top: &PrimitiveState, mode: BlendMode) -> PrimitiveState {
    use crate::compositor::{blend_color, blend_values};

    let dimmer = blend_values(base.dimmer, top.dimmer, mode);
    let color_base = [
        base.color[0],
        base.color[1],
        base.color[2],
        base.dimmer, // use dimmer as alpha
    ];
    let color_top = [top.color[0], top.color[1], top.color[2], top.dimmer];
    let blended_color = blend_color(&color_base, &color_top, mode);
    let strobe = blend_values(base.strobe, top.strobe, mode);
    let speed = if top.speed > 0.5 {
        top.speed
    } else {
        base.speed
    };

    // Position: winner-takes-all (top wins when dimmer > 0)
    let position = if top.dimmer > 0.0 {
        top.position
    } else {
        base.position
    };

    PrimitiveState {
        dimmer: dimmer.clamp(0.0, 1.0),
        color: [
            blended_color.get(0).copied().unwrap_or(0.0).clamp(0.0, 1.0),
            blended_color.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0),
            blended_color.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0),
        ],
        strobe: strobe.clamp(0.0, 1.0),
        position,
        speed,
    }
}

/// Composite a scaled cue universe on top of the current universe.
fn composite_cue_onto_universe(
    base: &mut UniverseState,
    cue_universe: &UniverseState,
    mode: BlendMode,
) {
    for (key, top_prim) in &cue_universe.primitives {
        if let Some(base_prim) = base.primitives.get(key) {
            let blended = blend_primitive(base_prim, top_prim, mode);
            base.primitives.insert(key.clone(), blended);
        } else {
            // Primitive only in cue (new fixture); insert directly
            base.primitives.insert(key.clone(), top_prim.clone());
        }
    }
}

/// Filter a universe to only the primitives belonging to `target_group_fixtures`.
/// `target_group_fixtures` is a set of fixture_id strings; primitive keys are "fixture_id:head".
fn filter_universe_to_fixtures(
    universe: UniverseState,
    fixture_ids: &HashSet<&str>,
) -> UniverseState {
    let primitives = universe
        .primitives
        .into_iter()
        .filter(|(key, _)| {
            // "fixture_id:head_index" — match prefix up to ':'
            if let Some(colon) = key.find(':') {
                fixture_ids.contains(&key[..colon])
            } else {
                fixture_ids.contains(key.as_str())
            }
        })
        .collect();
    UniverseState { primitives }
}

/// Scale dimmer and strobe in a universe by a multiplier.
fn scale_universe_intensity(mut universe: UniverseState, scale: f32) -> UniverseState {
    for prim in universe.primitives.values_mut() {
        prim.dimmer = (prim.dimmer * scale).clamp(0.0, 1.0);
        prim.strobe = (prim.strobe * scale).clamp(0.0, 1.0);
    }
    universe
}

// ============================================================================
// Perform mix
// ============================================================================

/// A cue instance queued for compositing this frame (no compiled ref — resolved via deck blend).
struct ActiveCueEntry<'a> {
    cue_id: &'a str,
    resolved_target: &'a ResolvedTarget,
    intensity: f32,
}

/// Render each deck's layer at its current time and blend by volume.
/// Also composites the manual live layer on top when active.
fn render_perform_mix(guard: &mut RenderEngineInner, frame_dt: f32) -> UniverseState {
    // Build effective deck states: real decks + simulated deck when no real decks are up.
    let sim_time = guard.simulated_deck_start.elapsed().as_secs_f32() % SIM_DECK_DURATION;
    // Sim deck always contributes — it has no score layer in perform_layers
    // so score_mix ignores it, but its cue buffers must stay reachable for MIDI.
    let sim_vol: f32 = 1.0;
    let mut effective_states: Vec<PerformDeckInput> = guard.perform_deck_states.clone();
    if sim_vol > 0.0 {
        effective_states.push(PerformDeckInput {
            deck_id: SIM_DECK_ID,
            time: sim_time,
            volume: sim_vol,
        });
    }

    // Step 1: score base (weighted average by deck volume)
    let mut universe = score_mix(&guard.perform_layers, &effective_states, frame_dt);

    // Step 2: collect all active + flash cue instances
    let master = guard.manual_layer.master_intensity;
    let mut entries: Vec<ActiveCueEntry> = Vec::new();

    for (cue_id, instance) in guard
        .manual_layer
        .global
        .active_cues
        .iter()
        .chain(guard.manual_layer.global.flash_cues.iter())
    {
        entries.push(ActiveCueEntry {
            cue_id,
            resolved_target: &instance.resolved_target,
            intensity: master,
        });
    }

    for (_, gs) in &guard.manual_layer.per_group {
        let group_intensity = gs.intensity * master;
        for (cue_id, instance) in gs.active_cues.iter().chain(gs.flash_cues.iter()) {
            if !entries.iter().any(|e| e.cue_id == cue_id.as_str()) {
                entries.push(ActiveCueEntry {
                    cue_id,
                    resolved_target: &instance.resolved_target,
                    intensity: group_intensity,
                });
            }
        }
    }

    if entries.is_empty() {
        return universe;
    }

    // Step 4: collect per-deck layers for each active cue, sorted by z_index.
    // Uses channel-selective compositing (same logic as the track-editor compositor)
    // so partial-channel cues (apply_strobe, apply_dimmer, etc.) only affect the
    // channels they actually set — other channels pass through from the base.
    struct CueCompositeEntry<'a> {
        z_index: i8,
        blend_mode: BlendMode,
        resolved_target: &'a ResolvedTarget,
        intensity: f32,
        /// (layer, deck_time, deck_volume) for each deck that has this cue compiled
        deck_layers: Vec<(&'a LayerTimeSeries, f32, f32)>,
    }

    let group_fixture_map = &guard.group_fixture_map;
    let cue_buffers = &guard.cue_buffers;

    let mut cue_entries: Vec<CueCompositeEntry> = entries
        .iter()
        .filter_map(|e| {
            let mut deck_layers = Vec::new();
            let mut blend_mode = BlendMode::Replace;
            let mut z_index = 0i8;
            for ds in &effective_states {
                if ds.volume <= 0.0 {
                    continue;
                }
                if let Some(compiled) = cue_buffers.get(&(ds.deck_id, e.cue_id.to_string())) {
                    deck_layers.push((&compiled.layer, ds.time, ds.volume));
                    blend_mode = compiled.blend_mode;
                    z_index = compiled.z_index;
                }
            }
            if deck_layers.is_empty() {
                None
            } else {
                Some(CueCompositeEntry {
                    z_index,
                    blend_mode,
                    resolved_target: e.resolved_target,
                    intensity: e.intensity,
                    deck_layers,
                })
            }
        })
        .collect();

    if cue_entries.is_empty() {
        return universe;
    }

    // Step 5: sort by z_index ascending (Painter's Algorithm)
    cue_entries.sort_by_key(|e| e.z_index);

    // Step 6: composite each cue channel-by-channel
    for entry in &cue_entries {
        let allowed: Option<HashSet<&str>> = match entry.resolved_target {
            ResolvedTarget::All => None,
            ResolvedTarget::Groups(groups) => Some(
                groups
                    .iter()
                    .flat_map(|gid| {
                        group_fixture_map
                            .get(gid)
                            .map(|v| v.iter().map(|s| s.as_str()))
                            .into_iter()
                            .flatten()
                    })
                    .collect(),
            ),
        };

        let total_vol: f32 = entry.deck_layers.iter().map(|&(_, _, v)| v).sum();
        for &(layer, time, vol) in &entry.deck_layers {
            let weight = if total_vol > 0.0 {
                vol / total_vol
            } else {
                1.0
            };
            let effective_intensity = entry.intensity * weight;
            crate::engine::composite_layer_frame(
                &mut universe,
                layer,
                time,
                entry.blend_mode,
                effective_intensity,
                allowed.as_ref(),
            );
        }
    }

    // Apply per-group intensity as a post-composite dimming pass.
    // This lets CC faders act as group dimmers regardless of how cues target fixtures.
    for (group_id, gs) in &guard.manual_layer.per_group {
        if (gs.intensity - 1.0).abs() < 0.001 {
            continue; // full intensity — skip
        }
        let Some(fixture_ids) = guard.group_fixture_map.get(group_id) else {
            continue;
        };
        let scale = gs.intensity;
        for (key, prim) in &mut universe.primitives {
            let fixture_id = if let Some(c) = key.find(':') {
                &key[..c]
            } else {
                key.as_str()
            };
            if fixture_ids.iter().any(|fid| fid == fixture_id) {
                prim.dimmer = (prim.dimmer * scale).clamp(0.0, 1.0);
            }
        }
    }

    universe
}

/// Blend a cue's compiled output across all active decks weighted by volume.
/// Mirrors score_mix — ensures audio-reactive cues follow fader positions.
/// Returns (blend_mode, z_index, blended_universe) or None if no deck has this cue.
fn render_cue_blended(
    buffers: &HashMap<(u8, String), CompiledCue>,
    deck_states: &[PerformDeckInput],
    cue_id: &str,
    frame_dt: f32,
) -> Option<(BlendMode, i8, UniverseState)> {
    let mut frames: Vec<(UniverseState, f32)> = Vec::new();
    let mut blend_mode = BlendMode::Replace;
    let mut z_index: i8 = 0;

    for ds in deck_states {
        if ds.volume <= 0.0 {
            continue;
        }
        if let Some(compiled) = buffers.get(&(ds.deck_id, cue_id.to_string())) {
            let t_prev = (ds.time - frame_dt).max(0.0);
            frames.push((
                render_frame_max(&compiled.layer, t_prev, ds.time),
                ds.volume,
            ));
            blend_mode = compiled.blend_mode;
            z_index = compiled.z_index;
        }
    }

    if frames.is_empty() {
        return None;
    }

    if frames.len() == 1 {
        let (u, _) = frames.into_iter().next().unwrap();
        return Some((blend_mode, z_index, u));
    }

    // Weighted average across decks (mirrors score_mix)
    let total_volume: f32 = frames.iter().map(|(_, v)| *v).sum();
    if total_volume <= 0.0 {
        return None;
    }

    let mut all_keys = std::collections::HashSet::new();
    for (state, _) in &frames {
        all_keys.extend(state.primitives.keys().cloned());
    }

    let mut blended = HashMap::with_capacity(all_keys.len());
    for key in all_keys {
        let mut dimmer = 0.0f32;
        let mut color = [0.0f32; 3];
        let mut strobe = 0.0f32;
        let mut speed = 0.0f32;
        let mut best_position = [0.0f32; 2];
        let mut best_vol = -1.0f32;

        for (state, vol) in &frames {
            let w = vol / total_volume;
            if let Some(prim) = state.primitives.get(&key) {
                dimmer += prim.dimmer * w;
                color[0] += prim.color[0] * w;
                color[1] += prim.color[1] * w;
                color[2] += prim.color[2] * w;
                strobe += prim.strobe * w;
                speed += prim.speed * w;
                if *vol > best_vol {
                    best_vol = *vol;
                    best_position = prim.position;
                }
            }
        }

        blended.insert(
            key,
            PrimitiveState {
                dimmer: dimmer.clamp(0.0, 1.0),
                color,
                strobe: strobe.clamp(0.0, 1.0),
                position: best_position,
                speed: if speed > 0.5 { 1.0 } else { 0.0 },
            },
        );
    }

    Some((
        blend_mode,
        z_index,
        UniverseState {
            primitives: blended,
        },
    ))
}

/// Score-only blend: weighted average by deck volume.
fn score_mix(
    layers: &HashMap<u8, LayerTimeSeries>,
    deck_states: &[PerformDeckInput],
    frame_dt: f32,
) -> UniverseState {
    let mut frames: Vec<(UniverseState, f32)> = Vec::new();
    for ds in deck_states {
        if ds.volume <= 0.0 {
            continue;
        }
        if let Some(layer) = layers.get(&ds.deck_id) {
            let t_prev = (ds.time - frame_dt).max(0.0);
            frames.push((render_frame_max(layer, t_prev, ds.time), ds.volume));
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

    let total_volume: f32 = frames.iter().map(|(_, v)| *v).sum();
    if total_volume <= 0.0 {
        return UniverseState {
            primitives: HashMap::new(),
        };
    }

    let mut all_keys = std::collections::HashSet::new();
    for (state, _) in &frames {
        all_keys.extend(state.primitives.keys().cloned());
    }

    let mut blended = HashMap::with_capacity(all_keys.len());
    for key in all_keys {
        let mut dimmer = 0.0f32;
        let mut color = [0.0f32; 3];
        let mut strobe = 0.0f32;
        let mut speed = 0.0f32;

        let mut best_position = [0.0f32; 2];
        let mut best_vol = -1.0f32;

        for (state, vol) in &frames {
            let w = vol / total_volume;
            if let Some(prim) = state.primitives.get(&key) {
                dimmer += prim.dimmer * w;
                color[0] += prim.color[0] * w;
                color[1] += prim.color[1] * w;
                color[2] += prim.color[2] * w;
                strobe += prim.strobe * w;
                speed += prim.speed * w;

                if *vol > best_vol {
                    best_vol = *vol;
                    best_position = prim.position;
                }
            }
        }

        blended.insert(
            key,
            PrimitiveState {
                dimmer: dimmer.clamp(0.0, 1.0),
                color,
                strobe: strobe.clamp(0.0, 1.0),
                position: best_position,
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

/// Clear the active layer so the render loop emits nothing.
/// Called when navigating away from the track/pattern editor.
#[tauri::command]
pub fn render_clear_active_layer(render_engine: State<'_, RenderEngine>) {
    render_engine.set_active_layer(None);
}

/// Trigger a two-blink identify sequence for a fixture (visualizer + ArtNet).
#[tauri::command]
pub fn render_identify_fixture(render_engine: State<'_, RenderEngine>, fixture_id: String) {
    render_engine.identify_fixture(fixture_id);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::node_graph::{PrimitiveTimeSeries, Series, SeriesSample};

    /// Build a constant-value LayerTimeSeries for a single primitive.
    fn make_layer(primitive_id: &str, dimmer: f32, color: [f32; 3]) -> LayerTimeSeries {
        LayerTimeSeries {
            primitives: vec![PrimitiveTimeSeries {
                primitive_id: primitive_id.to_string(),
                dimmer: Some(Series {
                    dim: 1,
                    labels: None,
                    samples: vec![
                        SeriesSample {
                            time: 0.0,
                            values: vec![dimmer],
                            label: None,
                        },
                        SeriesSample {
                            time: 9999.0,
                            values: vec![dimmer],
                            label: None,
                        },
                    ],
                }),
                color: Some(Series {
                    dim: 3,
                    labels: None,
                    samples: vec![
                        SeriesSample {
                            time: 0.0,
                            values: vec![color[0], color[1], color[2]],
                            label: None,
                        },
                        SeriesSample {
                            time: 9999.0,
                            values: vec![color[0], color[1], color[2]],
                            label: None,
                        },
                    ],
                }),
                position: None,
                strobe: None,
                speed: None,
            }],
        }
    }

    fn make_compiled(layer: LayerTimeSeries, z_index: i8, blend_mode: BlendMode) -> CompiledCue {
        CompiledCue {
            layer,
            execution_mode: CompiledCueMode::Loop,
            z_index,
            blend_mode,
        }
    }

    // --- State machine tests (no rendering required) ---

    #[test]
    fn latch_cue_on_and_off() {
        let engine = RenderEngine::default();
        engine.latch_cue_on("cue1", ResolvedTarget::All, 1);
        assert!(engine
            .get_manual_state_snapshot()
            .active_cue_ids
            .contains(&"cue1".to_string()));
        engine.latch_cue_off("cue1");
        assert!(engine.get_manual_state_snapshot().active_cue_ids.is_empty());
    }

    #[test]
    fn toggle_cue_on_off_on() {
        let engine = RenderEngine::default();
        assert!(engine.toggle_cue("cue1", ResolvedTarget::All, 1));
        assert!(engine
            .get_manual_state_snapshot()
            .active_cue_ids
            .contains(&"cue1".to_string()));
        assert!(!engine.toggle_cue("cue1", ResolvedTarget::All, 1));
        assert!(engine.get_manual_state_snapshot().active_cue_ids.is_empty());
    }

    #[test]
    fn flash_cue_on_off() {
        let engine = RenderEngine::default();
        engine.flash_cue_on("cue1", ResolvedTarget::All);
        assert!(engine
            .get_manual_state_snapshot()
            .flash_cue_ids
            .contains(&"cue1".to_string()));
        engine.flash_cue_off("cue1");
        assert!(engine.get_manual_state_snapshot().flash_cue_ids.is_empty());
    }

    #[test]
    fn clear_all_cues_removes_active_and_flash() {
        let engine = RenderEngine::default();
        engine.latch_cue_on("cue1", ResolvedTarget::All, 1);
        engine.flash_cue_on("cue2", ResolvedTarget::All);
        engine.clear_all_cues();
        let state = engine.get_manual_state_snapshot();
        assert!(state.active_cue_ids.is_empty());
        assert!(state.flash_cue_ids.is_empty());
    }

    #[test]
    fn modifier_tracking() {
        let engine = RenderEngine::default();
        engine.modifier_on("A");
        engine.modifier_on("B");
        let state = engine.get_manual_state_snapshot();
        assert!(state.held_modifiers.contains(&"A".to_string()));
        assert!(state.held_modifiers.contains(&"B".to_string()));
        engine.modifier_off("A");
        let state = engine.get_manual_state_snapshot();
        assert!(!state.held_modifiers.contains(&"A".to_string()));
        assert!(state.held_modifiers.contains(&"B".to_string()));
    }

    #[test]
    fn radio_button_exclusivity_same_z_index() {
        let engine = RenderEngine::default();
        // Insert two cue buffers at z_index=1 so exclusivity check has something to scan
        let layer = make_layer("fix:0", 1.0, [1.0, 0.0, 0.0]);
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cueA",
            make_compiled(layer.clone(), 1, BlendMode::Replace),
        );
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cueB",
            make_compiled(layer.clone(), 1, BlendMode::Replace),
        );

        engine.latch_cue_on("cueA", ResolvedTarget::All, 1);
        assert!(engine
            .get_manual_state_snapshot()
            .active_cue_ids
            .contains(&"cueA".to_string()));

        // Latching cueB at same z_index must evict cueA
        engine.latch_cue_on("cueB", ResolvedTarget::All, 1);
        let state = engine.get_manual_state_snapshot();
        assert!(
            state.active_cue_ids.contains(&"cueB".to_string()),
            "cueB should be active"
        );
        assert!(
            !state.active_cue_ids.contains(&"cueA".to_string()),
            "cueA should be evicted"
        );
    }

    #[test]
    fn different_z_index_cues_coexist() {
        let engine = RenderEngine::default();
        let layer = make_layer("fix:0", 1.0, [1.0, 0.0, 0.0]);
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cueA",
            make_compiled(layer.clone(), 0, BlendMode::Replace),
        );
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cueB",
            make_compiled(layer.clone(), 1, BlendMode::Replace),
        );

        engine.latch_cue_on("cueA", ResolvedTarget::All, 0);
        engine.latch_cue_on("cueB", ResolvedTarget::All, 1);
        let state = engine.get_manual_state_snapshot();
        assert!(state.active_cue_ids.contains(&"cueA".to_string()));
        assert!(state.active_cue_ids.contains(&"cueB".to_string()));
    }

    #[test]
    fn tap_timestamp_elapsed() {
        let engine = RenderEngine::default();
        engine.record_tap("bind1");
        // Immediately consume — elapsed should be very small (< 100ms)
        let elapsed = engine
            .consume_tap_elapsed_ms("bind1")
            .expect("tap should exist");
        assert!(elapsed < 100, "elapsed={}ms, should be near zero", elapsed);
    }

    #[test]
    fn tap_timestamp_consumed_once() {
        let engine = RenderEngine::default();
        engine.record_tap("bind1");
        assert!(engine.consume_tap_elapsed_ms("bind1").is_some());
        assert!(
            engine.consume_tap_elapsed_ms("bind1").is_none(),
            "second consume should return None"
        );
    }

    #[test]
    fn controller_state_snapshot_active_false_by_default() {
        let engine = RenderEngine::default();
        assert!(!engine.get_manual_state_snapshot().active);
    }

    #[test]
    fn set_manual_active() {
        let engine = RenderEngine::default();
        engine.set_manual_active(true);
        assert!(engine.get_manual_state_snapshot().active);
        engine.set_manual_active(false);
        assert!(!engine.get_manual_state_snapshot().active);
    }

    #[test]
    fn master_intensity_clamped() {
        let engine = RenderEngine::default();
        engine.set_group_intensity(None, 1.5);
        assert_eq!(engine.get_manual_state_snapshot().master_intensity, 1.0);
        engine.set_group_intensity(None, -0.5);
        assert_eq!(engine.get_manual_state_snapshot().master_intensity, 0.0);
    }

    // --- Rendering / blending tests ---

    #[test]
    fn render_cue_blended_no_decks_returns_none() {
        let buffers = HashMap::new();
        let states: Vec<PerformDeckInput> = vec![];
        assert!(render_cue_blended(&buffers, &states, "cue1", 0.016).is_none());
    }

    #[test]
    fn render_cue_blended_missing_cue_returns_none() {
        let mut buffers = HashMap::new();
        let layer = make_layer("fix:0", 1.0, [1.0, 0.0, 0.0]);
        buffers.insert(
            (1u8, "other_cue".to_string()),
            make_compiled(layer, 0, BlendMode::Replace),
        );
        let states = vec![PerformDeckInput {
            deck_id: 1,
            time: 0.0,
            volume: 1.0,
        }];
        assert!(render_cue_blended(&buffers, &states, "cue1", 0.016).is_none());
    }

    #[test]
    fn render_cue_blended_single_deck_returns_frame() {
        let mut buffers = HashMap::new();
        let layer = make_layer("fix:0", 0.8, [1.0, 0.0, 0.0]);
        buffers.insert(
            (1u8, "cue1".to_string()),
            make_compiled(layer, 2, BlendMode::Add),
        );
        let states = vec![PerformDeckInput {
            deck_id: 1,
            time: 0.0,
            volume: 1.0,
        }];

        let result = render_cue_blended(&buffers, &states, "cue1", 0.016);
        assert!(result.is_some());
        let (blend_mode, z_index, universe) = result.unwrap();
        assert!(matches!(blend_mode, BlendMode::Add));
        assert_eq!(z_index, 2);
        let prim = universe
            .primitives
            .get("fix:0")
            .expect("primitive should exist");
        assert!((prim.dimmer - 0.8).abs() < 0.01, "dimmer={}", prim.dimmer);
    }

    #[test]
    fn render_cue_blended_two_decks_weighted_average() {
        // Deck 1 at volume 0.5 → dimmer=1.0; Deck 2 at volume 0.5 → dimmer=0.0
        // Expected blended dimmer = 0.5
        let mut buffers = HashMap::new();
        let layer_bright = make_layer("fix:0", 1.0, [1.0, 0.0, 0.0]);
        let layer_dark = make_layer("fix:0", 0.0, [0.0, 0.0, 0.0]);
        buffers.insert(
            (1u8, "cue1".to_string()),
            make_compiled(layer_bright, 0, BlendMode::Replace),
        );
        buffers.insert(
            (2u8, "cue1".to_string()),
            make_compiled(layer_dark, 0, BlendMode::Replace),
        );

        let states = vec![
            PerformDeckInput {
                deck_id: 1,
                time: 0.0,
                volume: 0.5,
            },
            PerformDeckInput {
                deck_id: 2,
                time: 0.0,
                volume: 0.5,
            },
        ];
        let (_, _, universe) = render_cue_blended(&buffers, &states, "cue1", 0.016).unwrap();
        let prim = universe.primitives.get("fix:0").unwrap();
        assert!(
            (prim.dimmer - 0.5).abs() < 0.01,
            "expected 0.5, got {}",
            prim.dimmer
        );
    }

    #[test]
    fn render_cue_blended_unequal_faders() {
        // Deck 1 volume=0.8 dimmer=1.0, Deck 2 volume=0.2 dimmer=0.0 → expected ~0.8
        let mut buffers = HashMap::new();
        buffers.insert(
            (1u8, "cue1".to_string()),
            make_compiled(
                make_layer("fix:0", 1.0, [1.0, 0.0, 0.0]),
                0,
                BlendMode::Replace,
            ),
        );
        buffers.insert(
            (2u8, "cue1".to_string()),
            make_compiled(
                make_layer("fix:0", 0.0, [0.0, 0.0, 0.0]),
                0,
                BlendMode::Replace,
            ),
        );

        let states = vec![
            PerformDeckInput {
                deck_id: 1,
                time: 0.0,
                volume: 0.8,
            },
            PerformDeckInput {
                deck_id: 2,
                time: 0.0,
                volume: 0.2,
            },
        ];
        let (_, _, universe) = render_cue_blended(&buffers, &states, "cue1", 0.016).unwrap();
        let prim = universe.primitives.get("fix:0").unwrap();
        assert!(
            (prim.dimmer - 0.8).abs() < 0.01,
            "expected 0.8, got {}",
            prim.dimmer
        );
    }

    #[test]
    fn render_cue_blended_skips_zero_volume_deck() {
        let mut buffers = HashMap::new();
        buffers.insert(
            (1u8, "cue1".to_string()),
            make_compiled(
                make_layer("fix:0", 1.0, [1.0, 0.0, 0.0]),
                0,
                BlendMode::Replace,
            ),
        );
        buffers.insert(
            (2u8, "cue1".to_string()),
            make_compiled(
                make_layer("fix:0", 0.5, [0.5, 0.5, 0.5]),
                0,
                BlendMode::Replace,
            ),
        );

        let states = vec![
            PerformDeckInput {
                deck_id: 1,
                time: 0.0,
                volume: 1.0,
            },
            PerformDeckInput {
                deck_id: 2,
                time: 0.0,
                volume: 0.0,
            }, // faded out
        ];
        let (_, _, universe) = render_cue_blended(&buffers, &states, "cue1", 0.016).unwrap();
        let prim = universe.primitives.get("fix:0").unwrap();
        // Only deck 1 contributes
        assert!(
            (prim.dimmer - 1.0).abs() < 0.01,
            "expected 1.0, got {}",
            prim.dimmer
        );
    }

    #[test]
    fn simulated_deck_buffers_used_when_no_real_decks() {
        let engine = RenderEngine::default();
        let layer = make_layer("fix:0", 1.0, [1.0, 1.0, 1.0]);
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cue1",
            make_compiled(layer, 0, BlendMode::Replace),
        );
        engine.set_manual_active(true);
        engine.latch_cue_on("cue1", ResolvedTarget::All, 0);

        // Manually call render_perform_mix via the inner lock
        let result = {
            let mut guard = engine.inner.lock().unwrap();
            assert!(guard.perform_deck_states.is_empty(), "no real decks");
            render_perform_mix(&mut guard, 0.016)
        };

        let prim = result.primitives.get("fix:0");
        assert!(
            prim.is_some(),
            "simulated deck should have driven cue output"
        );
        assert!((prim.unwrap().dimmer - 1.0).abs() < 0.01);
    }

    #[test]
    fn simulated_deck_cues_active_alongside_real_deck() {
        let engine = RenderEngine::default();
        // Simulated deck has bright cue, real deck has nothing compiled for this cue
        let layer = make_layer("fix:0", 1.0, [1.0, 1.0, 1.0]);
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cue1",
            make_compiled(layer, 0, BlendMode::Replace),
        );
        engine.set_manual_active(true);
        engine.latch_cue_on("cue1", ResolvedTarget::All, 0);

        {
            let mut guard = engine.inner.lock().unwrap();
            // Real deck present (even with no layer for this cue)
            guard.perform_deck_states = vec![PerformDeckInput {
                deck_id: 1,
                time: 0.0,
                volume: 1.0,
            }];
        }

        let result = {
            let mut guard = engine.inner.lock().unwrap();
            render_perform_mix(&mut guard, 0.016)
        };

        // Sim deck always contributes cues even when real decks are present
        let prim = result.primitives.get("fix:0");
        assert!(
            prim.is_some(),
            "simulated deck cues should be active alongside real decks"
        );
        assert!((prim.unwrap().dimmer - 1.0).abs() < 0.01);
    }

    #[test]
    fn group_target_filtering() {
        let engine = RenderEngine::default();
        // Two primitives: front and back
        let layer = LayerTimeSeries {
            primitives: vec![
                PrimitiveTimeSeries {
                    primitive_id: "front_fix:0".to_string(),
                    dimmer: Some(Series {
                        dim: 1,
                        labels: None,
                        samples: vec![
                            SeriesSample {
                                time: 0.0,
                                values: vec![1.0],
                                label: None,
                            },
                            SeriesSample {
                                time: 9999.0,
                                values: vec![1.0],
                                label: None,
                            },
                        ],
                    }),
                    color: None,
                    position: None,
                    strobe: None,
                    speed: None,
                },
                PrimitiveTimeSeries {
                    primitive_id: "back_fix:0".to_string(),
                    dimmer: Some(Series {
                        dim: 1,
                        labels: None,
                        samples: vec![
                            SeriesSample {
                                time: 0.0,
                                values: vec![1.0],
                                label: None,
                            },
                            SeriesSample {
                                time: 9999.0,
                                values: vec![1.0],
                                label: None,
                            },
                        ],
                    }),
                    color: None,
                    position: None,
                    strobe: None,
                    speed: None,
                },
            ],
        };

        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cue1",
            make_compiled(layer, 0, BlendMode::Replace),
        );
        engine.set_group_fixture_map({
            let mut m = HashMap::new();
            m.insert("front".to_string(), vec!["front_fix".to_string()]);
            m
        });
        engine.set_manual_active(true);
        engine.latch_cue_on("cue1", ResolvedTarget::Groups(vec!["front".to_string()]), 0);

        let result = {
            let mut guard = engine.inner.lock().unwrap();
            render_perform_mix(&mut guard, 0.016)
        };

        assert!(
            result.primitives.contains_key("front_fix:0"),
            "front fixture should be present"
        );
        assert!(
            !result.primitives.contains_key("back_fix:0"),
            "back fixture should be filtered out"
        );
    }

    #[test]
    fn intensity_scaling_applied() {
        let engine = RenderEngine::default();
        let layer = make_layer("fix:0", 1.0, [1.0, 1.0, 1.0]);
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cue1",
            make_compiled(layer, 0, BlendMode::Replace),
        );
        engine.set_manual_active(true);
        engine.set_group_intensity(None, 0.5); // master at 50%
        engine.latch_cue_on("cue1", ResolvedTarget::All, 0);

        let result = {
            let mut guard = engine.inner.lock().unwrap();
            render_perform_mix(&mut guard, 0.016)
        };

        let prim = result.primitives.get("fix:0").unwrap();
        assert!(
            (prim.dimmer - 0.5).abs() < 0.01,
            "expected 0.5 after scaling, got {}",
            prim.dimmer
        );
    }

    #[test]
    fn manual_layer_cues_always_render_for_visualizer() {
        // Cues render to visualizer even when manual_layer.active (output) is false.
        // The `active` flag only gates ArtNet, not visualizer output.
        let engine = RenderEngine::default();
        let layer = make_layer("fix:0", 1.0, [1.0, 0.0, 0.0]);
        engine.set_cue_buffer(
            SIM_DECK_ID,
            "cue1",
            make_compiled(layer, 0, BlendMode::Replace),
        );
        // manual layer NOT active (output off), but cue is latched
        engine.latch_cue_on("cue1", ResolvedTarget::All, 0);

        let result = {
            let mut guard = engine.inner.lock().unwrap();
            render_perform_mix(&mut guard, 0.016)
        };

        // Cue should still render to the visualizer
        assert!(
            !result.primitives.is_empty(),
            "cue should render even when output is off"
        );
        let prim = result
            .primitives
            .get("fix:0")
            .expect("fix:0 should be present");
        assert!((prim.dimmer - 1.0).abs() < 0.01);
    }

    #[test]
    fn filter_universe_to_fixtures_prefix_match() {
        let mut primitives = HashMap::new();
        primitives.insert(
            "fix_a:0".to_string(),
            PrimitiveState {
                dimmer: 1.0,
                color: [1.0, 0.0, 0.0],
                strobe: 0.0,
                position: [0.0, 0.0],
                speed: 0.0,
            },
        );
        primitives.insert(
            "fix_a:1".to_string(),
            PrimitiveState {
                dimmer: 0.5,
                color: [0.0, 1.0, 0.0],
                strobe: 0.0,
                position: [0.0, 0.0],
                speed: 0.0,
            },
        );
        primitives.insert(
            "fix_b:0".to_string(),
            PrimitiveState {
                dimmer: 0.8,
                color: [0.0, 0.0, 1.0],
                strobe: 0.0,
                position: [0.0, 0.0],
                speed: 0.0,
            },
        );
        let universe = UniverseState { primitives };

        let allowed: HashSet<&str> = ["fix_a"].iter().copied().collect();
        let filtered = filter_universe_to_fixtures(universe, &allowed);

        assert!(filtered.primitives.contains_key("fix_a:0"));
        assert!(filtered.primitives.contains_key("fix_a:1"));
        assert!(!filtered.primitives.contains_key("fix_b:0"));
    }
}
