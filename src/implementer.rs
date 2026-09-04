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
//!   - output: stdout/stderr are read line by line rather than parsed as
//!     `--output-format stream-json`, so nothing breaks when the CLI's event
//!     shape changes. Phase 9 switched these from inherited to piped so each
//!     line can also be published to the web UI.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::Config;
use crate::spec::SPEC_FILENAME;
use crate::task::{Emitter, TaskEvent};
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
///
/// Phase 9 changed this from inheriting the terminal to PIPING stdout/stderr,
/// so each line can be both printed and published as `TaskEvent::Build` for the
/// web UI. The tradeoff is real: Claude Code no longer sees a TTY, so it may
/// drop colour and progress animations that it would show when run directly.
/// Line-by-line output is otherwise identical.
pub async fn run(config: &Config, repo: &Path, emitter: &Emitter) -> Result<()> {
    run_with_prompt(config, repo, emitter, &prompt()).await
}

/// Launch Claude Code with task-kind-specific instructions prepared by the
/// workflow module. Common process and streaming behavior stays here.
pub async fn run_with_prompt(
    config: &Config,
    repo: &Path,
    emitter: &Emitter,
    task_prompt: &str,
) -> Result<()> {
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

    let mut child = Command::new(CLAUDE_BIN)
        .current_dir(repo)
        .arg("-p")
        .arg(task_prompt)
        .arg("--model")
        .arg(&config.implementer_model)
        .arg("--permission-mode")
        .arg(&config.permission_mode)
        // Piped, not inherited, so every line can be forwarded to the web UI.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "could not start `{CLAUDE_BIN}` — is the Claude Code CLI installed and on PATH?"
            )
        })?;

    // `take` moves the handles out of the child so they can be read on their
    // own tasks while we wait for the process itself.
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Read both streams concurrently. Doing them in sequence would deadlock the
    // moment Claude Code filled the pipe we were not reading.
    let out_task = tokio::spawn(forward(stdout, emitter.clone(), false));
    let err_task = tokio::spawn(forward(stderr, emitter.clone(), true));

    let status = child
        .wait()
        .await
        .context("failed while waiting for Claude Code to finish")?;

    // Drain whatever is still buffered before reporting the exit status.
    let _ = out_task.await;
    let _ = err_task.await;

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

/// Print each line as it arrives and publish it as a build event.
async fn forward<R>(reader: R, emitter: Emitter, is_stderr: bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if is_stderr {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
        emitter.emit(TaskEvent::Build { chunk: line });
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
