//! Disposable, server-side task workspace preparation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::project::ProjectSource;
use crate::task::TaskId;

#[derive(Debug, Clone)]
pub struct WorkspaceRequest<'a> {
    pub task_id: TaskId,
    pub source: Option<&'a ProjectSource>,
    pub revision: Option<&'a str>,
}

#[derive(Debug)]
pub struct TaskWorkspace {
    pub path: PathBuf,
    pub revision: Option<String>,
}

pub trait WorkspaceProvider: Send + Sync {
    fn prepare(&self, request: WorkspaceRequest<'_>) -> Result<TaskWorkspace>;
    fn cleanup(&self, workspace: &TaskWorkspace) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct LocalWorkspaceProvider {
    root: PathBuf,
}

impl LocalWorkspaceProvider {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("could not create task workspace root {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn temporary() -> Result<Self> {
        Self::new(std::env::temp_dir().join("multiagent-chat-workspaces"))
    }

    fn task_path(&self, id: TaskId) -> PathBuf {
        self.root.join(id.to_string())
    }

    fn ensure_owned(&self, path: &Path) -> Result<()> {
        if path.parent() != Some(self.root.as_path()) {
            bail!("refusing to operate outside the managed workspace root");
        }
        Ok(())
    }
}

impl WorkspaceProvider for LocalWorkspaceProvider {
    fn prepare(&self, request: WorkspaceRequest<'_>) -> Result<TaskWorkspace> {
        let path = self.task_path(request.task_id);
        self.ensure_owned(&path)?;
        if path.exists() {
            bail!("task workspace already exists");
        }

        match request.source {
            Some(source) => {
                let mut command = Command::new("git");
                command.arg("clone").arg("--depth").arg("1");
                if let Some(revision) = request.revision {
                    command.arg("--branch").arg(revision);
                }
                let output = command
                    .arg(source.clone_url())
                    .arg(&path)
                    .output()
                    .context("could not start git to prepare repository workspace")?;
                if !output.status.success() {
                    let _ = fs::remove_dir_all(&path);
                    bail!(
                        "could not prepare repository workspace: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
            }
            None => {
                fs::create_dir(&path)
                    .with_context(|| format!("could not create workspace {}", path.display()))?;
                let output = Command::new("git")
                    .arg("init")
                    .arg("--quiet")
                    .current_dir(&path)
                    .output()
                    .context("could not initialize new-project workspace")?;
                if !output.status.success() {
                    let _ = fs::remove_dir_all(&path);
                    bail!("could not initialize new-project workspace");
                }
            }
        }

        let revision = git_output(&path, &["rev-parse", "HEAD"])
            .ok()
            .filter(|value| !value.is_empty());
        Ok(TaskWorkspace { path, revision })
    }

    fn cleanup(&self, workspace: &TaskWorkspace) -> Result<()> {
        self.ensure_owned(&workspace.path)?;
        if workspace.path.exists() {
            fs::remove_dir_all(&workspace.path).with_context(|| {
                format!("could not clean workspace {}", workspace.path.display())
            })?;
        }
        Ok(())
    }
}

pub fn diff_result(root: &Path) -> Result<String> {
    let status = git_output(root, &["status", "--short"])?;
    let diff = git_output(root, &["diff", "--no-ext-diff"])?;
    let mut result = String::new();
    if !status.is_empty() {
        result.push_str("Status:\n");
        result.push_str(&status);
    }
    if !diff.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str("Diff:\n");
        result.push_str(&diff);
    }
    if result.is_empty() {
        result.push_str("No working-tree changes.");
    }
    Ok(result)
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .context("could not run git in task workspace")?;
    if !output.status.success() {
        bail!("git command failed in task workspace");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn creates_independent_workspaces_and_cleans_idempotently() {
        let root = std::env::temp_dir().join(format!("mac-workspaces-{}", Uuid::new_v4()));
        let provider = LocalWorkspaceProvider::new(root.clone()).unwrap();
        let first = provider
            .prepare(WorkspaceRequest {
                task_id: Uuid::new_v4(),
                source: None,
                revision: None,
            })
            .unwrap();
        let second = provider
            .prepare(WorkspaceRequest {
                task_id: Uuid::new_v4(),
                source: None,
                revision: None,
            })
            .unwrap();
        assert_ne!(first.path, second.path);
        assert!(first.path.is_dir() && second.path.is_dir());
        provider.cleanup(&first).unwrap();
        provider.cleanup(&first).unwrap();
        assert!(second.path.is_dir());
        provider.cleanup(&second).unwrap();
        fs::remove_dir_all(root).ok();
    }
}
