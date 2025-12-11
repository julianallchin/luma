use std::collections::HashMap;
use tauri::{AppHandle, Manager};
use crate::database::Db;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub artnet_enabled: bool,
    pub artnet_interface: String,
    pub artnet_broadcast: bool,
    pub artnet_unicast_ip: String,
    pub artnet_net: u8,
    pub artnet_subnet: u8,
}

#[derive(sqlx::FromRow)]
struct SettingRow {
    key: String,
    value: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            artnet_enabled: false,
            artnet_interface: "0.0.0.0".to_string(),
            artnet_broadcast: true,
            artnet_unicast_ip: "".to_string(),
            artnet_net: 0,
            artnet_subnet: 0,
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
    let db = app.state::<Db>();
    
    let rows = sqlx::query_as::<_, SettingRow>("SELECT key, value FROM settings")
        .fetch_all(&db.0)
        .await
        .map_err(|e| e.to_string())?;

    let mut map = HashMap::new();
    for row in rows {
        map.insert(row.key, row.value);
    }

    Ok(AppSettings {
        artnet_enabled: map.get("artnet_enabled").map(|v| v == "true").unwrap_or(false),
        artnet_interface: map.get("artnet_interface").cloned().unwrap_or("0.0.0.0".to_string()),
        artnet_broadcast: map.get("artnet_broadcast").map(|v| v == "true").unwrap_or(true),
        artnet_unicast_ip: map.get("artnet_unicast_ip").cloned().unwrap_or_default(),
        artnet_net: map.get("artnet_net").and_then(|v| v.parse().ok()).unwrap_or(0),
        artnet_subnet: map.get("artnet_subnet").and_then(|v| v.parse().ok()).unwrap_or(0),
    })
}

pub async fn update_setting(app: &AppHandle, key: &str, value: &str) -> Result<(), String> {
    let db = app.state::<Db>();
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = ?")
        .bind(key)
        .bind(value)
        .bind(value)
        .execute(&db.0)
        .await
        .map_err(|e| e.to_string())?;
    
    // Trigger update in ArtNet manager
    crate::artnet::reload_settings(app).await?;
    
    Ok(())
}