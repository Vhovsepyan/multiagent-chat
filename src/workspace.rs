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
    /// Credential-free source identity retained by the server. This is never
    /// written to the worker repository's Git configuration.
    pub source_repository: Option<String>,
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
                    // `git clone` creates an `origin` remote. The workspace is
                    // handed to an unattended coding worker, so that inherited
                    // publishing capability must be removed before inspection
                    // or implementation can reach it. The checked-out commit
                    // below remains the task's source baseline.
                    remove_worker_remotes(&path, &self.limits)?;
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
                source_repository: request
                    .source
                    .map(|source| source.repository_identity().to_owned()),
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

/// Remove remotes inherited by a cloned source before the repository is made
/// available to a coding worker. A task workspace never needs a remote to
/// inspect, modify, verify, diff, or commit its local checkout; explicit
/// publishing operates later on persistent output through its own boundary.
fn remove_worker_remotes(repo: &Path, limits: &ExecutionLimits) -> Result<()> {
    let remotes = git_command(repo, &["remote"], limits)?;
    if !remotes.status.success() {
        bail!("could not inspect cloned repository remotes");
    }
    for remote in String::from_utf8_lossy(&remotes.stdout).lines() {
        let remote = remote.trim();
        if remote.is_empty() {
            continue;
        }
        let output = git_command(repo, &["remote", "remove", remote], limits)?;
        if !output.status.success() {
            bail!("could not disable inherited Git remote {remote:?}");
        }
    }
    let remaining = git_command(repo, &["remote"], limits)?;
    if !remaining.status.success() {
        bail!("could not verify cloned repository remotes");
    }
    if !String::from_utf8_lossy(&remaining.stdout).trim().is_empty() {
        bail!("cloned repository still has a worker-accessible Git remote");
    }
    Ok(())
}

#[cfg(test)]
pub fn diff_result(root: &Path) -> Result<String> {
    Ok(change_set(root)?.render())
}

/// What a captured result is measured against (task 0009 follow-up).
///
/// Once milestones are committed, the working tree matches HEAD and a
/// HEAD-relative diff reports nothing. A task result must instead describe
/// everything the run produced, committed or not.
#[derive(Debug, Clone, Copy)]
pub enum DiffBaseline<'a> {
    /// Uncommitted work only — the pre-0009 view. Production capture always
    /// measures from the task baseline, so only tests build this variant.
    #[allow(dead_code)]
    Head,
    /// The revision the workspace was prepared at: milestone commits made
    /// since then are part of the result, as is anything still uncommitted.
    Revision(&'a str),
    /// The task started from an empty project, so everything present is new.
    EmptyProject,
}

/// The complete change a task produced, whatever its Git mode.
///
/// `revision` is `TaskWorkspace::revision`: the source revision of an existing
/// repository, or `None` for a New Project, which starts empty.
pub fn task_result_diff(
    root: &Path,
    revision: Option<&str>,
    limits: &ExecutionLimits,
) -> Result<String> {
    let baseline = match revision {
        Some(revision) => DiffBaseline::Revision(revision),
        None => DiffBaseline::EmptyProject,
    };
    Ok(change_set_from(root, baseline, limits)?.render())
}

#[cfg(test)]
pub fn change_set(root: &Path) -> Result<ChangeSet> {
    change_set_with_limits(root, &ExecutionLimits::default())
}

#[cfg(test)]
fn change_set_with_limits(root: &Path, limits: &ExecutionLimits) -> Result<ChangeSet> {
    change_set_from(root, DiffBaseline::Head, limits)
}

