//! Explicit, user-triggered publication of a persisted project to GitHub.
//!
//! This module is deliberately separate from task orchestration. It performs
//! only read-only preflight checks until the caller has received an explicit
//! confirmation, then performs one ordinary (non-forced) push. Credentials are
//! supplied by Git's configured credential helper/SSH agent and never enter a
//! task prompt or audit event.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::execution_limits::ExecutionLimits;
use crate::task::{GitHubPublication, Task};
use crate::workspace::run_git;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishPreview {
    pub repository: String,
    pub remote_url: String,
    pub branch: String,
    pub head_sha: String,
    pub dirty_files: Vec<String>,
    pub finalization_commit_required: bool,
    pub verification_summary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositorySnapshot {
    root: PathBuf,
    repository: String,
    remote_url: String,
    branch: String,
    head_sha: String,
    dirty_files: Vec<String>,
    finalization_commit_required: bool,
}

const GENERATED_DOCUMENTS: [&str; 6] = [
    "README.md",
    "docs/ARCHITECTURE.md",
    "docs/DEVELOPMENT_LOG.md",
    "docs/DECISIONS.md",
    "docs/AI_USAGE.md",
    "docs/NEXT_STEPS.md",
];

pub fn preview(task: &Task, limits: &ExecutionLimits) -> Result<PublishPreview> {
    let snapshot = inspect(task, limits)?;
    Ok(PublishPreview {
        repository: snapshot.repository,
        remote_url: snapshot.remote_url,
        branch: snapshot.branch,
        head_sha: snapshot.head_sha,
        dirty_files: snapshot.dirty_files,
        finalization_commit_required: snapshot.finalization_commit_required,
        verification_summary: task
            .result
            .as_ref()
            .map(|result| {
                result
                    .verification
                    .iter()
                    .map(|item| {
                        format!(
                            "{}: {}",
                            item.command,
                            if item.success { "passed" } else { "failed" }
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

pub fn publish(task: &Task, limits: &ExecutionLimits) -> Result<GitHubPublication> {
    let snapshot = inspect(task, limits)?;
    if snapshot.finalization_commit_required {
        finalize_documents(task, &snapshot, limits)?;
    }

    // Re-read after the optional generated-document commit. This closes the
    // gap between the confirmation preview and the push without ever using
    // reset/clean or rewriting history.
    let snapshot = inspect(task, limits)?;
    if !snapshot.dirty_files.is_empty() {
        bail!(
            "publishing is blocked by unrelated uncommitted files: {}",
            snapshot.dirty_files.join(", ")
        );
    }
    let refspec = format!("HEAD:{}", snapshot.branch);
    let dry_run = git_output(
        &snapshot.root,
        &["push", "--dry-run", "origin", &refspec],
        limits,
    )?;
    if !dry_run.status.success() {
        bail!("GitHub push preflight failed: {}", stderr(&dry_run));
    }
    let output = git_output(&snapshot.root, &["push", "origin", &refspec], limits)?;
    if !output.status.success() {
        bail!("GitHub push failed: {}", stderr(&output));
    }
    Ok(GitHubPublication {
        repository: snapshot.repository,
        branch: snapshot.branch,
        commit_sha: snapshot.head_sha,
        published_at: chrono::DateTime::<Utc>::from(std::time::SystemTime::now()),
    })
}

fn inspect(task: &Task, limits: &ExecutionLimits) -> Result<RepositorySnapshot> {
    if task.status != crate::task::TaskStatus::Completed || task.result.is_none() {
        bail!("GitHub publishing requires a completed task result");
    }
    let persistence = task
        .persistence
        .as_ref()
        .filter(|item| item.status == crate::task::PersistenceStatus::Persisted)
        .ok_or_else(|| anyhow::anyhow!("GitHub publishing requires a persisted project"))?;
    let root = PathBuf::from(&persistence.destination);
    let metadata = fs::symlink_metadata(&root).context("could not inspect persisted project")?;
    if crate::repository_file::is_link(&metadata) || !metadata.is_dir() {
        bail!("persisted project is not a safe directory");
    }
    if !root.join(".git").is_dir() {
        bail!("persisted project is not a Git repository");
    }
    let branch = git(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        limits,
    )?
    .ok_or_else(|| anyhow::anyhow!("publishing requires a non-detached branch"))?;
    let head_sha = git(&root, &["rev-parse", "HEAD"], limits)?
        .ok_or_else(|| anyhow::anyhow!("publishing requires a committed HEAD"))?;
    for marker in [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        if root.join(".git").join(marker).exists() {
            bail!("publishing is blocked while Git is in a {marker} operation");
        }
    }
    let status = git(
        &root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
        limits,
    )?
    .unwrap_or_default();
    if status
        .lines()
        .any(|line| line.len() >= 2 && line.as_bytes()[0..2].contains(&b'U'))
    {
        bail!("publishing is blocked by unresolved merge conflicts");
    }
    let dirty_files = status
        .lines()
        .filter_map(|line| line.get(3..).map(str::trim).filter(|path| !path.is_empty()))
        .map(|path| path.trim_matches('"').to_string())
        .collect::<Vec<_>>();
    let written = task
        .history
        .iter()
        .find_map(|event| match &event.event {
            crate::task::TaskEvent::SubmissionDocumentationGenerated { written, .. } => {
                Some(written)
            }
            _ => None,
        })
        .cloned()
        .unwrap_or_default();
    let allowed =
        |path: &str| GENERATED_DOCUMENTS.contains(&path) && written.iter().any(|item| item == path);
    if dirty_files.iter().any(|path| !allowed(path)) {
        bail!(
            "publishing is blocked by unrelated uncommitted files: {}",
            dirty_files.join(", ")
        );
    }
    let remote = git(&root, &["remote", "get-url", "origin"], limits)?
        .ok_or_else(|| anyhow::anyhow!("no GitHub origin remote is configured"))?;
    let repository = github_repository(&remote)
        .ok_or_else(|| anyhow::anyhow!("origin is not a GitHub repository"))?;
    Ok(RepositorySnapshot {
        root,
        repository,
        remote_url: remote,
        branch,
        head_sha,
        finalization_commit_required: !dirty_files.is_empty(),
        dirty_files,
    })
}

fn finalize_documents(
    task: &Task,
    snapshot: &RepositorySnapshot,
    limits: &ExecutionLimits,
) -> Result<()> {
    let written = task
        .history
        .iter()
        .find_map(|event| match &event.event {
            crate::task::TaskEvent::SubmissionDocumentationGenerated { written, .. } => {
                Some(written)
            }
            _ => None,
        })
        .cloned()
        .unwrap_or_default();
    if written.is_empty() {
        bail!("generated-document changes could not be identified safely");
    }
    let mut stage = crate::process_environment::command("git");
    stage
        .args(["add", "--"])
        .args(&written)
        .current_dir(&snapshot.root);
    let staged = run_git(stage, limits)?;
    if !staged.status.success() {
        bail!(
            "could not stage final submission documents: {}",
            stderr(&staged)
        );
    }
    let commit = git_output(
        &snapshot.root,
        &[
            "-c",
            "user.name=multiagent-chat",
            "-c",
            "user.email=multiagent-chat@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--message",
            "docs: finalize submission artifacts",
        ],
        limits,
    )?;
    if !commit.status.success() {
        bail!(
            "could not create the final submission commit: {}",
            stderr(&commit)
        );
    }
    Ok(())
}

fn github_repository(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let candidate = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("http://github.com/"))
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let mut parts = candidate.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty()
        || repo.is_empty()
        || parts.next().is_some()
        || !owner.chars().all(valid_segment)
        || !repo.chars().all(valid_segment)
    {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn valid_segment(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

fn git(root: &Path, args: &[&str], limits: &ExecutionLimits) -> Result<Option<String>> {
    let output = git_output(root, args, limits)?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty()))
}

fn git_output(
    root: &Path,
    args: &[&str],
    limits: &ExecutionLimits,
) -> Result<std::process::Output> {
    let mut command = crate::process_environment::command("git");
    command.args(args).current_dir(root);
    run_git(command, limits)
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskEvent, TaskResult};

    #[test]
    fn accepts_github_remote_forms_without_credentials() {
        assert_eq!(
            github_repository("https://github.com/acme/app.git"),
            Some("acme/app".into())
        );
        assert_eq!(
            github_repository("git@github.com:acme/app.git"),
            Some("acme/app".into())
        );
        assert!(github_repository("https://evil.example/acme/app.git").is_none());
        assert!(github_repository("https://user:secret@github.com/acme/app.git").is_none());
    }

    #[test]
    fn generated_document_finalization_creates_only_the_safe_final_commit() {
        let root = std::env::temp_dir().join(format!("mac-publish-docs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("docs")).unwrap();
        let mut init = crate::process_environment::command("git");
        init.args(["init", "--quiet"]).current_dir(&root);
        assert!(
            run_git(init, &ExecutionLimits::default())
                .unwrap()
                .status
                .success()
        );
        fs::write(root.join("README.md"), "base\n").unwrap();
        let mut add = crate::process_environment::command("git");
        add.args(["add", "."]).current_dir(&root);
        run_git(add, &ExecutionLimits::default()).unwrap();
        let mut commit = crate::process_environment::command("git");
        commit
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "base",
            ])
            .current_dir(&root);
        assert!(
            run_git(commit, &ExecutionLimits::default())
                .unwrap()
                .status
                .success()
        );
        fs::write(root.join("README.md"), "generated\n").unwrap();

        let manager = crate::task::TaskManager::new();
        let task = manager.create("publish", "description", "legacy");
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::SubmissionDocumentationGenerated {
            written: vec!["README.md".into()],
            preserved: Vec::new(),
        });
        emitter.emit(TaskEvent::Result {
            result: TaskResult {
                source_revision: None,
                verification: Vec::new(),
                diff: String::new(),
            },
        });
        emitter.emit(TaskEvent::Finished {
            status: crate::task::TaskStatus::Completed,
            error: None,
        });
        let task = manager.get(task.id).unwrap();
        let snapshot = RepositorySnapshot {
            root: root.clone(),
            repository: "acme/app".into(),
            remote_url: "https://github.com/acme/app.git".into(),
            branch: "master".into(),
            head_sha: "base".into(),
            dirty_files: vec!["README.md".into()],
            finalization_commit_required: true,
        };
        finalize_documents(&task, &snapshot, &ExecutionLimits::default()).unwrap();
        let mut log = crate::process_environment::command("git");
        log.args(["log", "-1", "--format=%s"]).current_dir(&root);
        let output = run_git(log, &ExecutionLimits::default()).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "docs: finalize submission artifacts"
        );
        fs::remove_dir_all(root).ok();
    }
}
