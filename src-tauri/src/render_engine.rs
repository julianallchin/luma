//! Render Engine
//!
//! Owns all rendering state (layers, universe generation, ArtNet output).
//! Decoupled from audio playback — reads time from HostAudioState only in
//! edit mode. In perform mode it renders per-deck layers and blends by volume.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::sleep;

use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::eval::composite::composite_frame;
use crate::eval::{eval, Arena, Plan, Scene, Scope};
use crate::host_audio::HostAudioState;
use crate::models::node_graph::BlendMode;
use crate::models::universe::{PrimitiveState, UniverseState};

/// Sample a [`Scene`] at a single absolute time → one [`UniverseState`]. The
/// realtime collapse of the unified `render` API (the render loop's hot path).
#[inline]
fn sample_scene(scene: &Scene, t: f32, scratch: &mut Arena) -> UniverseState {
    scene
        .render(&[t], Scope::Composite, scratch)
        .pop()
        .unwrap_or_default()
}

/// Sample a single compiled cue [`Plan`] at one time → one [`UniverseState`].
#[inline]
fn sample_plan(plan: &Plan, t: f32, scratch: &mut Arena) -> UniverseState {
    eval(plan, &[t], scratch).pop().unwrap_or_default()
}

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

/// A compiled cue ready for evaluation in the render loop. The cue's pattern is
/// compiled to an eval [`Plan`] and sampled at the deck time each frame (seek-
/// safe — any time is a valid first frame, so loop/track-time are just different
/// `t` arguments, no precomputed buffer).
#[derive(Clone)]
pub struct CompiledCue {
    pub plan: Plan,
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

/// Blink-twice identify sequence for one or more targets. A target is a
/// member key: `"{fixture_id}"` (whole fixture) or `"{fixture_id}:{head}"`
/// (single head).
struct IdentifyState {
    targets: Vec<String>,
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
    /// Active scene for track editor / pattern editor (composited per frame).
    active_scene: Option<Scene>,
    /// Per-deck scenes for perform mode (the track's full composite per deck).
    perform_layers: HashMap<u8, Scene>,
    /// Per-deck time + volume from frontend each frame
    perform_deck_states: Vec<PerformDeckInput>,
    /// Fixture identify blink (highest priority)
    identify: Option<IdentifyState>,

    // --- Live controller layer ---
    /// Compiled cue buffers. Key = (deck_id, cue_id).
    pub cue_buffers: HashMap<(u8, String), CompiledCue>,
    /// Live LD state (modified by MIDI callback thread)
    pub manual_layer: ManualLayerState,
    /// group_id → [fixture_id, ...]. Built at cue-compile time. Used for target filtering.
    pub group_fixture_map: HashMap<String, Vec<String>>,
    /// Wall-clock start for the always-running simulated deck (deck_id=99).
    simulated_deck_start: Instant,
    /// Reusable eval scratch arena, held across frames so the hot path stays warm.
    scratch: Arena,
}

impl Default for RenderEngine {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RenderEngineInner {
                active_scene: None,
                perform_layers: HashMap::new(),
                perform_deck_states: Vec::new(),
                identify: None,
                cue_buffers: HashMap::new(),
                manual_layer: ManualLayerState::default(),
                group_fixture_map: HashMap::new(),
                simulated_deck_start: Instant::now(),
                scratch: Arena::default(),
            })),
        }
    }
}

