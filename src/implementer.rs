//! Hand an external approved specification artifact to Claude Code.
//!
//! DP-5 (decided):
//!   - launch the `claude` CLI in headless mode (`-p`) with the working
//!     directory set to the target repo (this is not a filesystem sandbox);
//!   - model: `claude-opus-4-8` by default (`IMPLEMENTER_MODEL`);
//!   - permission mode: `bypassPermissions` by default
//!     (`CLAUDE_PERMISSION_MODE`). Headless has nobody to answer a permission
//!     prompt, so a stricter mode leaves the implementer unable to run tests,
//!     install dependencies, or commit — it would write code it cannot verify.
//!     This is why the target repo should be a project you are happy to let it
//!     work in unattended;
//!   - output: stdout/stderr are read line by line rather than parsed as
//!     `--output-format stream-json`, so nothing breaks when the CLI's event
//!     shape changes. Phase 9 switched these from inherited to piped so each
//!     line can also be published to the web UI.

use std::path::Path;
#[cfg(test)]
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;

use crate::agent::{CodingAgent, CodingTaskRequest, CodingTaskResult, CodingTool};
use crate::config::Config;
use crate::task::Emitter;
use crate::ui;

/// The CLI we shell out to. Resolved from PATH.
const CLAUDE_BIN: &str = "claude";

/// The placeholder that stands in for the workspace-specific artifact path when
/// this same prompt is recorded as evidence (task 0007).
pub const SPEC_PATH_PLACEHOLDER: &str = "<APPROVED_SPEC_PATH>";

/// Common artifact instructions stay separate from task-specific behavior.
///
/// Since task 0008 this prompt does NOT tell the worker to work through the
/// specification's Steps section: with milestone execution the caller's
/// instructions decide the scope of one run, and a second, broader instruction
/// here would contradict them.
pub fn prompt(spec_path: &Path, task_prompt: &str) -> String {
    prompt_with_reference(
        &serde_json::to_string(&spec_path.to_string_lossy()).expect("path serializes"),
        task_prompt,
    )
}

/// The identical prompt with the server's absolute artifact path replaced by a
/// stable placeholder. Evidence records the real instruction this way rather
/// than a second, separately maintained description that could drift from it.
pub fn evidence_prompt(task_prompt: &str) -> String {
    prompt_with_reference(
        &serde_json::to_string(SPEC_PATH_PLACEHOLDER).expect("placeholder serializes"),
        task_prompt,
    )
}

fn prompt_with_reference(spec_reference: &str, task_prompt: &str) -> String {
    format!(
        "Read the approved specification at this external path: {spec_reference}\n\n\
         This orchestration artifact is authoritative background context for the \
         task as a whole. Do not copy it into the repository or overwrite a \
         project-owned SPEC.md with it.\n\n\
         The instructions below are authoritative for what to implement in THIS \
         run: do exactly what they ask and nothing beyond their scope. Follow the \
         existing conventions of this repository if it already has code. When you \
         are done, run the project's tests if it has any, and summarise what you \
         changed and anything the instructions asked for that you did not \
         implement.\n\n{task_prompt}"
    )
}

/// Claude Code behind the `CodingAgent` abstraction (task 0004).
///
/// It owns a `Config` clone rather than borrowing one, so the agent can be
/// boxed and handed to a pipeline stage without tying it to a caller frame.
/// `model` comes from the task's stored selection (task 0005), which is why it
/// is a field here rather than a lookup in `config` at launch time.
pub struct ClaudeCodeAgent {
    config: Config,
    model: String,
}

impl ClaudeCodeAgent {
    pub fn new(config: &Config, model: &str) -> Self {
        ClaudeCodeAgent {
            config: config.clone(),
            model: model.to_string(),
        }
    }
}

