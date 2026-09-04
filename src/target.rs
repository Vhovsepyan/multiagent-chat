//! Legacy CLI adapter for selecting a local repository.
//!
//! The production web application uses registered Projects and
//! `WorkspaceProvider`. This module preserves the original `--cli` behavior.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::ui;

fn workspace_root(config: &Config) -> Result<&Path> {
    config.workspace_root.as_deref().ok_or_else(|| {
        anyhow::anyhow!("WORKSPACE_ROOT is required only for the legacy CLI workflow")
    })
}

/// Words that carry no meaning in a project name, so a topic like
/// "I need credit applications" suggests "credit-applications".
const FILLER: &[&str] = &[
    "i", "we", "need", "needs", "want", "wants", "a", "an", "the", "to", "build", "create", "make",
    "please", "some", "new", "for", "my", "our", "that", "which", "with", "using",
];

/// How many words of the topic to keep in the suggested name.
const MAX_SLUG_WORDS: usize = 4;

/// Ask which repo to use, creating it if the user agrees.
pub fn resolve(config: &Config, topic: &str) -> Result<PathBuf> {
    let suggestion = slug_from_topic(topic);
    let name = ui::prompt("Project", &suggestion)?;
    let name = validate_name(&name)?;

    let path = workspace_root(config)?.join(name);

    if path.is_dir() {
        ui::system(&format!("  -> {}", path.display()));
        return Ok(path);
    }
    if path.exists() {
        bail!("{} exists but is not a directory", path.display());
    }

    ui::system(&format!("  -> {}", path.display()));
    ui::warn("that project does not exist yet.");
    if !ui::confirm("Create it and run git init?")? {
        bail!("no target repo chosen — nothing to do");
    }

    fs::create_dir_all(&path).with_context(|| format!("could not create {}", path.display()))?;
    git_init(&path)?;
    ui::success(&format!("created {}", path.display()));

    Ok(path)
}

/// Ask which existing repo to use, for `--implement-only`.
///
/// Unlike `resolve`, this never offers to create anything: the whole point is
/// to build from a spec that is already there, so a missing folder means the
/// name was wrong.
pub fn resolve_existing(config: &Config, topic: Option<&str>) -> Result<PathBuf> {
    let suggestion = topic.map(slug_from_topic).unwrap_or_default();
    let name = ui::prompt("Project", &suggestion)?;
    let name = validate_name(&name)?;

    let path = workspace_root(config)?.join(name);
    if !path.is_dir() {
        bail!(
            "{} does not exist — --implement-only needs a project that already holds a SPEC.md",
            path.display()
        );
    }

    ui::system(&format!("  -> {}", path.display()));
    Ok(path)
}

/// Every project folder inside `WORKSPACE_ROOT`, sorted.
///
/// Used by `GET /api/projects` to populate the picker. Hidden folders and
/// anything that is not a directory are skipped.
#[allow(dead_code)] // legacy CLI compatibility; production web uses ProjectStore
pub fn list_projects(config: &Config) -> Result<Vec<String>> {
    let root = workspace_root(config)?;
    let entries =
        fs::read_dir(root).with_context(|| format!("could not read {}", root.display()))?;

    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();

    names.sort();
    Ok(names)
}

/// Resolve a project by name for the web UI, creating it if it is new.
///
/// The non-interactive twin of `resolve`: the browser has already asked the
/// user, so there is nobody here to prompt.
#[allow(dead_code)] // legacy CLI compatibility; production web uses WorkspaceProvider
pub fn ensure_project(config: &Config, name: &str) -> Result<PathBuf> {
    let name = validate_name(name)?;
    let path = workspace_root(config)?.join(name);

    if path.is_dir() {
        return Ok(path);
    }
    if path.exists() {
        bail!("{} exists but is not a directory", path.display());
    }

    fs::create_dir_all(&path).with_context(|| format!("could not create {}", path.display()))?;
    git_init(&path)?;
    Ok(path)
}

/// Reject anything that would escape `WORKSPACE_ROOT`.
///
/// Without this, typing `../../Windows` would happily resolve to somewhere
/// outside the projects folder.
fn validate_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("project name cannot be empty");
    }
    if name.contains(['/', '\\']) || name.contains("..") {
        bail!("project name must be a plain folder name, not a path: {name:?}");
    }
    Ok(name)
}

/// Turn a free-text topic into a plausible folder name.
fn slug_from_topic(topic: &str) -> String {
    // Drop filler wherever it appears, not just at the front: the first live
    // run produced "small-cli-tool-that" from "a small CLI tool that renames
    // files", and a connective can sit in the middle too ("server with rate
    // limiting").
    let kept: Vec<String> = topic
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !FILLER.contains(&w.as_str()))
        .take(MAX_SLUG_WORDS)
        .collect();

    if kept.is_empty() {
        "project".to_string()
    } else {
        kept.join("-")
    }
}

/// `git init` in a freshly created folder. A failure here is not fatal — the
/// spec can still be written — so we only warn.
fn git_init(path: &Path) -> Result<()> {
    match Command::new("git").arg("init").current_dir(path).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            ui::warn(&format!(
                "git init failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            Ok(())
        }
        Err(e) => {
            ui::warn(&format!("could not run git: {e}"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::slug_from_topic;

    #[test]
    fn strips_leading_filler() {
        assert_eq!(
            slug_from_topic("I need credit applications"),
            "credit-applications"
        );
        assert_eq!(slug_from_topic("we want to build a messenger"), "messenger");
    }

    #[test]
    fn handles_punctuation_and_length() {
        assert_eq!(
            slug_from_topic("Credit  applications, v2!"),
            "credit-applications-v2"
        );
        assert_eq!(
            slug_from_topic("one two three four five six"),
            "one-two-three-four"
        );
    }

    /// Caught by the first live run, which suggested "small-cli-tool-that".
    #[test]
    fn drops_filler_anywhere_not_just_at_the_front() {
        assert_eq!(
            slug_from_topic("a small CLI tool that renames files in a folder"),
            "small-cli-tool-renames"
        );
        assert_eq!(
            slug_from_topic("an API server with rate limiting"),
            "api-server-rate-limiting"
        );
    }

    #[test]
    fn never_returns_empty() {
        assert_eq!(slug_from_topic("the a an"), "project");
        assert_eq!(slug_from_topic("!!!"), "project");
    }
}
