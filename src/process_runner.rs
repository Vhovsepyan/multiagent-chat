//! Bounded child execution. A timeout covers both exit and pipe draining.
//! Tree termination is best effort, not a sandbox against escaping descendants.

use crate::execution_limits::{ProcessLimits, TERMINATION_GRACE, TRUNCATED};
use crate::task::{Emitter, TaskEvent};
use anyhow::{Context, Result};
use std::process::{ExitStatus, Stdio};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

#[derive(Debug, Default)]
pub struct Capture {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    emitted: usize,
}

impl Capture {
    pub fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            text.push_str(&format!("\n{TRUNCATED}"));
        }
        text
    }

    fn emit_pending(
        &mut self,
        emitter: Option<&Emitter>,
        stderr: bool,
        event_bytes: usize,
        flush: bool,
    ) {
        let Some(emitter) = emitter else { return };
        while self.emitted < self.bytes.len() {
            let remaining = &self.bytes[self.emitted..];
            let newline = remaining.iter().position(|byte| *byte == b'\n');
            let end = match newline {
                Some(index) if index < event_bytes => index + 1,
                _ if remaining.len() >= event_bytes => event_bytes,
                _ if flush => remaining.len(),
                _ => break,
            };
            let text = String::from_utf8_lossy(&remaining[..end]);
            let text = text.strip_suffix('\n').unwrap_or(&text);
            let text = text.strip_suffix('\r').unwrap_or(text);
            if stderr {
                eprintln!("{text}");
            } else {
                println!("{text}");
            }
            emitter.emit(TaskEvent::Build { chunk: text.into() });
            self.emitted += end;
        }
    }
}

#[derive(Debug)]
pub struct ProcessOutput {
    pub status: Option<ExitStatus>,
    pub stdout: Capture,
    pub stderr: Capture,
    pub failure: Option<String>,
    pub timed_out: bool,
}

impl ProcessOutput {
    pub fn success(&self) -> bool {
        !self.timed_out
            && self.failure.is_none()
            && self.status.is_some_and(|status| status.success())
    }
    pub fn text(&self) -> String {
        let mut text = format!("{}{}", self.stdout.text(), self.stderr.text());
        if let Some(failure) = &self.failure {
            text.push_str(&format!("\n{failure}"));
        } else if let Some(status) = self.status
            && !status.success()
        {
            text.push_str(&format!("\ncommand exited with {status}"));
        }
        text
    }
}

async fn drain(
    mut reader: impl AsyncRead + Unpin,
    capture: &mut Capture,
    limit: usize,
    emitter: Option<&Emitter>,
    stderr: bool,
    event_bytes: usize,
) -> std::io::Result<()> {
    let mut buffer = [0; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(limit.saturating_sub(capture.bytes.len()));
        capture.bytes.extend_from_slice(&buffer[..keep]);
        capture.emit_pending(emitter, stderr, event_bytes, false);
        if keep < count && !capture.truncated {
            capture.truncated = true;
            capture.emit_pending(emitter, stderr, event_bytes, true);
            if let Some(emitter) = emitter {
                emitter.emit(TaskEvent::Build {
                    chunk: TRUNCATED.into(),
                });
            }
        }
        // Continue draining after the cap so a noisy child cannot block on pipes.
    }
    Ok(())
}

pub async fn run(
    mut command: Command,
    limits: ProcessLimits,
    emitter: Option<&Emitter>,
) -> Result<ProcessOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let mut child = command.spawn().context("could not launch command")?;
    #[cfg(windows)]
    let job = match crate::process_job::ProcessJob::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(TERMINATION_GRACE, child.wait()).await;
            return Err(error)
                .context("could not establish process-tree cleanup; execution stopped");
        }
    };
    let pid = child.id();
    let stdout_pipe = child.stdout.take().expect("stdout piped");
    let stderr_pipe = child.stderr.take().expect("stderr piped");
    let mut stdout = Capture::default();
    let mut stderr = Capture::default();
    let completion = tokio::time::timeout(limits.timeout, async {
        tokio::join!(
            child.wait(),
            drain(
                stdout_pipe,
                &mut stdout,
                limits.stdout_bytes,
                emitter,
                false,
                limits.event_bytes
            ),
            drain(
                stderr_pipe,
                &mut stderr,
                limits.stderr_bytes,
                emitter,
                true,
                limits.event_bytes
            ),
        )
    })
    .await;
    let (status, failure, timed_out) = match completion {
        Ok((status, out, err)) => {
            let failure = status
                .as_ref()
                .err()
                .map(|e| format!("process wait failed: {e}"))
                .or_else(|| out.err().map(|e| format!("stdout read failed: {e}")))
                .or_else(|| err.err().map(|e| format!("stderr read failed: {e}")));
            (status.ok(), failure, false)
        }
        Err(_) => {
            #[cfg(windows)]
            let job_note = job
                .terminate()
                .err()
                .map(|error| format!("; job termination failed: {error}"))
                .unwrap_or_default();
            #[cfg(not(windows))]
            let job_note = String::new();
            let termination = terminate(&mut child, pid).await;
            (
                None,
                Some(format!(
                    "command timed out after {:?}{job_note}{termination}",
                    limits.timeout
                )),
                true,
            )
        }
    };
    stdout.emit_pending(emitter, false, limits.event_bytes, true);
    stderr.emit_pending(emitter, true, limits.event_bytes, true);
    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
        failure,
        timed_out,
    })
}

