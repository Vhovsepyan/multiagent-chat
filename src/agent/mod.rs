//! Agent abstractions: the orchestration layer talks to ROLES, never to vendors.
//!
//! Task 0004. Before this module, `debate.rs` named `GeminiClient` and
//! `ClaudeClient` directly and `main.rs` called the Claude Code implementer by
//! name, so every role was welded to one provider.
//!
//! Two abstractions, because the two kinds of agent are genuinely different:
//!   - [`ChatAgent`] — send a conversation, get text back (Proposer, Critic);
//!   - [`CodingAgent`] — run in a workspace, touch files, run commands (Worker).
//!
//! Provider choice lives HERE, in the factories below, and nowhere else. A
//! pipeline stage never asks which provider it is holding.

// `provider()` / `model()` and the provenance fields are read by the tests and
// by the factories; per-task recording of them is task 0005. Same convention as
// the `api` modules.
#![allow(dead_code)]

pub mod chat;
pub mod coding;

use std::fmt;

use anyhow::{Result, bail};
use serde::Serialize;

pub use chat::{ChatAgent, ChatRequest, ChatResponse};
pub use coding::{CodingAgent, CodingTaskRequest, CodingTaskResult};

use crate::api::claude::ClaudeClient;
use crate::api::gemini::GeminiClient;
use crate::config::Config;
use crate::implementer::ClaudeCodeAgent;

/// Which vendor is behind an agent. Carried as metadata by every agent so the
/// UI and future audit trail can say who actually answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Gemini,
    Anthropic,
    ClaudeCode,
}

impl ProviderId {
    pub fn label(self) -> &'static str {
        match self {
            ProviderId::Gemini => "gemini",
            ProviderId::Anthropic => "anthropic",
            ProviderId::ClaudeCode => "claude-code",
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ---------------------------------------------------------------------------
// Current defaults
// ---------------------------------------------------------------------------

/// The wiring the app had before this abstraction existed, kept unchanged:
/// Proposer -> Gemini, Critic -> Anthropic, Worker -> Claude Code.
///
/// These constants are the only statement of that policy. Per-task selection is
/// a later task; nothing here reads a task or a request.
pub const DEFAULT_PROPOSER: ProviderId = ProviderId::Gemini;
pub const DEFAULT_CRITIC: ProviderId = ProviderId::Anthropic;
pub const DEFAULT_WORKER: ProviderId = ProviderId::ClaudeCode;

/// Build the agent for the Proposer role.
pub fn default_proposer(config: &Config) -> Result<Box<dyn ChatAgent>> {
    chat_agent(DEFAULT_PROPOSER, config)
}

/// Build the agent for the Critic role.
pub fn default_critic(config: &Config) -> Result<Box<dyn ChatAgent>> {
    chat_agent(DEFAULT_CRITIC, config)
}

/// Build the agent for the Worker role.
pub fn default_worker(config: &Config) -> Result<Box<dyn CodingAgent>> {
    coding_agent(DEFAULT_WORKER, config)
}

// ---------------------------------------------------------------------------
// Factories — the single place that maps a provider to an implementation
// ---------------------------------------------------------------------------

/// Construct a chat agent. The model comes from the provider's own `Config`
/// field, exactly as each client read it before.
pub fn chat_agent(provider: ProviderId, config: &Config) -> Result<Box<dyn ChatAgent>> {
    match provider {
        ProviderId::Gemini => Ok(Box::new(GeminiClient::new(config)?)),
        ProviderId::Anthropic => Ok(Box::new(ClaudeClient::new(config)?)),
        // Not a mistake worth a panic: a future request could name any provider,
        // and the caller should see a readable error before anything runs.
        ProviderId::ClaudeCode => bail!("{provider} is a coding agent, not a chat agent"),
    }
}

/// Construct a coding agent — one that works inside a task workspace.
pub fn coding_agent(provider: ProviderId, config: &Config) -> Result<Box<dyn CodingAgent>> {
    match provider {
        ProviderId::ClaudeCode => Ok(Box::new(ClaudeCodeAgent::new(config))),
        other => bail!("{other} is not a coding agent"),
    }
}

#[cfg(test)]
pub(crate) fn test_config() -> Config {
    Config {
        execution: Default::default(),
        gemini_api_key: "test".into(),
        anthropic_api_key: "test".into(),
        workspace_root: None,
        max_rounds: 1,
        gemini_model: "proposer-model".into(),
        critic_model: "critic-model".into(),
        implementer_model: "worker-model".into(),
        permission_mode: "acceptEdits".into(),
        port: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Requirement 8: the refactoring must not move any role to a new vendor.
    #[test]
    fn default_roles_keep_the_previous_wiring() {
        let config = test_config();

        let proposer = default_proposer(&config).unwrap();
        let critic = default_critic(&config).unwrap();
        let worker = default_worker(&config).unwrap();

        assert_eq!(proposer.provider(), ProviderId::Gemini);
        assert_eq!(critic.provider(), ProviderId::Anthropic);
        assert_eq!(worker.provider(), ProviderId::ClaudeCode);
    }

    /// Each agent reports the model its own config field names, so a later
    /// audit trail can record what actually answered.
    #[test]
    fn agents_report_their_configured_model() {
        let config = test_config();

        assert_eq!(default_proposer(&config).unwrap().model(), "proposer-model");
        assert_eq!(default_critic(&config).unwrap().model(), "critic-model");
        assert_eq!(default_worker(&config).unwrap().model(), "worker-model");
    }

    /// A chat role cannot be served by a workspace agent, or the other way
    /// round. This fails with a message rather than by panicking.
    #[test]
    fn the_two_agent_kinds_are_not_interchangeable() {
        let config = test_config();

        // `.err()` rather than `unwrap_err()`: a boxed trait object is not
        // `Debug`, which is what `unwrap_err` would need to print.
        let chat = chat_agent(ProviderId::ClaudeCode, &config)
            .err()
            .expect("a coding provider cannot serve a chat role")
            .to_string();
        assert!(chat.contains("claude-code"), "unexpected message: {chat}");

        let coding = coding_agent(ProviderId::Gemini, &config)
            .err()
            .expect("a chat provider cannot serve the worker role")
            .to_string();
        assert!(coding.contains("gemini"), "unexpected message: {coding}");
    }
}
