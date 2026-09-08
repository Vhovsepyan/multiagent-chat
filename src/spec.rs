//! Turns the finished debate into a specification and stores external artifacts.
//!
//! DP-3 (decided): the Proposer drafts the spec, then the Critic checks it
//! against the debate and returns a corrected version. Two extra calls, but it
//! catches the failure mode that matters — a Proposer quietly dropping a
//! concession it made under review.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::api::{claude::ClaudeClient, gemini::GeminiClient, push_user};
use crate::debate::Transcript;
use crate::task::Emitter;
use crate::ui;

/// Legacy, project-owned input for `--implement-only`. Never written by us.
pub const SPEC_FILENAME: &str = "SPEC.md";
pub const APPROVED_SPEC_FILENAME: &str = "approved-spec.md";

/// The section list from plan.md. Both calls are held to it.
const SECTIONS: &str = "\
## Problem
## Agreed solution
## Architecture
## Steps
## Out of scope
## Open risks";

const DRAFT_SYSTEM: &str = "\
You are writing a specification document that another engineer will implement \
without seeing this discussion. Write only the document.

Rules:
- Output GitHub-flavoured Markdown and nothing else. No preamble, no sign-off, \
and do not wrap the document in a code fence.
- Use exactly these top-level sections, in this order, and no others.
- Under 'Steps', give a numbered list of implementation steps in dependency \
order.
- Be concrete: name files, types, endpoints and data fields. A reader must be \
able to start work without asking a question.
- Record only what was actually agreed. If the discussion left something open, \
put it under 'Open risks' rather than inventing an answer.";

const CHECK_SYSTEM: &str = "\
You are checking a specification against the discussion that produced it. You \
approved that design, so you know what was agreed.

Look for: claims the discussion never agreed on, concessions the Proposer made \
under review but quietly dropped from the spec, missing sections, and vagueness \
that would block an implementer.

Output the corrected specification in full, as GitHub-flavoured Markdown and \
nothing else. No preamble, no list of the changes you made, and do not wrap the \
document in a code fence. If the draft was already correct, output it unchanged.";

/// Draft with the Proposer, then have the Critic check it (DP-3).
pub async fn build(
    proposer: &GeminiClient,
    critic: &ClaudeClient,
    transcript: &Transcript,
    approved: bool,
    emitter: &Emitter,
) -> Result<String> {
    let request = format!(
        "The design is settled. Write the specification document now.\n\n\
         Use exactly these sections:\n\n{SECTIONS}"
    );

    ui::system("drafting specification (Proposer)...");
    emitter.notice("drafting specification (Proposer)...");
    let mut messages = transcript.for_proposer();
    push_user(&mut messages, request);
    let draft = proposer
        .send(Some(DRAFT_SYSTEM), &messages)
        .await
        .context("the Proposer failed to draft the spec")?;

    // If the debate never reached APPROVED, the objections the Critic raised
    // are still live. They must survive into the document rather than being
    // silently dropped, or the implementer will build a design nobody agreed to.
    let unresolved = if approved {
        ""
    } else {
        "\n\nIMPORTANT: this discussion ended WITHOUT agreement. Every objection \
         you raised that was not resolved must appear explicitly under \
         'Open risks', worded so an implementer knows it is unsettled."
    };

    ui::system("checking specification against the debate (Critic)...");
    emitter.notice("checking specification against the debate (Critic)...");
    let mut messages = transcript.for_critic();
    push_user(
        &mut messages,
        format!(
            "Here is the specification drafted from our discussion. Check it \
             and output the corrected version in full.\n\n\
             Required sections:\n\n{SECTIONS}{unresolved}\n\n---\n\n{draft}"
        ),
    );
    let checked = critic
        .send(Some(CHECK_SYSTEM), &messages)
        .await
        .context("the Critic failed to check the spec")?;

    Ok(strip_code_fence(&checked))
}

/// Read the spec already sitting in the target repo (`--implement-only`).
pub fn read_from(repo: &Path) -> Result<String> {
    let path = repo.join(SPEC_FILENAME);
    let text = fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read {} — run without --implement-only to generate one",
            path.display()
        )
    })?;

    if text.trim().is_empty() {
        bail!("{} is empty — nothing to implement", path.display());
    }
    Ok(text)
}

