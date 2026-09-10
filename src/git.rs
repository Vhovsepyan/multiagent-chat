//! Optional per-milestone commits inside the task workspace (task 0009).
//!
//! The commit history is evidence that the work happened incrementally. It is
//! deliberately conservative: this module only ever ADDS a commit on the
//! checked-out branch of a workspace this application prepared. It never
//! resets, cleans, rewrites history, moves a branch, configures a remote, or
//! pushes. When the repository is not in a state where an isolated commit is
//! obviously safe, the run stops and says why rather than repairing anything.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::execution_limits::ExecutionLimits;
use crate::milestone::Milestone;
use crate::verification::VerificationResult;
use crate::workspace::run_git;

/// Committer identity for generated commits. A fixed, obviously non-human
/// identity keeps messages deterministic and never touches global Git config.
const COMMITTER_NAME: &str = "multiagent-chat";
const COMMITTER_EMAIL: &str = "multiagent-chat@localhost";

/// Longest commit subject we generate, so an unusual milestone title cannot
/// produce an unreadable single-line message.
const SUBJECT_BYTES: usize = 72;

/// What the user chose for this run. The default keeps the behavior every task
/// had before this feature existed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitMode {
    /// Never create a commit; the result is the working-tree diff only.
    #[default]
    None,
    /// One commit after each milestone that implemented AND verified cleanly.
    CommitPerMilestone,
}

impl GitMode {
    pub const ALL: [GitMode; 2] = [GitMode::None, GitMode::CommitPerMilestone];

    pub fn id(self) -> &'static str {
        match self {
            GitMode::None => "none",
            GitMode::CommitPerMilestone => "commit_per_milestone",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GitMode::None => "No commits",
            GitMode::CommitPerMilestone => "Commit after each successful milestone",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.id() == value)
    }

    pub fn commits_enabled(self) -> bool {
        matches!(self, GitMode::CommitPerMilestone)
    }
}

/// What one milestone commit produced. Stored on the milestone and published as
/// an audit event; it holds no path, no credential and no remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneCommit {
    pub sha: String,
    pub short_sha: String,
    pub message: String,
}

/// A path-free description of a finished project's repository (task 0010).
///
/// This is metadata about history that already exists; reading it never writes
/// to the repository and never configures or contacts a remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryStatus {
    /// The checked-out branch, or `None` when HEAD is not on one.
    pub branch: Option<String>,
    /// The commit the project is on, or `None` when nothing was committed.
    pub head_sha: Option<String>,
    /// How many commits that branch holds.
    pub commits: u32,
    /// Always false today: this application never configures a remote.
    pub has_remote: bool,
}

