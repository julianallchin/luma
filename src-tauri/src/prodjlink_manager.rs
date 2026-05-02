use prodjlink::{ProDJLinkClient, ProDJLinkEvent};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

use crate::commands::perform::{DeckEvent, DeckSnapshot, DeckState};

pub struct ProDJLinkManager {
    inner: Arc<Mutex<Option<ProDJLinkClient>>>,
}

impl ProDJLinkManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start(&self, app_handle: AppHandle, device_num: u8) -> Result<(), String> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Err("Pro DJ Link already running".into());
        }

        log::info!("[prodjlink] starting client, device_num={device_num}");
        let handle = app_handle.clone();
        let client = match ProDJLinkClient::start(device_num, move |event: ProDJLinkEvent| {
            let deck_event = to_deck_event(event);
            match &deck_event {
                DeckEvent::Connected { .. } => log::info!("[prodjlink] Connected"),
                DeckEvent::Disconnected { .. } => log::warn!("[prodjlink] Disconnected"),
                DeckEvent::Error { message } => log::error!("[prodjlink] Error: {message}"),
                DeckEvent::StateChanged(snap) => {
                    log::debug!("[prodjlink] StateChanged: {} decks", snap.decks.len())
                }
                DeckEvent::DeviceDiscovered { name, .. } => {
                    log::info!("[prodjlink] DeviceDiscovered: {name}")
                }
            }
            let _ = handle.emit("perform_event", &deck_event);
        })
        .await
        {
            Ok(c) => {
                log::info!("[prodjlink] client started");
                c
            }
            Err(e) => {
                return Err(e);
            }
        };

        *guard = Some(client);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        log::info!("[prodjlink] stopping");
        let mut guard = self.inner.lock().await;
        if let Some(client) = guard.take() {
            client.stop().await;
        }
        Ok(())
    }
}

/// Convert a `ProDJLinkEvent` to the shared `DeckEvent` wire type.
fn to_deck_event(event: ProDJLinkEvent) -> DeckEvent {
    match event {
        ProDJLinkEvent::Discovered { ip, name, .. } => DeckEvent::DeviceDiscovered {
            address: ip,
            name,
            version: "Pro DJ Link".to_string(),
        },
        ProDJLinkEvent::Connected => DeckEvent::Connected {
            address: String::from("prodjlink"),
        },
        ProDJLinkEvent::StateChanged(snapshot) => {
            DeckEvent::StateChanged(to_deck_snapshot(snapshot))
        }
        ProDJLinkEvent::Disconnected => DeckEvent::Disconnected {
            address: String::from("prodjlink"),
        },
        ProDJLinkEvent::Error { message } => DeckEvent::Error { message },
    }
}

fn to_deck_snapshot(snap: prodjlink::ProDJSnapshot) -> DeckSnapshot {
    DeckSnapshot {
        decks: snap.decks.into_iter().map(to_deck_state).collect(),
        crossfader: 0.5,   // no mixer data — centered
        master_tempo: 0.0, // will be derived from master deck's BPM in frontend
    }
}

fn to_deck_state(d: prodjlink::ProDJDeckState) -> DeckState {
    // Encode playback position as samples/sample_rate:
    //   samples     = position_ms      (integer milliseconds from CDJ)
    //   sample_rate = 1000.0           → time_s = position_ms / 1000
    //
    // track_network_path changes only when metadata has arrived,
    // so title + artist are always populated when the path changes.
    let track_network_path = if d.rekordbox_id != 0 {
        format!("prodjlink://{}/{}/{}", d.cdj_ip, d.slot, d.rekordbox_id)
    } else {
        String::new()
    };

    DeckState {
        id: d.player,
        title: d.title,
        artist: d.artist,
        bpm: d.bpm,
        playing: d.playing,
        // No fader data without DJM MIDI — default full volume.
        // on_air could be used but varies by mixer config.
        volume: 1.0,
        fader: 1.0,
        master: d.master,
        song_loaded: d.rekordbox_id != 0,
        track_length: d.duration_secs as f64,
        // sample_rate = 1000 → time = samples/1000 = position_ms/1000 = seconds
        sample_rate: 1000.0,
        track_network_path,
        beat: d.beat_number as f64,
        total_beats: 0.0,
        beat_bpm: d.effective_bpm,
        samples: d.position_ms as f64,
    }
}
