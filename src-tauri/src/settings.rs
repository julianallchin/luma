use crate::database::local::settings as db;
use crate::database::Db;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub audio_output_enabled: bool,
    pub artnet_enabled: bool,
    pub artnet_interface: String,
    pub artnet_broadcast: bool,
    pub artnet_unicast_ip: String,
    pub artnet_net: u8,
    pub artnet_subnet: u8,
    pub max_dimmer: u8,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            audio_output_enabled: true,
            artnet_enabled: false,
            artnet_interface: "0.0.0.0".to_string(),
            artnet_broadcast: true,
            artnet_unicast_ip: "".to_string(),
            artnet_net: 0,
            artnet_subnet: 0,
            max_dimmer: 100,
        }
    }
}

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<AppSettings, String> {
    get_all_settings(&app).await
}

#[tauri::command]
pub async fn set_setting(app: AppHandle, key: String, value: String) -> Result<(), String> {
    update_setting(&app, &key, &value).await
}

pub async fn get_all_settings(app: &AppHandle) -> Result<AppSettings, String> {
    let db_state = app.state::<Db>();
    let map = db::get_all_settings(&db_state.0).await?;

    Ok(AppSettings {
        audio_output_enabled: map
            .get("audio_output_enabled")
            .map(|v| v == "true")
            .unwrap_or(true),
        artnet_enabled: map
            .get("artnet_enabled")
            .map(|v| v == "true")
            .unwrap_or(false),
        artnet_interface: map
            .get("artnet_interface")
            .cloned()
            .unwrap_or("0.0.0.0".to_string()),
        artnet_broadcast: map
            .get("artnet_broadcast")
            .map(|v| v == "true")
            .unwrap_or(true),
        artnet_unicast_ip: map.get("artnet_unicast_ip").cloned().unwrap_or_default(),
        artnet_net: map
            .get("artnet_net")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        artnet_subnet: map
            .get("artnet_subnet")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        max_dimmer: map
            .get("max_dimmer")
            .and_then(|v| v.parse::<u8>().ok())
            .map(|v| v.min(100))
            .unwrap_or(100),
    })
}

pub async fn update_setting(app: &AppHandle, key: &str, value: &str) -> Result<(), String> {
    let db_state = app.state::<Db>();
    db::update_setting(&db_state.0, key, value).await?;

    // Trigger update in ArtNet manager
    crate::artnet::reload_settings(app).await?;
    crate::host_audio::reload_settings(app).await?;

    Ok(())
}
