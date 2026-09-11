//! Codex CLI worker adapter.
//!
//! Codex is deliberately an adapter rather than a pipeline branch. It receives
//! the same approved-specification and milestone instruction contract as the
//! Claude Code adapter, while `run_worker_command` supplies identical timeout,
//! output, cancellation, process-tree, and environment safeguards.

use anyhow::Result;
use async_trait::async_trait;

use crate::agent::{CodingAgent, CodingTaskRequest, CodingTaskResult, CodingTool};
use crate::config::Config;
use crate::implementer::{prompt, run_worker_command};
use crate::task::Emitter;

// npm-installed Codex exposes a `.cmd` launcher on Windows; Unix installs
// expose the bare executable. Keeping this at the adapter boundary avoids
// shell/PATHEXT resolution and works with the filtered process environment.
#[cfg(windows)]
const CODEX_BIN: &str = "codex.cmd";
#[cfg(not(windows))]
const CODEX_BIN: &str = "codex";

pub struct CodexAgent {
    config: Config,
    model: String,
}

impl CodexAgent {
    pub fn new(config: &Config, model: &str) -> Self {
        Self {
            config: config.clone(),
            model: model.to_string(),
        }
    }
}

#[async_trait]
impl CodingAgent for CodexAgent {
    fn tool(&self) -> CodingTool {
        CodingTool::Codex
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn execute(
        &self,
        request: CodingTaskRequest<'_>,
        emitter: &Emitter,
    ) -> Result<CodingTaskResult> {
        let model = self.model.clone();
        let spec_path = request.spec_path.to_path_buf();
        let instructions = request.instructions.to_string();
        run_worker_command(
            &self.config,
            "Codex",
            CODEX_BIN,
            request.workspace,
            emitter,
            move |command| {
                command
                    .arg("exec")
                    .arg("--sandbox")
                    .arg("workspace-write")
                    .arg("--model")
                    .arg(&model)
                    .arg(prompt(&spec_path, &instructions));
            },
        )
        .await?;
        Ok(CodingTaskResult {
            tool: self.tool(),
            model: self.model.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn codex_agent_reports_selected_tool_and_model() {
        let config = crate::agent::test_config();
        let agent = CodexAgent::new(&config, "codex-model-b");
        assert_eq!(agent.tool(), CodingTool::Codex);
        assert_eq!(agent.model(), "codex-model-b");
    }

    #[test]
    fn codex_command_keeps_prompt_and_model_provider_specific() {
        let model = "codex-model";
        let spec = Path::new("/tmp/approved.md");
        let mut command = crate::process_environment::worker_command(CODEX_BIN);
        command
            .arg("exec")
            .arg("--sandbox")
            .arg("workspace-write")
            .arg("--model")
            .arg(model)
            .arg(prompt(spec, "Implement milestone one."));
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            &args[..5],
            ["exec", "--sandbox", "workspace-write", "--model", model]
        );
        assert!(args[5].contains("approved.md"));
        assert!(args[5].contains("Implement milestone one."));
    }

    #[test]
    fn codex_command_does_not_inherit_provider_secrets() {
        let command = crate::process_environment::worker_command(CODEX_BIN);
        let env: Vec<_> = command.as_std().get_envs().collect();
        for secret in [
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GITHUB_TOKEN",
        ] {
            assert!(!env.iter().any(|(key, _)| *key == secret));
        }
    }

    #[tokio::test]
    async fn codex_adapter_path_uses_the_shared_timeout_and_failure_reporting() {
        let mut config = crate::agent::test_config();
        config.execution.implementer_timeout = std::time::Duration::from_millis(300);
        let binary = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let root = std::env::temp_dir().join(format!("mac-codex-timeout-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let manager = crate::task::TaskManager::new();
        let task = manager.create("codex", "timeout", "test");
        let result = run_worker_command(
            &config,
            "Codex",
            &binary,
            &root,
            &manager.emitter(task.id),
            |command| {
                command
                    .args([
                        "--exact",
                        "process_runner::tests::child_probe",
                        "--nocapture",
                    ])
                    .env("MAC_RUNNER_PROBE", "timeout");
            },
        )
        .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("timed out"), "unexpected error: {error}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
