//! Live Controller Manager
//!
//! Manages the MIDI pad controller (e.g. Donner Starrypad) connection and the
//! real-time callback. The callback runs on a dedicated midir thread and updates
//! ManualLayerState in the render engine without touching the DB.
//!
//! Named "controller" rather than "midi" because MIDI is also used elsewhere
//! (e.g. DJM mixer fader input) — this manager is specifically for the live
//! pad/cue controller.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use midir::{MidiInput, MidiInputConnection, MidiInputPort};
use tauri::{Emitter, Manager};

use crate::models::midi::{
    ControllerState, Cue, MidiAction, MidiBinding, MidiInput as MidiInputKind, ModifierDef, Target,
    TriggerMode,
};
use crate::render_engine::{RenderEngine, ResolvedTarget};

const CLIENT_NAME: &str = "Luma";

// ============================================================================
// Mapping Snapshot (no DB access in callback)
// ============================================================================

/// Pre-joined bindings+cues snapshot. Rebuilt after any CRUD and on venue change.
#[derive(Clone, Default)]
pub struct ControllerMappingSnapshot {
    pub modifiers: Vec<ModifierDef>,
    /// Bindings joined with the cue definition (if action is FireCue).
    pub bindings: Vec<(MidiBinding, Option<Cue>)>,
}

// ============================================================================
// Parsed MIDI event
// ============================================================================

#[derive(Debug, Clone)]
pub enum MidiEvent {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
    ControlChange { channel: u8, cc: u8, value: u8 },
}

impl MidiEvent {
    pub fn parse(data: &[u8]) -> Option<MidiEvent> {
        if data.len() < 2 {
            return None;
        }
        let status = data[0] & 0xF0;
        let channel = data[0] & 0x0F;
        match status {
            0x90 if data.len() >= 3 && data[2] > 0 => Some(MidiEvent::NoteOn {
                channel,
                note: data[1],
                velocity: data[2],
            }),
            // Note-on with velocity 0 = note-off
            0x90 if data.len() >= 3 => Some(MidiEvent::NoteOff {
                channel,
                note: data[1],
            }),
            0x80 if data.len() >= 3 => Some(MidiEvent::NoteOff {
                channel,
                note: data[1],
            }),
            0xB0 if data.len() >= 3 => Some(MidiEvent::ControlChange {
                channel,
                cc: data[1],
                value: data[2],
            }),
            _ => None,
        }
    }

    /// Whether this event matches a MidiInputKind as a "press" (NoteOn / CC>0).
    pub fn matches_press(&self, kind: &MidiInputKind) -> bool {
        match (self, kind) {
            (
                MidiEvent::NoteOn { channel, note, .. },
                MidiInputKind::Note {
                    channel: kc,
                    note: kn,
                },
            ) => channel == kc && note == kn,
            (
                MidiEvent::ControlChange { channel, cc, value },
                MidiInputKind::ControlChange {
                    channel: kc,
                    cc: kcc,
                },
            ) => channel == kc && cc == kcc && *value > 0,
            (
                MidiEvent::ControlChange { channel, cc, .. },
                MidiInputKind::ControlChangeValue {
                    channel: kc,
                    cc: kcc,
                },
            ) => channel == kc && cc == kcc,
            _ => false,
        }
    }

    /// Whether this event matches a MidiInputKind as a "release" (NoteOff / CC=0).
    pub fn matches_release(&self, kind: &MidiInputKind) -> bool {
        match (self, kind) {
            (
                MidiEvent::NoteOff { channel, note },
                MidiInputKind::Note {
                    channel: kc,
                    note: kn,
                },
            ) => channel == kc && note == kn,
            (
                MidiEvent::ControlChange { channel, cc, value },
                MidiInputKind::ControlChange {
                    channel: kc,
                    cc: kcc,
                },
            ) => channel == kc && cc == kcc && *value == 0,
            _ => false,
        }
    }

