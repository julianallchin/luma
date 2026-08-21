//! The typed view over the settings key/value table.
//!
//! Storage is a string map; this module owns the schema — which keys exist,
//! what they parse to, and what they fall back to. The commands that read and
//! write settings live on the dispatch seam
//! (`dispatch::handlers::settings`); nothing here knows about a host.

use crate::database::local::settings as db;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Services the agents can be pointed at, as `(stored value, label)`. Both
/// speak the same "creator/model" model ids; only the key and routing differ.
pub const AGENT_PROVIDERS: &[(&str, &str)] = &[
    ("openrouter", "OpenRouter"),
    ("vercel-ai-gateway", "Vercel AI Gateway"),
];

/// Models the settings picker offers for the track agent, as
/// `(stored value, label)`.
pub const AGENT_MODELS: &[(&str, &str)] = &[
    ("anthropic/claude-opus-5", "Claude Opus 5"),
    ("moonshotai/kimi-k3-fast", "Kimi K3 Fast"),
];

/// The track agent's model when nothing has been chosen.
pub const DEFAULT_AGENT_MODEL: &str = "moonshotai/kimi-k3-fast";

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
    pub agent_provider: String,
    pub agent_model: String,
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
            agent_provider: AGENT_PROVIDERS[0].0.to_string(),
            agent_model: DEFAULT_AGENT_MODEL.to_string(),
        }
    }
}

/// Resolve a stored value against a closed option list, falling back to
/// `default`. An id that has been retired must not strand the picker on a
/// value it can no longer show.
fn one_of(options: &[(&str, &str)], stored: Option<&String>, default: &str) -> String {
    stored
        .filter(|value| options.iter().any(|(id, _)| id == value))
        .cloned()
        .unwrap_or_else(|| default.to_string())
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
        agent_provider: one_of(
            AGENT_PROVIDERS,
            map.get("agent_provider"),
            AGENT_PROVIDERS[0].0,
        ),
        agent_model: one_of(AGENT_MODELS, map.get("agent_model"), DEFAULT_AGENT_MODEL),
    })
}
