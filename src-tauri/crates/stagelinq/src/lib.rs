pub mod device;
pub mod discovery;
pub mod protocol;
pub mod services;
pub mod types;

use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::discovery::{run_discovery, DiscoveredDevice};
use crate::services::beat_info::BeatUpdate;
use crate::services::state_map::StateChange;
use crate::types::*;

/// Events emitted by the StageLinQ client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DeckEvent {
    DeviceDiscovered {
        address: String,
        name: String,
        version: String,
    },
    Connected {
        address: String,
    },
    StateChanged(DeckSnapshot),
    Disconnected {
        address: String,
    },
    Error {
        message: String,
    },
}

/// Snapshot of all deck states at a point in time.
#[derive(Debug, Clone, Serialize)]
pub struct DeckSnapshot {
    pub decks: Vec<DeckState>,
    pub crossfader: f64,
    pub master_tempo: f64,
}

/// Per-deck state combining StateMap values and BeatInfo.
#[derive(Debug, Clone, Serialize)]
pub struct DeckState {
    pub id: u8,
    pub title: String,
    pub artist: String,
    pub bpm: f64,
    pub playing: bool,
    pub volume: f64,
    pub fader: f64,
    pub master: bool,
    pub song_loaded: bool,
    pub track_length: f64,
    pub sample_rate: f64,
    // Beat info
    pub beat: f64,
    pub total_beats: f64,
    pub beat_bpm: f64,
    pub samples: f64,
}

impl Default for DeckState {
    fn default() -> Self {
        Self {
            id: 0,
            title: String::new(),
            artist: String::new(),
            bpm: 0.0,
            playing: false,
            volume: 0.0,
            fader: 0.0,
            master: false,
            song_loaded: false,
            track_length: 0.0,
            sample_rate: 0.0,
            beat: 0.0,
            total_beats: 0.0,
            beat_bpm: 0.0,
            samples: 0.0,
        }
    }
}

/// Shared mutable state for all decks.
struct SharedState {
    decks: [DeckState; 4],
    crossfader: f64,
    master_tempo: f64,
}

impl SharedState {
    fn new() -> Self {
        let mut decks = [
            DeckState::default(),
            DeckState::default(),
            DeckState::default(),
            DeckState::default(),
        ];
        for (i, deck) in decks.iter_mut().enumerate() {
            deck.id = (i + 1) as u8;
        }
        Self {
            decks,
            crossfader: 0.0,
            master_tempo: 0.0,
        }
    }

    fn snapshot(&self) -> DeckSnapshot {
        DeckSnapshot {
            decks: self.decks.to_vec(),
            crossfader: self.crossfader,
            master_tempo: self.master_tempo,
        }
    }

    fn apply_state_change(&mut self, change: &StateChange) {
        let path = &change.path;

        // Parse deck number from path like /Engine/Deck1/...
        if let Some(deck_idx) = parse_deck_index(path) {
            let deck = &mut self.decks[deck_idx];
            if path.ends_with("/Play") {
                deck.playing = extract_bool(&change.value);
            } else if path.ends_with("/CurrentBPM") {
                deck.bpm = extract_f64(&change.value);
            } else if path.ends_with("/ExternalMixerVolume") {
                deck.volume = extract_f64(&change.value);
            } else if path.ends_with("/Track/SongName") {
                deck.title = extract_string(&change.value);
            } else if path.ends_with("/Track/ArtistName") {
                deck.artist = extract_string(&change.value);
            } else if path.ends_with("/Track/SongLoaded") {
                deck.song_loaded = extract_bool(&change.value);
            } else if path.ends_with("/Track/TrackLength") {
                deck.track_length = extract_f64(&change.value);
            } else if path.ends_with("/Track/SampleRate") {
                deck.sample_rate = extract_f64(&change.value);
            } else if path.ends_with("/DeckIsMaster") {
                deck.master = extract_bool(&change.value);
            } else if path.ends_with("/Track/SoundSwitchGuid")
                || path.ends_with("/Track/TrackUri")
                || path.ends_with("/Track/TrackNetworkPath")
            {
                eprintln!("[stagelinq] {path} = {}", change.value);
            }
        } else if let Some(ch) = parse_fader_channel(path) {
            if ch < 4 {
                self.decks[ch].fader = extract_f64(&change.value);
            }
        } else if path == "/Mixer/CrossfaderPosition" {
            self.crossfader = extract_f64(&change.value);
        } else if path == "/Engine/Master/MasterTempo" {
            self.master_tempo = extract_f64(&change.value);
        }
    }

