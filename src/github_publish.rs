//! Explicit, confirmation-bound publication of a persisted project to GitHub.
//!
//! Preparation may create one commit containing only task-0013 artifacts whose
//! bytes still match their recorded SHA-256 identities. The returned clean
//! snapshot is what the human confirms. Publication re-inspects immediately
//! before pushing and names the confirmed commit SHA explicitly, so a later
//! `HEAD` can never be pushed accidentally.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::execution_limits::ExecutionLimits;
use crate::task::{GeneratedArtifact, GitHubPublication, Task};
use crate::workspace::run_git;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishPreview {
    pub repository: String,
    pub remote_url: String,
    pub branch: String,
    pub head_sha: String,
    pub dirty_files: Vec<String>,
    pub working_tree: String,
    pub verification_summary: Vec<String>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositorySnapshot {
    root: PathBuf,
    repository: String,
    remote_url: String,
    branch: String,
    head_sha: String,
    changes: Vec<WorkingTreeChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkingTreeChange {
    code: String,
    path: String,
}

/// Finalize unchanged generated documentation, then return the exact clean
/// snapshot that the user must confirm.
pub fn prepare(task: &Task, limits: &ExecutionLimits) -> Result<PublishPreview> {
    let snapshot = inspect(task, limits)?;
    if !snapshot.changes.is_empty() {
        finalize_generated_artifacts(task, &snapshot, limits)?;
    }
    let snapshot = inspect(task, limits)?;
    require_clean(&snapshot)?;
    Ok(to_preview(task, snapshot))
}

/// Read-only preview for an already prepared repository.
pub fn preview(task: &Task, limits: &ExecutionLimits) -> Result<PublishPreview> {
    let snapshot = inspect(task, limits)?;
    require_clean(&snapshot)?;
    Ok(to_preview(task, snapshot))
}

pub fn validate_confirmation(
    task: &Task,
    expected_fingerprint: &str,
    limits: &ExecutionLimits,
) -> Result<PublishPreview> {
    let preview = preview(task, limits)?;
    if expected_fingerprint.trim().is_empty() || preview.fingerprint != expected_fingerprint {
        bail!(
            "publication snapshot changed; prepare and confirm the current repository state again"
        );
    }
    Ok(preview)
}

pub fn publish(
    task: &Task,
    expected_fingerprint: &str,
    limits: &ExecutionLimits,
) -> Result<GitHubPublication> {
    let confirmed = validate_confirmation(task, expected_fingerprint, limits)?;
    let refspec = format!("{}:refs/heads/{}", confirmed.head_sha, confirmed.branch);
    let root = repository_root(task)?;
    let dry_run = git_output(
        &root,
        &["push", "--dry-run", &confirmed.remote_url, &refspec],
        limits,
    )?;
    if !dry_run.status.success() {
        bail!("GitHub push preflight failed: {}", stderr(&dry_run));
    }

    // Re-inspect after the network/authentication preflight and immediately
    // before the real push. The refspec also uses the exact confirmed SHA.
    let final_snapshot = validate_confirmation(task, expected_fingerprint, limits)?;
    let final_refspec = format!(
        "{}:refs/heads/{}",
        final_snapshot.head_sha, final_snapshot.branch
    );
    let output = git_output(
        &root,
        &["push", &final_snapshot.remote_url, &final_refspec],
        limits,
    )?;
    if !output.status.success() {
        bail!("GitHub push failed: {}", stderr(&output));
    }
    Ok(GitHubPublication {
        repository: final_snapshot.repository,
        branch: final_snapshot.branch,
        commit_sha: final_snapshot.head_sha,
        published_at: chrono::DateTime::<Utc>::from(std::time::SystemTime::now()),
    })
}

fn to_preview(task: &Task, snapshot: RepositorySnapshot) -> PublishPreview {
    let verification_summary = verification_summary(task);
    let fingerprint = snapshot_fingerprint(
        &snapshot.repository,
        &snapshot.remote_url,
        &snapshot.branch,
        &snapshot.head_sha,
        &verification_summary,
    );
    PublishPreview {
        repository: snapshot.repository,
        remote_url: snapshot.remote_url,
        branch: snapshot.branch,
        head_sha: snapshot.head_sha,
        dirty_files: snapshot
            .changes
            .iter()
            .map(|change| format!("{} {}", change.code, change.path))
            .collect(),
        working_tree: if snapshot.changes.is_empty() {
            "clean".into()
        } else {
            "dirty".into()
        },
        verification_summary,
        fingerprint,
    }
}

fn verification_summary(task: &Task) -> Vec<String> {
    let summary = task
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
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if summary.is_empty() {
        let critic_pass = !task.milestones.is_empty()
            && task.milestones.iter().all(|milestone| {
                milestone
                    .review
                    .as_ref()
                    .is_some_and(|review| review.status.is_pass())
            });
        vec![if critic_pass {
            "No automatic verification commands were available; milestone critic reviews passed."
                .into()
        } else {
            "No automatic verification commands were available.".into()
        }]
    } else {
        summary
    }
}

fn snapshot_fingerprint(
    repository: &str,
    remote_url: &str,
    branch: &str,
    head_sha: &str,
    verification_summary: &[String],
) -> String {
    let mut value =
        format!("github-publish-v1\0{repository}\0{remote_url}\0{branch}\0{head_sha}\0clean");
    for line in verification_summary {
        value.push('\0');
        value.push_str(line);
    }
    sha256(value.as_bytes())
}

fn inspect(task: &Task, limits: &ExecutionLimits) -> Result<RepositorySnapshot> {
    if task.status != crate::task::TaskStatus::Completed || task.result.is_none() {
        bail!("GitHub publishing requires a completed task result");
    }
    let root = repository_root(task)?;
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
    let status = git_output(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        limits,
    )?;
    if !status.status.success() {
        bail!("could not inspect the persisted repository status");
    }
    let changes = parse_status(&status.stdout)?;
    if changes.iter().any(|change| is_conflict(&change.code)) {
        bail!("publishing is blocked by unresolved merge conflicts");
    }
    // Read the configured identity without applying Git's URL rewrite rules.
    // Production still pushes through ordinary Git, while tests can redirect
    // this exact GitHub URL to a local bare remote without weakening validation.
    let remote = git(&root, &["config", "--get", "remote.origin.url"], limits)?
        .ok_or_else(|| anyhow::anyhow!("no GitHub origin remote is configured"))?;
    let repository = github_repository(&remote)
        .ok_or_else(|| anyhow::anyhow!("origin is not a GitHub repository"))?;
    Ok(RepositorySnapshot {
        root,
        repository,
        remote_url: remote,
        branch,
        head_sha,
        changes,
    })
}

fn repository_root(task: &Task) -> Result<PathBuf> {
    task.persistence
        .as_ref()
        .filter(|item| item.status == crate::task::PersistenceStatus::Persisted)
        .map(|item| PathBuf::from(&item.destination))
        .ok_or_else(|| anyhow::anyhow!("GitHub publishing requires a persisted project"))
}

fn require_clean(snapshot: &RepositorySnapshot) -> Result<()> {
    if !snapshot.changes.is_empty() {
        bail!(
            "publication snapshot is not clean; prepare it first or review ambiguous changes: {}",
            snapshot
                .changes
                .iter()
                .map(|change| format!("{} {}", change.code, change.path))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn finalize_generated_artifacts(
    task: &Task,
    snapshot: &RepositorySnapshot,
    limits: &ExecutionLimits,
) -> Result<()> {
    let artifacts = recorded_artifacts(task)?;
    let mut paths = Vec::with_capacity(snapshot.changes.len());
    for change in &snapshot.changes {
        if !is_committable_change(&change.code) {
            bail!(
                "generated artifact {} has an ambiguous Git state ({})",
                change.path,
                change.code
            );
        }
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.path == change.path)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "publishing is blocked by unrelated or ambiguous change {}",
                    change.path
                )
            })?;
        let current = hash_repository_file(&snapshot.root, &artifact.path)?;
        if current != artifact.content_sha256 {
            bail!(
                "generated artifact {} was modified after generation; review it before publishing",
                artifact.path
            );
        }
        paths.push(artifact.path.clone());
    }
    let mut stage = crate::process_environment::command("git");
    stage
        .args([
            "--literal-pathspecs",
            "-c",
            "core.autocrlf=false",
            "add",
            "--",
        ])
        .args(&paths)
        .current_dir(&snapshot.root);
    let staged = run_git(stage, limits)?;
    if !staged.status.success() {
        bail!(
            "could not stage final submission documents: {}",
            stderr(&staged)
        );
    }
    // Verify the bytes now held by the index, not merely the file bytes read
    // before `git add`. The commit can therefore contain only the identities
    // recorded by task 0013 even if a file changes during preparation.
    for path in &paths {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.path == *path)
            .expect("every staged path was matched above");
        let index_path = format!(":{path}");
        let staged_contents = git_output(&snapshot.root, &["show", &index_path], limits)?;
        if !staged_contents.status.success()
            || sha256(&staged_contents.stdout) != artifact.content_sha256
        {
            bail!(
                "generated artifact {path} changed while publication was being prepared; refusing to commit"
            );
        }
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

fn recorded_artifacts(task: &Task) -> Result<Vec<GeneratedArtifact>> {
    task.history
        .iter()
        .rev()
        .find_map(|event| match &event.event {
            crate::task::TaskEvent::SubmissionDocumentationGenerated { artifacts, .. } => {
                Some(artifacts.clone())
            }
            _ => None,
        })
        .filter(|artifacts| !artifacts.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "generated-document identities were not recorded; refusing to auto-commit"
            )
        })
}

