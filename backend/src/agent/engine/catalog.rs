use super::{AgentError, Engine};
use crate::agent::model::{self, ModelId, Provider};
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
            let id = self
                .model
                .as_deref()
                .and_then(ModelId::parse)
                .ok_or_else(|| AgentError::Invalid("Choose an API model".into()))?;
            id.wire_id(provider)?;
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
            _ => {
                let provider = service.provider().expect("API service");
                Ok(model::MODELS
                    .iter()
                    .filter(|spec| {
                        ModelId::parse(spec.key).is_some_and(|id| id.wire_id(provider).is_ok())
                    })
                    .map(|spec| ModelChoice {
                        id: Some(spec.key.into()),
                        label: spec.display.into(),
                        resolved_model: Some(spec.key.into()),
                        effort_levels: ["low", "medium", "high"].map(str::to_string).into(),
                    })
                    .collect())
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), models)
        .await
        .map_err(|_| {
            AgentError::Invalid(format!("{} model discovery timed out", service.label()))
        })?
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
