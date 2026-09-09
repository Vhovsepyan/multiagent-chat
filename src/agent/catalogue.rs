//! Which providers and models this installation offers, and the one place a
//! request is turned into a frozen `AgentSelection`.
//!
//! Task 0005. The catalogue is built once from `Config` at startup. It holds
//! only names — never a key, never raw config — because it is also what the
//! browser is allowed to see.

use serde::Serialize;

use crate::agent::selection::{
    AgentSelection, AgentSelectionRequest, ChatAgentConfig, ChatAgentRequest, ChatProvider,
    CodingAgentConfig, CodingAgentRequest, CodingTool,
};
use crate::config::Config;

/// The models configured for one provider or tool, and which is preselected.
///
/// `default_model` is always present in `models`, so a default can never be a
/// selection the validator would reject.
#[derive(Debug, Clone, Serialize)]
pub struct ModelOptions {
    pub id: &'static str,
    pub label: &'static str,
    pub models: Vec<String>,
    pub default_model: String,
}

impl ModelOptions {
    fn new(id: &'static str, label: &'static str, configured: &[String], default: &str) -> Self {
        let mut models = Vec::with_capacity(configured.len() + 1);
        models.push(default.to_string());
        for model in configured {
            if !models.iter().any(|known| known == model) {
                models.push(model.clone());
            }
        }
        ModelOptions {
            id,
            label,
            models,
            default_model: default.to_string(),
        }
    }

    fn offers(&self, model: &str) -> bool {
        self.models.iter().any(|known| known == model)
    }
}

/// Everything the UI may know about agent choices.
#[derive(Debug, Clone, Serialize)]
pub struct AgentCatalogue {
    gemini: ModelOptions,
    anthropic: ModelOptions,
    claude_code: ModelOptions,
}

impl AgentCatalogue {
    pub fn from_config(config: &Config) -> Self {
        AgentCatalogue {
            gemini: ModelOptions::new(
                ChatProvider::Gemini.id(),
                ChatProvider::Gemini.label(),
                &config.gemini_models,
                &config.gemini_model,
            ),
            anthropic: ModelOptions::new(
                ChatProvider::Anthropic.id(),
                ChatProvider::Anthropic.label(),
                &config.anthropic_models,
                &config.critic_model,
            ),
            claude_code: ModelOptions::new(
                CodingTool::ClaudeCode.id(),
                CodingTool::ClaudeCode.label(),
                &config.claude_code_models,
                &config.implementer_model,
            ),
        }
    }

    pub fn chat_models(&self, provider: ChatProvider) -> &ModelOptions {
        match provider {
            ChatProvider::Gemini => &self.gemini,
            ChatProvider::Anthropic => &self.anthropic,
        }
    }

    pub fn coding_models(&self, tool: CodingTool) -> &ModelOptions {
        match tool {
            CodingTool::ClaudeCode => &self.claude_code,
        }
    }

    /// What a task gets when the user chooses nothing: the previous behavior.
    pub fn defaults(&self) -> AgentSelection {
        AgentSelection {
            proposer: ChatAgentConfig::new(ChatProvider::Gemini, self.gemini.default_model.clone()),
            critic: ChatAgentConfig::new(
                ChatProvider::Anthropic,
                self.anthropic.default_model.clone(),
            ),
            worker: CodingAgentConfig::new(
                CodingTool::ClaudeCode,
                self.claude_code.default_model.clone(),
            ),
        }
    }

    /// Turn a request into the selection the task will keep for its lifetime.
    ///
    /// An unset field falls back to the default; a set-but-unknown one is an
    /// error. A rejected selection is never quietly replaced by a working one —
    /// building against a model the user did not ask for is worse than failing.
    pub fn resolve(
        &self,
        request: Option<&AgentSelectionRequest>,
    ) -> Result<AgentSelection, String> {
        let empty = AgentSelectionRequest::default();
        let request = request.unwrap_or(&empty);
        Ok(AgentSelection {
            proposer: self.resolve_chat(
                "proposer",
                request.proposer.as_ref(),
                ChatProvider::Gemini,
            )?,
            critic: self.resolve_chat(
                "critic",
                request.critic.as_ref(),
                ChatProvider::Anthropic,
            )?,
            worker: self.resolve_worker(request.worker.as_ref())?,
        })
    }

    fn resolve_chat(
        &self,
        role: &str,
        request: Option<&ChatAgentRequest>,
        fallback: ChatProvider,
    ) -> Result<ChatAgentConfig, String> {
        let provider = request.and_then(|r| r.provider).unwrap_or(fallback);
        let options = self.chat_models(provider);
        let model = match request.and_then(|r| r.model.as_deref()) {
            None => options.default_model.clone(),
            Some(model) if model.trim().is_empty() => {
                return Err(format!("{role} model cannot be empty"));
            }
            Some(model) if options.offers(model) => model.to_string(),
            Some(model) => return Err(unknown_model(role, provider.label(), model, options)),
        };
        Ok(ChatAgentConfig::new(provider, model))
    }

