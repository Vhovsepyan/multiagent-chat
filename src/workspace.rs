//! Disposable, server-side task workspace preparation.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::execution_limits::{ExecutionLimits, GIT_OUTPUT_BYTES};
use crate::project::ProjectSource;
use crate::task::TaskId;

#[derive(Debug, Clone)]
pub struct WorkspaceRequest<'a> {
    pub task_id: TaskId,
    pub source: Option<&'a ProjectSource>,
    pub revision: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct TaskWorkspace {
    /// Provider-owned lifecycle root; repository and artifacts are siblings.
    pub root: PathBuf,
    /// Only this directory is inspected, implemented, verified, and diffed.
    pub path: PathBuf,
    pub revision: Option<String>,
}

impl TaskWorkspace {
    pub fn artifacts(&self) -> PathBuf {
        self.root.join("artifacts")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub previous_path: Option<String>,
    pub kind: ChangeKind,
    untracked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    pub files: Vec<FileChange>,
    tracked_diff: String,
    untracked_diffs: Vec<String>,
}

impl ChangeSet {
    pub fn render(&self) -> String {
        if self.files.is_empty() {
            return "No working-tree changes.".into();
        }

        let mut result = String::from("Changes:\n");
        for change in &self.files {
            let label = match change.kind {
                ChangeKind::Added => "Added",
                ChangeKind::Modified => "Modified",
                ChangeKind::Deleted => "Deleted",
                ChangeKind::Renamed => "Renamed",
            };
            if let Some(previous) = &change.previous_path {
                result.push_str(&format!("{label}: {previous} -> {}\n", change.path));
            } else {
                result.push_str(&format!("{label}: {}\n", change.path));
            }
        }

        let diffs = std::iter::once(self.tracked_diff.as_str())
            .chain(self.untracked_diffs.iter().map(String::as_str))
            .filter(|diff| !diff.is_empty())
            .collect::<Vec<_>>();
        if !diffs.is_empty() {
            result.push_str("\nDiff:\n");
            result.push_str(&diffs.join("\n"));
        }
        result.trim_end().to_string()
    }
}

pub trait WorkspaceProvider: Send + Sync {
    fn prepare(&self, request: WorkspaceRequest<'_>) -> Result<TaskWorkspace>;
    fn cleanup(&self, workspace: &TaskWorkspace) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct LocalWorkspaceProvider {
    root: PathBuf,
    limits: ExecutionLimits,
}

impl LocalWorkspaceProvider {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("could not create task workspace root {}", root.display()))?;
        Ok(Self {
            root,
            limits: ExecutionLimits::default(),
        })
    }