fn change_set_from(
    root: &Path,
    baseline: DiffBaseline<'_>,
    limits: &ExecutionLimits,
) -> Result<ChangeSet> {
    let status = git_output_bytes(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        limits,
    )?;
    let has_head = git_command(root, &["rev-parse", "--verify", "HEAD"], limits)?
        .status
        .success();
    let (files, tracked_diff) = match baseline {
        DiffBaseline::Head => {
            let tracked = if has_head {
                git_output(
                    root,
                    &["diff", "--no-ext-diff", "--find-renames", "HEAD"],
                    limits,
                )?
            } else {
                String::new()
            };
            (parse_status(&status), tracked)
        }
        DiffBaseline::Revision(revision) => {
            // `git diff <revision>` compares the WORKING TREE with that commit,
            // so committed milestones and uncommitted work arrive together.
            let named = git_output_bytes(
                root,
                &["diff", "--name-status", "-z", "--find-renames", revision],
                limits,
            )?;
            let mut files = parse_name_status(&named);
            // Untracked files belong to no tree, so they come from status.
            files.extend(
                parse_status(&status)
                    .into_iter()
                    .filter(|change| change.untracked),
            );
            files.sort_by(|left, right| left.path.cmp(&right.path));
            let tracked = git_output(
                root,
                &["diff", "--no-ext-diff", "--find-renames", revision],
                limits,
            )?;
            (files, tracked)
        }
        DiffBaseline::EmptyProject => {
            // Nothing existed at the start, so every file present now — whether
            // a milestone committed it or the worker just wrote it — is added,
            // and its content is read from disk rather than from a diff.
            let listing = git_output_bytes(
                root,
                &[
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                ],
                limits,
            )?;
            (parse_file_listing(root, &listing), String::new())
        }
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

/// `git diff --name-status -z`: a status field, then one path (two for a
/// rename or copy), each NUL-terminated.
fn parse_name_status(raw: &[u8]) -> Vec<FileChange> {
    let fields = raw
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut index = 0;
    while index + 1 < fields.len() {
        let code = fields[index][0];
        let renamed = matches!(code, b'R' | b'C');
        let (path, previous_path) = if renamed && index + 2 < fields.len() {
            let previous = String::from_utf8_lossy(fields[index + 1]).into_owned();
            let path = String::from_utf8_lossy(fields[index + 2]).into_owned();
            index += 3;
            (path, Some(previous))
        } else {
            let path = String::from_utf8_lossy(fields[index + 1]).into_owned();
            index += 2;
            (path, None)
        };
        let kind = match code {
            b'A' => ChangeKind::Added,
            b'D' => ChangeKind::Deleted,
            b'R' | b'C' => ChangeKind::Renamed,
            _ => ChangeKind::Modified,
        };
        changes.push(FileChange {
            path,
            previous_path,
            kind,
            // Its content is already part of the tracked diff.
            untracked: false,
        });
    }
    changes
}

/// `git ls-files -z --cached --others --exclude-standard`: every file the
/// project currently has. Index entries whose file is gone are skipped, since
/// there is nothing to render for them against an empty baseline.
fn parse_file_listing(root: &Path, raw: &[u8]) -> Vec<FileChange> {
    let mut paths = raw
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .filter(|path| root.join(path).exists())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| FileChange {
            path,
            previous_path: None,
            kind: ChangeKind::Added,
            // Rendered from disk, exactly like an untracked file.
            untracked: true,
        })
        .collect()
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

    fn git_text(root: &Path, args: &[&str]) -> String {
        let output = crate::process_environment::command("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
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
        assert!(git_text(&first.path, &["remote"]).is_empty());
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
    fn cloned_worker_workspace_removes_inherited_remotes_without_losing_baseline() {
        let source = test_repository();
        let root = std::env::temp_dir().join(format!("mac-worker-remotes-{}", Uuid::new_v4()));
        let remote = root.join("source.git");
        let worker = root.join("worker");
        fs::create_dir_all(&root).unwrap();
        git(&source, &["clone", "--bare", ".", remote.to_str().unwrap()]);
        git(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                worker.to_str().unwrap(),
            ],
        );
        git(
            &worker,
            &["remote", "add", "secondary", remote.to_str().unwrap()],
        );
        let baseline = git_text(&worker, &["rev-parse", "HEAD"]);

        remove_worker_remotes(&worker, &ExecutionLimits::default()).unwrap();

        assert!(git_text(&worker, &["remote"]).is_empty());
        let push = crate::process_environment::command("git")
            .args(["push", "origin", "HEAD"])
            .current_dir(&worker)
            .output()
            .unwrap();
        assert!(
            !push.status.success(),
            "worker unexpectedly retained origin"
        );

        fs::write(worker.join("tracked.txt"), "after\n").unwrap();
        let diff = task_result_diff(&worker, Some(&baseline), &ExecutionLimits::default()).unwrap();
        assert!(diff.contains("-before\n+after"), "{diff}");

        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(root).unwrap();
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

    // --- task 0009 follow-up: results are measured from the task baseline ---

    fn commit(root: &Path, message: &str) -> String {
        git(root, &["add", "--all", "."]);
        git(root, &["commit", "--quiet", "-m", message]);
        head(root)
    }

    fn head(root: &Path) -> String {
        let output = crate::process_environment::command("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// An empty project workspace, as `prepare` leaves it for a New Project:
    /// initialized, with no commit and therefore no source revision.
    fn empty_project() -> PathBuf {
        let root = std::env::temp_dir().join(format!("mac-newproject-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "tests@example.com"]);
        git(&root, &["config", "user.name", "Tests"]);
        root
    }

    /// The bug this fixes: after a milestone commit the working tree matches
    /// HEAD, so a HEAD-relative capture reports nothing at all.
    #[test]
    fn committed_milestone_work_stays_in_the_task_result() {
        let root = test_repository();
        let baseline = head(&root);
        fs::write(root.join("feature.rs"), "fn feature() {}\n").unwrap();
        commit(&root, "feat(milestone-01): feature");

        assert_eq!(diff_result(&root).unwrap(), "No working-tree changes.");

        let result = task_result_diff(&root, Some(&baseline), &ExecutionLimits::default()).unwrap();
        assert!(result.contains("Added: feature.rs"), "{result}");
        assert!(result.contains("fn feature() {}"), "{result}");
    }

    /// Several milestones accumulate into one result, not just the last one.
    #[test]
    fn multiple_milestone_commits_produce_a_cumulative_result() {
        let root = test_repository();
        let baseline = head(&root);

        fs::write(root.join("first.rs"), "fn first() {}\n").unwrap();
        commit(&root, "feat(milestone-01): first");
        fs::write(root.join("second.rs"), "fn second() {}\n").unwrap();
        fs::write(root.join("tracked.txt"), "after\n").unwrap();
        fs::remove_file(root.join("delete.txt")).unwrap();
        commit(&root, "feat(milestone-02): second");

        let result = task_result_diff(&root, Some(&baseline), &ExecutionLimits::default()).unwrap();

        assert!(result.contains("Added: first.rs"), "{result}");
        assert!(result.contains("Added: second.rs"), "{result}");
        assert!(result.contains("Modified: tracked.txt"), "{result}");
        assert!(result.contains("Deleted: delete.txt"), "{result}");
        assert!(result.contains("fn first() {}"), "{result}");
        assert!(result.contains("fn second() {}"), "{result}");
    }

    /// A milestone that fails after earlier ones committed must still show the
    /// committed work AND whatever the failing milestone left behind.
    #[test]
    fn a_failed_later_milestone_keeps_earlier_commits_and_current_work() {
        let root = test_repository();
        let baseline = head(&root);
        fs::write(root.join("done.rs"), "fn done() {}\n").unwrap();
        commit(&root, "feat(milestone-01): done");

        // Milestone two fails: its work is only in the working tree.
        fs::write(root.join("half-done.rs"), "fn half() {\n").unwrap();
        fs::write(
            root.join("tracked.txt"),
            "edited by the failing milestone\n",
        )
        .unwrap();

        let result = task_result_diff(&root, Some(&baseline), &ExecutionLimits::default()).unwrap();

        assert!(result.contains("Added: done.rs"), "{result}");
        assert!(result.contains("fn done() {}"), "{result}");
        assert!(result.contains("Added: half-done.rs"), "{result}");
        assert!(result.contains("fn half() {"), "{result}");
        assert!(result.contains("Modified: tracked.txt"), "{result}");
        assert!(
            result.contains("edited by the failing milestone"),
            "{result}"
        );
    }

    /// A New Project has no baseline revision, so everything it generated is
    /// the result — committed or not.
    #[test]
    fn a_committed_new_project_still_reports_its_generated_files() {
        let root = empty_project();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        commit(&root, "feat(milestone-01): bootstrap");
        // A later milestone is still in progress.
        fs::write(root.join("src/lib.rs"), "pub fn work() {}\n").unwrap();

        let result = task_result_diff(&root, None, &ExecutionLimits::default()).unwrap();

        assert_ne!(result, "No working-tree changes.");
        assert!(result.contains("Added: Cargo.toml"), "{result}");
        assert!(result.contains("Added: src/main.rs"), "{result}");
        assert!(result.contains("Added: src/lib.rs"), "{result}");
        assert!(result.contains("fn main() {}"), "{result}");
        assert!(result.contains("pub fn work() {}"), "{result}");
        // Git's own metadata is never part of the result.
        assert!(!result.contains(".git/"), "{result}");
    }

    /// With no commits the baseline capture must match what the task produced
    /// before this change: the same files, the same content.
    #[test]
    fn uncommitted_only_results_are_unchanged() {
        let root = test_repository();
        let baseline = head(&root);
        fs::write(root.join("tracked.txt"), "after\n").unwrap();
        fs::write(root.join("added.txt"), "new file\n").unwrap();
        fs::remove_file(root.join("delete.txt")).unwrap();

        let previous = diff_result(&root).unwrap();
        let result = task_result_diff(&root, Some(&baseline), &ExecutionLimits::default()).unwrap();

        for expected in [
            "Modified: tracked.txt",
            "Added: added.txt",
            "Deleted: delete.txt",
            "new file",
        ] {
            assert!(previous.contains(expected), "{previous}");
            assert!(result.contains(expected), "{result}");
        }
    }

    /// The same for a New Project that never committed: unchanged behavior.
    #[test]
    fn an_uncommitted_new_project_result_is_unchanged() {
        let root = empty_project();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let previous = diff_result(&root).unwrap();
        let result = task_result_diff(&root, None, &ExecutionLimits::default()).unwrap();

        assert!(previous.contains("Added: main.rs"), "{previous}");
        assert!(result.contains("Added: main.rs"), "{result}");
        assert!(result.contains("fn main() {}"), "{result}");
    }

    /// A clean run that committed everything is not "no changes"; a run that
    /// produced nothing at all still is.
    #[test]
    fn an_untouched_workspace_still_reports_no_changes() {
        let root = test_repository();
        let baseline = head(&root);

        assert_eq!(
            task_result_diff(&root, Some(&baseline), &ExecutionLimits::default()).unwrap(),
            "No working-tree changes."
        );
    }

    /// Renames survive the baseline capture, including across a commit.
    #[test]
    fn committed_renames_are_reported_as_renames() {
        let root = test_repository();
        let baseline = head(&root);
        git(&root, &["mv", "tracked.txt", "renamed.txt"]);
        commit(&root, "feat(milestone-01): rename");

        let result = task_result_diff(&root, Some(&baseline), &ExecutionLimits::default()).unwrap();

        assert!(
            result.contains("Renamed: tracked.txt -> renamed.txt"),
            "{result}"
        );
    }
}
