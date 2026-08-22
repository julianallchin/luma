//! The model seam: one trait, several transports, one model table.
//!
//! Provider and model are separate axes. "Kimi K3 Fast" is a *model id routed
//! over OpenRouter*, not a third implementation — conflating the two is what
//! produced four drifting model-id lists in the TypeScript stack, so [`MODELS`]
//! is the single table every caller reads (the settings picker, the graph
//! agent, the venue expert, a subagent's `model` override).

pub mod anthropic;
pub mod openrouter;
pub mod scripted;
mod sse;

use std::fmt;

use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

/// A streaming chat completion, provider-independent.
pub trait ModelClient: Send + Sync + 'static {
    /// Start one model step. The stream ends after [`ModelEvent::StepEnded`],
    /// or at the first error. Dropping it aborts the request.
    fn stream(&self, request: ModelRequest) -> BoxStream<'static, Result<ModelEvent, ModelError>>;
}

/// A **delta** vocabulary, deliberately unlike the snapshot-and-diff shape the
/// TypeScript stack inherited: the streaming renderer has to know which
/// characters are new, and a snapshot cannot say.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ModelEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStarted {
        id: String,
        name: String,
    },
    /// Partial JSON for the call's arguments, in arrival order.
    ToolCallArgsDelta {
        id: String,
        json: String,
    },
    ToolCallEnded {
        id: String,
    },
    StepEnded {
        stop_reason: StopReason,
        usage: Usage,
    },
}

/// Why one model step stopped.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    #[default]
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Aborted,
}

impl StopReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::ToolUse => "tool_use",
            StopReason::MaxTokens => "max_tokens",
            StopReason::Refusal => "refusal",
            StopReason::Aborted => "aborted",
        }
    }
}

/// Token accounting for one step. Fields the provider omits stay zero.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

/// Everything one model step needs.
#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub model: ModelId,
    pub system: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSpec>,
    pub reasoning: ReasoningLevel,
    pub max_tokens: u32,
}

/// One tool as the provider sees it.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub schema: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRole {
    User,
    Assistant,
}

/// One provider-facing message. Tool results ride in a `User` message, which is
/// the Anthropic shape; the OpenAI-completions transport lowers them to `tool`
/// messages on its way out.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub content: Vec<ContentBlock>,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ContentBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        content: Vec<ContentBlock>,
        is_error: bool,
    },
    /// Base64 image data — how `preview` and matplotlib figures reach a vision
    /// model.
    Image {
        media_type: String,
        data: String,
    },
}

/// How much the model should think before answering. Providers that have no
/// such control ignore it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    Off,
    Low,
    #[default]
    Medium,
    High,
}

impl ReasoningLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningLevel::Off => "off",
            ReasoningLevel::Low => "low",
            ReasoningLevel::Medium => "medium",
            ReasoningLevel::High => "high",
        }
    }
}

/// Where a model's tokens come from. A provider is a *transport plus key*, not
/// a catalogue: the same model can be reachable through more than one.
///
/// The two gateways are what Luma is actually configured for — one key reaches
/// every model. [`Provider::Anthropic`] is direct, first-party access: a
/// supported alternative, but one nobody gets by default, because reaching it
/// needs a second key for a subset of the same models.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    VercelAiGateway,
    OpenRouter,
    Anthropic,
}

/// Providers in preference order: a gateway before a first-party API, so a
/// model both can serve is billed through the key the user already has.
const PROVIDER_PREFERENCE: [Provider; 3] = [
    Provider::VercelAiGateway,
    Provider::OpenRouter,
    Provider::Anthropic,
];

