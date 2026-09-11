use super::{AgentError, Engine};
use crate::agent::model::{self, remote, ModelId, Provider};
use crate::models::agent_threads::AgentThread;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Service {
    Claude,
    Codex,
    OpenRouter,
    Vercel,
    Anthropic,
}

impl Service {
    pub const ALL: [Self; 5] = [
        Self::Claude,
        Self::Codex,
        Self::OpenRouter,
        Self::Vercel,
        Self::Anthropic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenRouter => "OpenRouter",
            Self::Vercel => "Vercel AI Gateway",
            Self::Anthropic => "Anthropic API",
        }
    }

    pub fn engine(self) -> Engine {
        match self {
            Self::Claude => Engine::Claude,
            Self::Codex => Engine::Codex,
            _ => Engine::Api,
        }
    }

    pub fn provider(self) -> Option<Provider> {
        match self {
            Self::OpenRouter => Some(Provider::OpenRouter),
            Self::Vercel => Some(Provider::VercelAiGateway),
            Self::Anthropic => Some(Provider::Anthropic),
            _ => None,
        }
    }

    /// Whether this service's models come from a list its gateway publishes
    /// — long, priced, and searched in the picker.
    pub fn lists_models(self) -> bool {
        self.provider().and_then(remote::url).is_some()
    }

    fn of(engine: Engine, provider: Option<&str>) -> Self {
        match engine {
            Engine::Claude => Self::Claude,
            Engine::Codex => Self::Codex,
            Engine::Api => match provider
                .and_then(Provider::parse)
                .unwrap_or(Provider::DEFAULT)
            {
                Provider::OpenRouter => Self::OpenRouter,
                Provider::VercelAiGateway => Self::Vercel,
                Provider::Anthropic => Self::Anthropic,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub service: Service,
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

impl Selection {
    pub fn from_thread(thread: &AgentThread) -> Result<Self, AgentError> {
        Ok(Self {
            service: Service::of(Engine::parse(&thread.engine)?, thread.provider.as_deref()),
            model: thread.model.clone(),
            effort: thread.effort.clone(),
        })
    }

    pub fn configured(settings: &HashMap<String, String>) -> Result<Self, AgentError> {
        if let Some(saved) = settings.get("agent_selection") {
            let selection: Self = serde_json::from_str(saved).map_err(|error| {
                AgentError::Storage(format!("Invalid saved model selection: {error}"))
            })?;
            selection.validate()?;
            return Ok(selection);
        }
        let engine = Engine::configured(settings)?;
        let mut service = Service::of(engine, settings.get("agent_provider").map(String::as_str));
        let model = if engine == Engine::Api {
            let model = model::configured(settings)?;
            let provider = model.route(service.provider().expect("API provider"))?.0;
            service = Service::of(engine, Some(provider.as_str()));
            Some(model.spec().key.to_string())
        } else {
            settings
                .get(&format!("agent_{}_model", engine.key()))
                .filter(|s| !s.trim().is_empty())
                .cloned()
        };
        Ok(Self {
            service,
            model,
            effort: None,
        })
    }

    pub fn validate(&self) -> Result<(), AgentError> {
        if let Some(effort) = &self.effort {
            if effort.trim().is_empty() {
                return Err(AgentError::Invalid(format!("Unknown effort: {effort}")));
            }
            if self.service.provider().is_some() {
                self.api_reasoning()?;
            }
        }
        if let Some(provider) = self.service.provider() {
            let model = self
                .model
                .as_deref()
                .ok_or_else(|| AgentError::Invalid("Choose an API model".into()))?;
            match ModelId::resolve(model, provider) {
                Some(id) => {
                    id.wire_id(provider)?;
                }
                // Picked from the gateway's own list; the gateway checks it
                // exists when a turn sends it.
                None if remote::url(provider).is_some() && remote::is_wire_id(model) => {}
                None => return Err(AgentError::Invalid("Choose an API model".into())),
            }
        } else if self
            .model
            .as_ref()
            .is_some_and(|model| model.trim().is_empty())
        {
            return Err(AgentError::Invalid("Model cannot be empty".into()));
        }
        Ok(())
    }

    pub fn api_reasoning(&self) -> Result<Option<model::ReasoningLevel>, AgentError> {
        self.effort
            .as_ref()
            .map(|effort| {
                serde_json::from_value(serde_json::json!(effort))
                    .map_err(|_| AgentError::Invalid(format!("Unsupported API effort: {effort}")))
            })
            .transpose()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelChoice {
    pub id: Option<String>,
    pub label: String,
    pub resolved_model: Option<String>,
    pub effort_levels: Vec<String>,
    /// Prompt tokens the model accepts, when its list says.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// USD per million input and output tokens, when its list says.
    #[serde(default)]
    pub price: Option<(f64, f64)>,
}

impl ModelChoice {
    pub fn matches(&self, model: &Option<String>) -> bool {
        &self.id == model || (self.id.is_some() && model.is_some() && &self.resolved_model == model)
    }

    pub fn selection(&self, service: Service, previous: &Selection) -> Selection {
        Selection {
            service,
            model: self
                .id
                .as_ref()
                .map(|id| self.resolved_model.clone().unwrap_or_else(|| id.clone())),
            effort: previous
                .effort
                .clone()
                .filter(|effort| self.effort_levels.contains(effort)),
        }
    }
}

pub async fn models(
    service: Service,
    cwd: &std::path::Path,
) -> Result<Vec<ModelChoice>, AgentError> {
    let models = async {
        match service {
            Service::Claude => super::claude::models(cwd).await,
            Service::Codex => super::codex::models(cwd).await,
            _ => Ok(table_choices(service.provider().expect("API service"))),
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), models)
        .await
        .map_err(|_| {
            AgentError::Invalid(format!("{} model discovery timed out", service.label()))
        })?
}

/// Read `provider`'s list from the gateway and save it at `cache`. When the
/// gateway cannot be reached, the saved copy, so the picker is never empty
/// only because the network is.
///
/// # Errors
///
/// When the gateway cannot be reached and nothing is saved.
pub async fn refresh_gateway(
    provider: Provider,
    cache: &std::path::Path,
) -> Result<Vec<ModelChoice>, AgentError> {
    let read = tokio::time::timeout(std::time::Duration::from_secs(15), remote::fetch(provider))
        .await
        .unwrap_or_else(|_| {
            Err(model::ModelError::Transport(format!(
                "{} model list timed out",
                provider.as_str()
            )))
        });
    match read {
        Ok(models) => {
            // A cache that cannot be written costs the next open its instant
            // rows, not this one its list.
            if let Err(error) = remote::write_cache(cache, &models).await {
                eprintln!(
                    "[models] could not save the {} list: {error}",
                    provider.as_str()
                );
            }
            Ok(api_choices(provider, models))
        }
        Err(error) => match remote::read_cache(cache).await {
            Some(models) => Ok(api_choices(provider, models)),
            None => Err(error.into()),
        },
    }
}

/// `provider`'s list as last saved at `cache`, for the rows the picker shows
/// while [`refresh_gateway`] is on the wire.
pub async fn cached_gateway(
    provider: Provider,
    cache: &std::path::Path,
) -> Option<Vec<ModelChoice>> {
    remote::read_cache(cache)
        .await
        .map(|models| api_choices(provider, models))
}

/// The table's models that `provider` routes: what a service that publishes
/// no list of its own (the first-party API) offers.
fn table_choices(provider: Provider) -> Vec<ModelChoice> {
    model::MODELS
        .iter()
        .filter_map(|spec| {
            ModelId::parse(spec.key)?.wire_id(provider).ok()?;
            Some(ModelChoice {
                id: Some(spec.key.into()),
                label: spec.display.into(),
                resolved_model: Some(spec.key.into()),
                effort_levels: remote::EFFORTS.map(str::to_string).into(),
                context_window: Some(spec.context_window),
                price: None,
            })
        })
        .collect()
}

/// A gateway's list as picker rows: only what the gateway lists, in its
/// order. A row the model table also carries is chosen under the table's key,
/// so a thread that saved that key still finds its row.
fn api_choices(provider: Provider, listed: Vec<remote::RemoteModel>) -> Vec<ModelChoice> {
    listed
        .into_iter()
        .map(|model| {
            let key = ModelId::resolve(&model.id, provider)
                .filter(|id| id.wire_id(provider).ok() == Some(model.id.as_str()))
                .map(|id| id.key().to_string());
            ModelChoice {
                resolved_model: Some(key.unwrap_or_else(|| model.id.clone())),
                id: Some(model.id),
                label: model.name,
                effort_levels: model.efforts,
                context_window: (model.context_window > 0).then_some(model.context_window),
                price: model.price,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_pin_versions_and_drop_unsupported_effort() {
        let model = ModelChoice {
            id: Some("sonnet".into()),
            resolved_model: Some("claude-sonnet-5".into()),
            label: "Sonnet 5".into(),
            effort_levels: vec!["low".into(), "high".into()],
            context_window: None,
            price: None,
        };
        let previous = Selection {
            service: Service::Claude,
            model: None,
            effort: Some("high".into()),
        };
        let chosen = model.selection(Service::Claude, &previous);
        assert_eq!(chosen.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(chosen.effort.as_deref(), Some("high"));
        assert!(model.matches(&chosen.model));
        assert!(model.matches(&Some("sonnet".into())));
        assert!(model
            .selection(
                Service::Claude,
                &Selection {
                    effort: Some("max".into()),
                    ..previous
                }
            )
            .effort
            .is_none());
        let default = ModelChoice { id: None, ..model };
        assert!(default.matches(&None));
        assert!(!default.matches(&chosen.model));
    }

    #[test]
    fn selections_never_silently_route_to_a_different_service() {
        assert!(Selection {
            service: Service::Anthropic,
            model: Some("kimi-k3-fast".into()),
            effort: None
        }
        .validate()
        .is_err());
        assert!(Selection {
            service: Service::OpenRouter,
            model: Some("kimi-k3-fast".into()),
            effort: None
        }
        .validate()
        .is_ok());
        assert!(Selection {
            service: Service::Vercel,
            model: None,
            effort: None
        }
        .validate()
        .is_err());
    }

    /// A model picked from a gateway's list is a wire id the table does not
    /// carry. It is accepted over the gateways, which publish such lists, and
    /// nowhere else.
    #[test]
    fn a_listed_gateway_model_is_a_valid_selection() {
        let listed = |service| Selection {
            service,
            model: Some("openai/gpt-5.6-sol".into()),
            effort: Some("high".into()),
        };
        assert!(listed(Service::OpenRouter).validate().is_ok());
        assert!(listed(Service::Vercel).validate().is_ok());
        assert!(listed(Service::Anthropic).validate().is_err());
        assert!(Selection {
            service: Service::OpenRouter,
            model: Some("free text".into()),
            effort: None
        }
        .validate()
        .is_err());
    }

    /// A gateway's rows are its list and nothing else. The one row the model
    /// table also carries keeps the table's key, so a selection saved as
    /// `kimi-k3-fast` still finds it.
    #[test]
    fn gateway_rows_are_only_what_the_gateway_lists() {
        let listed = remote::parse(
            Provider::OpenRouter,
            &serde_json::json!({ "data": [
                { "id": "moonshotai/kimi-k3-fast", "name": "MoonshotAI: Kimi K3 Fast",
                  "created": 2, "context_length": 256_000,
                  "pricing": { "prompt": "0.0000045", "completion": "0.00002" },
                  "supported_parameters": ["tools", "reasoning"],
                  "reasoning": { "supported_efforts": ["low", "high", "max"] } },
                { "id": "openai/gpt-5.6-sol", "name": "OpenAI: GPT-5.6 Sol", "created": 3,
                  "context_length": 400_000,
                  "pricing": { "prompt": "0.000001", "completion": "0.000005" },
                  "supported_parameters": ["tools"] }
            ]}),
        );
        let choices = api_choices(Provider::OpenRouter, listed);
        let ids: Vec<_> = choices.iter().filter_map(|c| c.id.as_deref()).collect();
        assert_eq!(ids, ["openai/gpt-5.6-sol", "moonshotai/kimi-k3-fast"]);
        let kimi = &choices[1];
        assert_eq!(kimi.label, "Kimi K3 Fast");
        assert_eq!(kimi.effort_levels, ["low", "high"]);
        assert!(kimi.matches(&Some("kimi-k3-fast".into())));
        let previous = Selection {
            service: Service::OpenRouter,
            model: None,
            effort: None,
        };
        assert_eq!(
            kimi.selection(Service::OpenRouter, &previous)
                .model
                .as_deref(),
            Some("kimi-k3-fast")
        );
        let sol = &choices[0];
        assert_eq!(sol.label, "GPT-5.6 Sol");
        assert_eq!(sol.context_window, Some(400_000));
        assert_eq!(
            sol.selection(Service::OpenRouter, &previous)
                .model
                .as_deref(),
            Some("openai/gpt-5.6-sol")
        );
        assert!(api_choices(Provider::VercelAiGateway, Vec::new()).is_empty());
    }

    #[tokio::test]
    #[ignore = "requires installed Codex and Claude CLIs; makes no inference requests"]
    async fn installed_cli_catalogs_are_discoverable_without_starting_a_turn() {
        let cwd = tempfile::tempdir().unwrap();
        for service in [Service::Claude, Service::Codex] {
            let models = models(service, cwd.path()).await.unwrap();
            assert!(models.iter().any(|model| model.id.is_some()));
            assert!(models.iter().all(|model| !model.label.is_empty()));
            assert!(models.iter().any(|model| !model.effort_levels.is_empty()));
            for model in &models {
                eprintln!(
                    "{}: {} {:?} {:?}",
                    service.label(),
                    model.label,
                    model.resolved_model,
                    model.effort_levels
                );
            }
        }
    }
}