fn is_committable_change(code: &str) -> bool {
    matches!(code, "??" | " M" | "M " | "MM" | "A " | "AM")
}

fn hash_repository_file(root: &Path, relative: &str) -> Result<String> {
    let path = root.join(relative);
    let mut file = crate::repository_file::open(root, &path)
        .with_context(|| format!("could not safely read generated artifact {relative}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn parse_status(bytes: &[u8]) -> Result<Vec<WorkingTreeChange>> {
    let mut entries = bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty());
    let mut changes = Vec::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 || entry[2] != b' ' {
            bail!("could not parse Git working-tree state safely");
        }
        let code = std::str::from_utf8(&entry[..2])?.to_string();
        let path = std::str::from_utf8(&entry[3..])?.to_string();
        let renamed = code.as_bytes().contains(&b'R') || code.as_bytes().contains(&b'C');
        changes.push(WorkingTreeChange { code, path });
        if renamed {
            let previous = entries
                .next()
                .ok_or_else(|| anyhow::anyhow!("could not parse renamed Git path safely"))?;
            changes.push(WorkingTreeChange {
                code: "rename-source".into(),
                path: std::str::from_utf8(previous)?.to_string(),
            });
        }
    }
    Ok(changes)
}

fn is_conflict(code: &str) -> bool {
    matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU")
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

    struct Fixture {
        root: PathBuf,
        repo: PathBuf,
        remote: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mac-github-publish-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let repo = root.join("repo");
            let remote = root.join("remote.git");
            fs::create_dir_all(&repo).unwrap();
            fs::create_dir_all(&remote).unwrap();
            run(&remote, &["init", "--bare", "--quiet"]);
            run(&repo, &["init", "--quiet"]);
            fs::write(repo.join("app.txt"), "base\n").unwrap();
            run(&repo, &["add", "app.txt"]);
            commit(&repo, "base");
            run(&repo, &["branch", "-M", "main"]);
            run(
                &repo,
                &["remote", "add", "origin", "https://github.com/acme/app.git"],
            );
            let file_url = format!(
                "file:///{}",
                remote.display().to_string().replace('\\', "/")
            );
            run(
                &repo,
                &[
                    "config",
                    &format!("url.{file_url}.insteadOf"),
                    "https://github.com/acme/app.git",
                ],
            );
            Self { root, repo, remote }
        }

        fn task(&self, artifacts: Vec<GeneratedArtifact>) -> Task {
            let manager = crate::task::TaskManager::new();
            let task = manager.create("publish", "description", "legacy");
            let emitter = manager.emitter(task.id);
            if !artifacts.is_empty() {
                emitter.emit(TaskEvent::SubmissionDocumentationGenerated {
                    written: artifacts.iter().map(|item| item.path.clone()).collect(),
                    preserved: Vec::new(),
                    artifacts,
                });
            }
            emitter.emit(TaskEvent::Result {
                result: TaskResult {
                    source_revision: None,
                    verification: vec![crate::verification::VerificationResult {
                        command: "cargo test".into(),
                        success: true,
                        output: "ok".into(),
                    }],
                    diff: String::new(),
                },
            });
            emitter.emit(TaskEvent::ProjectPersisted {
                destination: self.repo.display().to_string(),
                git: None,
                git_warning: None,
            });
            emitter.emit(TaskEvent::Finished {
                status: crate::task::TaskStatus::Completed,
                error: None,
            });
            manager.get(task.id).unwrap()
        }

        fn remote_head(&self) -> Option<String> {
            git(&self.remote, &["rev-parse", "refs/heads/main"], &limits()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    fn limits() -> ExecutionLimits {
        ExecutionLimits::default()
    }

    fn run(root: &Path, args: &[&str]) -> std::process::Output {
        let output = git_output(root, args, &limits()).unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            stderr(&output)
        );
        output
    }

    fn commit(root: &Path, message: &str) {
        run(
            root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                message,
            ],
        );
    }

    fn artifact(path: &str, contents: &str) -> GeneratedArtifact {
        GeneratedArtifact {
            path: path.into(),
            content_sha256: sha256(contents.as_bytes()),
        }
    }

    #[test]
    fn explicit_confirmation_is_required_and_success_pushes_the_confirmed_sha() {
        let fixture = Fixture::new("confirm-success");
        let task = fixture.task(Vec::new());
        let preview = prepare(&task, &limits()).unwrap();
        assert!(publish(&task, "", &limits()).is_err());
        assert!(fixture.remote_head().is_none());

        let publication = publish(&task, &preview.fingerprint, &limits()).unwrap();
        assert_eq!(publication.commit_sha, preview.head_sha);
        assert_eq!(
            fixture.remote_head().as_deref(),
            Some(preview.head_sha.as_str())
        );
    }

    #[test]
    fn snapshot_mismatch_blocks_a_different_commit() {
        let fixture = Fixture::new("snapshot-mismatch");
        let task = fixture.task(Vec::new());
        let preview = prepare(&task, &limits()).unwrap();
        fs::write(fixture.repo.join("app.txt"), "changed\n").unwrap();
        run(&fixture.repo, &["add", "app.txt"]);
        commit(&fixture.repo, "later");

        let error = publish(&task, &preview.fingerprint, &limits())
            .unwrap_err()
            .to_string();
        assert!(error.contains("snapshot changed"), "{error}");
        assert!(fixture.remote_head().is_none());
    }

    #[test]
    fn generated_variant_is_finalized_only_while_its_hash_matches() {
        let fixture = Fixture::new("generated-finalize");
        fs::create_dir_all(fixture.repo.join("docs")).unwrap();
        let path = "docs/ARCHITECTURE.generated.md";
        let contents = "generated architecture\n";
        fs::write(fixture.repo.join(path), contents).unwrap();
        let task = fixture.task(vec![artifact(path, contents)]);

        let preview = prepare(&task, &limits()).unwrap();
        assert_eq!(preview.working_tree, "clean");
        let subject = git(&fixture.repo, &["log", "-1", "--format=%s"], &limits())
            .unwrap()
            .unwrap();
        assert_eq!(subject, "docs: finalize submission artifacts");
    }

    #[test]
    fn modified_generated_artifact_and_unrelated_dirty_file_are_blocked() {
        let fixture = Fixture::new("dirty-blocks");
        let generated = "README.generated.md";
        fs::write(fixture.repo.join(generated), "original\n").unwrap();
        let task = fixture.task(vec![artifact(generated, "original\n")]);
        fs::write(fixture.repo.join(generated), "user edit\n").unwrap();
        let error = prepare(&task, &limits()).unwrap_err().to_string();
        assert!(error.contains("modified after generation"), "{error}");

        fs::write(fixture.repo.join(generated), "original\n").unwrap();
        fs::write(fixture.repo.join("notes.txt"), "user notes\n").unwrap();
        let error = prepare(&task, &limits()).unwrap_err().to_string();
        assert!(error.contains("unrelated or ambiguous"), "{error}");
    }

    #[test]
    fn detached_and_in_progress_repository_states_are_blocked() {
        let detached = Fixture::new("detached");
        let task = detached.task(Vec::new());
        run(&detached.repo, &["checkout", "--detach", "--quiet"]);
        assert!(
            prepare(&task, &limits())
                .unwrap_err()
                .to_string()
                .contains("detached")
        );

        let conflicted = Fixture::new("conflicted");
        let task = conflicted.task(Vec::new());
        let head = git(&conflicted.repo, &["rev-parse", "HEAD"], &limits())
            .unwrap()
            .unwrap();
        fs::write(conflicted.repo.join(".git/MERGE_HEAD"), format!("{head}\n")).unwrap();
        assert!(
            prepare(&task, &limits())
                .unwrap_err()
                .to_string()
                .contains("MERGE_HEAD")
        );
    }

    #[test]
    fn non_fast_forward_failure_preserves_local_repository() {
        let fixture = Fixture::new("non-ff");
        let first_task = fixture.task(Vec::new());
        let first = prepare(&first_task, &limits()).unwrap();
        publish(&first_task, &first.fingerprint, &limits()).unwrap();

        let other = fixture.root.join("other");
        run(
            &fixture.root,
            &[
                "clone",
                "--quiet",
                fixture.remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        run(&other, &["checkout", "main"]);
        fs::write(other.join("remote.txt"), "remote\n").unwrap();
        run(&other, &["add", "remote.txt"]);
        commit(&other, "remote advance");
        run(&other, &["push", "origin", "main"]);

        fs::write(fixture.repo.join("local.txt"), "local\n").unwrap();
        run(&fixture.repo, &["add", "local.txt"]);
        commit(&fixture.repo, "local advance");
        let task = fixture.task(Vec::new());
        let preview = prepare(&task, &limits()).unwrap();
        let before_head = preview.head_sha.clone();
        let before_contents = fs::read_to_string(fixture.repo.join("local.txt")).unwrap();
        let error = publish(&task, &preview.fingerprint, &limits())
            .unwrap_err()
            .to_string();
        assert!(error.contains("preflight failed"), "{error}");
        assert_eq!(
            git(&fixture.repo, &["rev-parse", "HEAD"], &limits())
                .unwrap()
                .unwrap(),
            before_head
        );
        assert_eq!(
            fs::read_to_string(fixture.repo.join("local.txt")).unwrap(),
            before_contents
        );
        assert_ne!(fixture.remote_head().as_deref(), Some(before_head.as_str()));
    }

    #[test]
    fn rejected_push_preserves_the_local_repository() {
        let fixture = Fixture::new("server-reject");
        let hook = fixture.remote.join("hooks/pre-receive");
        fs::write(&hook, "#!/bin/sh\necho rejected-by-test >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let task = fixture.task(Vec::new());
        let preview = prepare(&task, &limits()).unwrap();
        let before_head = preview.head_sha.clone();
        let before_contents = fs::read_to_string(fixture.repo.join("app.txt")).unwrap();

        let error = publish(&task, &preview.fingerprint, &limits())
            .unwrap_err()
            .to_string();

        assert!(error.contains("GitHub push failed"), "{error}");
        assert_eq!(
            git(&fixture.repo, &["rev-parse", "HEAD"], &limits())
                .unwrap()
                .unwrap(),
            before_head
        );
        assert_eq!(
            fs::read_to_string(fixture.repo.join("app.txt")).unwrap(),
            before_contents
        );
        assert!(fixture.remote_head().is_none());
    }

    #[test]
    fn publication_audit_keeps_safe_metadata_and_redacts_failures() {
        let manager = crate::task::TaskManager::with_history_limits_and_secrets(
            Default::default(),
            ["super-secret-token".to_string()],
        );
        let task = manager.create("publish", "description", "legacy");
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::GitHubPublishCompleted {
            publication: GitHubPublication {
                repository: "acme/app".into(),
                branch: "main".into(),
                commit_sha: "abc123".into(),
                published_at: chrono::DateTime::<Utc>::from(std::time::SystemTime::now()),
            },
        });
        emitter.emit(TaskEvent::GitHubPublishFailed {
            repository: Some("acme/app".into()),
            branch: Some("main".into()),
            error: "credential super-secret-token failed".into(),
        });
        let json = serde_json::to_string(&manager.get(task.id).unwrap()).unwrap();
        assert!(json.contains("acme/app"));
        assert!(json.contains("abc123"));
        assert!(!json.contains("super-secret-token"));
        assert!(json.contains("[REDACTED]"));
    }
}