    /// Extract the CC value as 0.0–1.0 for ControlChangeValue inputs.
    pub fn cc_value_f32(&self) -> Option<f32> {
        if let MidiEvent::ControlChange { value, .. } = self {
            Some(*value as f32 / 127.0)
        } else {
            None
        }
    }
}

// ============================================================================
// ControllerManager
// ============================================================================

struct ControllerManagerInner {
    connection: Option<MidiInputConnection<()>>,
    connected_port_name: Option<String>,
}

/// Shared learn-mode state — captured by the MIDI callback closure.
struct LearnState {
    active: bool,
    app_handle: Option<tauri::AppHandle>,
}

pub struct ControllerManager {
    inner: Mutex<ControllerManagerInner>,
    /// Persistent MidiInput client used only for port enumeration.
    /// Creating a new CoreMIDI client on every poll exhausts a macOS resource
    /// limit, causing enumeration to silently return empty after a few calls.
    enumerator: Mutex<Option<MidiInput>>,
    /// Port name the user last explicitly connected to.
    /// Cleared on explicit disconnect. Used for auto-reconnect.
    preferred_port: Mutex<Option<String>>,
    /// AppHandle stored on connect so auto-reconnect doesn't need a new one.
    cached_app_handle: Mutex<Option<tauri::AppHandle>>,
    /// Shared with callback closure — controls learn mode.
    learn_state: Arc<Mutex<LearnState>>,
    snapshot: Arc<RwLock<ControllerMappingSnapshot>>,
    render_engine: RenderEngine,
}

impl ControllerManager {
    pub fn new(render_engine: RenderEngine) -> Self {
        Self {
            inner: Mutex::new(ControllerManagerInner {
                connection: None,
                connected_port_name: None,
            }),
            enumerator: Mutex::new(None),
            preferred_port: Mutex::new(None),
            cached_app_handle: Mutex::new(None),
            learn_state: Arc::new(Mutex::new(LearnState {
                active: false,
                app_handle: None,
            })),
            snapshot: Arc::new(RwLock::new(ControllerMappingSnapshot::default())),
            render_engine,
        }
    }

    /// Store the preferred port (from saved venue setting) so auto-reconnect
    /// kicks in even before the user manually connects this session.
    pub fn set_preferred_port(&self, port: Option<String>, app_handle: tauri::AppHandle) {
        if let Ok(mut p) = self.preferred_port.lock() {
            *p = port;
        }
        if let Ok(mut h) = self.cached_app_handle.lock() {
            *h = Some(app_handle);
        }
    }

    /// List available MIDI input port names.
    pub fn list_ports(&self) -> Result<Vec<String>, String> {
        let mut guard = self
            .enumerator
            .lock()
            .map_err(|_| "enumerator mutex poisoned")?;
        if guard.is_none() {
            *guard = Some(
                MidiInput::new(CLIENT_NAME)
                    .map_err(|e| format!("Failed to create MIDI input: {}", e))?,
            );
        }
        let midi_in = guard.as_ref().unwrap();
        let ports = midi_in.ports();
        Ok(ports
            .iter()
            .filter_map(|p| midi_in.port_name(p).ok())
            .collect())
    }

    /// Connect to a MIDI port by name.
    pub fn connect(&self, port_name: &str, app_handle: tauri::AppHandle) -> Result<(), String> {
        // Store preferred port and handle before attempting connection so
        // auto-reconnect works even if the first attempt fails.
        if let Ok(mut p) = self.preferred_port.lock() {
            *p = Some(port_name.to_string());
        }
        if let Ok(mut h) = self.cached_app_handle.lock() {
            *h = Some(app_handle.clone());
        }

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "controller manager mutex poisoned")?;

        // Disconnect existing connection
        inner.connection = None;

        let midi_in = MidiInput::new(CLIENT_NAME).map_err(|e| format!("MIDI init: {}", e))?;
        let ports = midi_in.ports();
        let port: MidiInputPort = ports
            .into_iter()
            .find(|p| {
                midi_in
                    .port_name(p)
                    .map(|n| n == port_name)
                    .unwrap_or(false)
            })
            .ok_or_else(|| format!("MIDI port '{}' not found", port_name))?;

