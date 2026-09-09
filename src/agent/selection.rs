//! What the user chose for one task: a provider/tool and a model per role.
//!
//! Task 0005. These types are DOMAIN data, not configuration: once a task is
//! created its selection is frozen on the task and every later stage reads it
//! from there. Changing `.env` afterwards must not change what a running (or
//! finished) task used, which is what makes a run reproducible and auditable.
//!
//! Two enums rather than one, because a chat provider and a coding tool are not
//! interchangeable — a wrong pairing cannot even be written down.

use serde::{Deserialize, Serialize};

/// A provider that can serve the Proposer or Critic role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatProvider {
    Gemini,
    Anthropic,
}

impl ChatProvider {
    /// Every provider the build knows about, in UI order.
    pub const ALL: [ChatProvider; 2] = [ChatProvider::Gemini, ChatProvider::Anthropic];

    /// The wire/form value. Kept in one place so the API, the HTML form and the
    /// audit trail cannot drift apart.
    pub fn id(self) -> &'static str {
        match self {
            ChatProvider::Gemini => "gemini",
            ChatProvider::Anthropic => "anthropic",
        }
    }

    /// What a human sees.
    pub fn label(self) -> &'static str {
        match self {
            ChatProvider::Gemini => "Gemini",
            ChatProvider::Anthropic => "Anthropic",
        }
    }

    /// The environment variable that makes this provider available. Only the
    /// NAME is ever shown to a user; the value never leaves `Config`.
    pub fn credential_variable(self) -> &'static str {
        match self {
            ChatProvider::Gemini => "GEMINI_API_KEY",
            ChatProvider::Anthropic => "ANTHROPIC_API_KEY",
        }
    }

    /// Parse a wire/form value. `None` means the id is not one we serve — the
    /// caller reports that rather than guessing a provider.
    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.id() == value)
    }
}

impl std::fmt::Display for ChatProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// A tool that can serve the Worker role. Codex joins this enum in task 0015.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingTool {
    ClaudeCode,
}

impl CodingTool {
    pub const ALL: [CodingTool; 1] = [CodingTool::ClaudeCode];

    pub fn id(self) -> &'static str {
        match self {
            CodingTool::ClaudeCode => "claude_code",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CodingTool::ClaudeCode => "Claude Code",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.id() == value)
    }
}

impl std::fmt::Display for CodingTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// A resolved chat role: which provider, which model. Never partial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAgentConfig {
    pub provider: ChatProvider,
    pub model: String,
}

impl ChatAgentConfig {
    pub fn new(provider: ChatProvider, model: impl Into<String>) -> Self {
        ChatAgentConfig {
            provider,
            model: model.into(),
        }
    }
}

/// A resolved worker role. The tool and the model stay separate values: a tool
/// is a program we launch, a model is what that program reasons with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentConfig {
    pub tool: CodingTool,
    pub model: String,
}

impl CodingAgentConfig {
    pub fn new(tool: CodingTool, model: impl Into<String>) -> Self {
        CodingAgentConfig {
            tool,
            model: model.into(),
        }
    }
}

/// The three roles of one run, fully resolved. Stored on the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSelection {
    pub proposer: ChatAgentConfig,
    pub critic: ChatAgentConfig,
    pub worker: CodingAgentConfig,
}

impl AgentSelection {
    /// The wiring the app has always had, with the compiled-in default models.
    ///
    /// Used where no `Config` is in reach (legacy test helpers). Production
    /// creation resolves against `AgentCatalogue`, which reads the environment.
    pub fn compiled_defaults() -> Self {
        AgentSelection {
            proposer: ChatAgentConfig::new(
                ChatProvider::Gemini,
                crate::config::DEFAULT_GEMINI_MODEL,
            ),
            critic: ChatAgentConfig::new(
                ChatProvider::Anthropic,
                crate::config::DEFAULT_CRITIC_MODEL,
            ),
            worker: CodingAgentConfig::new(
                CodingTool::ClaudeCode,
                crate::config::DEFAULT_IMPLEMENTER_MODEL,
            ),
        }
    }

    /// One line per role for logs and the audit trail. Never contains secrets:
    /// only role, provider/tool and model name.
    pub fn audit_rows(&self) -> Vec<(&'static str, &'static str, &str)> {
        vec![
            (
                "proposer",
                self.proposer.provider.id(),
                self.proposer.model.as_str(),
            ),
            (
                "critic",
                self.critic.provider.id(),
                self.critic.model.as_str(),
            ),
            ("worker", self.worker.tool.id(), self.worker.model.as_str()),
        ]
    }
}

// ---------------------------------------------------------------------------
// What a request may ask for
// ---------------------------------------------------------------------------

/// A requested chat role. Both fields are optional: omitting either means
/// "use the configured default", which is how existing clients keep working.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAgentRequest {
    #[serde(default)]
    pub provider: Option<ChatProvider>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingAgentRequest {
    #[serde(default)]
    pub tool: Option<CodingTool>,
    #[serde(default)]
    pub model: Option<String>,
}

/// The optional `agents` block of a task creation request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSelectionRequest {
    #[serde(default)]
    pub proposer: Option<ChatAgentRequest>,
    #[serde(default)]
    pub critic: Option<ChatAgentRequest>,
    #[serde(default)]
    pub worker: Option<CodingAgentRequest>,
}

impl AgentSelectionRequest {
    pub fn is_empty(&self) -> bool {
        self == &AgentSelectionRequest::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_defaults_keep_the_original_wiring() {
        let selection = AgentSelection::compiled_defaults();

        assert_eq!(selection.proposer.provider, ChatProvider::Gemini);
        assert_eq!(selection.critic.provider, ChatProvider::Anthropic);
        assert_eq!(selection.worker.tool, CodingTool::ClaudeCode);
    }

    /// The audit view carries role, provider and model — and nothing else.
    #[test]
    fn audit_rows_expose_only_role_provider_and_model() {
        let selection = AgentSelection::compiled_defaults();
        let rows = selection.audit_rows();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "proposer");
        assert_eq!(rows[0].1, "gemini");
        assert_eq!(rows[2].1, "claude_code");
    }

    /// Provider ids are a wire contract shared by the API, the form and the
    /// audit trail, so they are pinned by a test rather than by habit.
    #[test]
    fn ids_round_trip_through_json() {
        let json = serde_json::to_string(&ChatProvider::Anthropic).unwrap();
        assert_eq!(json, "\"anthropic\"");
        assert_eq!(
            serde_json::to_string(&CodingTool::ClaudeCode).unwrap(),
            "\"claude_code\""
        );
        assert_eq!(
            serde_json::from_str::<ChatProvider>("\"gemini\"").unwrap(),
            ChatProvider::Gemini
        );
        assert!(serde_json::from_str::<ChatProvider>("\"openai\"").is_err());
    }
}
