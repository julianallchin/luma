use stagelinq::{DeckEvent, StageLinqClient};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

pub struct StageLinqManager {
    inner: Arc<Mutex<Option<StageLinqClient>>>,
}

impl StageLinqManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start(&self, app_handle: AppHandle) -> Result<(), String> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Err("StageLinQ already running".into());
        }

        log::info!("[stagelinq] starting client");
        let handle = app_handle.clone();
        let client = match StageLinqClient::start(move |event: DeckEvent| {
            match &event {
                DeckEvent::DeviceDiscovered { name, .. } => {
                    log::info!("[stagelinq] DeviceDiscovered: {name}")
                }
                DeckEvent::Connected { address } => log::info!("[stagelinq] Connected: {address}"),
                DeckEvent::Disconnected { address } => {
                    log::warn!("[stagelinq] Disconnected: {address}")
                }
                DeckEvent::Error { message } => log::error!("[stagelinq] Error: {message}"),
                DeckEvent::StateChanged(snap) => {
                    log::debug!("[stagelinq] StateChanged: {} decks", snap.decks.len())
                }
            }
            let _ = handle.emit("perform_event", &event);
        })
        .await
        {
            Ok(c) => {
                log::info!("[stagelinq] client started");
                c
            }
            Err(e) => {
                log::warn!("[stagelinq] failed to start: {e}");
                return Err(e);
            }
        };

        *guard = Some(client);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        log::info!("[stagelinq] stopping");
        let mut guard = self.inner.lock().await;
        if let Some(client) = guard.take() {
            client.stop().await;
        }
        Ok(())
    }
}