impl Provider {
    /// The provider Luma uses when the settings table names none.
    pub const DEFAULT: Provider = Provider::VercelAiGateway;

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenRouter => "openrouter",
            Provider::VercelAiGateway => "vercel-ai-gateway",
        }
    }

    /// Resolve a stored `agent_provider` value. An unrecognised value is
    /// [`None`] rather than a guess, so a typo cannot silently move a user's
    /// traffic — and their key — to another service.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        PROVIDER_PREFERENCE
            .into_iter()
            .find(|provider| provider.as_str() == value)
    }

    /// The environment variable checked before the settings table.
    #[must_use]
    pub fn key_env_var(self) -> &'static str {
        match self {
            Provider::Anthropic => "LUMA_ANTHROPIC_API_KEY",
            Provider::OpenRouter => "LUMA_OPENROUTER_API_KEY",
            Provider::VercelAiGateway => "LUMA_AI_GATEWAY_API_KEY",
        }
    }

    /// The `settings` row holding this provider's key.
    #[must_use]
    pub fn key_setting(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic_api_key",
            Provider::OpenRouter => "openrouter_api_key",
            Provider::VercelAiGateway => "ai_gateway_api_key",
        }
    }
}

/// One model, and the wire id it carries on each provider that routes it.
///
/// A `None` route means "this provider does not serve this model", which is how
/// [`ModelId::route`] can answer honestly instead of minting a plausible-looking
/// id the provider will reject.
#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    /// The stable Luma id: what settings, a subagent override and the picker
    /// all store.
    pub key: &'static str,
    pub display: &'static str,
    pub anthropic: Option<&'static str>,
    pub openrouter: Option<&'static str>,
    pub gateway: Option<&'static str>,
    pub default_reasoning: ReasoningLevel,
}

/// The one model table.
///
/// Both gateways address models in the same `creator/model` space, so those two
/// columns usually agree; they are still separate because either service can
/// stop carrying a model without the other noticing.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        key: "claude-opus-5",
        display: "Claude Opus 5",
        anthropic: Some("claude-opus-5"),
        openrouter: Some("anthropic/claude-opus-5"),
        gateway: Some("anthropic/claude-opus-5"),
        default_reasoning: ReasoningLevel::High,
    },
    ModelSpec {
        key: "kimi-k3-fast",
        display: "Kimi K3 Fast",
        anthropic: None,
        openrouter: Some("moonshotai/kimi-k3-fast"),
        gateway: Some("moonshotai/kimi-k3-fast"),
        default_reasoning: ReasoningLevel::Medium,
    },
    ModelSpec {
        key: "grok-4.5",
        display: "Grok 4.5",
        anthropic: None,
        openrouter: Some("x-ai/grok-4.5"),
        gateway: Some("xai/grok-4.5"),
        default_reasoning: ReasoningLevel::Medium,
    },
];

/// The model Luma picks when nothing has been chosen.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// The model the settings table selects.
///
/// The one place `agent_model` is read: a turn resolves its client through it
/// and the composer's chip names it through it, so the panel cannot advertise
/// a model the next turn would not use. A stored value naming a model this
/// build dropped falls back to [`DEFAULT_MODEL`] rather than failing — the
/// picker's list retires with the TypeScript loop and stale rows outlive it.
///
/// # Errors
///
/// [`ModelError::Unknown`] only if [`DEFAULT_MODEL`] is itself missing from
/// [`MODELS`], which a test forbids.
pub fn configured(
    settings: &std::collections::HashMap<String, String>,
) -> Result<ModelId, ModelError> {
    let requested = settings
        .get("agent_model")
        .map_or(DEFAULT_MODEL, String::as_str);
    ModelId::parse(requested)
        .or_else(|| ModelId::parse(DEFAULT_MODEL))
        .ok_or_else(|| ModelError::Unknown(requested.to_string()))
}

/// The model, the effort and the live transport the settings table selects.
///
/// The whole of "which provider, which key, which client" — a caller that only
/// wants to run a turn should not have to know that a gateway and the
/// first-party API share a transport, or which env var backs which service.
///
/// # Errors
///
/// [`ModelError::Unroutable`] if no configured provider serves the chosen
/// model, or [`ModelError::NotConfigured`] if the one that does has no key.
pub async fn configured_client(
    settings: &std::collections::HashMap<String, String>,
    pool: &SqlitePool,
) -> Result<(std::sync::Arc<dyn ModelClient>, ModelId, ReasoningLevel), ModelError> {
    let id = configured(settings)?;
    let preferred = settings
        .get("agent_provider")
        .map(String::as_str)
        .and_then(Provider::parse)
        .unwrap_or(Provider::DEFAULT);
    let (provider, _) = id.route(preferred)?;
    let key = api_key(provider, pool).await?;
    let client: std::sync::Arc<dyn ModelClient> = match provider {
        Provider::VercelAiGateway => std::sync::Arc::new(anthropic::AnthropicClient::gateway(key)),
        Provider::Anthropic => std::sync::Arc::new(anthropic::AnthropicClient::new(key)),
        Provider::OpenRouter => std::sync::Arc::new(openrouter::OpenRouterClient::new(key)),
    };
    Ok((client, id, id.spec().default_reasoning))
}

