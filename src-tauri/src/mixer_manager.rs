//! MIDI Mixer Manager
//!
//! Manages a MIDI connection to a hardware DJ mixer (DJM, Xone, Rane, etc.)
//! and maps CC messages to channel fader + crossfader values.
//!
//! Completely independent of the deck source (StageLinQ / Pro DJ Link) — both
//! can be active simultaneously. The mixer state is emitted as a `mixer_state`
//! Tauri event on every CC change.
//!
//! Named "mixer_manager" to distinguish from `controller_manager`, which
//! handles the live pad/cue MIDI controller.

use std::sync::{Arc, Mutex};

use midir::{MidiInput, MidiInputConnection, MidiInputPort};
use tauri::Emitter;

use crate::models::mixer::{MixerMapping, MixerState, MixerStatus};

const CLIENT_NAME: &str = "Luma Mixer";

// ── learn state ───────────────────────────────────────────────────────────────

struct LearnState {
    active: bool,
    app_handle: Option<tauri::AppHandle>,
}

// ── inner connection state ────────────────────────────────────────────────────

struct MixerManagerInner {
    connection: Option<MidiInputConnection<()>>,
    connected_port_name: Option<String>,
}

// ── public manager ────────────────────────────────────────────────────────────

pub struct MixerManager {
    inner: Mutex<MixerManagerInner>,
    /// Persistent MIDI client for port enumeration (creating new ones per call
    /// exhausts CoreMIDI resources on macOS).
    enumerator: Mutex<Option<MidiInput>>,
    /// Port the user last explicitly connected to; cleared on explicit disconnect.
    preferred_port: Mutex<Option<String>>,
    /// Mapping associated with the preferred port.
    preferred_mapping: Mutex<Option<MixerMapping>>,
    /// AppHandle cached so auto-reconnect in `status()` works without a new one.
    cached_app_handle: Mutex<Option<tauri::AppHandle>>,
    /// Shared with callback closure — controls learn mode.
    learn_state: Arc<Mutex<LearnState>>,
    /// Current fader/crossfader values, updated by the MIDI callback.
    state: Arc<Mutex<MixerState>>,
}

