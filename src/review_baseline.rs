//! Read-only milestone review baselines, separate from the cumulative task result.

use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::execution_limits::{ExecutionLimits, bounded_text};
use crate::git::GitMode;
use crate::review::REVIEW_DIFF_BYTES;
use crate::workspace::{TaskWorkspace, run_git, task_result_diff};

const SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const SNAPSHOT_FILES: usize = 10_000;

/// Captured before the worker starts and retained across all correction rounds.
/// Cloning shares the snapshot's lifetime rather than copying repository data.
#[derive(Clone)]
pub enum ReviewBaseline {
    Commit(Option<String>),
    Snapshot(Arc<Snapshot>),
}

pub struct Snapshot {
    directory: PathBuf,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // Only this exclusively created, provider-owned scratch directory.
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl ReviewBaseline {
    pub fn capture(
        workspace: &TaskWorkspace,
        mode: GitMode,
        limits: &ExecutionLimits,
    ) -> Result<Self> {
        if mode.commits_enabled() {
            let status = crate::git::repository_status(&workspace.path, limits)?
                .context("milestone review requires a Git repository")?;
            return Ok(Self::Commit(status.head_sha));
        }
        let snapshot = scratch(workspace)?;
        copy_repository(&workspace.path, &snapshot.directory.join("before"), limits)?;
        Ok(Self::Snapshot(Arc::new(snapshot)))
    }

    pub fn diff(&self, workspace: &TaskWorkspace, limits: &ExecutionLimits) -> Result<String> {
        let diff = match self {
            Self::Commit(revision) => {
                task_result_diff(&workspace.path, revision.as_deref(), limits)?
            }
            Self::Snapshot(before) => {
                // A fresh after-snapshot for each review; the original baseline
                // never advances during the fix loop. Neither snapshot touches
                // the repository's files, index, objects, or history.
                let after = scratch(workspace)?;
                copy_repository(&workspace.path, &after.directory.join("after"), limits)?;
                // Relative arguments also avoid Git for Windows' problems with
                // canonical verbatim (\\?\) paths in no-index directory diffs.
                let before_path = format!(
                    "{}/before",
                    before.directory.file_name().unwrap().to_string_lossy()
                );
                let after_path = format!(
                    "{}/after",
                    after.directory.file_name().unwrap().to_string_lossy()
                );
                let mut command = crate::process_environment::command("git");
                command
                    .current_dir(
                        before
                            .directory
                            .parent()
                            .context("snapshot has no parent")?,
                    )
                    .args(["diff", "--no-index", "--no-ext-diff", "--no-textconv", "--"])
                    .arg(&before_path)
                    .arg(&after_path);
                let output = run_git(command, limits)?;
                if !output.status.success() && output.status.code() != Some(1) {
                    bail!(
                        "could not compare milestone snapshots: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                // Git's no-index headers include the scratch path. Normalize
                // only headers, never source lines that happen to contain it.
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(|line| {
                        if line.starts_with("diff --git ")
                            || line.starts_with("--- ")
                            || line.starts_with("+++ ")
                            || line.starts_with("Binary files ")
                        {
                            line.replace(&format!("{before_path}/"), "")
                                .replace(&format!("{after_path}/"), "")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        Ok(if diff.is_empty() {
            "No current milestone changes.".into()
        } else {
            bounded_text(&diff, REVIEW_DIFF_BYTES)
        })
    }
}

fn scratch(workspace: &TaskWorkspace) -> Result<Snapshot> {
    let artifacts = workspace.artifacts().canonicalize()?;
    if artifacts.starts_with(workspace.path.canonicalize()?) {
        bail!("review snapshots must be outside the repository");
    }
    let directory = artifacts.join(format!("review-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    Ok(Snapshot { directory })
}

fn copy_repository(root: &Path, destination: &Path, limits: &ExecutionLimits) -> Result<()> {
    let mut command = crate::process_environment::command("git");
    command.current_dir(root).args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ]);
    let output = run_git(command, limits)?;
    if !output.status.success() {
        bail!("could not list files for milestone review");
    }
    let names =
        std::str::from_utf8(&output.stdout).context("review requires UTF-8 repository paths")?;
    let names = names
        .split('\0')
        .filter(|name| !name.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    if names.len() > SNAPSHOT_FILES {
        bail!("milestone snapshot exceeds {SNAPSHOT_FILES} files");
    }
    fs::create_dir(destination)?;
    let mut remaining = SNAPSHOT_BYTES;
    for name in names {
        let relative = Path::new(name);
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            bail!("invalid milestone snapshot path");
        }
        let path = root.join(relative);
        // Deleted tracked files are still listed by Git, but not present in
        // this working-tree snapshot. Links (including broken links) fail closed.
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            result => {
                result?;
            }
        }
        let file = crate::repository_file::open(root, &path)
            .with_context(|| format!("cannot snapshot repository file {name:?}"))?;
        let metadata = file.metadata()?;
        if metadata.len() > remaining {
            bail!("milestone snapshot exceeds {SNAPSHOT_BYTES} bytes");
        }
        let mut contents = Vec::new();
        file.take(remaining + 1).read_to_end(&mut contents)?;
        if contents.len() as u64 > remaining {
            bail!("milestone snapshot exceeds {SNAPSHOT_BYTES} bytes");
        }
        remaining -= contents.len() as u64;
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().context("snapshot file has no parent")?)?;
        fs::File::create(&target)?.write_all(&contents)?;
        // Preserve executable-bit changes on Unix. Windows read-only attributes
        // are not Git modes and must not prevent disposal of scratch copies.
        #[cfg(unix)]
        fs::set_permissions(target, metadata.permissions())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{LocalWorkspaceProvider, WorkspaceProvider, WorkspaceRequest};

    struct Repository {
        workspace: TaskWorkspace,
        provider: LocalWorkspaceProvider,
        directory: PathBuf,
    }

    impl Repository {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("mac-review-baseline-{}", uuid::Uuid::new_v4()));
            let provider = LocalWorkspaceProvider::new(directory.clone()).unwrap();
            let workspace = provider
                .prepare(WorkspaceRequest {
                    task_id: uuid::Uuid::new_v4(),
                    source: None,
                    revision: None,
                })
                .unwrap();
            Self {
                workspace,
                provider,
                directory,
            }
        }

        fn git(&self, args: &[&str]) -> Vec<u8> {
            let mut command = crate::process_environment::command("git");
            command.current_dir(&self.workspace.path).args(args);
            let output = run_git(command, &ExecutionLimits::default()).unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        }

        fn write(&self, path: &str, content: &[u8]) {
            fs::write(self.workspace.path.join(path), content).unwrap();
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            self.provider.cleanup(&self.workspace).unwrap();
            fs::remove_dir(&self.directory).unwrap();
        }
    }

    #[test]
    fn no_commit_snapshot_preserves_index_head_and_all_change_kinds() {
        let repo = Repository::new();
        let limits = ExecutionLimits::default();
        repo.write("modified.txt", b"before\n");
        repo.write("deleted.txt", b"deleted content\n");
        repo.write(".gitignore", b"ignored.txt\n");
        repo.git(&["add", "."]);
        repo.write("modified.txt", b"staged then edited\n");
        let index = fs::read(repo.workspace.path.join(".git/index")).unwrap();
        let head = fs::read(repo.workspace.path.join(".git/HEAD")).unwrap();
        let baseline = ReviewBaseline::capture(&repo.workspace, GitMode::None, &limits).unwrap();
        assert_eq!(
            baseline.diff(&repo.workspace, &limits).unwrap(),
            "No current milestone changes."
        );
        repo.write("modified.txt", b"current edit\n");
        repo.write("new file.txt", b"new content\n");
        repo.write("binary.bin", &[0, 1, 2, 3]);
        repo.write("ignored.txt", b"must not be reviewed\n");
        fs::remove_file(repo.workspace.path.join("deleted.txt")).unwrap();
        let diff = baseline.diff(&repo.workspace, &limits).unwrap();
        for expected in [
            "-staged then edited",
            "+current edit",
            "-deleted content",
            "+new content",
            "Binary files",
            "--- /dev/null",
            "+++ /dev/null",
        ] {
            assert!(diff.contains(expected), "missing {expected}: {diff}");
        }
        assert!(!diff.contains("must not be reviewed"));
        assert!(
            !diff.contains("review-"),
            "scratch paths must not reach prompts: {diff}"
        );
        assert_eq!(
            fs::read(repo.workspace.path.join(".git/index")).unwrap(),
            index
        );
        assert_eq!(
            fs::read(repo.workspace.path.join(".git/HEAD")).unwrap(),
            head
        );
        assert_eq!(
            repo.git(&["ls-files", "--cached"]),
            b".gitignore\ndeleted.txt\nmodified.txt\n"
        );
        drop(baseline);
        assert_eq!(fs::read_dir(repo.workspace.artifacts()).unwrap().count(), 0);
    }

    #[test]
    fn current_review_diff_is_bounded_and_truncation_is_explicit_in_both_modes() {
        let repo = Repository::new();
        let limits = ExecutionLimits::default();
        let snapshot = ReviewBaseline::capture(&repo.workspace, GitMode::None, &limits).unwrap();
        let commit =
            ReviewBaseline::capture(&repo.workspace, GitMode::CommitPerMilestone, &limits).unwrap();
        repo.write(
            "large.txt",
            "current change\n".repeat(REVIEW_DIFF_BYTES).as_bytes(),
        );
        for baseline in [snapshot, commit] {
            let diff = baseline.diff(&repo.workspace, &limits).unwrap();
            assert!(diff.len() <= REVIEW_DIFF_BYTES);
            assert!(diff.contains("truncated"));
        }
    }

    #[test]
    fn oversized_snapshots_fail_closed_and_clean_up() {
        let repo = Repository::new();
        fs::File::create(repo.workspace.path.join("oversized.bin"))
            .unwrap()
            .set_len(SNAPSHOT_BYTES + 1)
            .unwrap();
        let result =
            ReviewBaseline::capture(&repo.workspace, GitMode::None, &ExecutionLimits::default());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("snapshot exceeds")
        );
        assert_eq!(fs::read_dir(repo.workspace.artifacts()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_external_links() {
        let repo = Repository::new();
        let external = repo.workspace.artifacts().join("external.txt");
        fs::write(&external, "not repository data").unwrap();
        std::os::unix::fs::symlink(&external, repo.workspace.path.join("link.txt")).unwrap();
        assert!(
            ReviewBaseline::capture(&repo.workspace, GitMode::None, &ExecutionLimits::default())
                .is_err()
        );
        assert_eq!(fs::read_to_string(external).unwrap(), "not repository data");
    }
}
