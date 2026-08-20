//! Phase 5: hand SPEC.md to Claude Code and let it build.
//!
//! DP-5 (decided):
//!   - launch the `claude` CLI in headless mode (`-p`) with the working
//!     directory set to the target repo, so it only ever sees that project;
//!   - model: `claude-opus-4-8` by default (`IMPLEMENTER_MODEL`);
//!   - permission mode: `bypassPermissions` by default
//!     (`CLAUDE_PERMISSION_MODE`). Headless has nobody to answer a permission
//!     prompt, so a stricter mode leaves the implementer unable to run tests,
//!     install dependencies, or commit — it would write code it cannot verify.
//!     This is why the target repo should be a project you are happy to let it
//!     work in unattended;
//!   - output: the child inherits stdout/stderr, so Claude Code's own progress
//!     prints straight through, live, with no JSON parsing to break when the
//!     CLI's event shape changes.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use crate::config::Config;
use crate::spec::SPEC_FILENAME;
use crate::ui;

/// The CLI we shell out to. Resolved from PATH.
const CLAUDE_BIN: &str = "claude";

/// What we ask Claude Code to do. Deliberately short: everything it needs to
/// know is in SPEC.md, which is sitting in its working directory.
fn prompt() -> String {
    format!(
        "Read {SPEC_FILENAME} in this repository and implement it.\n\n\
         Work through the Steps section in order. Follow the existing \
         conventions of this repository if it already has code. When you are \
         done, run the project's tests if it has any, and summarise what you \
         built and anything from {SPEC_FILENAME} you did not implement."
    )
}

/// Launch Claude Code in `repo` and stream its output until it exits.
pub async fn run(config: &Config, repo: &Path) -> Result<()> {
    ui::header("Implementer");
    ui::system(&format!(
        "claude -p --model {} --permission-mode {}",
        config.implementer_model, config.permission_mode
    ));
    ui::system(&format!("working directory: {}", repo.display()));

    if config.permission_mode == "bypassPermissions" {
        ui::warn("Claude Code will edit files and run commands here unattended.");
    }
    println!();

    let status = Command::new(CLAUDE_BIN)
        .current_dir(repo)
        .arg("-p")
        .arg(prompt())
        .arg("--model")
        .arg(&config.implementer_model)
        .arg("--permission-mode")
        .arg(&config.permission_mode)
        // Inherit the terminal so its output appears live.
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| {
            format!(
                "could not start `{CLAUDE_BIN}` — is the Claude Code CLI installed and on PATH?"
            )
        })?;

    println!();
    match status.code() {
        Some(0) => {
            ui::success("Claude Code finished.");
            Ok(())
        }
        Some(code) => bail!("Claude Code exited with status {code}"),
        // On Windows this is unusual; on Unix it means a signal killed it.
        None => bail!("Claude Code was terminated before it finished"),
    }
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
        let status = Command::new(CLAUDE_BIN)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .expect("`claude` should be on PATH");

        assert!(status.success(), "claude --version failed: {status:?}");
    }

    #[test]
    fn the_prompt_names_the_spec_file() {
        let p = prompt();
        assert!(p.contains(SPEC_FILENAME));
        assert!(p.contains("Steps"));
    }
}
