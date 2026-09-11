//! Which providers and models this installation offers, and the one place a
//! request is turned into a frozen `AgentSelection`.
//!
//! Task 0005. The catalogue is built once from `Config` at startup. It holds
//! only names — never a key, never raw config — because it is also what the
//! browser is allowed to see.
//!
//! Availability is per provider: a chat provider is offered only when its own
//! credential is configured, so an installation with one key is a supported
//! setup rather than a startup failure. The Claude Code worker authenticates
//! itself, so it is always offered.

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
///
/// Only AVAILABLE providers are present, in `ChatProvider::ALL` order, so
/// listing the catalogue and validating against it can never disagree.
#[derive(Debug, Clone)]
pub struct AgentCatalogue {
    chat: Vec<(ChatProvider, ModelOptions)>,
    coding: Vec<(CodingTool, ModelOptions)>,
}

impl AgentCatalogue {
    pub fn from_config(config: &Config) -> Self {
        let mut chat = Vec::new();
        for provider in ChatProvider::ALL {
            let (configured, default) = match provider {
                ChatProvider::Gemini => (&config.gemini_models, &config.gemini_model),
                ChatProvider::Anthropic => (&config.anthropic_models, &config.critic_model),
            };
            if config.chat_credential(provider).is_some() {
                chat.push((
                    provider,
                    ModelOptions::new(provider.id(), provider.label(), configured, default),
                ));
            }
        }

        // Worker tools authenticate themselves, so they do not depend on a
        // chat-provider HTTP credential. Their model allow-lists are separate.
        let coding = CodingTool::ALL
            .into_iter()
            .map(|tool| {
                let (configured, default): (&Vec<String>, &str) = match tool {
                    CodingTool::ClaudeCode => {
                        (&config.claude_code_models, &config.implementer_model)
                    }
                    CodingTool::Codex => (&config.codex_models, &config.codex_model),
                };
                (
                    tool,
                    ModelOptions::new(tool.id(), tool.label(), configured, default),
                )
            })
            .collect();

        AgentCatalogue { chat, coding }
    }

    /// `None` when the provider is not configured in this installation.
    pub fn chat_models(&self, provider: ChatProvider) -> Option<&ModelOptions> {
        self.chat
            .iter()
            .find(|(known, _)| *known == provider)
            .map(|(_, options)| options)
    }

    pub fn coding_models(&self, tool: CodingTool) -> Option<&ModelOptions> {
        self.coding
            .iter()
            .find(|(known, _)| *known == tool)
            .map(|(_, options)| options)
    }

    /// The providers this installation can actually use, for the UI.
    pub fn available_chat_providers(&self) -> Vec<ModelOptions> {
        self.chat
            .iter()
            .map(|(_, options)| options.clone())
            .collect()
    }

    pub fn available_coding_tools(&self) -> Vec<ModelOptions> {
        self.coding
            .iter()
            .map(|(_, options)| options.clone())
            .collect()
    }

    /// What a task gets when the user chooses nothing: the previous behavior.
    ///
    /// Fails — rather than substituting another provider — when a default role
    /// has no credential, so a misconfigured installation is told what to fix.
    pub fn defaults(&self) -> Result<AgentSelection, String> {
        let (proposer, critic) = self.default_chat_pair()?;
        Ok(AgentSelection {
            proposer,
            critic,
            worker: self.default_worker()?,
        })
    }

    /// The two chat roles only, for callers that do not need a worker yet.
    pub fn default_chat_pair(&self) -> Result<(ChatAgentConfig, ChatAgentConfig), String> {
        Ok((
            self.default_chat("proposer", ChatProvider::Gemini)?,
            self.default_chat("critic", ChatProvider::Anthropic)?,
        ))
    }

    /// The worker only. Separate because `--implement-only` runs no debate and
    /// must not require a chat provider at all.
    pub fn default_worker(&self) -> Result<CodingAgentConfig, String> {
        let options = self
            .coding_models(CodingTool::ClaudeCode)
            .ok_or_else(|| unavailable_tool("worker", CodingTool::ClaudeCode))?;
        Ok(CodingAgentConfig::new(
            CodingTool::ClaudeCode,
            options.default_model.clone(),
        ))
    }

    fn default_chat(&self, role: &str, provider: ChatProvider) -> Result<ChatAgentConfig, String> {
        let options = self
            .chat_models(provider)
            .ok_or_else(|| unavailable_provider(role, provider))?;
        Ok(ChatAgentConfig::new(
            provider,
            options.default_model.clone(),
        ))
    }

    /// Turn a request into the selection the task will keep for its lifetime.
    ///
    /// An unset field falls back to the default; a set-but-unknown or
    /// unavailable one is an error. A rejected selection is never quietly
    /// replaced by a working one — building against a provider or model the
    /// user did not ask for is worse than failing.
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
        let options = self
            .chat_models(provider)
            .ok_or_else(|| unavailable_provider(role, provider))?;
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
        let options = self
            .coding_models(tool)
            .ok_or_else(|| unavailable_tool("worker", tool))?;
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

/// Names the variable to set, never any value of it.
fn unavailable_provider(role: &str, provider: ChatProvider) -> String {
    format!(
        "{role} provider {} is not available in this installation; set {} to enable it",
        provider.label(),
        provider.credential_variable()
    )
}

fn unavailable_tool(role: &str, tool: CodingTool) -> String {
    format!(
        "{role} tool {} is not available in this installation",
        tool.label()
    )
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
        AgentCatalogue::from_config(&configured())
    }

