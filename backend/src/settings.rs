//! The typed view over the settings key/value table.
//!
//! Storage is a string map; this module owns the schema — which keys exist,
//! what they parse to, and what they fall back to. The commands that read and
//! write settings live on the dispatch seam
//! (`dispatch::handlers::settings`); nothing here knows about a host.

use crate::database::local::settings as db;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Services the agents can be pointed at, as `(stored value, label)`, in
/// [`crate::agent::model::PROVIDER_PREFERENCE`] order. Both speak the same
/// "creator/model" model ids; only the key and routing differ.
///
/// A gateway key reaches every model in [`crate::agent::model::MODELS`].
pub const AGENT_PROVIDERS: &[(&str, &str)] = &{
    use crate::agent::model::PROVIDER_PREFERENCE;
    let mut providers = [("", ""); PROVIDER_PREFERENCE.len()];
    let mut i = 0;
    while i < providers.len() {
        providers[i] = (
            PROVIDER_PREFERENCE[i].as_str(),
            PROVIDER_PREFERENCE[i].label(),
        );
        i += 1;
    }
    providers
};

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

/// The `settings` row behind each [`AppSettings`] field. A writer names the
/// row it writes through these; the stored spellings are persisted data and
/// must not change.
pub mod key {
    pub const AUDIO_OUTPUT_ENABLED: &str = "audio_output_enabled";
    pub const DMX_OUTPUT_ENABLED: &str = "dmx_output_enabled";
    pub const ARTNET_INTERFACE: &str = "artnet_interface";
    pub const MAX_DIMMER: &str = "max_dimmer";
    pub const STAGE_GRID: &str = "stage_grid";
    pub const STAGE_GIZMOS: &str = "stage_gizmos";
    pub const RENDER_SCALE: &str = "render_scale";
    pub const HDR_OUTPUT: &str = "hdr_output";
    pub const STAGE_FRAME_RATE: &str = "stage_frame_rate";
    pub const STAGE_LOOK: &str = "stage_look";
    pub const STAGE_QUALITY: &str = "stage_quality";
    /// The two-level setting [`STAGE_QUALITY`] replaced. Read only, so a
    /// machine that chose low keeps the budget it chose, now called medium.
    pub const STAGE_LOW_QUALITY: &str = "stage_low_quality";
    pub const AGENT_ENGINE: &str = "agent_engine";
    pub const AGENT_PROVIDER: &str = "agent_provider";
    pub const AGENT_MODEL: &str = "agent_model";
    /// The composer's last model pick, as a serialized selection.
    pub const AGENT_SELECTION: &str = "agent_selection";
    pub const AUTO_UPDATE: &str = "auto_update";
    pub const UPDATE_CHANNEL: &str = "update_channel";
}

/// Write one setting.
///
/// The settings page's agent defaults and the composer's last pick
/// ([`key::AGENT_SELECTION`]) both name the model a new thread starts on, and
/// the pick is read first. The later choice wins: writing a default drops the
/// pick, or the settings page would stop mattering after one composer change.
///
/// # Errors
/// Fails if a row cannot be written.
pub async fn write(pool: &SqlitePool, name: &str, value: &str) -> Result<(), String> {
    db::update_setting(pool, name, value).await?;
    if [key::AGENT_ENGINE, key::AGENT_PROVIDER, key::AGENT_MODEL].contains(&name) {
        db::delete_setting(pool, key::AGENT_SELECTION).await?;
    }
    Ok(())
}

/// The values `max_dimmer` may hold, as a percentage of full output.
pub const MAX_DIMMER_RANGE: std::ops::RangeInclusive<u8> = 0..=100;

/// The values `stage_quality` may hold, best first: the renderer's cost
/// levels, by the names its `Quality` serializes to.
pub const STAGE_QUALITIES: [&str; 3] = ["high", "medium", "low"];

/// The values `render_scale` may hold, as a percentage of native resolution.
/// Past a quarter of the pixels the upscale is mush.
pub const RENDER_SCALE_RANGE: std::ops::RangeInclusive<u8> = 25..=100;

/// The values `update_channel` may hold: the release channels an install
/// can follow, by the names `luma_update::Channel` stores.
pub const UPDATE_CHANNELS: [&str; 2] = ["stable", "nightly"];

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
    /// Whether the stage shows its frame rate in a corner, and the frame stats
    /// it opens. Device-global: it is about this machine's renderer.
    pub stage_frame_rate: bool,
    /// The visualizer's camera — exposure, tone curve and glare — as the
    /// renderer's `Look` in JSON. Empty means the renderer's default. Kept
    /// opaque here: the backend has no renderer types, and a value the
    /// renderer cannot parse falls back to its default there.
    pub stage_look: String,
    /// The visualizer's cost level, one of [`STAGE_QUALITIES`]: `medium`
    /// for a laptop GPU, `low` for one that cannot march haze. A cost knob
    /// of this machine, like `render_scale`.
    #[serde(default = "high_quality")]
    pub stage_quality: String,
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
    /// The release channel this install follows, one of [`UPDATE_CHANNELS`];
    /// `None` until someone chooses, which follows the build's own. Only the
    /// app knows what it was compiled as, so the default is the app's. Not
    /// synced: each machine chooses.
    #[serde(default)]
    pub update_channel: Option<String>,
}

fn enabled() -> bool {
    true
}