impl RenderEngine {
    pub fn reset_for_identity_switch(&self) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.active_scene = None;
        guard.perform_layers.clear();
        guard.perform_deck_states.clear();
        guard.identify = None;
        guard.cue_buffers.clear();
        guard.manual_layer = ManualLayerState::default();
        guard.group_fixture_map.clear();
        guard.simulated_deck_start = Instant::now();
        guard.scratch = Arena::default();
    }

    pub fn set_active_scene(&self, scene: Option<Scene>) {
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.active_scene = scene;
    }

    pub fn set_perform_deck_states(&self, states: Vec<PerformDeckInput>) {
        log::debug!("[render] set_perform_deck_states: {} decks", states.len());
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.perform_deck_states = states;
    }

    /// Move the current active_scene into a perform deck slot.
    /// Called after compositing a track to redirect the result to a specific deck.
    pub fn promote_active_scene_to_deck(&self, deck_id: u8) {
        log::info!("[render] promoting active_scene to deck {deck_id}");
        let mut guard = self.inner.lock().expect("render engine poisoned");
        if let Some(scene) = guard.active_scene.take() {
            guard.perform_layers.insert(deck_id, scene);
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

    /// Targets are member keys: `"fid"` (whole fixture) or `"fid:N"` (one head).
    pub fn identify_targets(&self, targets: Vec<String>) {
        if targets.is_empty() {
            return;
        }
        let mut guard = self.inner.lock().expect("render engine poisoned");
        guard.identify = Some(IdentifyState {
            targets,
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
    pub(crate) fn inner_arc(&self) -> Arc<Mutex<RenderEngineInner>> {
        self.inner.clone()
    }

    /// Spawn the ~60fps render loop that emits universe-state-update + ArtNet.
    pub fn spawn_render_loop(&self, app_handle: AppHandle) {
        let state = self.inner.clone();
        tauri::async_runtime::spawn(async move {
            let mut last_had_output: bool = false;
            // ArtNet/DMX is wire-limited to ~44Hz; the visualizer feed runs much
            // faster (below) so scrubbing reflects the exact value at the playhead.
            let mut last_artnet = Instant::now();
            const ARTNET_INTERVAL: Duration = Duration::from_millis(23); // ~44Hz
            loop {
                let mut universe_state = {
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
                            let blink = PrimitiveState {
                                dimmer,
                                color: [1.0, 1.0, 1.0],
                                strobe: 0.0,
                                position: [0.0, 0.0],
                                speed: 0.0,
                            };
                            let mut primitives = HashMap::new();
                            for target in &id.targets {
                                if target.contains(':') {
                                    // Single head: blink exactly that primitive.
                                    primitives.insert(target.clone(), blink.clone());
                                } else {
                                    // Whole fixture: emit head indices 0–15 to
                                    // cover multi-head fixtures.
                                    for head in 0..16 {
                                        primitives
                                            .insert(format!("{}:{}", target, head), blink.clone());
                                    }
                                }
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
                        Some(render_perform_mix(&mut guard))
                    } else if guard.active_scene.is_some() {
                        // Track editor mode: read time from host audio, composite the
                        // active scene at that frame.
                        if let Some(host) = app_handle.try_state::<HostAudioState>() {
                            let abs_time = host.render_time();
                            let inner = &mut *guard;
                            inner
                                .active_scene
                                .as_ref()
                                .map(|s| sample_scene(s, abs_time, &mut inner.scratch))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    u_state
                };

                let has_output = universe_state.is_some();

                // Falling edge: synthesize one final all-dark frame so DMX
                // fixtures actually receive the blackout instead of latching
                // on the last lit frame indefinitely.
                if last_had_output && !has_output {
                    universe_state = Some(UniverseState {
                        primitives: HashMap::new(),
                    });
                }

                if has_output != last_had_output {
                    if has_output {
                        log::info!("[render] output RESUMED");
                    } else {
                        // Log exactly why we have no output
                        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        log::warn!(
                            "[render] output STOPPED — sending final blackout frame, deck_states={}, active_scene={}, manual_active={}, manual_cues={}",
                            guard.perform_deck_states.len(),
                            guard.active_scene.is_some(),
                            guard.manual_layer.active,
                            guard.manual_layer.has_any_cues(),
                        );
                    }
                    last_had_output = has_output;
                }

                if let Some(u_state) = universe_state {
                    // Visualizer feed: every tick (~240Hz) so the rig reflects the
                    // exact value at the current playhead, not a stale 60Hz sample.
                    let _ = app_handle.emit(UNIVERSE_EVENT, &u_state);

                    // DMX: only at the wire rate (~44Hz), regardless of tick rate.
                    if last_artnet.elapsed() >= ARTNET_INTERVAL {
                        if let Some(artnet) =
                            app_handle.try_state::<std::sync::Arc<crate::artnet::ArtNetManager>>()
                        {
                            artnet.broadcast(&u_state);
                        }
                        last_artnet = Instant::now();
                    }
                }

                // Fast visualizer tick (~240Hz). Eval is ~0.2ms, so this is cheap;
                // it makes scrubbing land on the exact playhead value. DMX is gated
                // to 44Hz above, so the wire isn't oversaturated.
                sleep(Duration::from_millis(4)).await;
            }
        });
    }
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
fn render_perform_mix(guard: &mut RenderEngineInner) -> UniverseState {
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
    let mut universe = score_mix(&guard.perform_layers, &effective_states, &mut guard.scratch);

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
        /// (cue plan, deck_time, deck_volume) for each deck that has this cue compiled
        deck_plans: Vec<(&'a Plan, f32, f32)>,
    }

    let group_fixture_map = &guard.group_fixture_map;
    let cue_buffers = &guard.cue_buffers;

    let mut cue_entries: Vec<CueCompositeEntry> = entries
        .iter()
        .filter_map(|e| {
            let mut deck_plans = Vec::new();
            let mut blend_mode = BlendMode::Replace;
            let mut z_index = 0i8;
            for ds in &effective_states {
                if ds.volume <= 0.0 {
                    continue;
                }
                if let Some(compiled) = cue_buffers.get(&(ds.deck_id, e.cue_id.to_string())) {
                    deck_plans.push((&compiled.plan, ds.time, ds.volume));
                    blend_mode = compiled.blend_mode;
                    z_index = compiled.z_index;
                }
            }
            if deck_plans.is_empty() {
                None
            } else {
                Some(CueCompositeEntry {
                    z_index,
                    blend_mode,
                    resolved_target: e.resolved_target,
                    intensity: e.intensity,
                    deck_plans,
                })
            }
        })
        .collect();

    if cue_entries.is_empty() {
        return universe;
    }

    // Step 5: sort by z_index ascending (Painter's Algorithm)
    cue_entries.sort_by_key(|e| e.z_index);

    // Step 6: eval + composite each cue (channel-selective via the plan's
    // OutputBinding set-mask), weighted across the decks that hold it.
    let scratch = &mut guard.scratch;
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

        let total_vol: f32 = entry.deck_plans.iter().map(|&(_, _, v)| v).sum();
        for &(plan, time, vol) in &entry.deck_plans {
            let weight = if total_vol > 0.0 {
                vol / total_vol
            } else {
                1.0
            };
            let effective_intensity = entry.intensity * weight;
            let frame = sample_plan(plan, time, scratch);
            composite_frame(
                &mut universe,
                &frame,
                &plan.outputs,
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
            // Members are either whole fixtures ("fid") or single heads ("fid:N").
            if fixture_ids
                .iter()
                .any(|m| m == fixture_id || m == key.as_str())
            {
                prim.dimmer = (prim.dimmer * scale).clamp(0.0, 1.0);
            }
        }
    }

    universe
}

/// Score-only blend: evaluate each deck's scene at its time, weighted-average by
/// deck volume. The per-deck composite is the unified `Scene::render`.
fn score_mix(
    layers: &HashMap<u8, Scene>,
    deck_states: &[PerformDeckInput],
    scratch: &mut Arena,
) -> UniverseState {
    let mut frames: Vec<(UniverseState, f32)> = Vec::new();
    for ds in deck_states {
        if ds.volume <= 0.0 {
            continue;
        }
        if let Some(scene) = layers.get(&ds.deck_id) {
            frames.push((sample_scene(scene, ds.time, scratch), ds.volume));
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

/// Resolve caller-supplied primitive targets through their fixture rows, then
/// retain one admitted venue snapshot until the identify effect is installed.
/// A mixed-venue or unknown target fails as one opaque not-found result.
pub(crate) async fn authorize_identify_targets<'a>(
    pool: &'a sqlx::SqlitePool,
    targets: &[String],
) -> Result<VenueAccess<'a, Read>, String> {
    let fixture_ids = targets
        .iter()
        .map(|target| identify_fixture_id(target))
        .collect::<Result<Vec<_>, _>>()?;
    let first = fixture_ids
        .first()
        .ok_or_else(|| "Identify requires at least one fixture target".to_string())?;
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Fixture(first)).await?;
    let venue_id = access.venue_id().to_owned();

    for fixture_id in fixture_ids.into_iter().skip(1) {
        let belongs: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM fixtures WHERE id = ? AND venue_id = ?")
                .bind(fixture_id)
                .bind(&venue_id)
                .fetch_optional(access.connection())
                .await
                .map_err(|error| format!("Failed to authorize identify target: {error}"))?;
        if belongs.is_none() {
            return Err("Venue resource not found".into());
        }
    }
    Ok(access)
}

fn identify_fixture_id(target: &str) -> Result<&str, String> {
    let (fixture_id, head) = match target.split_once(':') {
        Some((fixture_id, head)) => (fixture_id, Some(head)),
        None => (target, None),
    };
    if fixture_id.is_empty()
        || head.is_some_and(|head| head.is_empty() || head.parse::<usize>().is_err())
    {
        return Err("Invalid identify target".into());
    }
    Ok(fixture_id)
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::{authorize_identify_targets, identify_fixture_id};

    async fn identify_test_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("render-identify.db"))
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO venues (id, uid, name) VALUES
                ('venue-a', 'alice', 'Venue A'),
                ('venue-b', 'alice', 'Venue B');
             INSERT INTO fixtures
                (id, uid, venue_id, address, num_channels, manufacturer, model, mode_name, fixture_path)
             VALUES
                ('fixture-a', 'alice', 'venue-a', 1, 1, 'Test', 'A', 'Default', 'a.json'),
                ('fixture-a2', 'alice', 'venue-a', 2, 1, 'Test', 'A2', 'Default', 'a2.json'),
                ('fixture-b', 'alice', 'venue-b', 3, 1, 'Test', 'B', 'Default', 'b.json')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (directory, pool)
    }

    #[test]
    fn identify_target_parser_accepts_only_fixture_and_numeric_head_forms() {
        assert_eq!(identify_fixture_id("fixture-a").unwrap(), "fixture-a");
        assert_eq!(identify_fixture_id("fixture-a:0").unwrap(), "fixture-a");
        assert_eq!(identify_fixture_id("fixture-a:42").unwrap(), "fixture-a");

        for invalid in ["", ":0", "fixture-a:", "fixture-a:head", "fixture-a:0:1"] {
            assert!(
                identify_fixture_id(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[tokio::test]
    async fn identify_authorization_requires_one_admitted_venue_for_every_target() {
        let (_directory, pool) = identify_test_pool().await;

        let access =
            authorize_identify_targets(&pool, &["fixture-a:0".into(), "fixture-a2".into()])
                .await
                .unwrap();
        assert_eq!(access.venue_id(), "venue-a");
        drop(access);

        let mixed = authorize_identify_targets(&pool, &["fixture-a".into(), "fixture-b".into()])
            .await
            .err()
            .unwrap();
        assert_eq!(mixed, "Venue resource not found");

        let unknown = authorize_identify_targets(&pool, &["fixture-a".into(), "missing".into()])
            .await
            .err()
            .unwrap();
        assert_eq!(unknown, "Venue resource not found");

        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        let unauthorized = authorize_identify_targets(&pool, &["fixture-a".into()])
            .await
            .err()
            .unwrap();
        assert_eq!(unauthorized, "Venue resource not found");
    }
}
