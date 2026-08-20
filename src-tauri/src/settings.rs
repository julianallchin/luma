//! The typed view over the settings key/value table.
//!
//! Storage is a string map; this module owns the schema — which keys exist,
//! what they parse to, and what they fall back to. The commands that read and
//! write settings live on the dispatch seam
//! (`dispatch::handlers::settings`); nothing here knows about a host.

use crate::database::local::settings as db;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Wire shape of `get_settings`. Deliberately **not** `rename_all` —
/// the frontend reads these keys in `snake_case`.
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

/// Read every setting and apply the typing and defaults. An unparseable value
/// silently falls back to its default — a settings row must never be able to
/// break startup.
pub async fn load_settings(pool: &SqlitePool) -> Result<AppSettings, String> {
    let map = db::get_all_settings(pool).await?;

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