        let snapshot_arc = self.snapshot.clone();
        let render_engine = self.render_engine.clone();
        let app_handle_cb = app_handle.clone();
        let learn_state = self.learn_state.clone();

        let connection = midi_in
            .connect(
                &port,
                "luma-controller-in",
                move |_timestamp_ns, data, _| {
                    let Some(event) = MidiEvent::parse(data) else {
                        return;
                    };

                    // Learn mode: emit capture event and bail
                    {
                        if let Ok(mut ls) = learn_state.lock() {
                            if ls.active {
                                if let Some(ref ah) = ls.app_handle {
                                    if let Some(input) = event_to_midi_input(&event) {
                                        let _ = ah.emit("midi_learn_captured", &input);
                                    }
                                }
                                ls.active = false;
                                ls.app_handle = None;
                                return;
                            }
                        }
                    }

                    let snap = match snapshot_arc.read() {
                        Ok(s) => s.clone(),
                        Err(_) => return,
                    };

                    process_midi_event(&event, &snap, &render_engine, &app_handle_cb);
                },
                (),
            )
            .map_err(|e| format!("Failed to connect to MIDI port: {}", e))?;

        inner.connection = Some(connection);
        inner.connected_port_name = Some(port_name.to_string());

        // Emit port-change event
        let _ = app_handle.emit(
            "controller_port_change",
            serde_json::json!({ "ports": self.list_ports().unwrap_or_default() }),
        );

        Ok(())
    }

    /// Disconnect current MIDI connection and clear the preferred port so
    /// auto-reconnect does not kick in after an explicit disconnect.
    pub fn disconnect(&self) -> Result<(), String> {
        if let Ok(mut p) = self.preferred_port.lock() {
            *p = None;
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "controller manager mutex poisoned")?;
        inner.connection = None;
        inner.connected_port_name = None;
        Ok(())
    }

    /// Get connection status, with dead-connection detection and auto-reconnect.
    ///
    /// Called on every frontend poll (≈2s). If the preferred port has disappeared
    /// from the port list the dead connection is dropped. When it reappears the
    /// manager reconnects automatically.
    pub fn status(&self) -> crate::models::midi::ControllerStatus {
        let available_ports = self.list_ports().unwrap_or_default();

        // Detect dead connection: port we thought we were connected to is gone.
        {
            let mut inner = self.inner.lock().unwrap();
            let dead = inner
                .connected_port_name
                .as_deref()
                .map(|n| !available_ports.contains(&n.to_string()))
                .unwrap_or(false);
            if dead {
                inner.connection = None;
                inner.connected_port_name = None;
            }
        }

        // Auto-reconnect: preferred port is available but we are not connected.
        let preferred = self.preferred_port.lock().ok().and_then(|g| g.clone());
        if let Some(ref port) = preferred {
            let already_connected = self
                .inner
                .lock()
                .map(|g| g.connected_port_name.as_deref() == Some(port.as_str()))
                .unwrap_or(false);
            if !already_connected && available_ports.contains(port) {
                if let Some(handle) = self.cached_app_handle.lock().ok().and_then(|g| g.clone()) {
                    let _ = self.connect(port, handle);
                }
            }
        }

        let (connected, port_name) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.connection.is_some(),
                inner.connected_port_name.clone(),
            )
        };
        crate::models::midi::ControllerStatus {
            connected,
            port_name,
            available_ports,
        }
    }

    /// Arm learn mode: next MIDI event will be emitted as "midi_learn_captured".
    pub fn start_learn(&self, app_handle: tauri::AppHandle) -> Result<(), String> {
        if let Ok(mut ls) = self.learn_state.lock() {
            ls.active = true;
            ls.app_handle = Some(app_handle);
        }
        Ok(())
    }

    /// Cancel learn mode.
    pub fn cancel_learn(&self) -> Result<(), String> {
        if let Ok(mut ls) = self.learn_state.lock() {
            ls.active = false;
            ls.app_handle = None;
        }
        Ok(())
    }

    /// Rebuild ControllerMappingSnapshot from provided cues, modifiers, and bindings.
    pub fn reload_mapping(
        &self,
        cues: Vec<Cue>,
        modifiers: Vec<ModifierDef>,
        bindings: Vec<MidiBinding>,
    ) {
        let cue_map: std::collections::HashMap<String, Cue> =
            cues.into_iter().map(|c| (c.id.clone(), c)).collect();

        let joined: Vec<(MidiBinding, Option<Cue>)> = bindings
            .into_iter()
            .map(|b| {
                let cue = match &b.action {
                    MidiAction::FireCue { cue_id } => cue_map.get(cue_id).cloned(),
                    _ => None,
                };
                (b, cue)
            })
            .collect();

        if let Ok(mut snap) = self.snapshot.write() {
            snap.modifiers = modifiers;
            snap.bindings = joined;
        }
    }
}

