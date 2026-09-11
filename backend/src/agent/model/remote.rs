//! The model lists the two gateways publish.
//!
//! Both are public, unauthenticated GETs of every model the gateway carries.
//! Neither is searched on the server: the Vercel AI Gateway has no search, and
//! one shape for both gateways is one code path. The picker filters the whole
//! list as the user types.
//!
//! The last list each gateway returned is saved to disk, so the picker has rows
//! to show while the next read is still on the wire.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ModelError, ModelId, Provider};

/// The reasoning levels a turn can send, in slider order. A model's own list
/// is cut down to these, because [`super::ReasoningLevel`] has no others.
pub const EFFORTS: [&str; 3] = ["low", "medium", "high"];

/// The window assumed for a model whose list entry gives none — or that a
/// thread names but the saved list no longer carries.
pub const FALLBACK_CONTEXT_WINDOW: u32 = 128_000;

/// What a gateway's list says about one model: enough to show it in the picker
/// and to run it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteModel {
    /// The wire id, `creator/model`.
    pub id: String,
    pub name: String,
    /// Prompt tokens the model accepts; zero when the list does not say.
    pub context_window: u32,
    /// USD per million input and output tokens, when the model is priced by
    /// the token.
    pub price: Option<(f64, f64)>,
    /// The [`EFFORTS`] the model accepts, in slider order.
    pub efforts: Vec<String>,
    /// Whether the model reasons at all.
    pub reasoning: bool,
    /// When the model was released, for newest-first order. Unix seconds.
    pub released: u64,
}

impl RemoteModel {
    /// A stand-in for a model a thread names but no saved list describes:
    /// the gateway still knows it, only its details are unknown.
    #[must_use]
    pub fn unlisted(id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: id.to_string(),
            context_window: 0,
            price: None,
            efforts: Vec::new(),
            reasoning: true,
            released: 0,
        }
    }
}

/// Where `provider` publishes its list, or [`None`] for a provider without one
/// Luma reads.
#[must_use]
pub fn url(provider: Provider) -> Option<&'static str> {
    match provider {
        // Filtered on the server to what a turn can use: text out, tools in.
        Provider::OpenRouter => Some(
            "https://openrouter.ai/api/v1/models?supported_parameters=tools&output_modalities=text&limit=1000",
        ),
        Provider::VercelAiGateway => Some("https://ai-gateway.vercel.sh/v1/models"),
        Provider::Anthropic => None,
    }
}

/// Whether `value` has the shape of a gateway wire id: `creator/model`, in the
/// characters both gateways use. Only the gateway can say whether the model
/// exists; this keeps a free-form string from reaching it.
#[must_use]
pub fn is_wire_id(value: &str) -> bool {
    let Some((creator, model)) = value.split_once('/') else {
        return false;
    };
    let part = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '~'))
    };
    part(creator) && part(model)
}

