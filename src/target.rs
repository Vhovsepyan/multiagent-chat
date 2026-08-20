//! Picks the repo that this run will write SPEC.md into.
//!
//! The target repo is *per-run input*, not configuration: today the topic is
//! credit applications, tomorrow a messenger. `.env` only says where projects
//! live (`WORKSPACE_ROOT`); the project itself is chosen here, and created on
//! the spot if it does not exist yet.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::ui;

/// Words that carry no meaning in a project name, so a topic like
/// "I need credit applications" suggests "credit-applications".
const FILLER: &[&str] = &[
    "i", "we", "need", "needs", "want", "wants", "a", "an", "the", "to", "build", "create", "make",
    "please", "some", "new", "for", "my", "our",
];

/// How many words of the topic to keep in the suggested name.
const MAX_SLUG_WORDS: usize = 4;

/// Ask which repo to use, creating it if the user agrees.
pub fn resolve(config: &Config, topic: &str) -> Result<PathBuf> {
    let suggestion = slug_from_topic(topic);
    let name = ui::prompt("Project", &suggestion)?;
    let name = validate_name(&name)?;

    let path = config.workspace_root.join(name);

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
    let words: Vec<&str> = topic
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    // Skip leading filler, but never skip everything.
    let start = words
        .iter()
        .position(|w| !FILLER.contains(&w.to_lowercase().as_str()))
        .unwrap_or(0);

    let slug = words[start..]
        .iter()
        .take(MAX_SLUG_WORDS)
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
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

    #[test]
    fn never_returns_empty() {
        assert_eq!(slug_from_topic("the a an"), "the-a-an");
        assert_eq!(slug_from_topic("!!!"), "project");
    }
}