// ============================================================================
// MIDI event processing (called from callback thread)
// ============================================================================

fn process_midi_event(
    event: &MidiEvent,
    snapshot: &ControllerMappingSnapshot,
    render_engine: &RenderEngine,
    app_handle: &tauri::AppHandle,
) {
    // 1. Check if event is a modifier press/release
    for modifier in &snapshot.modifiers {
        if event.matches_press(&modifier.input) {
            render_engine.modifier_on(&modifier.name);
            emit_controller_state(render_engine, app_handle);
            return;
        }
        if event.matches_release(&modifier.input) {
            render_engine.modifier_off(&modifier.name);
            emit_controller_state(render_engine, app_handle);
            return;
        }
    }

    // 2. Collect held modifiers for binding resolution
    let held: HashSet<String> = render_engine
        .get_manual_state_snapshot()
        .held_modifiers
        .into_iter()
        .collect();

    // 3. Find most-specific matching binding
    let Some((binding, cue)) = find_best_binding(&snapshot.bindings, event, &held) else {
        return;
    };

    // 4. Dispatch action
    match &binding.action {
        MidiAction::ControllerActive => {
            let current = render_engine.get_manual_state_snapshot().active;
            render_engine.set_manual_active(!current);
        }

        MidiAction::SetIntensity { group_id } => {
            if let Some(v) = event.cc_value_f32() {
                render_engine.set_group_intensity(group_id.clone(), v);
            }
        }

        MidiAction::Blackout => {
            render_engine.clear_all_cues();
        }

        MidiAction::FireCue { cue_id } => {
            let Some(cue) = cue else { return };
            let target = binding
                .target_override
                .as_ref()
                .unwrap_or(&cue.default_target);
            let resolved = resolve_target(target, &snapshot.modifiers, &held);
            process_cue_trigger(
                binding,
                cue_id,
                resolved,
                cue.z_index as i8,
                event,
                render_engine,
            );
        }
    }

    emit_controller_state(render_engine, app_handle);
}

/// Find the binding that best matches the event + held modifiers.
fn find_best_binding<'a>(
    bindings: &'a [(MidiBinding, Option<Cue>)],
    event: &MidiEvent,
    held: &HashSet<String>,
) -> Option<(&'a MidiBinding, Option<&'a Cue>)> {
    let is_press = matches!(event, MidiEvent::NoteOn { .. })
        || matches!(event, MidiEvent::ControlChange { value, .. } if *value > 0);
    let is_release = matches!(event, MidiEvent::NoteOff { .. })
        || matches!(event, MidiEvent::ControlChange { value, .. } if *value == 0);

    let mut best: Option<(&MidiBinding, Option<&Cue>)> = None;
    let mut best_specificity: usize = 0;
    let mut best_index: usize = 0;

    for (idx, (b, c)) in bindings.iter().enumerate() {
        let trigger_matches = if is_press {
            event.matches_press(&b.trigger)
        } else if is_release {
            event.matches_release(&b.trigger)
        } else {
            false
        };
        if !trigger_matches {
            continue;
        }
        if !b.required_modifiers.iter().all(|m| held.contains(m)) {
            continue;
        }
        if b.exclusive {
            let extra_count = held
                .iter()
                .filter(|m| !b.required_modifiers.contains(m))
                .count();
            if extra_count > 0 {
                continue;
            }
        }
        let specificity = b.required_modifiers.len();
        if best.is_none()
            || specificity > best_specificity
            || (specificity == best_specificity && idx > best_index)
        {
            best_specificity = specificity;
            best_index = idx;
            best = Some((b, c.as_ref()));
        }
    }

    best
}