    fn resolve_worker(
        &self,
        request: Option<&CodingAgentRequest>,
    ) -> Result<CodingAgentConfig, String> {
        let tool = request
            .and_then(|r| r.tool)
            .unwrap_or(CodingTool::ClaudeCode);
        let options = self.coding_models(tool);
        let model = match request.and_then(|r| r.model.as_deref()) {
            None => options.default_model.clone(),
            Some(model) if model.trim().is_empty() => {
                return Err("worker model cannot be empty".into());
            }
            Some(model) if options.offers(model) => model.to_string(),
            Some(model) => return Err(unknown_model("worker", tool.label(), model, options)),
        };
        Ok(CodingAgentConfig::new(tool, model))
    }
}

/// A message that names what is on offer, so the user can fix the request
/// without reading the server configuration.
fn unknown_model(role: &str, provider: &str, model: &str, options: &ModelOptions) -> String {
    format!(
        "{role} model {model:?} is not configured for {provider}; configured models: {}",
        options.models.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue() -> AgentCatalogue {
        let mut config = crate::agent::test_config();
        config.gemini_models = vec!["gemini-fast".into()];
        config.anthropic_models = vec!["claude-extra".into()];
        AgentCatalogue::from_config(&config)
    }

    /// Required test 1: no selection means exactly what the app did before.
    #[test]
    fn an_empty_request_resolves_to_the_configured_defaults() {
        let resolved = catalogue().resolve(None).unwrap();

        assert_eq!(resolved.proposer.provider, ChatProvider::Gemini);
        assert_eq!(resolved.proposer.model, "proposer-model");
        assert_eq!(resolved.critic.provider, ChatProvider::Anthropic);
        assert_eq!(resolved.critic.model, "critic-model");
        assert_eq!(resolved.worker.tool, CodingTool::ClaudeCode);
        assert_eq!(resolved.worker.model, "worker-model");
    }

    /// The configured default is always offered, even if the model list left
    /// it out — otherwise the default itself would fail validation.
    #[test]
    fn the_default_model_is_always_offered() {
        let catalogue = catalogue();
        let gemini = catalogue.chat_models(ChatProvider::Gemini);

        assert_eq!(gemini.default_model, "proposer-model");
        assert_eq!(gemini.models, vec!["proposer-model", "gemini-fast"]);
    }

    #[test]
    fn a_configured_model_is_accepted_for_its_provider() {
        let request = AgentSelectionRequest {
            proposer: Some(ChatAgentRequest {
                provider: Some(ChatProvider::Anthropic),
                model: Some("claude-extra".into()),
            }),
            ..Default::default()
        };

        let resolved = catalogue().resolve(Some(&request)).unwrap();

        assert_eq!(resolved.proposer.provider, ChatProvider::Anthropic);
        assert_eq!(resolved.proposer.model, "claude-extra");
        // The roles are independent: the critic keeps its default.
        assert_eq!(resolved.critic.model, "critic-model");
    }

    /// Required test 6: a model configured for one provider is not accepted
    /// for another.
    #[test]
    fn a_model_from_another_provider_is_rejected() {
        let request = AgentSelectionRequest {
            proposer: Some(ChatAgentRequest {
                provider: Some(ChatProvider::Gemini),
                model: Some("claude-extra".into()),
            }),
            ..Default::default()
        };

        let error = catalogue().resolve(Some(&request)).unwrap_err();

        assert!(error.contains("claude-extra"), "unexpected: {error}");
        assert!(error.contains("Gemini"), "unexpected: {error}");
    }

    /// Required test 8: an explicitly empty model is a mistake, not a default.
    #[test]
    fn an_empty_model_is_rejected_rather_than_defaulted() {
        for request in [
            AgentSelectionRequest {
                critic: Some(ChatAgentRequest {
                    provider: None,
                    model: Some("   ".into()),
                }),
                ..Default::default()
            },
            AgentSelectionRequest {
                worker: Some(CodingAgentRequest {
                    tool: None,
                    model: Some(String::new()),
                }),
                ..Default::default()
            },
        ] {
            let error = catalogue().resolve(Some(&request)).unwrap_err();
            assert!(error.contains("cannot be empty"), "unexpected: {error}");
        }
    }

    /// Choosing a provider without a model is legitimate: it means "that
    /// provider, its default model".
    #[test]
    fn a_provider_without_a_model_uses_that_providers_default() {
        let request = AgentSelectionRequest {
            proposer: Some(ChatAgentRequest {
                provider: Some(ChatProvider::Anthropic),
                model: None,
            }),
            ..Default::default()
        };

        let resolved = catalogue().resolve(Some(&request)).unwrap();

        assert_eq!(resolved.proposer.model, "critic-model");
    }

    #[test]
    fn an_unknown_worker_model_is_rejected() {
        let request = AgentSelectionRequest {
            worker: Some(CodingAgentRequest {
                tool: Some(CodingTool::ClaudeCode),
                model: Some("not-configured".into()),
            }),
            ..Default::default()
        };

        let error = catalogue().resolve(Some(&request)).unwrap_err();

        assert!(error.contains("Claude Code"), "unexpected: {error}");
    }
}
