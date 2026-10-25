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
/// A gateway key reaches every model in [`crate::agent::model::MODELS`].
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

/// Wire shape of `get_settings`. Deliberately **not** `rename_all`: the keys
/// are `snake_case` on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub audio_output_enabled: bool,
    /// Whether DMX goes out at all, over every transport. Off, no driver
    /// holds a socket or a device.
    pub dmx_output_enabled: bool,
    /// The address the Art-Net socket binds; `0.0.0.0` for every interface.
    pub artnet_interface: String,
    pub max_dimmer: u8,
    /// Whether the visualizer draws the floor grid. Device-global, like every
    /// other view preference: it is how this machine looks at any venue.
    pub stage_grid: bool,
    /// Whether the visualizer draws selection and transform gizmos.
    pub stage_gizmos: bool,
    /// Viewport render scale as a percentage of native resolution, 25..=100.
    /// A cost knob, so it belongs to the machine rather than the venue.
    pub render_scale: u8,
    /// Whether the window may present HDR where the display supports it.
    /// Device-global: it is a fact about this machine's display.
    pub hdr_output: bool,
    /// The visualizer's camera — exposure, tone curve and glare — as the
    /// renderer's `Look` in JSON. Empty means the renderer's default. Kept
    /// opaque here: the backend has no renderer types, and a value the
    /// renderer cannot parse falls back to its default there.
    pub stage_look: String,
    /// Whether the visualizer renders at low quality, for a laptop GPU. A
    /// cost knob of this machine, like `render_scale`.
    #[serde(default)]
    pub stage_low_quality: bool,
    /// Whether the score editor shows the timeline beside the stage and the
    /// inspector, instead of under them. A view preference of this machine.
    #[serde(default)]
    pub split_view: bool,
    #[serde(default)]
    pub agent_engine: crate::agent::engine::Engine,
    pub agent_provider: String,
    pub agent_model: String,
    /// One entry per [`AGENT_PROVIDERS`] service, in that order: whether it
    /// has a key, and enough of it to recognise. Never the key itself — this
    /// struct crosses the seam whole, so a secret placed here is a secret
    /// handed to every caller of `get_settings`.
    #[serde(default)]
    pub agent_api_keys: Vec<ApiKeyStatus>,
    /// Whether the app looks for, downloads and installs releases by itself.
    /// A fact about this install, so it is not synced.
    #[serde(default = "enabled")]
    pub auto_update: bool,
}

fn enabled() -> bool {
    true
}

/// What the settings screen may know about one service's API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyStatus {
    /// The [`AGENT_PROVIDERS`] id.
    pub provider: String,
    /// The `settings` row a key for this service is written to.
    pub setting: String,
    /// A masked rendering of the stored key, `None` while none is stored.
    pub hint: Option<String>,
    /// The environment variable that, when set, is used instead of the stored
    /// key — see [`crate::agent::model::api_key`]. `None` while it is unset.
    pub overridden_by: Option<String>,
}

/// Enough of a key to tell two apart, and no more: the last four characters,
/// and only when the key is long enough that four of them give little away.
fn key_hint(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() < 16 {
        return "••••".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

fn api_key_statuses(map: &std::collections::HashMap<String, String>) -> Vec<ApiKeyStatus> {
    AGENT_PROVIDERS
        .iter()
        .filter_map(|(id, _)| crate::agent::model::Provider::parse(id))
        .map(|provider| {
            let env = provider.key_env_var();
            ApiKeyStatus {
                provider: provider.as_str().to_string(),
                setting: provider.key_setting().to_string(),
                hint: map
                    .get(provider.key_setting())
                    .map(|key| key.trim())
                    .filter(|key| !key.is_empty())
                    .map(key_hint),
                overridden_by: std::env::var(env)
                    .is_ok_and(|key| !key.trim().is_empty())
                    .then(|| env.to_string()),
            }
        })
        .collect()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            audio_output_enabled: true,
            dmx_output_enabled: false,
            artnet_interface: "0.0.0.0".to_string(),
            max_dimmer: 100,
            stage_grid: true,
            stage_gizmos: true,
            render_scale: 100,
            hdr_output: true,
            stage_look: String::new(),
            stage_low_quality: false,
            split_view: false,
            agent_engine: crate::agent::engine::Engine::default(),
            agent_provider: DEFAULT_AGENT_PROVIDER.to_string(),
            agent_model: DEFAULT_AGENT_MODEL.to_string(),
            agent_api_keys: api_key_statuses(&Default::default()),
            auto_update: true,
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
        dmx_output_enabled: map
            .get("dmx_output_enabled")
            .map(|v| v == "true")
            .unwrap_or(false),
        artnet_interface: map
            .get("artnet_interface")
            .cloned()
            .unwrap_or("0.0.0.0".to_string()),
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
        hdr_output: map.get("hdr_output").map(|v| v == "true").unwrap_or(true),
        stage_look: map.get("stage_look").cloned().unwrap_or_default(),
        stage_low_quality: map.get("stage_low_quality").is_some_and(|v| v == "true"),
        split_view: map.get("split_view").is_some_and(|v| v == "true"),
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
        agent_api_keys: api_key_statuses(&map),
        auto_update: map.get("auto_update").map_or(true, |v| v == "true"),
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

    /// A stored key reads back as set, recognisable and never whole; a
    /// cleared (empty) row reads as unset, the same way `api_key` resolves it.
    #[test]
    fn key_status_says_a_key_is_set_without_carrying_it() {
        let provider = crate::agent::model::Provider::OpenRouter;
        let secret = "sk-or-v1-0123456789abcdef0123456789wxyz";
        let map = [(provider.key_setting().to_string(), secret.to_string())].into();
        let statuses = api_key_statuses(&map);
        assert_eq!(statuses.len(), AGENT_PROVIDERS.len());
        let status = statuses
            .iter()
            .find(|status| status.provider == provider.as_str())
            .expect("a status per offered provider");
        let hint = status.hint.as_deref().expect("a stored key reads as set");
        assert!(hint.ends_with("wxyz"));
        let wire = serde_json::to_string(&statuses).expect("serializable");
        assert!(!wire.contains(secret) && !wire.contains("0123456789"));

        let short = [(provider.key_setting().to_string(), "short-key".to_string())].into();
        let short = api_key_statuses(&short);
        assert!(!serde_json::to_string(&short).unwrap().contains("short"));

        let cleared = [(provider.key_setting().to_string(), "  ".to_string())].into();
        assert!(api_key_statuses(&cleared).iter().all(|s| s.hint.is_none()));
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