fn process_cue_trigger(
    binding: &MidiBinding,
    cue_id: &str,
    resolved_target: ResolvedTarget,
    z_index: i8,
    event: &MidiEvent,
    render_engine: &RenderEngine,
) {
    let is_press = matches!(event, MidiEvent::NoteOn { .. })
        || matches!(event, MidiEvent::ControlChange { value, .. } if *value > 0);
    let is_release = matches!(event, MidiEvent::NoteOff { .. })
        || matches!(event, MidiEvent::ControlChange { value, .. } if *value == 0);

    match &binding.mode {
        TriggerMode::Toggle => {
            if is_press {
                render_engine.toggle_cue(cue_id, resolved_target, z_index);
            }
        }

        TriggerMode::Flash => {
            if is_press {
                render_engine.flash_cue_on(cue_id, resolved_target);
            } else if is_release {
                render_engine.flash_cue_off(cue_id);
            }
        }

        TriggerMode::TapToggleHoldFlash { threshold_ms } => {
            if is_press {
                render_engine.record_tap(&binding.id);
                render_engine.flash_cue_on(cue_id, resolved_target);
            } else if is_release {
                render_engine.flash_cue_off(cue_id);
                if let Some(elapsed) = render_engine.consume_tap_elapsed_ms(&binding.id) {
                    if elapsed < *threshold_ms {
                        render_engine.toggle_cue(cue_id, resolved_target, z_index);
                    }
                }
            }
        }
    }
}

fn resolve_target(
    target: &Target,
    modifiers: &[ModifierDef],
    held: &HashSet<String>,
) -> ResolvedTarget {
    match target {
        Target::All => ResolvedTarget::All,
        Target::Explicit { groups } => ResolvedTarget::Groups(groups.clone()),
        Target::FromModifiers => {
            let groups: Vec<String> = modifiers
                .iter()
                .filter(|m| held.contains(&m.name))
                .filter_map(|m| m.groups.as_ref())
                .flat_map(|g| g.iter().cloned())
                .collect();
            if groups.is_empty() {
                ResolvedTarget::All
            } else {
                ResolvedTarget::Groups(groups)
            }
        }
    }
}

fn event_to_midi_input(event: &MidiEvent) -> Option<MidiInputKind> {
    match event {
        MidiEvent::NoteOn { channel, note, .. } => Some(MidiInputKind::Note {
            channel: *channel,
            note: *note,
        }),
        MidiEvent::ControlChange { channel, cc, .. } => Some(MidiInputKind::ControlChange {
            channel: *channel,
            cc: *cc,
        }),
        _ => None,
    }
}

fn emit_controller_state(render_engine: &RenderEngine, app_handle: &tauri::AppHandle) {
    let state: ControllerState = render_engine.get_manual_state_snapshot();
    let _ = app_handle.emit("controller_state", &state);

    // When no cues are active and output is off, push a dark universe frame
    // so the visualizer clears and ArtNet fixtures go dark rather than holding
    // the last lit frame.
    let arc = render_engine.inner_arc();
    let guard = arc.lock().expect("poisoned");
    if !guard.manual_layer.has_any_cues() && !guard.manual_layer.active {
        use std::collections::HashMap;
        let dark = crate::models::universe::UniverseState {
            primitives: HashMap::new(),
        };
        let _ = app_handle.emit("universe-state-update", &dark);
        if let Some(artnet) = app_handle.try_state::<crate::artnet::ArtNetManager>() {
            artnet.broadcast(&dark);
        }
    }
}
