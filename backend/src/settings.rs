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
///
/// A gateway is the only thing offered here because a gateway key reaches every
/// model in [`crate::agent::model::MODELS`]. The first-party Anthropic API is
/// reachable too — write `agent_provider = "anthropic"` by hand — but it serves
/// a subset with a key nobody here is assumed to hold, so it is not a choice
/// the picker can strand someone on.
pub const AGENT_PROVIDERS: &[(&str, &str)] = &[
    ("vercel-ai-gateway", "Vercel AI Gateway"),
    ("openrouter", "OpenRouter"),
];

/// The picker reads the same catalog as inference routing.
pub fn agent_models() -> Vec<(&'static str, &'static str)> {
    crate::agent::model::MODELS
        .iter()
        .map(|model| (model.key, model.display))
        .collect()
}

/// The service the agents call when nothing has been chosen — the same one the
/// agent loop falls back to, so an unset installation and a freshly written
/// settings row route identically.
pub const DEFAULT_AGENT_PROVIDER: &str = crate::agent::model::Provider::DEFAULT.as_str();

/// The track agent's model when nothing has been chosen.
pub const DEFAULT_AGENT_MODEL: &str = crate::agent::model::DEFAULT_MODEL;

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
    /// Whether the visualizer draws the floor grid. Device-global, like every
    /// other view preference: it is how this machine looks at any venue.
    pub stage_grid: bool,
    /// Whether the visualizer draws selection and transform gizmos.
    pub stage_gizmos: bool,
    /// Viewport render scale as a percentage of native resolution, 25..=100.
    /// A cost knob, so it belongs to the machine rather than the venue.
    pub render_scale: u8,
    #[serde(default)]
    pub agent_engine: crate::agent::engine::Engine,
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
            stage_grid: true,
            stage_gizmos: true,
            render_scale: 100,
            agent_engine: crate::agent::engine::Engine::default(),
            agent_provider: DEFAULT_AGENT_PROVIDER.to_string(),
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
/// uses its default where the setting has one; invalid agent configuration
/// is reported so the picker and runtime cannot silently disagree.
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
        stage_grid: map.get("stage_grid").map(|v| v == "true").unwrap_or(true),
        stage_gizmos: map.get("stage_gizmos").map(|v| v == "true").unwrap_or(true),
        // The write path validates nothing, so this read is the only place a
        // row outside the supported range gets caught.
        render_scale: map
            .get("render_scale")
            .and_then(|v| v.parse::<u8>().ok())
            .map(|v| v.clamp(25, 100))
            .unwrap_or(100),
        agent_engine: crate::agent::engine::Engine::configured(&map).map_err(|e| e.to_string())?,
        agent_provider: one_of(
            AGENT_PROVIDERS,
            map.get("agent_provider"),
            DEFAULT_AGENT_PROVIDER,
        ),
        agent_model: crate::agent::model::configured(&map)
            .map_err(|e| e.to_string())?
            .key()
            .into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every offered model resolves through the same routing catalog.
    #[test]
    fn every_offered_model_resolves_in_the_model_table() {
        for (stored, _) in agent_models() {
            assert!(
                crate::agent::model::ModelId::parse(stored).is_some(),
                "settings offers '{stored}', which agent::model::MODELS does not carry"
            );
        }
        assert!(crate::agent::model::ModelId::parse(DEFAULT_AGENT_MODEL).is_some());
    }

    /// Same contract one axis over: the picker must not offer a service the
    /// loop cannot build a client for.
    #[test]
    fn every_offered_provider_resolves_in_the_model_seam() {
        for (stored, _) in AGENT_PROVIDERS {
            assert!(
                crate::agent::model::Provider::parse(stored).is_some(),
                "settings offers provider '{stored}', which agent::model does not know"
            );
        }
        assert!(crate::agent::model::Provider::parse(DEFAULT_AGENT_PROVIDER).is_some());
    }
}
