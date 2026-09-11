//! Persistent New Project output (task 0010).
//!
//! A New Project is still built in the disposable task workspace. When the user
//! asked for a persistent result, the finished project is copied out of that
//! workspace into a destination inside ONE configured, allowed parent folder
//! before the workspace is cleaned up.
//!
//! The design is deliberately narrow:
//!
//! * the browser sends a plain folder NAME, never a server path, so nothing the
//!   user types can address a directory outside the configured root;
//! * links (symlinks and Windows junctions/reparse points) are refused rather
//!   than followed, both at the destination and inside the copied project;
//! * an existing non-empty destination is never overwritten;
//! * the project is materialized in a staging directory and only then moved
//!   into place, so a failure leaves the destination exactly as it was.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::execution_limits::ExecutionLimits;
use crate::git::RepositoryStatus;
use crate::project::ProjectSource;

/// Longest destination folder name accepted from the browser.
const MAX_NAME_BYTES: usize = 64;

/// Reject anything that is not a plain, single-segment folder name.
///
/// Separators, drive letters and `..` never reach the filesystem: a name that
/// contains one is refused here, so joining it onto the configured root can
/// only ever produce a direct child of that root.
pub fn validate_name(raw: &str) -> Result<&str> {
    let name = raw.trim();
    if name.is_empty() {
        bail!("a persistent project needs a destination folder name");
    }
    if name.len() > MAX_NAME_BYTES {
        bail!("destination folder name must be at most {MAX_NAME_BYTES} characters");
    }
    if name.contains("..") {
        bail!("destination folder name must not contain \"..\": {name:?}");
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
    {
        bail!(
            "destination folder name may use only letters, digits, dot, dash and underscore: {name:?}"
        );
    }
    if name.starts_with(['.', '-']) {
        bail!("destination folder name must not start with a dot or a dash: {name:?}");
    }
    Ok(name)
}

/// A validated destination: the configured root plus one user-chosen folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentDestination {
    root: PathBuf,
    name: String,
    path: PathBuf,
}

impl PersistentDestination {
    /// Validate the configured root and the requested folder name together.
    ///
    /// `root` is `None` when this installation configures no persistent output
    /// folder at all, which is refused rather than defaulted to somewhere.
    pub fn resolve(root: Option<&Path>, name: &str) -> Result<Self> {
        let root = root.ok_or_else(|| {
            anyhow::anyhow!(
                "persistent project output is not configured on this server; set PERSISTENT_OUTPUT_ROOT"
            )
        })?;
        let name = validate_name(name)?.to_string();
        let metadata = fs::symlink_metadata(root).with_context(|| {
            format!("persistent output root does not exist: {}", root.display())
        })?;
        if crate::repository_file::is_link(&metadata) {
            bail!("persistent output root must not be a link");
        }
        if !metadata.is_dir() {
            bail!(
                "persistent output root is not a directory: {}",
                root.display()
            );
        }
        // The root is server configuration, so only the last segment is ever
        // user-controlled. Resolving the root proves it is reachable, and the
        // name check above guarantees the join stays one level inside it.
        let canonical_root = root
            .canonicalize()
            .context("could not resolve the persistent output root")?;
        if !canonical_root.is_dir() {
            bail!(
                "persistent output root is not a directory: {}",
                root.display()
            );
        }
        let path = root.join(&name);
        if path.parent() != Some(root) {
            bail!("destination folder name must name one folder inside the output root");
        }
        Ok(Self {
            root: root.to_path_buf(),
            name,
            path,
        })
    }