/// Read the repository state of a project directory, or `None` when it is not
/// a Git repository at all.
pub fn repository_status(
    repo: &Path,
    limits: &ExecutionLimits,
) -> Result<Option<RepositoryStatus>> {
    if !repo.join(".git").exists() {
        return Ok(None);
    }
    let value = |args: &[&str]| -> Result<Option<String>> {
        Ok(git(repo, args, limits)?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    };
    let branch = value(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let head_sha = value(&["rev-parse", "HEAD"])?;
    let commits = match &head_sha {
        Some(_) => value(&["rev-list", "--count", "HEAD"])?
            .and_then(|count| count.parse().ok())
            .unwrap_or(0),
        None => 0,
    };
    Ok(Some(RepositoryStatus {
        branch,
        head_sha,
        commits,
        has_remote: value(&["remote"])?.is_some(),
    }))
}

/// The deterministic commit subject for a milestone.
pub fn commit_message(milestone: &Milestone) -> String {
    let subject = format!(
        "feat(milestone-{:02}): {}",
        milestone.order,
        milestone.title.trim()
    );
    let subject = subject.replace(['\n', '\r'], " ");
    if subject.len() <= SUBJECT_BYTES {
        return subject;
    }
    let mut end = SUBJECT_BYTES;
    while end > 0 && !subject.is_char_boundary(end) {
        end -= 1;
    }
    subject[..end].trim_end().to_string()
}

/// Check that committing into this workspace can be done in isolation, and
/// initialize a repository when a new project does not have one yet.
///
/// Runs once, before the first milestone, so a problem is reported before any
/// worker touches the workspace. Every failure leaves the working tree exactly
/// as it was.
pub fn ensure_commit_ready(repo: &Path, limits: &ExecutionLimits) -> Result<()> {
    if !repo.is_dir() {
        bail!("task workspace repository is missing");
    }

    // A repository directory belonging to something else must never be
    // initialized into, or committed to, by this application.
    let toplevel = git(repo, &["rev-parse", "--show-toplevel"], limits)?;
    match toplevel {
        Some(top) => {
            let top = std::fs::canonicalize(top.trim())
                .context("could not resolve the workspace repository root")?;
            let repo_path =
                std::fs::canonicalize(repo).context("could not resolve the task workspace path")?;
            if top != repo_path {
                bail!(
                    "task workspace is inside another Git repository; refusing to create milestone commits there"
                );
            }
        }
        None => {
            // New Project workspaces are initialized during preparation; this
            // is the explicit, safe fallback when one is not a repository yet.
            let output = git_output(repo, &["init", "--quiet"], limits)?;
            if !output.status.success() {
                bail!("could not initialize a Git repository for milestone commits");
            }
        }
    }

    if git(repo, &["symbolic-ref", "--quiet", "HEAD"], limits)?.is_none() {
        let head = git(repo, &["rev-parse", "--short", "HEAD"], limits)?
            .map(|sha| sha.trim().to_string())
            .unwrap_or_else(|| "unknown commit".into());
        bail!(
            "workspace HEAD is detached at {head}; milestone commits require a branch. Register the project with a branch instead of a tag or commit"
        );
    }

    let status = git(
        repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
        limits,
    )?
    .ok_or_else(|| anyhow::anyhow!("could not read the workspace repository status"))?;
    if status.lines().any(is_conflict) {
        bail!("workspace repository has unresolved merge conflicts; refusing to create commits");
    }
    if !status.trim().is_empty() {
        bail!(
            "workspace repository already has uncommitted changes this run did not create; refusing to commit them"
        );
    }
    if repo.join(".git").join("MERGE_HEAD").exists()
        || repo.join(".git").join("REBASE_HEAD").exists()
    {
        bail!("workspace repository has a merge or rebase in progress; refusing to create commits");
    }
    Ok(())
}

/// Commit the milestone's work, when the run asked for commits and the
/// milestone actually verified.
///
/// Returns `Ok(None)` when commits are disabled, when verification did not
/// pass, or when the milestone produced no change to record. A failure to
/// create a requested commit is an error: the caller must not treat the
/// milestone as finalized.
pub fn commit_milestone_if_enabled(
    mode: GitMode,
    repo: &Path,
    milestone: &Milestone,
    verification: &[VerificationResult],
    limits: &ExecutionLimits,
) -> Result<Option<MilestoneCommit>> {
    if !mode.commits_enabled() {
        return Ok(None);
    }
    // Defence in depth: a milestone whose verification failed is never
    // committed as a success, whatever the caller believes.
    if verification.iter().any(|result| !result.success) {
        return Ok(None);
    }
    commit_milestone(repo, milestone, limits)
}

/// Stage everything in the workspace repository and record one commit.
fn commit_milestone(
    repo: &Path,
    milestone: &Milestone,
    limits: &ExecutionLimits,
) -> Result<Option<MilestoneCommit>> {
    let staged = git_output(repo, &["add", "--all", "."], limits)?;
    if !staged.status.success() {
        bail!("could not stage milestone changes: {}", stderr_of(&staged));
    }

    // `git diff --cached --quiet` exits 0 when the index matches HEAD.
    let pending = git_output(repo, &["diff", "--cached", "--quiet"], limits)?;
    if pending.status.success() {
        return Ok(None);
    }

    let message = commit_message(milestone);
    let output = git_output(
        repo,
        &[
            "-c",
            &format!("user.name={COMMITTER_NAME}"),
            "-c",
            &format!("user.email={COMMITTER_EMAIL}"),
            // Unattended execution cannot answer a signing passphrase prompt.
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--message",
            &message,
        ],
        limits,
    )?;
    if !output.status.success() {
        bail!(
            "could not create the milestone commit: {}",
            stderr_of(&output)
        );
    }

    let sha = git(repo, &["rev-parse", "HEAD"], limits)?
        .ok_or_else(|| anyhow::anyhow!("milestone commit was created but HEAD could not be read"))?
        .trim()
        .to_string();
    let short_sha = sha.chars().take(7).collect();
    Ok(Some(MilestoneCommit {
        sha,
        short_sha,
        message,
    }))
}

/// A conflicted path in porcelain v1 output.
fn is_conflict(line: &str) -> bool {
    let code = line.as_bytes();
    if code.len() < 2 {
        return false;
    }
    matches!(
        &code[..2],
        b"DD" | b"AU" | b"UD" | b"UA" | b"DU" | b"AA" | b"UU"
    )
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

/// Run a Git command and return its trimmed stdout, or `None` when the command
/// itself reported failure (an expected answer for the probes above).
fn git(repo: &Path, args: &[&str], limits: &ExecutionLimits) -> Result<Option<String>> {
    let output = git_output(repo, args, limits)?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
}

fn git_output(
    repo: &Path,
    args: &[&str],
    limits: &ExecutionLimits,
) -> Result<std::process::Output> {
    let mut command = crate::process_environment::command("git");
    command.args(args).current_dir(repo);
    run_git(command, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milestone::MilestoneStatus;

    fn limits() -> ExecutionLimits {
        ExecutionLimits::default()
    }

    fn milestone(order: u32, title: &str) -> Milestone {
        Milestone {
            id: format!("m{order}"),
            order,
            title: title.into(),
            objective: format!("{title} objective"),
            verification_instructions: vec!["cargo test".into()],
            status: MilestoneStatus::Pending,
            started_at: None,
            completed_at: None,
            worker_result_summary: None,
            commit: None,
        }
    }

    fn passed() -> Vec<VerificationResult> {
        vec![VerificationResult {
            command: "cargo test".into(),
            success: true,
            output: "ok".into(),
        }]
    }

    fn failed() -> Vec<VerificationResult> {
        vec![VerificationResult {
            command: "cargo test".into(),
            success: false,
            output: "failed".into(),
        }]
    }

    /// A repository that looks like a prepared task workspace: initialized,
    /// on a branch, with one commit so HEAD exists.
    fn repository(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mac-git-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "--quiet"]);
        std::fs::write(root.join("README.md"), "base\n").unwrap();
        run(&root, &["add", "--all", "."]);
        run(
            &root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--message",
                "base",
            ],
        );
        root
    }

    fn run(root: &Path, args: &[&str]) -> std::process::Output {
        let output = git_output(root, args, &limits()).unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            stderr_of(&output)
        );
        output
    }

    fn log_subjects(root: &Path) -> Vec<String> {
        git(root, &["log", "--format=%s"], &limits())
            .unwrap()
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Required test 1: a verified milestone produces exactly one commit
    /// holding that milestone's work.
    #[test]
    fn a_verified_milestone_creates_one_commit() {
        let root = repository("commit");
        // The gate runs on the clean workspace, before any worker writes.
        ensure_commit_ready(&root, &limits()).unwrap();
        std::fs::write(root.join("feature.rs").as_path(), "fn main() {}\n").unwrap();

        let commit = commit_milestone_if_enabled(
            GitMode::CommitPerMilestone,
            &root,
            &milestone(3, "Capacity-safe registration"),
            &passed(),
            &limits(),
        )
        .unwrap()
        .expect("a commit should be created");

        assert_eq!(
            commit.message,
            "feat(milestone-03): Capacity-safe registration"
        );
        assert_eq!(commit.short_sha, commit.sha[..7]);
        assert_eq!(
            log_subjects(&root),
            vec![
                "feat(milestone-03): Capacity-safe registration".to_string(),
                "base".to_string()
            ]
        );
        // The commit contains the milestone's file and nothing else.
        let files = git(
            &root,
            &["show", "--name-only", "--format=", "HEAD"],
            &limits(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(files.trim(), "feature.rs");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Required test 2: the default mode never writes history.
    #[test]
    fn no_commit_is_created_when_the_mode_is_disabled() {
        let root = repository("disabled");
        std::fs::write(root.join("feature.rs"), "fn main() {}\n").unwrap();

        let commit = commit_milestone_if_enabled(
            GitMode::None,
            &root,
            &milestone(1, "Bootstrap"),
            &passed(),
            &limits(),
        )
        .unwrap();

        assert!(commit.is_none());
        assert_eq!(log_subjects(&root), vec!["base".to_string()]);
        // The worker's change is still there, uncommitted, for the diff.
        assert!(root.join("feature.rs").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Required test 3: failed verification is never committed as success.
    #[test]
    fn a_failed_milestone_is_not_committed() {
        let root = repository("failed");
        std::fs::write(root.join("broken.rs"), "fn main() {\n").unwrap();

        let commit = commit_milestone_if_enabled(
            GitMode::CommitPerMilestone,
            &root,
            &milestone(2, "Broken"),
            &failed(),
            &limits(),
        )
        .unwrap();

        assert!(commit.is_none());
        assert_eq!(log_subjects(&root), vec!["base".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A milestone that changed nothing is reported as "no commit", not as a
    /// commit that does not exist.
    #[test]
    fn a_milestone_without_changes_creates_no_empty_commit() {
        let root = repository("nochange");

        let commit = commit_milestone_if_enabled(
            GitMode::CommitPerMilestone,
            &root,
            &milestone(1, "Nothing to do"),
            &passed(),
            &limits(),
        )
        .unwrap();

        assert!(commit.is_none());
        assert_eq!(log_subjects(&root), vec!["base".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Required test 5: a New Project workspace that is not a repository yet is
    /// initialized explicitly, with no remote.
    #[test]
    fn a_new_project_workspace_is_initialized_safely() {
        let root = std::env::temp_dir().join(format!(
            "mac-git-init-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();

        ensure_commit_ready(&root, &limits()).unwrap();

        assert!(root.join(".git").is_dir(), "repository was not initialized");
        assert!(
            git(&root, &["remote"], &limits())
                .unwrap()
                .unwrap()
                .is_empty(),
            "no remote may be configured"
        );
        // An unborn HEAD still points at a branch, so commits are allowed.
        assert!(
            git(&root, &["symbolic-ref", "--quiet", "HEAD"], &limits())
                .unwrap()
                .is_some()
        );

        std::fs::write(root.join("first.rs"), "fn main() {}\n").unwrap();
        let commit = commit_milestone_if_enabled(
            GitMode::CommitPerMilestone,
            &root,
            &milestone(1, "Project bootstrap"),
            &passed(),
            &limits(),
        )
        .unwrap()
        .expect("the first milestone should commit");
        assert_eq!(commit.message, "feat(milestone-01): Project bootstrap");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Required test 6: pre-existing uncommitted changes stop the run instead
    /// of being swept into a milestone commit or discarded.
    #[test]
    fn pre_existing_changes_stop_the_run_without_touching_them() {
        let root = repository("dirty");
        std::fs::write(root.join("README.md"), "someone else was here\n").unwrap();

        let error = ensure_commit_ready(&root, &limits())
            .unwrap_err()
            .to_string();

        assert!(error.contains("uncommitted changes"), "unexpected: {error}");
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "someone else was here\n",
            "the existing change must be preserved"
        );
        assert_eq!(log_subjects(&root), vec!["base".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The answer chosen for 0009: a detached HEAD is surfaced, not repaired.
    #[test]
    fn a_detached_head_is_refused_before_any_milestone_runs() {
        let root = repository("detached");
        let head = git(&root, &["rev-parse", "HEAD"], &limits())
            .unwrap()
            .unwrap();
        run(&root, &["checkout", "--quiet", "--detach", head.trim()]);

        let error = ensure_commit_ready(&root, &limits())
            .unwrap_err()
            .to_string();

        assert!(error.contains("detached"), "unexpected: {error}");
        assert!(error.contains("branch"), "unexpected: {error}");
        assert_eq!(log_subjects(&root), vec!["base".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A workspace nested inside another repository must never be initialized
    /// or committed into.
    #[test]
    fn a_workspace_inside_another_repository_is_refused() {
        let outer = repository("outer");
        let inner = outer.join("nested-workspace");
        std::fs::create_dir(&inner).unwrap();

        let error = ensure_commit_ready(&inner, &limits())
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("inside another Git repository"),
            "unexpected: {error}"
        );
        assert!(
            !inner.join(".git").exists(),
            "nested repository was created"
        );
        std::fs::remove_dir_all(&outer).ok();
    }

    /// Required test 7: a commit that cannot be created is an error the caller
    /// must handle, never a silent success.
    #[test]
    fn a_commit_failure_is_reported() {
        let root = std::env::temp_dir().join(format!(
            "mac-git-nonrepo-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), "content\n").unwrap();

        let error = commit_milestone_if_enabled(
            GitMode::CommitPerMilestone,
            &root,
            &milestone(1, "Bootstrap"),
            &passed(),
            &limits(),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("stage"), "unexpected: {error}");
        assert!(!root.join(".git").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn commit_subjects_are_deterministic_and_bounded() {
        let short = commit_message(&milestone(7, "Realtime updates"));
        assert_eq!(short, "feat(milestone-07): Realtime updates");
        assert_eq!(short, commit_message(&milestone(7, "Realtime updates")));

        let long = commit_message(&milestone(12, &"very long milestone title ".repeat(10)));
        assert!(long.len() <= SUBJECT_BYTES, "{long}");
        assert!(long.starts_with("feat(milestone-12): "));
        assert!(!long.contains('\n'));

        let multiline = commit_message(&milestone(1, "line one\nline two"));
        assert!(!multiline.contains('\n'), "{multiline}");
    }

    #[test]
    fn git_modes_round_trip_through_their_wire_ids() {
        assert_eq!(GitMode::default(), GitMode::None);
        assert!(!GitMode::None.commits_enabled());
        assert!(GitMode::CommitPerMilestone.commits_enabled());
        assert_eq!(
            GitMode::from_id("commit_per_milestone"),
            Some(GitMode::CommitPerMilestone)
        );
        assert_eq!(GitMode::from_id("push"), None);
        assert_eq!(
            serde_json::to_string(&GitMode::CommitPerMilestone).unwrap(),
            "\"commit_per_milestone\""
        );
    }
}
