//! Agent abstractions: the orchestration layer talks to ROLES, never to vendors.
//!
//! Task 0004 introduced two traits, because the two kinds of agent are
//! genuinely different:
//!   - [`ChatAgent`] — send a conversation, get text back (Proposer, Critic);
//!   - [`CodingAgent`] — run in a workspace, touch files, run commands (Worker).
//!
//! Task 0005 moved the CHOICE onto the task: a run carries an
//! [`AgentSelection`], and [`resolve`] is the one place that turns it into live
//! agents. A pipeline stage never asks which provider it is holding, and never
//! re-reads the environment to find out which model to use.

// `provider()` / `tool()` / `model()` and the provenance fields are read by the
// UI, the tests and the audit rows; some are not read on every path yet. Same
// convention as the `api` modules.
#![allow(dead_code)]

pub mod catalogue;
pub mod chat;
pub mod coding;
pub mod selection;

use anyhow::{Context, Result};

pub use catalogue::{AgentCatalogue, ModelOptions};
pub use chat::{ChatAgent, ChatRequest, ChatResponse};
pub use coding::{CodingAgent, CodingTaskRequest, CodingTaskResult};
pub use selection::{
    AgentSelection, AgentSelectionRequest, ChatAgentConfig, ChatAgentRequest, ChatProvider,
    CodingAgentConfig, CodingAgentRequest, CodingTool,
};

use crate::api::claude::ClaudeClient;
use crate::api::gemini::GeminiClient;
use crate::config::Config;
use crate::implementer::ClaudeCodeAgent;

/// The three live agents of one run.
pub struct ResolvedAgents {
    pub proposer: Box<dyn ChatAgent>,
    pub critic: Box<dyn ChatAgent>,
    pub worker: Box<dyn CodingAgent>,
}

/// Turn a task's stored selection into live agents — the central resolver.
///
/// Every role is built here, before any workspace or API call, so a selection
/// that cannot be served fails the task with a clear message instead of being
/// quietly swapped for one that works (task 0005, requirement 14).
pub fn resolve(selection: &AgentSelection, config: &Config) -> Result<ResolvedAgents> {
    Ok(ResolvedAgents {
        proposer: chat_agent(&selection.proposer, config)
            .context("the selected proposer agent could not be started")?,
        critic: chat_agent(&selection.critic, config)
            .context("the selected critic agent could not be started")?,
        worker: coding_agent(&selection.worker, config)
            .context("the selected worker agent could not be started")?,
    })
}

/// Construct one chat agent. The model comes from the SELECTION, not the
/// environment; `config` only supplies credentials and transport settings.
pub fn chat_agent(selection: &ChatAgentConfig, config: &Config) -> Result<Box<dyn ChatAgent>> {
    match selection.provider {
        ChatProvider::Gemini => Ok(Box::new(GeminiClient::new(config, &selection.model)?)),
        ChatProvider::Anthropic => Ok(Box::new(ClaudeClient::new(config, &selection.model)?)),
    }
}

/// Construct the worker agent — one that works inside a task workspace.
pub fn coding_agent(
    selection: &CodingAgentConfig,
    config: &Config,
) -> Result<Box<dyn CodingAgent>> {
    match selection.tool {
        CodingTool::ClaudeCode => Ok(Box::new(ClaudeCodeAgent::new(config, &selection.model))),
    }
}

#[cfg(test)]
pub(crate) fn test_config() -> Config {
    Config {
        execution: Default::default(),
        gemini_api_key: Some("test".into()),
        anthropic_api_key: Some("test".into()),
        workspace_root: None,
        max_rounds: 1,
        gemini_model: "proposer-model".into(),
        critic_model: "critic-model".into(),
        implementer_model: "worker-model".into(),
        gemini_models: Vec::new(),
        anthropic_models: Vec::new(),
        claude_code_models: Vec::new(),
        permission_mode: "acceptEdits".into(),
        port: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requirement 5/8: with no explicit selection the roles are wired exactly
    /// as they were before per-task selection existed.
    #[test]
    fn default_roles_keep_the_previous_wiring() {
        let config = test_config();
        let selection = AgentCatalogue::from_config(&config).defaults().unwrap();

        let agents = resolve(&selection, &config).unwrap();

        assert_eq!(agents.proposer.provider(), ChatProvider::Gemini);
        assert_eq!(agents.critic.provider(), ChatProvider::Anthropic);
        assert_eq!(agents.worker.tool(), CodingTool::ClaudeCode);
    }

    /// Each agent answers with the model the SELECTION names, not with the one
    /// its role happens to default to in the environment.
    #[test]
    fn agents_use_the_selected_model_not_the_environment_default() {
        let mut config = test_config();
        config.anthropic_models = vec!["claude-extra".into()];
        let catalogue = AgentCatalogue::from_config(&config);
        let request = AgentSelectionRequest {
            proposer: Some(ChatAgentRequest {
                provider: Some(ChatProvider::Anthropic),
                model: Some("claude-extra".into()),
            }),
            ..Default::default()
        };
        let selection = catalogue.resolve(Some(&request)).unwrap();

        let agents = resolve(&selection, &config).unwrap();

        assert_eq!(agents.proposer.provider(), ChatProvider::Anthropic);
        assert_eq!(agents.proposer.model(), "claude-extra");
        assert_eq!(agents.critic.model(), "critic-model");
        assert_eq!(agents.worker.model(), "worker-model");
    }

    /// Both roles may be served by the same provider with different models.
    #[test]
    fn one_provider_can_serve_two_roles_with_different_models() {
        let mut config = test_config();
        config.gemini_models = vec!["gemini-fast".into()];
        let selection = AgentSelection {
            proposer: ChatAgentConfig::new(ChatProvider::Gemini, "gemini-fast"),
            critic: ChatAgentConfig::new(ChatProvider::Gemini, "proposer-model"),
            worker: CodingAgentConfig::new(CodingTool::ClaudeCode, "worker-model"),
        };

        let agents = resolve(&selection, &config).unwrap();

        assert_eq!(agents.proposer.model(), "gemini-fast");
        assert_eq!(agents.critic.provider(), ChatProvider::Gemini);
        assert_eq!(agents.critic.model(), "proposer-model");
    }
}