#[async_trait]
impl CodingAgent for ClaudeCodeAgent {
    fn tool(&self) -> CodingTool {
        CodingTool::ClaudeCode
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn execute(
        &self,
        request: CodingTaskRequest<'_>,
        emitter: &Emitter,
    ) -> Result<CodingTaskResult> {
        run_with_prompt(
            &self.config,
            &self.model,
            request.workspace,
            request.spec_path,
            emitter,
            request.instructions,
        )
        .await?;
        Ok(CodingTaskResult {
            tool: self.tool(),
            model: self.model.clone(),
        })
    }
}

/// Launch Claude Code in `repo` and stream its output until it exits.
///
/// Phase 9 changed this from inheriting the terminal to PIPING stdout/stderr,
/// so each line can be both printed and published as `TaskEvent::Build` for the
/// web UI. The tradeoff is real: Claude Code no longer sees a TTY, so it may
/// drop colour and progress animations that it would show when run directly.
/// Line-by-line output is otherwise identical.
///
/// Private since task 0004: orchestration goes through `CodingAgent::execute`.
async fn run_with_prompt(
    config: &Config,
    model: &str,
    repo: &Path,
    spec_path: &Path,
    emitter: &Emitter,
    task_prompt: &str,
) -> Result<()> {
    ui::header("Implementer");
    ui::system(&format!(
        "claude -p --model {} --permission-mode {}",
        model, config.permission_mode
    ));
    ui::system(&format!("working directory: {}", repo.display()));

    if config.permission_mode == "bypassPermissions" {
        ui::warn("Claude Code will edit files and run commands here unattended.");
    }
    println!();

    let mut command = crate::process_environment::implementer_command(
        CLAUDE_BIN,
        config.anthropic_api_key.as_deref(),
    );
    command
        .current_dir(repo)
        .arg("-p")
        .arg(prompt(spec_path, task_prompt))
        .arg("--model")
        .arg(model)
        .arg("--permission-mode")
        .arg(&config.permission_mode);
    let output = crate::process_runner::run(
        command,
        config
            .execution
            .process(config.execution.implementer_timeout),
        Some(emitter),
    )
    .await
    .with_context(|| {
        format!("could not start `{CLAUDE_BIN}` — is the Claude Code CLI installed and on PATH?")
    })?;

    println!();
    if !output.success() {
        let reason = output
            .failure
            .unwrap_or_else(|| format!("Claude Code exited with status {:?}", output.status));
        emitter.warn(&reason);
        bail!("{reason}");
    }
    ui::success("Claude Code finished.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirms the CLI is installed and that a bare `claude` resolves on
    /// PATH — on Windows that is not obvious, since the launcher is a .exe and
    /// the name we pass has no extension. Costs nothing but a --version.
    ///
    ///   cargo test -- --ignored the_cli_is_reachable
    #[tokio::test]
    #[ignore = "requires the Claude Code CLI on PATH"]
    async fn the_cli_is_reachable() {
        let status = crate::process_environment::async_command(CLAUDE_BIN)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .expect("`claude` should be on PATH");

        assert!(status.success(), "claude --version failed: {status:?}");
    }

    /// Task 0004: the worker role is reached through the abstraction, and the
    /// agent reports the provider and model it will actually use.
    #[test]
    fn the_claude_code_agent_reports_itself() {
        let config = crate::agent::test_config();
        let agent = ClaudeCodeAgent::new(&config, "worker-model-b");

        assert_eq!(agent.tool(), CodingTool::ClaudeCode);
        // The selected model wins over the environment default (task 0005).
        assert_eq!(agent.model(), "worker-model-b");
        assert_ne!(agent.model(), config.implementer_model);
    }

    #[test]
    fn the_prompt_names_the_spec_file() {
        let path = Path::new("/task with spaces/artifacts/approved-spec.md");
        let p = prompt(path, "Fix the root cause.");
        assert!(p.contains("/task with spaces/artifacts/approved-spec.md"));
        assert!(p.contains("Fix the root cause."));
        assert!(!p.contains("Read SPEC.md"));
    }

    /// Task 0008: the common prompt must not widen the scope of a run. Only the
    /// caller's instructions say what to implement.
    #[test]
    fn the_common_prompt_does_not_order_the_whole_steps_section() {
        let p = prompt(Path::new("/tmp/artifacts/approved-spec.md"), "Do X.");

        assert!(!p.contains("Steps"), "prompt still scopes by Steps: {p}");
        assert!(!p.to_lowercase().contains("work through"), "{p}");
        assert!(p.contains("authoritative for what to implement in THIS"));
    }

    /// Task 0007: evidence must show the real instruction, so the two builders
    /// may differ only in the artifact path.
    #[test]
    fn the_evidence_prompt_is_the_real_prompt_with_a_placeholder_path() {
        let path = Path::new("/srv/tmp/task-9/artifacts/approved-spec.md");
        let real = prompt(path, "Implement milestone m1.");

        let evidence = evidence_prompt("Implement milestone m1.");

        assert_eq!(
            real.replace(
                "/srv/tmp/task-9/artifacts/approved-spec.md",
                SPEC_PATH_PLACEHOLDER
            ),
            evidence
        );
        assert!(!evidence.contains("/srv/tmp"), "path leaked: {evidence}");
    }
}