/// A validated entry of [`MODELS`]. Constructing one is the only way to name a
/// model, so an unvalidated free-form string cannot reach a provider.
#[derive(Clone, Copy, Debug)]
pub struct ModelId(&'static ModelSpec);

impl ModelId {
    /// Look a model up by its stable Luma key, or by any provider wire id it
    /// carries — settings written by the TypeScript stack stored wire ids.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        MODELS
            .iter()
            .find(|spec| {
                spec.key == value
                    || spec.anthropic == Some(value)
                    || spec.openrouter == Some(value)
                    || spec.gateway == Some(value)
            })
            .map(ModelId)
    }

    #[must_use]
    pub fn spec(self) -> &'static ModelSpec {
        self.0
    }

    #[must_use]
    pub fn key(self) -> &'static str {
        self.0.key
    }

    /// The id `provider` knows this model by.
    ///
    /// What a transport asks, having already been handed its provider — it must
    /// never fall back, or one service's id goes out over another's connection.
    ///
    /// # Errors
    ///
    /// [`ModelError::Unroutable`] if that provider does not serve the model.
    pub fn wire_id(self, provider: Provider) -> Result<&'static str, ModelError> {
        match provider {
            Provider::Anthropic => self.0.anthropic,
            Provider::OpenRouter => self.0.openrouter,
            Provider::VercelAiGateway => self.0.gateway,
        }
        .ok_or(ModelError::Unroutable(self.0.key))
    }

    /// The provider to use and the wire id to send, preferring `preferred` and
    /// falling back to whatever provider actually routes this model.
    ///
    /// # Errors
    ///
    /// [`ModelError::Unroutable`] if no provider serves it.
    pub fn route(self, preferred: Provider) -> Result<(Provider, &'static str), ModelError> {
        if let Ok(id) = self.wire_id(preferred) {
            return Ok((preferred, id));
        }
        PROVIDER_PREFERENCE
            .into_iter()
            .find_map(|provider| Some((provider, self.wire_id(provider).ok()?)))
            .ok_or(ModelError::Unroutable(self.0.key))
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.key)
    }
}

/// Why a model call could not be made, or did not finish.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    /// No key for the provider that was actually selected — which the message
    /// names, unlike the TypeScript stack's fixed "OpenRouter" text.
    #[error("no API key for {0}: set {1} or store it in settings")]
    NotConfigured(&'static str, &'static str),
    #[error("unknown model '{0}'")]
    Unknown(String),
    #[error("no provider routes model '{0}'")]
    Unroutable(&'static str),
    #[error("{0}")]
    Transport(String),
    #[error("{provider} returned {status}: {body}")]
    Status {
        provider: &'static str,
        status: u16,
        body: String,
    },
    #[error("malformed stream from {provider}: {detail}")]
    Protocol {
        provider: &'static str,
        detail: String,
    },
}