impl MixerManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MixerManagerInner {
                connection: None,
                connected_port_name: None,
            }),
            enumerator: Mutex::new(None),
            preferred_port: Mutex::new(None),
            preferred_mapping: Mutex::new(None),
            cached_app_handle: Mutex::new(None),
            learn_state: Arc::new(Mutex::new(LearnState {
                active: false,
                app_handle: None,
            })),
            state: Arc::new(Mutex::new(MixerState {
                channel_faders: std::collections::HashMap::new(),
                crossfader: 0.5,
            })),
        }
    }

    // ── port enumeration ──────────────────────────────────────────────────────

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

    // ── connect ───────────────────────────────────────────────────────────────

    pub fn connect(
        &self,
        port_name: &str,
        mapping: MixerMapping,
        app_handle: tauri::AppHandle,
    ) -> Result<(), String> {
        // Persist preferred config so auto-reconnect works.
        if let Ok(mut p) = self.preferred_port.lock() {
            *p = Some(port_name.to_string());
        }
        if let Ok(mut m) = self.preferred_mapping.lock() {
            *m = Some(mapping.clone());
        }
        if let Ok(mut h) = self.cached_app_handle.lock() {
            *h = Some(app_handle.clone());
        }

        // Reset shared state: all mapped faders → 1.0, crossfader → 0.5.
        if let Ok(mut st) = self.state.lock() {
            st.channel_faders.clear();
            for deck_id in mapping.channel_faders.keys() {
                st.channel_faders.insert(*deck_id, 1.0);
            }
            st.crossfader = 0.5;
        }

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "mixer manager mutex poisoned")?;
        inner.connection = None; // drop existing

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

        let learn_state = self.learn_state.clone();
        let state_cb = self.state.clone();
        let app_cb = app_handle.clone();

        let connection = midi_in
            .connect(
                &port,
                "luma-mixer-in",
                move |_ts, data, _| {
                    // Only process CC messages (status byte 0xBn).
                    if data.len() < 3 || (data[0] & 0xF0) != 0xB0 {
                        return;
                    }
                    let channel = data[0] & 0x0F;
                    let cc = data[1];
                    let value = data[2];

                    // Learn mode: capture and emit; don't process as fader.
                    {
                        if let Ok(mut ls) = learn_state.lock() {
                            if ls.active {
                                if let Some(ref ah) = ls.app_handle {
                                    let _ = ah.emit(
                                        "mixer_learned",
                                        serde_json::json!({ "channel": channel, "cc": cc }),
                                    );
                                }
                                ls.active = false;
                                ls.app_handle = None;
                                return;
                            }
                        }
                    }

                    // Map the CC to a fader value and emit updated state.
                    let fader_value = value as f64 / 127.0;
                    let mut updated = false;
                    {
                        if let Ok(mut st) = state_cb.lock() {
                            for (deck_id, spec) in &mapping.channel_faders {
                                if spec.channel == channel && spec.cc == cc {
                                    st.channel_faders.insert(*deck_id, fader_value);
                                    updated = true;
                                }
                            }
                            if let Some(ref xfade) = mapping.crossfader {
                                if xfade.channel == channel && xfade.cc == cc {
                                    st.crossfader = fader_value;
                                    updated = true;
                                }
                            }
                        }
                    }
                    if updated {
                        if let Ok(st) = state_cb.lock() {
                            let _ = app_cb.emit("mixer_state", &*st);
                        }
                    }
                },
                (),
            )
            .map_err(|e| format!("Failed to connect to MIDI port: {}", e))?;

        inner.connection = Some(connection);
        inner.connected_port_name = Some(port_name.to_string());

        // Emit initial state so the frontend knows the mixer is live.
        if let Ok(st) = self.state.lock() {
            let _ = app_handle.emit("mixer_state", &*st);
        }

        Ok(())
    }

    // ── disconnect ────────────────────────────────────────────────────────────

    /// Disconnect and clear saved config so auto-reconnect does not kick in.
    pub fn disconnect(&self) -> Result<(), String> {
        if let Ok(mut p) = self.preferred_port.lock() {
            *p = None;
        }
        if let Ok(mut m) = self.preferred_mapping.lock() {
            *m = None;
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "mixer manager mutex poisoned")?;
        inner.connection = None;
        inner.connected_port_name = None;
        Ok(())
    }

    // ── init (auto-reconnect seed) ────────────────────────────────────────────

    /// Called when a venue loads. Seeds the preferred port + mapping so the
    /// auto-reconnect loop in `status()` can pick them up without user action.
    pub fn set_preferred_config(
        &self,
        port: Option<String>,
        mapping: Option<MixerMapping>,
        app_handle: tauri::AppHandle,
    ) {
        if let Ok(mut p) = self.preferred_port.lock() {
            *p = port;
        }
        if let Ok(mut m) = self.preferred_mapping.lock() {
            *m = mapping;
        }
        if let Ok(mut h) = self.cached_app_handle.lock() {
            *h = Some(app_handle);
        }
    }

    // ── status (with dead-connection detection + auto-reconnect) ─────────────

    /// Polled by the frontend every ~2 s. Detects disconnected ports and
    /// reconnects automatically when the preferred port reappears.
    pub fn status(&self) -> MixerStatus {
        let available_ports = self.list_ports().unwrap_or_default();

        // Detect dead connection.
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

        // Auto-reconnect when preferred port is available but we are not connected.
        let preferred = self.preferred_port.lock().ok().and_then(|g| g.clone());
        let preferred_mapping = self.preferred_mapping.lock().ok().and_then(|g| g.clone());
        if let (Some(ref port), Some(mapping)) = (preferred.as_ref(), preferred_mapping) {
            let already_connected = self
                .inner
                .lock()
                .map(|g| g.connected_port_name.as_deref() == Some(port.as_str()))
                .unwrap_or(false);
            if !already_connected && available_ports.contains(port) {
                if let Some(app_handle) = self.cached_app_handle.lock().ok().and_then(|g| g.clone())
                {
                    let _ = self.connect(port, mapping, app_handle);
                }
            }
        }

        let inner = self.inner.lock().unwrap();
        MixerStatus {
            connected: inner.connection.is_some(),
            port_name: inner.connected_port_name.clone(),
            available_ports,
        }
    }

    // ── learn ─────────────────────────────────────────────────────────────────

    /// Arm learn mode: the next CC message received will be emitted as
    /// `mixer_learned { channel, cc }` instead of processed as a fader.
    pub fn start_learn(&self, app_handle: tauri::AppHandle) -> Result<(), String> {
        let mut ls = self
            .learn_state
            .lock()
            .map_err(|_| "learn state mutex poisoned")?;
        ls.active = true;
        ls.app_handle = Some(app_handle);
        Ok(())
    }

    pub fn cancel_learn(&self) -> Result<(), String> {
        let mut ls = self
            .learn_state
            .lock()
            .map_err(|_| "learn state mutex poisoned")?;
        ls.active = false;
        ls.app_handle = None;
        Ok(())
    }
}