/// Write an orchestration artifact in a provider-owned directory outside repo.
pub fn write_artifact(artifacts: &Path, spec: &str) -> Result<PathBuf> {
    if crate::repository_file::is_link(&fs::symlink_metadata(artifacts)?) {
        bail!("refusing a linked artifact directory");
    }
    let artifacts = artifacts.canonicalize()?;
    let path = artifacts.join(APPROVED_SPEC_FILENAME);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if crate::repository_file::is_link(&metadata) || !metadata.is_file() => {
            bail!("refusing to replace a linked or non-regular specification artifact");
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    // Never open the existing destination: rename replaces its directory entry,
    // so even a link swapped in after the check cannot redirect the write.
    let temporary = artifacts.join(format!(".spec-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(spec.as_bytes())?;
        drop(file);
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
}

/// Legacy CLI runs do not own the project directory's lifecycle. Store their
/// artifacts separately and retain the path for manual review/recovery.
pub fn write_cli_artifact(spec: &str) -> Result<PathBuf> {
    let artifacts =
        std::env::temp_dir().join(format!("multiagent-chat-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&artifacts)?;
    write_artifact(&artifacts, spec)
}

/// Models often wrap a whole document in ```markdown fences despite being told
/// not to. Unwrap it, but only when the fence encloses the entire text — a spec
/// may legitimately contain code blocks of its own.
fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();

    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    let Some(body) = rest.split_once('\n').map(|(_lang, body)| body) else {
        return trimmed.to_string();
    };
    let Some(inner) = body.trim_end().strip_suffix("```") else {
        return trimmed.to_string();
    };

    // If a fence closes before the end, the outer pair was not wrapping
    // everything and we must leave the text alone.
    if inner.contains("\n```") {
        return trimmed.to_string();
    }
    inner.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_a_whole_document_fence() {
        let raw = "```markdown\n## Problem\nCredit apps are manual.\n```";
        assert_eq!(strip_code_fence(raw), "## Problem\nCredit apps are manual.");
    }

    #[test]
    fn unwraps_a_fence_with_no_language() {
        let raw = "```\n## Problem\ntext\n```";
        assert_eq!(strip_code_fence(raw), "## Problem\ntext");
    }

    #[test]
    fn leaves_a_plain_document_alone() {
        let raw = "## Problem\nCredit apps are manual.";
        assert_eq!(strip_code_fence(raw), raw);
    }

    /// The important case: a spec containing its own code blocks must survive.
    #[test]
    fn keeps_inner_code_blocks() {
        let raw = "## Architecture\n\n```rust\nfn main() {}\n```\n\n## Steps\n1. go";
        assert_eq!(strip_code_fence(raw), raw);
    }

    #[test]
    fn does_not_eat_a_document_that_merely_starts_with_a_code_block() {
        let raw = "```rust\nfn main() {}\n```\n\n## Steps\n1. go";
        assert_eq!(strip_code_fence(raw), raw);
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mac-spec-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_back_a_written_spec() {
        let dir = scratch_dir("roundtrip");
        fs::write(
            dir.join(SPEC_FILENAME),
            "## Problem
something",
        )
        .unwrap();

        assert_eq!(
            read_from(&dir).unwrap(),
            "## Problem
something"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_spec_says_how_to_make_one() {
        let dir = scratch_dir("missing");
        let err = read_from(&dir).unwrap_err().to_string();

        assert!(err.contains("--implement-only"), "unexpected: {err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_spec_is_rejected() {
        let dir = scratch_dir("empty");
        fs::write(dir.join(SPEC_FILENAME), "   \n\n").unwrap();

        let err = read_from(&dir).unwrap_err().to_string();
        assert!(err.contains("empty"), "unexpected: {err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writes_the_file_into_the_artifact_directory() {
        let dir = std::env::temp_dir().join(format!("mac-spec-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let path = write_artifact(&dir, "## Problem\nx").unwrap();

        assert_eq!(path.file_name().unwrap(), APPROVED_SPEC_FILENAME);
        assert_eq!(fs::read_to_string(&path).unwrap(), "## Problem\nx");

        fs::remove_dir_all(&dir).ok();
    }
}