fn high_quality() -> String {
    STAGE_QUALITIES[0].to_string()
}

/// The stored cost level. Before there were three, `stage_low_quality` held
/// a flag whose low is today's medium; it answers only while no level has
/// been written.
fn stage_quality(map: &std::collections::HashMap<String, String>) -> String {
    if let Some(level) = map
        .get(key::STAGE_QUALITY)
        .filter(|level| STAGE_QUALITIES.contains(&level.as_str()))
    {
        return level.clone();
    }
    let legacy_low = map
        .get(key::STAGE_LOW_QUALITY)
        .is_some_and(|value| value == "true");
    if legacy_low {
        STAGE_QUALITIES[1].to_string()
    } else {
        high_quality()
    }
}

/// The chosen release channel, or `None` for a row that names none this
/// app follows: an unknown value falls back to the build's own.
fn update_channel(map: &std::collections::HashMap<String, String>) -> Option<String> {
    map.get(key::UPDATE_CHANNEL)
        .filter(|channel| UPDATE_CHANNELS.contains(&channel.as_str()))
        .cloned()
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
            max_dimmer: *MAX_DIMMER_RANGE.end(),
            stage_grid: true,
            stage_gizmos: true,
            render_scale: *RENDER_SCALE_RANGE.end(),
            hdr_output: true,
            stage_frame_rate: true,
            stage_look: String::new(),
            stage_quality: high_quality(),
            agent_engine: crate::agent::engine::Engine::default(),
            agent_provider: DEFAULT_AGENT_PROVIDER.to_string(),
            agent_model: DEFAULT_AGENT_MODEL.to_string(),
            agent_api_keys: api_key_statuses(&Default::default()),
            auto_update: true,
            update_channel: None,
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

    let defaults = AppSettings::default();
    let flag = |key: &str, default: bool| map.get(key).map_or(default, |v| v == "true");
    // The write path validates nothing, so this read is the only place a row
    // outside its range gets caught.
    let percent = |key: &str, range: std::ops::RangeInclusive<u8>, default: u8| {
        map.get(key)
            .and_then(|v| v.parse::<u8>().ok())
            .map_or(default, |v| v.clamp(*range.start(), *range.end()))
    };

    Ok(AppSettings {
        audio_output_enabled: flag(key::AUDIO_OUTPUT_ENABLED, defaults.audio_output_enabled),
        dmx_output_enabled: flag(key::DMX_OUTPUT_ENABLED, defaults.dmx_output_enabled),
        artnet_interface: map
            .get(key::ARTNET_INTERFACE)
            .cloned()
            .unwrap_or(defaults.artnet_interface),
        max_dimmer: percent(key::MAX_DIMMER, MAX_DIMMER_RANGE, defaults.max_dimmer),
        stage_grid: flag(key::STAGE_GRID, defaults.stage_grid),
        stage_gizmos: flag(key::STAGE_GIZMOS, defaults.stage_gizmos),
        render_scale: percent(key::RENDER_SCALE, RENDER_SCALE_RANGE, defaults.render_scale),
        hdr_output: flag(key::HDR_OUTPUT, defaults.hdr_output),
        stage_frame_rate: flag(key::STAGE_FRAME_RATE, defaults.stage_frame_rate),
        stage_look: map
            .get(key::STAGE_LOOK)
            .cloned()
            .unwrap_or(defaults.stage_look),
        stage_quality: stage_quality(&map),
        agent_engine: crate::agent::engine::Engine::configured(&map).map_err(|e| e.to_string())?,
        agent_provider: one_of(
            AGENT_PROVIDERS,
            map.get(key::AGENT_PROVIDER),
            DEFAULT_AGENT_PROVIDER,
        ),
        agent_model: crate::agent::model::configured(&map)
            .map_err(|e| e.to_string())?
            .key()
            .into(),
        agent_api_keys: api_key_statuses(&map),
        auto_update: flag(key::AUTO_UPDATE, defaults.auto_update),
        update_channel: update_channel(&map),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored level reads back as itself; with none, the two-level flag's
    /// low reads as medium, and anything else as high.
    #[test]
    fn stage_quality_reads_the_legacy_low_flag_as_medium() {
        let read = |rows: &[(&str, &str)]| {
            let map = rows
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            stage_quality(&map)
        };
        assert_eq!(read(&[]), "high");
        assert_eq!(read(&[(key::STAGE_LOW_QUALITY, "true")]), "medium");
        assert_eq!(read(&[(key::STAGE_LOW_QUALITY, "false")]), "high");
        for level in STAGE_QUALITIES {
            assert_eq!(
                read(&[
                    (key::STAGE_QUALITY, level),
                    (key::STAGE_LOW_QUALITY, "true")
                ]),
                level
            );
        }
        assert_eq!(
            read(&[
                (key::STAGE_QUALITY, "ultra"),
                (key::STAGE_LOW_QUALITY, "true")
            ]),
            "medium"
        );
    }

    #[test]
    fn update_channel_reads_only_a_channel_it_knows() {
        let read = |value: &str| {
            update_channel(&[(key::UPDATE_CHANNEL.to_string(), value.to_string())].into())
        };
        for channel in UPDATE_CHANNELS {
            assert_eq!(read(channel).as_deref(), Some(channel));
        }
        assert_eq!(read("dev"), None);
        assert_eq!(update_channel(&Default::default()), None);
    }

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