    fn apply_beat_update(&mut self, update: &BeatUpdate) {
        for (i, info) in update.decks.iter().enumerate() {
            if i < 4 {
                self.decks[i].beat = info.beat;
                self.decks[i].total_beats = info.total_beats;
                self.decks[i].beat_bpm = info.bpm;
                self.decks[i].samples = info.samples;
            }
        }
    }
}

/// Parse deck index (0-based) from a path like "/Engine/Deck1/..."
fn parse_deck_index(path: &str) -> Option<usize> {
    // Match /Engine/Deck{N}/ or /Client/Deck{N}/
    let patterns = ["/Engine/Deck", "/Client/Deck"];
    for pat in &patterns {
        if let Some(rest) = path.strip_prefix(pat) {
            if let Some(ch) = rest.chars().next() {
                if let Some(n) = ch.to_digit(10) {
                    if n >= 1 && n <= 4 {
                        return Some((n - 1) as usize);
                    }
                }
            }
        }
    }
    None
}

/// Parse fader channel (0-based) from "/Mixer/CH1faderPosition"
fn parse_fader_channel(path: &str) -> Option<usize> {
    let prefix = "/Mixer/CH";
    let suffix = "faderPosition";
    if let Some(rest) = path.strip_prefix(prefix) {
        if let Some(num_str) = rest.strip_suffix(suffix) {
            if let Ok(n) = num_str.parse::<usize>() {
                if n >= 1 && n <= 4 {
                    return Some(n - 1);
                }
            }
        }
    }
    None
}

fn extract_f64(v: &serde_json::Value) -> f64 {
    v.get("value")
        .or_else(|| v.get("state"))
        .and_then(|v| v.as_f64())
        .or_else(|| v.as_f64())
        .unwrap_or(0.0)
}

fn extract_bool(v: &serde_json::Value) -> bool {
    v.get("state")
        .and_then(|v| v.as_bool())
        .or_else(|| v.as_bool())
        .unwrap_or(false)
}