/// Resolve a provider's key: environment first (headless, CI, tests), then the
/// settings table. A macOS keychain, if it ever lands, lands here — this is the
/// one function that answers the question.
///
/// # Errors
///
/// [`ModelError::NotConfigured`] naming the provider that was actually wanted.
pub async fn api_key(provider: Provider, pool: &SqlitePool) -> Result<String, ModelError> {
    if let Ok(key) = std::env::var(provider.key_env_var()) {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }
    let settings = crate::database::local::settings::get_all_settings(pool)
        .await
        .map_err(ModelError::Transport)?;
    settings
        .get(provider.key_setting())
        .map(String::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .ok_or(ModelError::NotConfigured(
            provider.as_str(),
            provider.key_env_var(),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_resolve_by_key_and_by_wire_id() {
        assert_eq!(
            ModelId::parse("anthropic/claude-opus-5").map(ModelId::key),
            Some("claude-opus-5")
        );
        assert_eq!(
            ModelId::parse("moonshotai/kimi-k3-fast").map(ModelId::key),
            Some("kimi-k3-fast")
        );
        assert!(ModelId::parse("gpt-9").is_none());
    }

    #[test]
    fn routing_falls_back_to_a_provider_that_serves_the_model() {
        let kimi = ModelId::parse("kimi-k3-fast").expect("known model");
        assert_eq!(
            kimi.route(Provider::Anthropic).expect("routable"),
            (Provider::VercelAiGateway, "moonshotai/kimi-k3-fast")
        );
        let opus = ModelId::parse("claude-opus-5").expect("known model");
        assert_eq!(
            opus.route(Provider::Anthropic).expect("routable"),
            (Provider::Anthropic, "claude-opus-5")
        );
    }

    /// Nothing may reach the first-party Anthropic API without asking for it:
    /// it is the one provider whose key a gateway user does not have.
    #[test]
    fn the_default_provider_is_a_gateway_for_every_model() {
        assert_ne!(Provider::DEFAULT, Provider::Anthropic);
        for spec in MODELS {
            let (provider, _) = ModelId(spec).route(Provider::DEFAULT).expect("routable");
            assert_ne!(
                provider,
                Provider::Anthropic,
                "'{}' falls through to the first-party API by default",
                spec.key
            );
        }
    }

    #[test]
    fn a_provider_round_trips_through_its_stored_spelling() {
        for provider in PROVIDER_PREFERENCE {
            assert_eq!(Provider::parse(provider.as_str()), Some(provider));
        }
        assert_eq!(Provider::parse("vercel"), None);
    }

    /// One real turn, resolved the way a turn resolves it: settings rows in,
    /// streamed text out. Only the live service can confirm the id space, the
    /// bearer header and the SSE schema all agree.
    ///
    /// Ignored by default — it costs tokens and needs a network. Run with
    /// `cargo test --lib a_live_gateway_turn -- --ignored --nocapture`, with
    /// `LUMA_AI_GATEWAY_API_KEY` set.
    #[tokio::test]
    #[ignore = "live: needs LUMA_AI_GATEWAY_API_KEY and a network"]
    async fn a_live_gateway_turn_streams_text() {
        use futures_util::StreamExt;

        assert!(
            std::env::var(Provider::VercelAiGateway.key_env_var())
                .is_ok_and(|key| !key.trim().is_empty()),
            "no gateway credential on this machine: set {} to smoke-test",
            Provider::VercelAiGateway.key_env_var()
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::database::local::database::init_app_db_at(dir.path())
            .await
            .expect("app db");
        // Exactly the rows the settings screen writes.
        let settings = std::collections::HashMap::from([
            (
                "agent_provider".to_string(),
                "vercel-ai-gateway".to_string(),
            ),
            (
                "agent_model".to_string(),
                "anthropic/claude-opus-5".to_string(),
            ),
        ]);

        let (client, id, reasoning) = configured_client(&settings, &db.0)
            .await
            .expect("the settings table did not resolve to a usable client");
        assert_eq!(id.key(), "claude-opus-5");

        let mut stream = client.stream(ModelRequest {
            model: id,
            system: "Answer in one word.".into(),
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: vec![ContentBlock::Text("Say pong.".into())],
            }],
            tools: Vec::new(),
            reasoning,
            max_tokens: 1024,
        });

        let mut text = String::new();
        let mut ended = None;
        while let Some(event) = stream.next().await {
            match event.expect("the gateway stream carried an error") {
                ModelEvent::TextDelta(delta) => text.push_str(&delta),
                ModelEvent::StepEnded { usage, .. } => ended = Some(usage),
                _ => {}
            }
        }
        let usage = ended.expect("the stream ended without a StepEnded");
        println!("gateway said {text:?} ({usage:?})");
        assert!(!text.trim().is_empty(), "the gateway streamed no text");
        assert!(usage.output_tokens > 0, "the gateway reported no usage");
    }

    /// The tool round trip over the live wire, with the *real* tool spec — the
    /// half the scripted loop cannot check, because the scripted model never
    /// reads `input_schema` and never has to accept a `tool_result` back.
    ///
    /// Two steps, exactly as a turn makes them: declare the tool and let the
    /// model call it, then replay its `tool_use` alongside a `tool_result` and
    /// require it to answer from the result.
    ///
    /// Ignored by default — it costs tokens and needs a network. Run with
    /// `cargo test --lib a_live_gateway_tool -- --ignored --nocapture`, with
    /// `LUMA_AI_GATEWAY_API_KEY` set.
    #[tokio::test]
    #[ignore = "live: needs LUMA_AI_GATEWAY_API_KEY and a network"]
    async fn a_live_gateway_tool_call_round_trips() {
        use crate::agent::tools;
        use futures_util::StreamExt;

        let key = std::env::var(Provider::VercelAiGateway.key_env_var())
            .expect("no gateway credential: set LUMA_AI_GATEWAY_API_KEY to smoke-test");
        let client = anthropic::AnthropicClient::gateway(key);
        let id = ModelId::parse(DEFAULT_MODEL).expect("known model");

        // The shipped registry's own spec, not a hand-written stand-in: the
        // schema a real turn sends is the thing under test.
        let specs = tools::registry(crate::agent::AgentKind::TrackCopilot).specs();
        assert_eq!(specs.len(), 1, "the track agent declares one tool");
        println!(
            "input_schema: {}",
            serde_json::to_string(&specs[0].schema).expect("schema")
        );

        let step = |messages: Vec<ModelMessage>| ModelRequest {
            model: id,
            system: "Use the python tool to answer. Do not answer from memory.".into(),
            messages,
            tools: specs.clone(),
            reasoning: id.spec().default_reasoning,
            max_tokens: 4096,
        };

        let mut stream = client.stream(step(vec![ModelMessage {
            role: ModelRole::User,
            content: vec![ContentBlock::Text(
                "Compute 6 * 7 by running a python cell.".into(),
            )],
        }]));

        let (mut call, mut args, mut stop) = (None, String::new(), None);
        while let Some(event) = stream.next().await {
            match event.expect("the gateway stream carried an error") {
                ModelEvent::ToolCallStarted { id, name } => call = Some((id, name)),
                ModelEvent::ToolCallArgsDelta { json, .. } => args.push_str(&json),
                ModelEvent::StepEnded { stop_reason, .. } => stop = Some(stop_reason),
                _ => {}
            }
        }
        let (call_id, name) = call.expect("the gateway declared the tool but streamed no tool_use");
        assert_eq!(name, "python");
        assert_eq!(stop, Some(StopReason::ToolUse));
        let input: Value = serde_json::from_str(args.trim())
            .unwrap_or_else(|error| panic!("tool arguments did not parse: {error} in {args:?}"));
        println!("called {name}({input})");
        assert!(input.get("code").is_some(), "no `code` argument: {input}");

        // Step two: the model's own call, answered.
        let mut stream = client.stream(step(vec![
            ModelMessage {
                role: ModelRole::User,
                content: vec![ContentBlock::Text(
                    "Compute 6 * 7 by running a python cell.".into(),
                )],
            },
            ModelMessage {
                role: ModelRole::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name,
                    input,
                }],
            },
            ModelMessage {
                role: ModelRole::User,
                content: vec![ContentBlock::ToolResult {
                    id: call_id,
                    content: vec![ContentBlock::Text("stdout:\n42".into())],
                    is_error: false,
                }],
            },
        ]));

        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let ModelEvent::TextDelta(delta) =
                event.expect("the gateway rejected the tool_result")
            {
                text.push_str(&delta);
            }
        }
        println!("answered {text:?}");
        assert!(text.contains("42"), "the model did not read the result");
    }
}