    fn configured() -> Config {
        let mut config = crate::agent::test_config();
        config.gemini_models = vec!["gemini-fast".into()];
        config.anthropic_models = vec!["claude-extra".into()];
        config.codex_models = vec!["codex-fast".into()];
        config
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
        let gemini = catalogue.chat_models(ChatProvider::Gemini).unwrap();

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

    #[test]
    fn codex_is_available_with_its_own_default_and_model_allow_list() {
        let catalogue = catalogue();
        let options = catalogue.coding_models(CodingTool::Codex).unwrap();
        assert_eq!(options.default_model, "codex-worker-model");
        assert_eq!(options.models, vec!["codex-worker-model", "codex-fast"]);

        let request = AgentSelectionRequest {
            worker: Some(CodingAgentRequest {
                tool: Some(CodingTool::Codex),
                model: Some("codex-fast".into()),
            }),
            ..Default::default()
        };
        let resolved = catalogue.resolve(Some(&request)).unwrap();
        assert_eq!(resolved.worker.tool, CodingTool::Codex);
        assert_eq!(resolved.worker.model, "codex-fast");
    }

    #[test]
    fn an_unknown_codex_model_is_rejected() {
        let request = AgentSelectionRequest {
            worker: Some(CodingAgentRequest {
                tool: Some(CodingTool::Codex),
                model: Some("not-configured".into()),
            }),
            ..Default::default()
        };
        let error = catalogue().resolve(Some(&request)).unwrap_err();
        assert!(error.contains("Codex"), "unexpected: {error}");
    }

    // --- availability ------------------------------------------------------

    fn only(provider: Option<ChatProvider>) -> AgentCatalogue {
        let mut config = configured();
        config.gemini_api_key = (provider == Some(ChatProvider::Gemini)).then(|| "key".into());
        config.anthropic_api_key =
            (provider == Some(ChatProvider::Anthropic)).then(|| "key".into());
        AgentCatalogue::from_config(&config)
    }

    /// A provider with no credential is not offered at all, and one with a
    /// credential is unaffected by the other one being missing.
    #[test]
    fn only_providers_with_credentials_are_offered() {
        let gemini_only = only(Some(ChatProvider::Gemini));
        assert!(gemini_only.chat_models(ChatProvider::Gemini).is_some());
        assert!(gemini_only.chat_models(ChatProvider::Anthropic).is_none());
        assert_eq!(gemini_only.available_chat_providers().len(), 1);

        let anthropic_only = only(Some(ChatProvider::Anthropic));
        assert!(anthropic_only.chat_models(ChatProvider::Gemini).is_none());
        assert!(
            anthropic_only
                .chat_models(ChatProvider::Anthropic)
                .is_some()
        );

        let neither = only(None);
        assert!(neither.available_chat_providers().is_empty());
    }

    /// The worker does not depend on the Anthropic HTTP key: Claude Code signs
    /// in on its own, so it stays available with no chat provider configured.
    #[test]
    fn the_worker_stays_available_without_any_chat_credential() {
        let neither = only(None);

        assert_eq!(neither.available_coding_tools().len(), 2);
        assert_eq!(neither.default_worker().unwrap().model, "worker-model");
    }

    /// A default role whose provider is unavailable fails with a message that
    /// names the variable to set — and never substitutes the other provider.
    #[test]
    fn an_unavailable_default_provider_fails_with_a_configuration_error() {
        let error = only(Some(ChatProvider::Gemini)).defaults().unwrap_err();
        assert!(error.contains("critic"), "unexpected: {error}");
        assert!(error.contains("Anthropic"), "unexpected: {error}");
        assert!(error.contains("ANTHROPIC_API_KEY"), "unexpected: {error}");

        let error = only(Some(ChatProvider::Anthropic)).defaults().unwrap_err();
        assert!(error.contains("proposer"), "unexpected: {error}");
        assert!(error.contains("GEMINI_API_KEY"), "unexpected: {error}");
    }

    /// With one provider configured, a task that names it for both chat roles
    /// still resolves — availability is per provider, not all-or-nothing.
    #[test]
    fn one_configured_provider_can_serve_both_chat_roles() {
        let request = AgentSelectionRequest {
            proposer: Some(ChatAgentRequest {
                provider: Some(ChatProvider::Anthropic),
                model: None,
            }),
            critic: Some(ChatAgentRequest {
                provider: Some(ChatProvider::Anthropic),
                model: Some("claude-extra".into()),
            }),
            ..Default::default()
        };

        let resolved = only(Some(ChatProvider::Anthropic))
            .resolve(Some(&request))
            .unwrap();

        assert_eq!(resolved.proposer.model, "critic-model");
        assert_eq!(resolved.critic.model, "claude-extra");
        assert_eq!(resolved.worker.model, "worker-model");
    }

    #[test]
    fn selecting_an_unavailable_provider_is_refused() {
        let request = AgentSelectionRequest {
            proposer: Some(ChatAgentRequest {
                provider: Some(ChatProvider::Gemini),
                model: None,
            }),
            ..Default::default()
        };

        let error = only(Some(ChatProvider::Anthropic))
            .resolve(Some(&request))
            .unwrap_err();

        assert!(error.contains("GEMINI_API_KEY"), "unexpected: {error}");
    }
}
