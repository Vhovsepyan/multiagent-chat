//! Turns the finished debate into a clean SPEC.md.
//!
//! DP-3 (decided): the Proposer drafts the spec, then the Critic checks it
//! against the debate and returns a corrected version. Two extra calls, but it
//! catches the failure mode that matters — a Proposer quietly dropping a
//! concession it made under review.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::api::{claude::ClaudeClient, gemini::GeminiClient, push_user};
use crate::debate::Transcript;
use crate::ui;

/// The file the implementer will read.
pub const SPEC_FILENAME: &str = "SPEC.md";

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
) -> Result<String> {
    let request = format!(
        "The design is settled. Write the specification document now.\n\n\
         Use exactly these sections:\n\n{SECTIONS}"
    );

    ui::system("drafting SPEC.md (Proposer)...");
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

    ui::system("checking SPEC.md against the debate (Critic)...");
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

/// Write the spec into the target repo, returning where it landed.
pub fn write_to(repo: &Path, spec: &str) -> Result<PathBuf> {
    let path = repo.join(SPEC_FILENAME);
    fs::write(&path, spec).with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
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

    #[test]
    fn writes_the_file_into_the_repo() {
        let dir = std::env::temp_dir().join(format!("mac-spec-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let path = write_to(&dir, "## Problem\nx").unwrap();

        assert_eq!(path.file_name().unwrap(), SPEC_FILENAME);
        assert_eq!(fs::read_to_string(&path).unwrap(), "## Problem\nx");

        fs::remove_dir_all(&dir).ok();
    }
}
