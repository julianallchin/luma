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

        let handle = app_handle.clone();
        let client = StageLinqClient::start(move |event: DeckEvent| {
            let _ = handle.emit("stagelinq_event", &event);
        })
        .await?;

        *guard = Some(client);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let mut guard = self.inner.lock().await;
        if let Some(client) = guard.take() {
            client.stop().await;
        }
        Ok(())
    }
}
