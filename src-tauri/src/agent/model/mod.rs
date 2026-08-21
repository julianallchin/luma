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
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Anthropic,
    OpenRouter,
}

impl Provider {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenRouter => "openrouter",
        }
    }

    /// The environment variable checked before the settings table.
    #[must_use]
    pub fn key_env_var(self) -> &'static str {
        match self {
            Provider::Anthropic => "LUMA_ANTHROPIC_API_KEY",
            Provider::OpenRouter => "LUMA_OPENROUTER_API_KEY",
        }
    }

    /// The `settings` row holding this provider's key.
    #[must_use]
    pub fn key_setting(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic_api_key",
            Provider::OpenRouter => "openrouter_api_key",
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
    pub default_reasoning: ReasoningLevel,
}

/// The one model table.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        key: "claude-opus-5",
        display: "Claude Opus 5",
        anthropic: Some("claude-opus-5"),
        openrouter: Some("anthropic/claude-opus-5"),
        default_reasoning: ReasoningLevel::High,
    },
    ModelSpec {
        key: "kimi-k3-fast",
        display: "Kimi K3 Fast",
        anthropic: None,
        openrouter: Some("moonshotai/kimi-k3-fast"),
        default_reasoning: ReasoningLevel::Medium,
    },
    ModelSpec {
        key: "grok-4.5",
        display: "Grok 4.5",
        anthropic: None,
        openrouter: Some("x-ai/grok-4.5"),
        default_reasoning: ReasoningLevel::Medium,
    },
];

/// The model Luma picks when nothing has been chosen.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

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
                spec.key == value || spec.anthropic == Some(value) || spec.openrouter == Some(value)
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

    /// The provider to use and the wire id to send, preferring `preferred` and
    /// falling back to whatever provider actually routes this model.
    ///
    /// # Errors
    ///
    /// [`ModelError::Unroutable`] if no provider serves it.
    pub fn route(self, preferred: Provider) -> Result<(Provider, &'static str), ModelError> {
        let on = |provider: Provider| match provider {
            Provider::Anthropic => self.0.anthropic,
            Provider::OpenRouter => self.0.openrouter,
        };
        if let Some(id) = on(preferred) {
            return Ok((preferred, id));
        }
        for provider in [Provider::Anthropic, Provider::OpenRouter] {
            if let Some(id) = on(provider) {
                return Ok((provider, id));
            }
        }
        Err(ModelError::Unroutable(self.0.key))
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
            (Provider::OpenRouter, "moonshotai/kimi-k3-fast")
        );
        let opus = ModelId::parse("claude-opus-5").expect("known model");
        assert_eq!(
            opus.route(Provider::Anthropic).expect("routable"),
            (Provider::Anthropic, "claude-opus-5")
        );
    }
}