    pub fn temporary_with_limits(limits: ExecutionLimits) -> Result<Self> {
        let mut provider = Self::new(std::env::temp_dir().join("multiagent-chat-workspaces"))?;
        provider.limits = limits;
        Ok(provider)
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
        let root = self.task_path(request.task_id);
        self.ensure_owned(&root)?;
        // Exclusive creation avoids reusing an existing task or linked root.
        fs::create_dir(&root).context("could not create a fresh task workspace")?;
        let path = root.join("repo");
        let prepared = (|| -> Result<TaskWorkspace> {
            fs::create_dir(root.join("artifacts"))?;
            match request.source {
                Some(source) => {
                    let mut command = crate::process_environment::command("git");
                    command.arg("clone").arg("--depth").arg("1");
                    if let Some(revision) = request.revision {
                        command.arg("--branch").arg(revision);
                    }
                    command.arg(source.clone_url()).arg(&path);
                    let output = run_git(command, &self.limits)
                        .context("could not start git to prepare repository workspace")?;
                    if !output.status.success() {
                        bail!(
                            "could not prepare repository workspace: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        );
                    }
                }
                None => {
                    fs::create_dir(&path).with_context(|| {
                        format!("could not create workspace {}", path.display())
                    })?;
                    let mut command = crate::process_environment::command("git");
                    command.arg("init").arg("--quiet").current_dir(&path);
                    let output = run_git(command, &self.limits)
                        .context("could not initialize new-project workspace")?;
                    if !output.status.success() {
                        bail!("could not initialize new-project workspace");
                    }
                }
            }

            let revision_output = git_command(&path, &["rev-parse", "HEAD"], &self.limits)?;
            let revision = revision_output.status.success().then(|| {
                String::from_utf8_lossy(&revision_output.stdout)
                    .trim()
                    .to_owned()
            });
            Ok(TaskWorkspace {
                root: root.clone(),
                path,
                revision,
            })
        })();
        if prepared.is_err() {
            let _ = fs::remove_dir_all(&root);
        }
        prepared
    }

    fn cleanup(&self, workspace: &TaskWorkspace) -> Result<()> {
        self.ensure_owned(&workspace.root)?;
        if workspace.path != workspace.root.join("repo") {
            bail!("repository path does not belong to the task workspace");
        }
        if workspace.root.exists() {
            fs::remove_dir_all(&workspace.root).with_context(|| {
                format!("could not clean workspace {}", workspace.root.display())
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub fn diff_result(root: &Path) -> Result<String> {
    Ok(change_set(root)?.render())
}

pub fn diff_result_with_limits(root: &Path, limits: &ExecutionLimits) -> Result<String> {
    Ok(change_set_with_limits(root, limits)?.render())
}

#[cfg(test)]
pub fn change_set(root: &Path) -> Result<ChangeSet> {
    change_set_with_limits(root, &ExecutionLimits::default())
}

fn change_set_with_limits(root: &Path, limits: &ExecutionLimits) -> Result<ChangeSet> {
    let status = git_output_bytes(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        limits,
    )?;
    let files = parse_status(&status);
    let has_head = git_command(root, &["rev-parse", "--verify", "HEAD"], limits)?
        .status
        .success();
    let tracked_diff = if has_head {
        git_output(
            root,
            &["diff", "--no-ext-diff", "--find-renames", "HEAD"],
            limits,
        )?
    } else {
        String::new()
    };
    let mut remaining = GIT_OUTPUT_BYTES.saturating_sub(tracked_diff.len() + status.len());
    let mut untracked_diffs = Vec::new();
    for change in files.iter().filter(|change| change.untracked) {
        let diff = added_file_diff(root, &change.path, remaining)?;
        remaining = remaining.checked_sub(diff.len()).ok_or_else(|| {
            anyhow::anyhow!("result size limit exceeded; workspace required for full recovery")
        })?;
        untracked_diffs.push(diff);
    }

    Ok(ChangeSet {
        files,
        tracked_diff,
        untracked_diffs,
    })
}

fn parse_status(status: &[u8]) -> Vec<FileChange> {
    let entries = status.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        let entry = entries[index];
        if entry.len() < 4 {
            index += 1;
            continue;
        }
        let code = &entry[..2];
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let renamed = code.contains(&b'R') || code.contains(&b'C');
        let (path, previous_path) = if renamed && index + 1 < entries.len() {
            index += 1;
            (
                path,
                Some(String::from_utf8_lossy(entries[index]).into_owned()),
            )
        } else {
            (path, None)
        };
        let kind = if code == b"??" || code.contains(&b'A') {
            ChangeKind::Added
        } else if code.contains(&b'D') {
            ChangeKind::Deleted
        } else if renamed {
            ChangeKind::Renamed
        } else {
            ChangeKind::Modified
        };
        changes.push(FileChange {
            path,
            previous_path,
            kind,
            untracked: code == b"??",
        });
        index += 1;
    }
    changes
}

fn added_file_diff(root: &Path, path: &str, budget: usize) -> Result<String> {
    let display = display_diff_path(path);
    let file_path = root.join(path);
    match fs::symlink_metadata(&file_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Ok(format!(
                "diff --git /dev/null {display}\nnew file\nUnsupported file type; contents omitted."
            ));
        }
        Err(error) => {
            return Ok(format!(
                "diff --git /dev/null {display}\nnew file\nUnable to inspect added file: {error}"
            ));
        }
        Ok(_) => {}
    }
    use std::io::Read;
    let content_limit = budget / 2;
    let bytes = match crate::repository_file::open(root, &file_path).and_then(|file| {
        let mut bytes = Vec::new();
        file.take(content_limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(format!(
                "diff --git /dev/null {display}\nnew file\nUnable to read added file: {error}"
            ));
        }
    };
    if bytes.len() > content_limit {
        bail!("result size limit exceeded; workspace required for full recovery");
    }
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Ok(format!(
            "diff --git /dev/null {display}\nnew file\nBinary or unsupported file; contents omitted."
        ));
    }

    let content = String::from_utf8(bytes).expect("UTF-8 was checked");
    let line_count = content.lines().count();
    let mut diff = format!(
        "diff --git /dev/null {display}\nnew file mode 100644\n--- /dev/null\n+++ {display}\n@@ -0,0 +1,{line_count} @@\n"
    );
    for line in content.split_inclusive('\n') {
        diff.push('+');
        diff.push_str(line);
    }
    if !content.is_empty() && !content.ends_with('\n') {
        diff.push_str("\n\\ No newline at end of file\n");
    }
    Ok(diff.trim_end().to_string())
}

fn display_diff_path(path: &str) -> String {
    let path = format!("b/{path}");
    if path.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
    }) {
        path
    } else {
        format!("{path:?}")
    }
}

fn git_output(root: &Path, args: &[&str], limits: &ExecutionLimits) -> Result<String> {
    let output = git_command(root, args, limits)?;
    if !output.status.success() {
        bail!("git command failed in task workspace");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_output_bytes(root: &Path, args: &[&str], limits: &ExecutionLimits) -> Result<Vec<u8>> {
    let output = git_command(root, args, limits)?;
    if !output.status.success() {
        bail!("git command failed in task workspace");
    }
    Ok(output.stdout)
}

fn git_command(
    root: &Path,
    args: &[&str],
    limits: &ExecutionLimits,
) -> Result<std::process::Output> {
    let mut command = crate::process_environment::command("git");
    command.args(args).current_dir(root);
    run_git(command, limits)
}

pub(crate) fn run_git(
    command: std::process::Command,
    limits: &ExecutionLimits,
) -> Result<std::process::Output> {
    let output = crate::process_runner::run_blocking(command, limits.git())?;
    if let Some(failure) = output.failure {
        bail!("{failure}: {}", output.stderr.text());
    }
    if output.stdout.truncated || output.stderr.truncated {
        bail!("Git output limit exceeded; incomplete result must not be parsed");
    }
    Ok(std::process::Output {
        status: output
            .status
            .ok_or_else(|| anyhow::anyhow!("Git exited without a status"))?,
        stdout: output.stdout.bytes,
        stderr: output.stderr.bytes,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_added_file_fails_capture_without_removing_the_file() {
        let root = test_repository();
        fs::write(root.join("large.txt"), "large content".repeat(20)).unwrap();
        let error = added_file_diff(&root, "large.txt", 64).unwrap_err();
        assert!(error.to_string().contains("result size limit exceeded"));
        assert!(root.join("large.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }
    use super::*;
    use uuid::Uuid;

    fn git(root: &Path, args: &[&str]) {
        let output = crate::process_environment::command("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("mac-diff-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "tests@example.com"]);
        git(&root, &["config", "user.name", "Tests"]);
        fs::write(root.join("tracked.txt"), "before\n").unwrap();
        fs::write(root.join("delete.txt"), "delete me\n").unwrap();
        git(&root, &["add", "tracked.txt", "delete.txt"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        root
    }

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
        assert_eq!(first.path, first.root.join("repo"));
        assert!(first.artifacts().is_dir());
        assert!(!first.artifacts().starts_with(&first.path));
        let artifact =
            crate::spec::write_artifact(&first.artifacts(), "approved orchestration-only text")
                .unwrap();
        assert!(!first.path.join("SPEC.md").exists());
        assert!(change_set(&first.path).unwrap().files.is_empty());
        assert_eq!(
            diff_result(&first.path).unwrap(),
            "No working-tree changes."
        );
        provider.cleanup(&first).unwrap();
        assert!(!artifact.exists());
        assert!(!first.root.exists());
        provider.cleanup(&first).unwrap();
        assert!(second.path.is_dir());
        provider.cleanup(&second).unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn preserves_project_owned_spec_and_excludes_artifact_from_changes() {
        let root = std::env::temp_dir().join(format!("mac-spec-workspace-{}", Uuid::new_v4()));
        let provider = LocalWorkspaceProvider::new(root.clone()).unwrap();
        let workspace = provider
            .prepare(WorkspaceRequest {
                task_id: Uuid::new_v4(),
                source: None,
                revision: None,
            })
            .unwrap();
        let owned = b"# User-owned SPEC\r\nDo not replace.\r\n";
        fs::write(workspace.path.join("SPEC.md"), owned).unwrap();
        git(
            &workspace.path,
            &["config", "user.email", "tests@example.com"],
        );
        git(&workspace.path, &["config", "user.name", "Tests"]);
        git(&workspace.path, &["add", "SPEC.md"]);
        git(
            &workspace.path,
            &["commit", "--quiet", "-m", "project-owned specification"],
        );
        let artifact =
            crate::spec::write_artifact(&workspace.artifacts(), "approved task text").unwrap();
        assert_eq!(fs::read(workspace.path.join("SPEC.md")).unwrap(), owned);
        assert_eq!(fs::read_to_string(artifact).unwrap(), "approved task text");
        assert!(change_set(&workspace.path).unwrap().files.is_empty());
        provider.cleanup(&workspace).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn includes_modified_tracked_file_content() {
        let root = test_repository();
        fs::write(root.join("tracked.txt"), "after\n").unwrap();
        let result = diff_result(&root).unwrap();
        assert!(result.contains("Modified: tracked.txt"));
        assert!(result.contains("-before"));
        assert!(result.contains("+after"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn renders_untracked_text_as_an_added_file_diff() {
        let root = test_repository();
        fs::write(root.join("new.txt"), "first\nsecond\n").unwrap();
        let result = diff_result(&root).unwrap();
        assert!(result.contains("Added: new.txt"));
        assert!(result.contains("--- /dev/null"));
        assert!(result.contains("+++ b/new.txt"));
        assert!(result.contains("+first\n+second"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn includes_deleted_files() {
        let root = test_repository();
        fs::remove_file(root.join("delete.txt")).unwrap();
        let result = diff_result(&root).unwrap();
        assert!(result.contains("Deleted: delete.txt"));
        assert!(result.contains("deleted file mode"));
        assert!(result.contains("-delete me"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn combines_multiple_kinds_of_change() {
        let root = test_repository();
        fs::write(root.join("tracked.txt"), "changed\n").unwrap();
        fs::remove_file(root.join("delete.txt")).unwrap();
        fs::write(root.join("new.txt"), "created\n").unwrap();
        let changes = change_set(&root).unwrap();
        assert_eq!(changes.files.len(), 3);
        let result = changes.render();
        assert!(result.contains("Modified: tracked.txt"));
        assert!(result.contains("Deleted: delete.txt"));
        assert!(result.contains("Added: new.txt"));
        assert!(result.contains("+created"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reports_renames_when_git_detects_them() {
        let root = test_repository();
        git(&root, &["mv", "tracked.txt", "renamed.txt"]);
        let changes = change_set(&root).unwrap();
        assert!(changes.files.iter().any(|change| {
            change.kind == ChangeKind::Renamed
                && change.previous_path.as_deref() == Some("tracked.txt")
                && change.path == "renamed.txt"
        }));
        let result = changes.render();
        assert!(result.contains("Renamed: tracked.txt -> renamed.txt"));
        assert!(result.contains("rename from tracked.txt"));
        assert!(result.contains("rename to renamed.txt"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn omits_untracked_binary_contents() {
        let root = test_repository();
        fs::write(root.join("image.bin"), [0, 159, 146, 150]).unwrap();
        let result = diff_result(&root).unwrap();
        assert!(result.contains("Added: image.bin"));
        assert!(result.contains("Binary or unsupported file; contents omitted."));
        assert!(!result.contains(char::REPLACEMENT_CHARACTER));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reports_an_empty_working_tree() {
        let root = test_repository();
        assert_eq!(diff_result(&root).unwrap(), "No working-tree changes.");
        fs::remove_dir_all(root).ok();
    }
}