fn extract_string(v: &serde_json::Value) -> String {
    v.get("string")
        .and_then(|v| v.as_str())
        .or_else(|| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// The main StageLinQ client.
pub struct StageLinqClient {
    stop_tx: mpsc::Sender<()>,
}

impl StageLinqClient {
    /// Start the client. Discovered devices are automatically connected.
    /// The callback receives `DeckEvent`s for each state change.
    pub async fn start(
        callback: impl Fn(DeckEvent) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let our_token: [u8; 16] = SOUNDSWITCH_TOKEN;
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let callback = Arc::new(callback);

        let (discovery_handle, mut discovery_rx) =
            run_discovery(our_token).await.map_err(|e| e.to_string())?;

        let cb = callback.clone();
        tokio::spawn(async move {
            let state = Arc::new(Mutex::new(SharedState::new()));
            let mut seen_devices = HashSet::new();

            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        discovery_handle.stop().await;
                        break;
                    }
                    device = discovery_rx.recv() => {
                        match device {
                            Some(dev) => {
                                // Deduplicate: only connect once per address:port
                                let key = (dev.address, dev.port);
                                if seen_devices.contains(&key) {
                                    continue;
                                }
                                seen_devices.insert(key);

                                let cb2 = cb.clone();
                                let state2 = state.clone();
                                let token = our_token;
                                tokio::spawn(async move {
                                    handle_device(dev, &token, cb2, state2).await;
                                });
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        Ok(Self { stop_tx })
    }

    /// Stop the client and disconnect from all devices.
    pub async fn stop(&self) {
        let _ = self.stop_tx.send(()).await;
    }
}

/// Handle a discovered device: connect, start services, forward events.
async fn handle_device(
    dev: DiscoveredDevice,
    our_token: &[u8; 16],
    callback: Arc<dyn Fn(DeckEvent) + Send + Sync>,
    state: Arc<Mutex<SharedState>>,
) {
    let addr_str = dev.address.to_string();

    callback(DeckEvent::DeviceDiscovered {
        address: addr_str.clone(),
        name: dev.software_name.clone(),
        version: dev.software_version.clone(),
    });

    // Discover services (keep main_conn alive — dropping it disconnects the device)
    let (services, _main_conn) =
        match device::connect_and_discover_services(dev.address, dev.port, our_token).await {
            Ok(s) => s,
            Err(e) => {
                callback(DeckEvent::Error {
                    message: format!("Failed to connect to {addr_str}: {e}"),
                });
                return;
            }
        };

    eprintln!("[stagelinq] connected to {addr_str}, discovered {} services", services.len());
    callback(DeckEvent::Connected {
        address: addr_str.clone(),
    });

    // Collect all state paths to subscribe to
    let mut paths = Vec::new();
    for deck in 1..=4u8 {
        paths.extend(deck_state_paths(deck));
    }
    paths.extend(mixer_state_paths());

    // Stop handles must live for the duration of the event loop — dropping them
    // signals the service tasks to exit, which closes the channels.
    let mut _state_map_stop = None;
    let mut _beat_info_stop = None;

    // Start StateMap service
    let mut state_map_rx = if let Some(&port) = services.get(SERVICE_STATE_MAP) {
        eprintln!("[stagelinq] starting StateMap service on port {port}");
        match services::state_map::run_state_map(dev.address, port, our_token, &paths).await {
            Ok((stop, rx)) => {
                eprintln!("[stagelinq] StateMap service started");
                _state_map_stop = Some(stop);
                Some(rx)
            }
            Err(e) => {
                callback(DeckEvent::Error {
                    message: format!("StateMap connect failed: {e}"),
                });
                None
            }
        }
    } else {
        eprintln!("[stagelinq] WARNING: StateMap service not found in announced services");
        None
    };

    // Start BeatInfo service
    let mut beat_info_rx = if let Some(&port) = services.get(SERVICE_BEAT_INFO) {
        eprintln!("[stagelinq] starting BeatInfo service on port {port}");
        match services::beat_info::run_beat_info(dev.address, port, our_token).await {
            Ok((stop, rx)) => {
                eprintln!("[stagelinq] BeatInfo service started");
                _beat_info_stop = Some(stop);
                Some(rx)
            }
            Err(e) => {
                callback(DeckEvent::Error {
                    message: format!("BeatInfo connect failed: {e}"),
                });
                None
            }
        }
    } else {
        eprintln!("[stagelinq] WARNING: BeatInfo service not found in announced services");
        None
    };

    eprintln!("[stagelinq] entering event loop (state_map={}, beat_info={})", state_map_rx.is_some(), beat_info_rx.is_some());

    // Event loop: forward service messages as DeckEvents.
    // This keeps handle_device alive (and thus _main_conn alive) for the
    // entire duration of the connection.
    loop {
        tokio::select! {
            state_change = async {
                match state_map_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match state_change {
                    Some(change) => {
                        let snapshot = {
                            let mut s = state.lock().await;
                            s.apply_state_change(&change);
                            s.snapshot()
                        };
                        callback(DeckEvent::StateChanged(snapshot));
                    }
                    None => break, // StateMap disconnected
                }
            }
            beat_update = async {
                match beat_info_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match beat_update {
                    Some(update) => {
                        let snapshot = {
                            let mut s = state.lock().await;
                            s.apply_beat_update(&update);
                            s.snapshot()
                        };
                        callback(DeckEvent::StateChanged(snapshot));
                    }
                    None => break, // BeatInfo disconnected
                }
            }
        }
    }

    callback(DeckEvent::Disconnected {
        address: addr_str,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::DeckBeatInfo;
    use crate::services::state_map::StateChange;
    use crate::services::beat_info::BeatUpdate;

    #[test]
    fn parse_deck_index_from_engine_path() {
        assert_eq!(parse_deck_index("/Engine/Deck1/Play"), Some(0));
        assert_eq!(parse_deck_index("/Engine/Deck2/Track/SongName"), Some(1));
        assert_eq!(parse_deck_index("/Engine/Deck3/CurrentBPM"), Some(2));
        assert_eq!(parse_deck_index("/Engine/Deck4/DeckIsMaster"), Some(3));
    }

    #[test]
    fn parse_deck_index_from_client_path() {
        assert_eq!(parse_deck_index("/Client/Deck1/Something"), Some(0));
        assert_eq!(parse_deck_index("/Client/Deck4/Other"), Some(3));
    }

    #[test]
    fn parse_deck_index_invalid() {
        assert_eq!(parse_deck_index("/Engine/Deck0/Play"), None);
        assert_eq!(parse_deck_index("/Engine/Deck5/Play"), None);
        assert_eq!(parse_deck_index("/Mixer/CrossfaderPosition"), None);
        assert_eq!(parse_deck_index(""), None);
    }

    #[test]
    fn parse_fader_channel_valid() {
        assert_eq!(parse_fader_channel("/Mixer/CH1faderPosition"), Some(0));
        assert_eq!(parse_fader_channel("/Mixer/CH2faderPosition"), Some(1));
        assert_eq!(parse_fader_channel("/Mixer/CH3faderPosition"), Some(2));
        assert_eq!(parse_fader_channel("/Mixer/CH4faderPosition"), Some(3));
    }

    #[test]
    fn parse_fader_channel_invalid() {
        assert_eq!(parse_fader_channel("/Mixer/CH0faderPosition"), None);
        assert_eq!(parse_fader_channel("/Mixer/CH5faderPosition"), None);
        assert_eq!(parse_fader_channel("/Mixer/CrossfaderPosition"), None);
    }

    #[test]
    fn extract_bool_from_state_json() {
        let v: serde_json::Value = serde_json::json!({"state": true});
        assert!(extract_bool(&v));
        let v: serde_json::Value = serde_json::json!({"state": false});
        assert!(!extract_bool(&v));
    }

    #[test]
    fn extract_f64_from_value_json() {
        let v: serde_json::Value = serde_json::json!({"value": 128.5});
        assert!((extract_f64(&v) - 128.5).abs() < f64::EPSILON);
    }

    #[test]
    fn extract_string_from_string_json() {
        let v: serde_json::Value = serde_json::json!({"string": "My Song"});
        assert_eq!(extract_string(&v), "My Song");
    }

    #[test]
    fn shared_state_apply_play() {
        let mut state = SharedState::new();
        state.apply_state_change(&StateChange {
            path: "/Engine/Deck1/Play".into(),
            value: serde_json::json!({"state": true}),
        });
        assert!(state.decks[0].playing);
    }

    #[test]
    fn shared_state_apply_track_info() {
        let mut state = SharedState::new();
        state.apply_state_change(&StateChange {
            path: "/Engine/Deck2/Track/SongName".into(),
            value: serde_json::json!({"string": "Test Track"}),
        });
        state.apply_state_change(&StateChange {
            path: "/Engine/Deck2/Track/ArtistName".into(),
            value: serde_json::json!({"string": "DJ Test"}),
        });
        assert_eq!(state.decks[1].title, "Test Track");
        assert_eq!(state.decks[1].artist, "DJ Test");
    }

    #[test]
    fn shared_state_apply_crossfader() {
        let mut state = SharedState::new();
        state.apply_state_change(&StateChange {
            path: "/Mixer/CrossfaderPosition".into(),
            value: serde_json::json!({"value": 0.75}),
        });
        assert!((state.crossfader - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn shared_state_apply_fader() {
        let mut state = SharedState::new();
        state.apply_state_change(&StateChange {
            path: "/Mixer/CH1faderPosition".into(),
            value: serde_json::json!({"value": 0.5}),
        });
        assert!((state.decks[0].fader - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn shared_state_apply_beat_update() {
        let mut state = SharedState::new();
        state.apply_beat_update(&BeatUpdate {
            clock: 0,
            decks: vec![
                DeckBeatInfo { beat: 2.5, total_beats: 500.0, bpm: 128.0, samples: 44100.0 },
                DeckBeatInfo { beat: 3.0, total_beats: 600.0, bpm: 140.0, samples: 88200.0 },
            ],
        });
        assert!((state.decks[0].beat - 2.5).abs() < f64::EPSILON);
        assert!((state.decks[0].beat_bpm - 128.0).abs() < f64::EPSILON);
        assert!((state.decks[1].beat - 3.0).abs() < f64::EPSILON);
        assert!((state.decks[1].beat_bpm - 140.0).abs() < f64::EPSILON);
    }

    #[test]
    fn shared_state_snapshot() {
        let mut state = SharedState::new();
        state.apply_state_change(&StateChange {
            path: "/Engine/Deck1/Track/SongName".into(),
            value: serde_json::json!({"string": "Snapshot Test"}),
        });
        let snap = state.snapshot();
        assert_eq!(snap.decks.len(), 4);
        assert_eq!(snap.decks[0].title, "Snapshot Test");
        assert_eq!(snap.decks[0].id, 1);
        assert_eq!(snap.decks[3].id, 4);
    }

    #[test]
    fn shared_state_master_tempo() {
        let mut state = SharedState::new();
        state.apply_state_change(&StateChange {
            path: "/Engine/Master/MasterTempo".into(),
            value: serde_json::json!({"value": 132.0}),
        });
        assert!((state.master_tempo - 132.0).abs() < f64::EPSILON);
    }
}