async fn terminate(child: &mut Child, pid: Option<u32>) -> String {
    let mut note = String::new();
    #[cfg(windows)]
    let _ = pid;
    #[cfg(unix)]
    if let Some(pid) = pid {
        let mut tree = {
            let mut cmd = crate::process_environment::async_command("/bin/kill");
            cmd.args(["-KILL", "--", &format!("-{pid}")]);
            cmd
        };
        {
            tree.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            if !matches!(tokio::time::timeout(TERMINATION_GRACE, tree.status()).await, Ok(Ok(status)) if status.success())
            {
                note.push_str("; process-tree termination could not be confirmed");
            }
        }
    }
    // Always try the direct child too, even when the platform tree helper fails.
    let _ = child.start_kill();
    if !matches!(
        tokio::time::timeout(TERMINATION_GRACE, child.wait()).await,
        Ok(Ok(_))
    ) {
        note.push_str("; direct-child termination could not be confirmed");
    }
    note
}

/// Adapter for the existing synchronous workspace provider. Production callers
/// run workspace operations on blocking workers, never on an HTTP/Tokio worker.
pub fn run_blocking(
    command: std::process::Command,
    limits: ProcessLimits,
) -> Result<ProcessOutput> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run(command.into(), limits, None))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("command runner thread panicked"))?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    pub fn probe(mode: &str, root: &std::path::Path) -> Command {
        let mut command =
            crate::process_environment::async_command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_runner::tests::child_probe",
                "--nocapture",
            ])
            .env("MAC_RUNNER_PROBE", mode)
            .current_dir(root);
        command
    }

    pub fn limits() -> ProcessLimits {
        ProcessLimits {
            timeout: Duration::from_secs(3),
            stdout_bytes: 1024,
            stderr_bytes: 1024,
            event_bytes: 128,
        }
    }

    #[test]
    fn child_probe() {
        let Ok(mode) = std::env::var("MAC_RUNNER_PROBE")
            .or_else(|_| std::fs::read_to_string(".multiagent-test-probe"))
        else {
            return;
        };
        match mode.as_str() {
            "noisy" => {
                for _ in 0..32 {
                    std::io::stdout().write_all(&[b'o'; 8192]).unwrap();
                    std::io::stderr().write_all(&[b'e'; 8192]).unwrap();
                }
            }
            "tree" => {
                let root = std::env::current_dir().unwrap();
                let mut descendant = probe("delayed-file", &root).into_std();
                descendant
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let mut descendant = descendant.spawn().unwrap();
                println!("partial before timeout");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(2));
                let _ = descendant.wait();
            }
            "delayed-file" => {
                std::thread::sleep(Duration::from_millis(1500));
                std::fs::write("should-not-exist.txt", "orphan survived").unwrap();
            }
            "timeout" => {
                println!("partial before timeout");
                eprintln!("partial stderr");
                std::io::stdout().flush().unwrap();
                std::fs::write("partial.txt", "work before timeout\n").unwrap();
                std::thread::sleep(Duration::from_secs(2));
                std::fs::write("should-not-exist.txt", "process survived").unwrap();
            }
            "fail" => {
                println!("useful failure output");
                std::fs::write("partial.txt", "work before failure\n").unwrap();
                std::process::exit(7);
            }
            _ => {
                println!("small stdout");
                eprintln!("small stderr");
            }
        }
    }

    #[tokio::test]
    async fn normal_small_output_remains_unchanged() {
        let root = std::env::temp_dir();
        let output = run(probe("small", &root), limits(), None).await.unwrap();
        assert!(output.success());
        assert!(output.stdout.text().contains("small stdout\n"));
        assert_eq!(output.stderr.text(), "small stderr\n");
        assert!(!output.text().contains(TRUNCATED));
    }

    #[tokio::test]
    async fn bounds_both_streams_and_marks_truncation_without_deadlock() {
        let output = run(probe("noisy", &std::env::temp_dir()), limits(), None)
            .await
            .unwrap();
        assert!(output.success());
        assert_eq!(output.stdout.bytes.len(), 1024);
        assert_eq!(output.stderr.bytes.len(), 1024);
        assert!(output.stdout.truncated && output.stderr.truncated);
        assert!(output.stdout.text().ends_with(TRUNCATED));
        assert!(output.stderr.text().ends_with(TRUNCATED));
    }

    #[tokio::test]
    async fn timeout_retains_partial_output_and_terminates_child_tree() {
        for mode in ["timeout", "tree"] {
            let root = std::env::temp_dir().join(format!("mac-timeout-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let mut limits = limits();
            limits.timeout = Duration::from_millis(400);
            let output = run(probe(mode, &root), limits, None).await.unwrap();
            assert!(output.timed_out && !output.success());
            assert!(
                output.text().contains("partial before timeout"),
                "{}",
                output.text()
            );
            assert!(output.failure.as_ref().unwrap().contains("timed out"));
            tokio::time::sleep(Duration::from_millis(1700)).await;
            assert!(
                !root.join("should-not-exist.txt").exists(),
                "child or descendant survived timeout in {mode}: {:?}",
                output.failure
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn streaming_payloads_and_retained_history_stay_bounded() {
        let manager =
            crate::task::TaskManager::with_history_limits(crate::execution_limits::HistoryLimits {
                event_bytes: 128,
                log_events: 3,
                log_bytes: 384,
            });
        let task = manager.create("test", "description", "legacy");
        let output = run(
            probe("noisy", &std::env::temp_dir()),
            limits(),
            Some(&manager.emitter(task.id)),
        )
        .await
        .unwrap();
        assert!(output.success());
        let task = manager.get(task.id).unwrap();
        assert!(task.history.len() <= 3);
        assert!(task.discarded_log_events > 0);
        assert!(
            task.history
                .iter()
                .all(|event| matches!(event, TaskEvent::Build {chunk} if chunk.len() <= 128))
        );
    }
}