    /// Where the finished project will be, as shown to the user.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn display(&self) -> String {
        self.path.display().to_string()
    }

    /// Whether the destination can be used right now.
    ///
    /// A missing destination and an existing EMPTY directory are both fine; an
    /// existing non-empty directory, a link, or a file is refused. This is
    /// checked before the run starts and again before the project is moved in,
    /// because the folder may appear while the task is running.
    pub fn ensure_available(&self) -> Result<()> {
        let metadata = match fs::symlink_metadata(self.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not inspect the destination {}", self.display())
                });
            }
        };
        if crate::repository_file::is_link(&metadata) {
            bail!(
                "destination {} is a link; refusing to write there",
                self.name
            );
        }
        if !metadata.is_dir() {
            bail!(
                "destination {} already exists and is not a directory",
                self.name
            );
        }
        if fs::read_dir(self.path())
            .with_context(|| format!("could not read the destination {}", self.display()))?
            .next()
            .is_some()
        {
            bail!(
                "destination {} already exists and is not empty; choose another name or remove it yourself",
                self.name
            );
        }
        Ok(())
    }

    /// Copy the finished project out of the task workspace and finalize it.
    ///
    /// The project is materialized in a staging folder next to the destination
    /// and moved into place only once it is complete, so an interrupted or
    /// failed copy never leaves a half-written project behind and never
    /// disturbs whatever is already at the destination.
    #[cfg(test)]
    pub fn persist(&self, source: &Path, limits: &ExecutionLimits) -> Result<PersistedProject> {
        self.persist_with_source_repository(source, None, limits)
    }

    /// Persist a project while retaining its already-normalized source
    /// repository identity. The identity is metadata for the later explicit
    /// publication boundary; it is never restored as a worker Git remote.
    pub fn persist_with_source_repository(
        &self,
        source: &Path,
        source_repository: Option<&str>,
        limits: &ExecutionLimits,
    ) -> Result<PersistedProject> {
        let source_repository = source_repository
            .map(ProjectSource::github)
            .transpose()?
            .map(|source| source.repository_identity().to_owned());
        self.ensure_available()?;
        if !source.is_dir() {
            bail!("the generated project is no longer available in the task workspace");
        }
        let staging = self
            .root
            .join(format!(".multiagent-persist-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&staging)
            .context("could not create the persistent output staging folder")?;
        let staged = copy_tree(source, &staging);
        if let Err(error) = staged {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        if let Err(error) = self.finalize(&staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        // The rename above published the project, so persistence has SUCCEEDED
        // from here on. Describing the repository is reporting, not publishing:
        // when that fails the metadata is unavailable and the run says so,
        // rather than claiming the destination was left unchanged.
        let (git, git_warning) = match crate::git::repository_status(self.path(), limits) {
            Ok(git) => (git, None),
            Err(error) => (
                None,
                Some(format!(
                    "the project was persisted but its repository could not be described: {error:#}"
                )),
            ),
        };
        Ok(PersistedProject {
            destination: self.display(),
            git,
            git_warning,
            source_repository,
        })
    }

    /// Move the completed staging folder onto the destination.
    fn finalize(&self, staging: &Path) -> Result<()> {
        // Re-check under the same rules: the destination may have appeared
        // while the project was being built or copied.
        self.ensure_available()?;
        if self.path.exists() {
            // Only ever removes an EMPTY directory: a destination that gained
            // content since the check above makes this fail instead.
            fs::remove_dir(&self.path).with_context(|| {
                format!("could not replace the empty destination {}", self.display())
            })?;
        }
        fs::rename(staging, &self.path)
            .with_context(|| format!("could not finalize the destination {}", self.display()))
    }
}

/// What a successful persistence produced. Holds the user's own destination and
/// repository metadata only — never a temporary workspace path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedProject {
    pub destination: String,
    /// The repository the persisted project holds. `None` means either that the
    /// project is not a repository or that `git_warning` says why it could not
    /// be inspected — never that persistence failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<RepositoryStatus>,
    /// Set when the project WAS published but its repository metadata could not
    /// be read. This is a non-fatal warning about reporting, not about the
    /// project, which is already at its destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_warning: Option<String>,
    /// Canonical `owner/repository` identity from a registered source project.
    /// This deliberately stores no URL, credentials, or temporary path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repository: Option<String>,
}

/// Copy a directory tree, refusing anything that is not a plain file or folder.
///
/// `.git` is copied verbatim, which is what preserves the milestone history:
/// the commits, and their SHAs, are the same objects, not recreated ones.
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let entries =
        fs::read_dir(source).with_context(|| format!("could not read {}", source.display()))?;
    for entry in entries {
        let entry = entry.context("could not read the generated project")?;
        // `DirEntry::metadata` does not follow links, so a link is detected
        // here rather than copied as whatever it points at.
        let metadata = entry
            .metadata()
            .context("could not inspect a generated project file")?;
        let name = entry.file_name();
        let from = entry.path();
        let to = target.join(&name);
        if crate::repository_file::is_link(&metadata) {
            bail!(
                "refusing to persist the link {:?}; the generated project must contain only regular files and folders",
                name
            );
        }
        if metadata.is_dir() {
            fs::create_dir(&to).with_context(|| format!("could not create {}", to.display()))?;
            copy_tree(&from, &to)?;
        } else if metadata.is_file() {
            fs::copy(&from, &to).with_context(|| format!("could not copy {:?}", name))?;
        } else {
            bail!("refusing to persist the unsupported file type {:?}", name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn limits() -> ExecutionLimits {
        ExecutionLimits::default()
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mac-persist-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(root.join("output")).unwrap();
            fs::create_dir_all(root.join("workspace")).unwrap();
            Self { root }
        }

        fn output(&self) -> PathBuf {
            self.root.join("output")
        }

        /// A finished New Project as the worker leaves it in the workspace.
        fn generated_project(&self) -> PathBuf {
            let project = self.root.join("workspace");
            fs::create_dir_all(project.join("src")).unwrap();
            fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
            fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
            project
        }

        fn destination(&self, name: &str) -> PersistentDestination {
            PersistentDestination::resolve(Some(&self.output()), name).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn git(root: &Path, args: &[&str]) -> String {
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

    /// Required test 1: a successful persistent New Project.
    #[test]
    fn a_generated_project_is_persisted_to_the_chosen_destination() {
        let fixture = Fixture::new("success");
        let project = fixture.generated_project();
        let destination = fixture.destination("invoice-tool");

        let persisted = destination.persist(&project, &limits()).unwrap();

        assert_eq!(persisted.destination, destination.display());
        assert!(persisted.git.is_none(), "no repository was generated");
        assert!(
            persisted.git_warning.is_none(),
            "a project without a repository is not a reporting failure"
        );
        assert_eq!(
            fs::read_to_string(destination.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(destination.path().join("Cargo.toml").is_file());
        // Nothing is left behind next to the finished project.
        let siblings = fs::read_dir(fixture.output())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(siblings, vec![std::ffi::OsString::from("invoice-tool")]);
    }

    #[test]
    fn source_identity_is_normalized_and_credentials_are_never_persisted() {
        let fixture = Fixture::new("source-identity");
        let project = fixture.generated_project();
        let destination = fixture.destination("safe-source");

        let persisted = destination
            .persist_with_source_repository(
                &project,
                Some("https://github.com/acme/app.git"),
                &limits(),
            )
            .unwrap();
        assert_eq!(persisted.source_repository.as_deref(), Some("acme/app"));

        let rejected = fixture.destination("rejected-source");
        assert!(
            rejected
                .persist_with_source_repository(
                    &project,
                    Some("https://token@github.com/acme/app.git"),
                    &limits(),
                )
                .is_err()
        );
        assert!(!rejected.path().exists());
    }

    /// Required test 3: the persistent project is a real copy, so cleaning the
    /// temporary task workspace afterwards cannot take it away.
    #[test]
    fn the_persistent_project_survives_workspace_cleanup() {
        let fixture = Fixture::new("cleanup");
        let project = fixture.generated_project();
        let destination = fixture.destination("survivor");

        destination.persist(&project, &limits()).unwrap();
        fs::remove_dir_all(&project).unwrap();

        assert!(!project.exists());
        assert_eq!(
            fs::read_to_string(destination.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    /// Required test 6: milestone commits survive with their identity intact.
    #[test]
    fn git_history_survives_persistence_with_its_commit_shas() {
        let fixture = Fixture::new("git");
        let project = fixture.generated_project();
        git(&project, &["init", "--quiet"]);
        git(&project, &["config", "user.email", "tests@example.com"]);
        git(&project, &["config", "user.name", "Tests"]);
        git(&project, &["add", "--all", "."]);
        git(
            &project,
            &["commit", "--quiet", "-m", "feat(milestone-01): bootstrap"],
        );
        fs::write(project.join("src/lib.rs"), "pub fn work() {}\n").unwrap();
        git(&project, &["add", "--all", "."]);
        git(
            &project,
            &["commit", "--quiet", "-m", "feat(milestone-02): work"],
        );
        let expected = git(&project, &["log", "--format=%H %s"]);
        let destination = fixture.destination("history");

        let persisted = destination.persist(&project, &limits()).unwrap();

        assert_eq!(
            git(destination.path(), &["log", "--format=%H %s"]),
            expected
        );
        let status = persisted.git.expect("a repository was persisted");
        assert_eq!(status.commits, 2);
        assert_eq!(
            status.head_sha.as_deref(),
            Some(git(destination.path(), &["rev-parse", "HEAD"]).as_str())
        );
        assert!(status.branch.is_some(), "history must stay on a branch");
        assert!(!status.has_remote, "no remote may be configured");
    }

    /// Required test 4: an existing non-empty destination is never overwritten.
    #[test]
    fn an_existing_non_empty_destination_is_rejected_and_left_alone() {
        let fixture = Fixture::new("occupied");
        let project = fixture.generated_project();
        let destination = fixture.destination("existing");
        fs::create_dir(destination.path()).unwrap();
        fs::write(destination.path().join("mine.txt"), "user content\n").unwrap();

        let error = destination
            .persist(&project, &limits())
            .unwrap_err()
            .to_string();

        assert!(error.contains("not empty"), "unexpected: {error}");
        assert_eq!(
            fs::read_to_string(destination.path().join("mine.txt")).unwrap(),
            "user content\n"
        );
        assert!(!destination.path().join("Cargo.toml").exists());
    }

    /// An empty destination the user created themselves is usable.
    #[test]
    fn an_existing_empty_destination_is_used() {
        let fixture = Fixture::new("empty-dir");
        let project = fixture.generated_project();
        let destination = fixture.destination("prepared");
        fs::create_dir(destination.path()).unwrap();

        destination.persist(&project, &limits()).unwrap();

        assert!(destination.path().join("Cargo.toml").is_file());
    }

    /// Required test 5: traversal and other unsafe destinations are refused.
    #[test]
    fn unsafe_destination_names_are_rejected() {
        let fixture = Fixture::new("unsafe");
        let output = fixture.output();
        for name in [
            "",
            "   ",
            "..",
            "../escape",
            "..\\escape",
            "nested/child",
            "nested\\child",
            "C:/Windows",
            "/etc/passwd",
            "sneaky/../../escape",
            ".hidden",
            "-flag",
            "name with spaces",
            "quote\"name",
            "null\0name",
        ] {
            assert!(
                PersistentDestination::resolve(Some(&output), name).is_err(),
                "{name:?} must be rejected"
            );
        }
        // A name is only usable when the server configures a root at all.
        assert!(PersistentDestination::resolve(None, "project").is_err());
        // Nothing was created while rejecting any of them.
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    }

    /// Required test 7: a failed persistence never destroys or half-replaces
    /// what is already at the destination, and leaves no staging folder behind.
    #[test]
    fn a_failed_persistence_keeps_the_existing_destination_intact() {
        let fixture = Fixture::new("failure");
        let project = fixture.generated_project();
        let missing = fixture.root.join("workspace-gone");

        // The user prepared the folder themselves; the run then fails.
        let prepared = fixture.destination("keep-empty");
        fs::create_dir(prepared.path()).unwrap();
        let error = prepared
            .persist(&missing, &limits())
            .unwrap_err()
            .to_string();
        assert!(error.contains("no longer available"), "unexpected: {error}");
        assert!(prepared.path().is_dir(), "the destination was removed");
        assert_eq!(fs::read_dir(prepared.path()).unwrap().count(), 0);

        // A real project already lives there and must not be replaced.
        let occupied = fixture.destination("keep-project");
        fs::create_dir_all(occupied.path().join("src")).unwrap();
        fs::write(occupied.path().join("src/main.rs"), "existing work\n").unwrap();
        let error = occupied
            .persist(&project, &limits())
            .unwrap_err()
            .to_string();
        assert!(error.contains("not empty"), "unexpected: {error}");
        assert_eq!(
            fs::read_to_string(occupied.path().join("src/main.rs")).unwrap(),
            "existing work\n"
        );
        assert!(!occupied.path().join("Cargo.toml").exists());

        // Only the two destinations exist: no staging folder was left behind.
        assert_eq!(fs::read_dir(fixture.output()).unwrap().count(), 2);
    }

    /// Links are refused rather than followed, and refusing one does not touch
    /// what is already at the destination.
    #[test]
    #[cfg_attr(
        windows,
        ignore = "requires Windows Developer Mode or symlink privilege; runs normally on Unix"
    )]
    fn a_linked_project_file_is_refused_without_disturbing_the_destination() {
        let fixture = Fixture::new("links");
        let project = fixture.generated_project();
        let secret = fixture.root.join("secret.txt");
        fs::write(&secret, "private\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, project.join("link.txt")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&secret, project.join("link.txt")).unwrap();
        let destination = fixture.destination("linked");
        fs::create_dir(destination.path()).unwrap();

        let error = destination
            .persist(&project, &limits())
            .unwrap_err()
            .to_string();

        assert!(error.contains("link"), "unexpected: {error}");
        // The prepared destination is still there, still empty: the failed run
        // neither published a partial project nor removed the folder.
        assert!(destination.path().is_dir());
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(&secret).unwrap(), "private\n");
        // The copy failed after staging began; the staging folder is gone.
        assert_eq!(fs::read_dir(fixture.output()).unwrap().count(), 1);
    }

    /// Regression: a repository that cannot be INSPECTED after the project has
    /// already been renamed into place is missing metadata, not a failed
    /// persistence. The project is published either way.
    #[test]
    fn unreadable_git_metadata_after_finalization_still_persists_the_project() {
        let fixture = Fixture::new("git-metadata");
        let project = fixture.generated_project();
        git(&project, &["init", "--quiet"]);
        git(&project, &["config", "user.email", "tests@example.com"]);
        git(&project, &["config", "user.name", "Tests"]);
        git(&project, &["add", "--all", "."]);
        git(
            &project,
            &["commit", "--quiet", "-m", "feat(milestone-01): bootstrap"],
        );
        let destination = fixture.destination("described-badly");
        // Every Git command this run makes now times out, so the copy and the
        // rename still succeed and only the description of the result fails.
        let unusable_git = ExecutionLimits {
            git_timeout: std::time::Duration::from_nanos(1),
            ..ExecutionLimits::default()
        };

        let persisted = destination.persist(&project, &unusable_git).unwrap();

        assert_eq!(persisted.destination, destination.display());
        assert!(
            persisted.git.is_none(),
            "metadata is unavailable, not empty"
        );
        let warning = persisted
            .git_warning
            .expect("an unreadable repository is reported as a warning");
        assert!(warning.contains("could not be described"), "{warning}");
        // The project, and its history, really are at the destination.
        assert_eq!(
            fs::read_to_string(destination.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(destination.path().join(".git").is_dir());
        assert_eq!(
            git(destination.path(), &["log", "--format=%s"]),
            "feat(milestone-01): bootstrap"
        );
        // With a working Git the same destination describes itself normally.
        let status = crate::git::repository_status(destination.path(), &limits())
            .unwrap()
            .expect("a repository is present");
        assert_eq!(status.commits, 1);
    }
}