/// Read `provider`'s list from the gateway, newest first.
///
/// # Errors
///
/// [`ModelError::Transport`] or [`ModelError::Status`] when the list cannot be
/// read, and [`ModelError::Unknown`] for a provider without one.
pub async fn fetch(provider: Provider) -> Result<Vec<RemoteModel>, ModelError> {
    let url = url(provider).ok_or_else(|| ModelError::Unknown(provider.as_str().into()))?;
    let response = reqwest::Client::new()
        .get(url)
        .header("HTTP-Referer", "https://luma.show")
        .header("X-Title", "Luma")
        .send()
        .await
        .map_err(|error| ModelError::Transport(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(ModelError::Status {
            provider: provider.as_str(),
            status: status.as_u16(),
            body: response.text().await.unwrap_or_default(),
        });
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| ModelError::Protocol {
            provider: provider.as_str(),
            detail: error.to_string(),
        })?;
    Ok(parse(provider, &body))
}

/// The models in one list body that a turn can run, newest first. An entry
/// this build cannot read is skipped rather than failing the whole list.
#[must_use]
pub fn parse(provider: Provider, body: &Value) -> Vec<RemoteModel> {
    let entries = body.get("data").and_then(Value::as_array);
    let mut models: Vec<_> = entries
        .into_iter()
        .flatten()
        .filter_map(|entry| match provider {
            Provider::OpenRouter => openrouter(entry),
            Provider::VercelAiGateway => vercel(entry),
            Provider::Anthropic => None,
        })
        .filter(|model| is_wire_id(&model.id))
        .collect();
    models.sort_by(|a, b| b.released.cmp(&a.released));
    models
}

fn openrouter(entry: &Value) -> Option<RemoteModel> {
    let parameters = strings(entry.get("supported_parameters"));
    if !parameters.iter().any(|p| p == "tools") {
        return None;
    }
    let reasoning = entry.get("reasoning").filter(|r| !r.is_null());
    let name = text(entry, "name")?;
    Some(RemoteModel {
        id: text(entry, "id")?,
        // "MoonshotAI: Kimi K3" — the creator is already in the id.
        name: name
            .split_once(": ")
            .map_or(name.as_str(), |(_, rest)| rest)
            .to_string(),
        context_window: window(entry.get("context_length")),
        price: price(entry.get("pricing"), "prompt", "completion"),
        efforts: efforts(reasoning.and_then(|r| r.get("supported_efforts"))),
        reasoning: reasoning.is_some() || parameters.iter().any(|p| p == "reasoning"),
        released: entry.get("created").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn vercel(entry: &Value) -> Option<RemoteModel> {
    let tags = strings(entry.get("tags"));
    if text(entry, "type").as_deref() != Some("language") || !tags.iter().any(|t| t == "tool-use") {
        return None;
    }
    let effort = entry
        .get("reasoning_options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|option| option.get("type").and_then(Value::as_str) == Some("effort"));
    Some(RemoteModel {
        id: text(entry, "id")?,
        name: text(entry, "name")?,
        context_window: window(entry.get("context_window")),
        price: price(entry.get("pricing"), "input", "output"),
        efforts: efforts(effort.and_then(|option| option.get("values"))),
        reasoning: tags.iter().any(|t| t == "reasoning"),
        released: ["released", "created"]
            .iter()
            .find_map(|key| entry.get(*key).and_then(Value::as_u64))
            .unwrap_or(0),
    })
}

fn text(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn window(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .and_then(|window| u32::try_from(window).ok())
        .unwrap_or(0)
}

fn efforts(value: Option<&Value>) -> Vec<String> {
    let listed = strings(value);
    EFFORTS
        .into_iter()
        .filter(|level| listed.iter().any(|l| l == level))
        .map(str::to_string)
        .collect()
}

/// Both halves priced per token, as strings; `None` when either is missing or
/// negative (OpenRouter's "priced per request" marker).
fn price(pricing: Option<&Value>, input: &str, output: &str) -> Option<(f64, f64)> {
    let per_million = |key: &str| {
        pricing?
            .get(key)?
            .as_str()?
            .parse::<f64>()
            .ok()
            .filter(|value| *value >= 0.0)
            .map(|value| value * 1_000_000.0)
    };
    Some((per_million(input)?, per_million(output)?))
}

/// The list saved at `path`, or [`None`] when there is none this build can read.
pub async fn read_cache(path: &Path) -> Option<Vec<RemoteModel>> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Save a list to `path`. Written beside and renamed over the old copy, so a
/// reader never sees half a file.
///
/// # Errors
///
/// Any I/O error, as text.
pub async fn write_cache(path: &Path, models: &[RemoteModel]) -> Result<(), String> {
    let json = serde_json::to_vec(models).map_err(|error| error.to_string())?;
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|error| error.to_string())?;
    }
    let partial = path.with_extension("json.partial");
    tokio::fs::write(&partial, json)
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::rename(&partial, path)
        .await
        .map_err(|error| error.to_string())
}

/// The id for `value` on `provider`, registering it from the saved list at
/// `cache` when this process has not met it yet.
///
/// What a turn calls before it resolves its model: a thread keeps only the
/// wire id, and the table's run-time half is empty after a restart.
///
/// # Errors
///
/// [`ModelError::Unknown`] when `value` is neither in the model table nor a
/// gateway wire id.
pub async fn ensure(provider: Provider, value: &str, cache: &Path) -> Result<ModelId, ModelError> {
    if let Some(id) = ModelId::resolve(value, provider) {
        return Ok(id);
    }
    if url(provider).is_none() || !is_wire_id(value) {
        return Err(ModelError::Unknown(value.to_string()));
    }
    let listed = read_cache(cache)
        .await
        .and_then(|models| models.into_iter().find(|model| model.id == value));
    super::register(
        provider,
        &listed.unwrap_or_else(|| RemoteModel::unlisted(value)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Two entries in OpenRouter's shape, trimmed from a live read: one
    /// reasoning model with its own effort list, one without reasoning.
    fn openrouter_body() -> Value {
        json!({ "data": [
            { "id": "openai/gpt-4o-mini", "name": "OpenAI: GPT-4o-mini", "created": 1_721_260_800,
              "context_length": 128_000,
              "pricing": { "prompt": "0.00000015", "completion": "0.0000006" },
              "supported_parameters": ["tools"], "reasoning": null },
            { "id": "moonshotai/kimi-k3", "name": "MoonshotAI: Kimi K3", "created": 1_784_215_858,
              "context_length": 1_048_576,
              "pricing": { "prompt": "0.00000189", "completion": "0.00000948" },
              "supported_parameters": ["reasoning", "tools"],
              "reasoning": { "supported_efforts": ["max", "high", "low"] } },
            { "id": "openrouter/auto", "name": "Auto Router", "created": 1,
              "pricing": { "prompt": "-1", "completion": "-1" },
              "supported_parameters": ["tools"] },
            { "id": "acme/no-tools", "name": "No Tools", "supported_parameters": ["temperature"] }
        ]})
    }

    #[test]
    fn openrouter_entries_become_runnable_models_newest_first() {
        let models = parse(Provider::OpenRouter, &openrouter_body());
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [
                "moonshotai/kimi-k3",
                "openai/gpt-4o-mini",
                "openrouter/auto"
            ]
        );
        let kimi = &models[0];
        assert_eq!(kimi.name, "Kimi K3");
        assert_eq!(kimi.context_window, 1_048_576);
        assert_eq!(kimi.efforts, ["low", "high"]);
        assert!(kimi.reasoning);
        let (input, output) = kimi.price.expect("priced");
        assert!((input - 1.89).abs() < 1e-9 && (output - 9.48).abs() < 1e-9);
        assert!(!models[1].reasoning && models[1].efforts.is_empty());
        assert_eq!(
            models[2].price, None,
            "per-request pricing is not a token price"
        );
    }

    #[test]
    fn vercel_lists_only_tool_using_language_models() {
        let body = json!({ "data": [
            { "id": "moonshotai/kimi-k3", "name": "Kimi K3", "type": "language",
              "released": 1_780_000_000, "context_window": 1_000_000,
              "tags": ["reasoning", "tool-use"],
              "pricing": { "input": "0.000003", "output": "0.000015" },
              "reasoning_options": [{ "type": "effort", "values": ["low", "high", "max"] }] },
            { "id": "spacexai/grok-4.5", "name": "Grok 4.5", "type": "language",
              "released": 1_770_000_000, "context_window": 500_000,
              "tags": ["reasoning", "tool-use"],
              "pricing": { "input": "0.000002", "output": "0.000006" } },
            { "id": "google/gemini-3-pro-image", "name": "Nano Banana Pro", "type": "language",
              "tags": ["image-generation"] },
            { "id": "openai/text-embedding-3", "name": "Embedding", "type": "embedding",
              "tags": ["tool-use"] }
        ]});
        let models = parse(Provider::VercelAiGateway, &body);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["moonshotai/kimi-k3", "spacexai/grok-4.5"]
        );
        assert_eq!(models[0].efforts, ["low", "high"]);
        assert!(models[1].reasoning && models[1].efforts.is_empty());
        assert_eq!(models[1].context_window, 500_000);
    }

    #[test]
    fn a_wire_id_is_creator_slash_model() {
        for good in [
            "openai/gpt-5.6-sol",
            "moonshotai/kimi-k3:batch",
            "~moonshotai/kimi-latest",
        ] {
            assert!(is_wire_id(good), "{good}");
        }
        for bad in ["gpt-5", "a/", "/b", "a/b/c", "a/b c", ""] {
            assert!(!is_wire_id(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_saved_list_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/openrouter.json");
        assert!(read_cache(&path).await.is_none());
        let models = parse(Provider::OpenRouter, &openrouter_body());
        write_cache(&path, &models).await.expect("written");
        assert_eq!(read_cache(&path).await, Some(models));
    }

    /// Both live lists still parse into a useful number of priced models.
    /// Run with `cargo test --lib both_live_lists -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "live: needs a network"]
    async fn both_live_lists_parse() {
        for provider in [Provider::OpenRouter, Provider::VercelAiGateway] {
            let models = fetch(provider).await.expect("the list was read");
            let priced = models.iter().filter(|m| m.price.is_some()).count();
            println!(
                "{}: {} models, {priced} priced, newest {:?}",
                provider.as_str(),
                models.len(),
                models.iter().take(3).map(|m| &m.id).collect::<Vec<_>>()
            );
            assert!(
                models.len() > 50,
                "{} listed {}",
                provider.as_str(),
                models.len()
            );
            assert!(priced * 2 > models.len());
        }
    }

    /// A thread that names a listed model runs it with the list's details; one
    /// the list has never carried still runs, on the fallback window.
    #[tokio::test]
    async fn a_turn_registers_its_model_from_the_saved_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("openrouter.json");
        write_cache(&path, &parse(Provider::OpenRouter, &openrouter_body()))
            .await
            .expect("written");

        let listed = ensure(Provider::OpenRouter, "openai/gpt-4o-mini", &path)
            .await
            .expect("registered");
        assert_eq!(listed.key(), "openai/gpt-4o-mini");
        assert_eq!(listed.context_window(), 128_000);
        assert_eq!(
            listed.wire_id(Provider::OpenRouter).expect("routes"),
            "openai/gpt-4o-mini"
        );
        assert!(
            listed.wire_id(Provider::VercelAiGateway).is_err(),
            "registered for one gateway only"
        );
        assert_eq!(
            listed.spec().default_reasoning,
            super::super::ReasoningLevel::Off
        );

        let unlisted = ensure(Provider::OpenRouter, "acme/unlisted-model", &path)
            .await
            .expect("registered");
        assert_eq!(unlisted.context_window(), FALLBACK_CONTEXT_WINDOW);

        assert!(ensure(Provider::OpenRouter, "not a model", &path)
            .await
            .is_err());
        assert!(ensure(Provider::Anthropic, "acme/model", &path)
            .await
            .is_err());
        // The static table still wins.
        assert_eq!(
            ensure(Provider::OpenRouter, "moonshotai/kimi-k3-fast", &path)
                .await
                .expect("known")
                .key(),
            "kimi-k3-fast"
        );
    }
}
