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

use crate::agent::ChatAgent;
use crate::api::push_user;
use crate::debate::Transcript;
use crate::evidence::{EvidencePayload, EvidenceRole, EvidenceStatus};
use crate::task::{AgentStage, Emitter, TaskEvent};
use crate::ui;

/// Legacy, project-owned input for `--implement-only`. Never written by us.
pub const SPEC_FILENAME: &str = "SPEC.md";
pub const APPROVED_SPEC_FILENAME: &str = "approved-spec.md";

/// The format contract is deliberately shown verbatim to the generating agent.
const TEMPLATE: &str = r#"# Specification

## Goal

Short description of what must be implemented.

## Requirements

- Requirement one.
- Requirement two.
- Requirement three.

## Acceptance Criteria

- AC-1: First observable result.
- AC-2: Second observable result.
- AC-3: Third observable result.

## Steps

1. Implement first milestone
2. Implement second milestone
3. Add or update tests
4. Run verification

## Verification

- Run relevant unit tests.
- Run relevant integration tests.
- Run the project's required final verification."#;

const REQUIRED_SECTIONS: [&str; 5] = [
    "Goal",
    "Requirements",
    "Acceptance Criteria",
    "Steps",
    "Verification",
];
const MAX_FORMAT_REPAIRS: usize = 1;

const DRAFT_SYSTEM: &str = "\
You are writing a specification document that another engineer will implement \
without seeing this discussion. Write only the document.

Rules:
- Output GitHub-flavoured Markdown and nothing else. No preamble, no sign-off, \
and do not wrap the document in a code fence.
- Follow the supplied template exactly: do not rename required sections, include \
exactly one '## Steps' section, and use only top-level numbered list entries \
for milestones. Do not use headings or bullets as milestones.
- Be concrete: name files, types, endpoints and data fields. A reader must be \
able to start work without asking a question.
- Record only what was actually agreed. State unresolved assumptions explicitly \
under 'Requirements' rather than inventing an answer.";

const CHECK_SYSTEM: &str = "\
You are checking a specification against the discussion that produced it. You \
approved that design, so you know what was agreed.

Look for: claims the discussion never agreed on, concessions the Proposer made \
under review but quietly dropped from the spec, missing sections, and vagueness \
that would block an implementer.

Output the corrected specification in full, as GitHub-flavoured Markdown and \
nothing else. No preamble, no list of the changes you made, and do not wrap the \
document in a code fence. If the draft was already correct, output it unchanged.";

const REPAIR_SYSTEM: &str = "\
Repair only the Markdown structure of this specification. Preserve its technical \
meaning. Return Markdown only, with no preamble and no outer code fence. Follow \
the supplied template exactly: required sections keep their names, there is exactly \
one ## Steps section, and every milestone is a top-level numbered list item.";

/// Draft with the Proposer, then have the Critic check it (DP-3).
pub async fn build(
    proposer: &dyn ChatAgent,
    critic: &dyn ChatAgent,
    transcript: &Transcript,
    approved: bool,
    emitter: &Emitter,
) -> Result<String> {
    let request = format!(
        "The design is settled. Write the specification document now.\n\n\
         Follow this exact structural template:\n\n```markdown\n{TEMPLATE}\n```"
    );

    ui::system("drafting specification (Proposer)...");
    emitter.notice("drafting specification (Proposer)...");
    emitter.emit(TaskEvent::ProposerStarted {
        stage: AgentStage::Specification,
        round: None,
        provider: proposer.provider(),
        model: proposer.model().to_string(),
    });
    let mut messages = transcript.for_proposer();
    push_user(&mut messages, request);
    let prompt = crate::evidence::chat_prompt(Some(DRAFT_SYSTEM), &messages);
    let started = std::time::Instant::now();
    let draft = match proposer
        .complete_text(Some(DRAFT_SYSTEM), &messages)
        .await
        .context("the Proposer failed to draft the spec")
    {
        Ok(draft) => {
            emitter.record_evidence(EvidencePayload::AgentInteraction {
                stage: AgentStage::Specification,
                role: EvidenceRole::Proposer,
                round: None,
                provider: proposer.provider(),
                model: proposer.model().to_string(),
                prompt,
                response: Some(draft.clone()),
                status: EvidenceStatus::Completed,
                error: None,
                duration_ms: crate::evidence::elapsed_ms(started),
                truncated: false,
            });
            emitter.emit(TaskEvent::ProposerCompleted {
                stage: AgentStage::Specification,
                round: None,
                provider: proposer.provider(),
                model: proposer.model().to_string(),
            });
            draft
        }
        Err(error) => {
            let message = format!("{error:#}");
            emitter.record_evidence(EvidencePayload::AgentInteraction {
                stage: AgentStage::Specification,
                role: EvidenceRole::Proposer,
                round: None,
                provider: proposer.provider(),
                model: proposer.model().to_string(),
                prompt,
                response: None,
                status: EvidenceStatus::Failed,
                error: Some(message.clone()),
                duration_ms: crate::evidence::elapsed_ms(started),
                truncated: false,
            });
            emitter.emit(TaskEvent::ProposerFailed {
                stage: AgentStage::Specification,
                round: None,
                provider: proposer.provider(),
                model: proposer.model().to_string(),
                error: message,
            });
            return Err(error);
        }
    };

    // If the debate never reached APPROVED, the objections the Critic raised
    // are still live. They must survive into the document rather than being
    // silently dropped, or the implementer will build a design nobody agreed to.
    let unresolved = if approved {
        ""
    } else {
        "\n\nIMPORTANT: this discussion ended WITHOUT agreement. Every objection \
         you raised that was not resolved must appear explicitly under \
         'Requirements', worded so an implementer knows it is unsettled."
    };

    ui::system("checking specification against the debate (Critic)...");
    emitter.notice("checking specification against the debate (Critic)...");
    emitter.emit(TaskEvent::CriticStarted {
        stage: AgentStage::Specification,
        round: None,
        provider: critic.provider(),
        model: critic.model().to_string(),
    });
    let mut messages = transcript.for_critic();
    push_user(
        &mut messages,
        format!(
            "Here is the specification drafted from our discussion. Check it \
             and output the corrected version in full.\n\n\
             Required structural template:\n\n```markdown\n{TEMPLATE}\n```{unresolved}\n\n---\n\n{draft}"
        ),
    );
    let prompt = crate::evidence::chat_prompt(Some(CHECK_SYSTEM), &messages);
    let started = std::time::Instant::now();
    let checked = match critic
        .complete_text(Some(CHECK_SYSTEM), &messages)
        .await
        .context("the Critic failed to check the spec")
    {
        Ok(checked) => {
            emitter.record_evidence(EvidencePayload::AgentInteraction {
                stage: AgentStage::Specification,
                role: EvidenceRole::Critic,
                round: None,
                provider: critic.provider(),
                model: critic.model().to_string(),
                prompt,
                response: Some(checked.clone()),
                status: EvidenceStatus::Completed,
                error: None,
                duration_ms: crate::evidence::elapsed_ms(started),
                truncated: false,
            });
            emitter.emit(TaskEvent::CriticCompleted {
                stage: AgentStage::Specification,
                round: None,
                provider: critic.provider(),
                model: critic.model().to_string(),
            });
            checked
        }
        Err(error) => {
            let message = format!("{error:#}");
            emitter.record_evidence(EvidencePayload::AgentInteraction {
                stage: AgentStage::Specification,
                role: EvidenceRole::Critic,
                round: None,
                provider: critic.provider(),
                model: critic.model().to_string(),
                prompt,
                response: None,
                status: EvidenceStatus::Failed,
                error: Some(message.clone()),
                duration_ms: crate::evidence::elapsed_ms(started),
                truncated: false,
            });
            emitter.emit(TaskEvent::CriticFailed {
                stage: AgentStage::Specification,
                round: None,
                provider: critic.provider(),
                model: critic.model().to_string(),
                error: message,
            });
            return Err(error);
        }
    };

    let mut document = strip_code_fence(&checked);
    if let Err(error) = validate_format(&document) {
        emitter.notice(format!("specification format validation failed: {error}"));
        for attempt in 1..=MAX_FORMAT_REPAIRS {
            emitter.notice(format!(
                "repairing specification format (attempt {attempt})..."
            ));
            let mut messages = transcript.for_critic();
            push_user(
                &mut messages,
                format!(
                    "The specification below failed structural validation: {error}\n\n\
                     Required template:\n\n```markdown\n{TEMPLATE}\n```\n\n\
                     Preserve technical meaning and repair formatting only.\n\n---\n\n{document}"
                ),
            );
            let prompt = crate::evidence::chat_prompt(Some(REPAIR_SYSTEM), &messages);
            let started = std::time::Instant::now();
            let repaired = match critic.complete_text(Some(REPAIR_SYSTEM), &messages).await {
                Ok(repaired) => repaired,
                Err(error) => {
                    let message = format!("{error:#}");
                    emitter.record_evidence(EvidencePayload::AgentInteraction {
                        stage: AgentStage::Specification,
                        role: EvidenceRole::Critic,
                        round: None,
                        provider: critic.provider(),
                        model: critic.model().to_string(),
                        prompt,
                        response: None,
                        status: EvidenceStatus::Failed,
                        error: Some(message),
                        duration_ms: crate::evidence::elapsed_ms(started),
                        truncated: false,
                    });
                    return Err(error)
                        .context("the Critic failed to repair the specification format");
                }
            };
            emitter.record_evidence(EvidencePayload::AgentInteraction {
                stage: AgentStage::Specification,
                role: EvidenceRole::Critic,
                round: None,
                provider: critic.provider(),
                model: critic.model().to_string(),
                prompt,
                response: Some(repaired.clone()),
                status: EvidenceStatus::Completed,
                error: None,
                duration_ms: crate::evidence::elapsed_ms(started),
                truncated: false,
            });
            document = strip_code_fence(&repaired);
            match validate_format(&document) {
                Ok(()) => break,
                Err(next_error) if attempt == MAX_FORMAT_REPAIRS => {
                    bail!(
                        "specification format validation failed after {attempt} repair attempt(s): {next_error}"
                    );
                }
                Err(next_error) => {
                    emitter.notice(format!(
                        "specification format repair {attempt} failed: {next_error}"
                    ));
                }
            }
        }
    }
    validate_format(&document).context("specification format validation failed")?;
    emitter.notice("specification format validation passed");
    Ok(document)
}

/// Validate the generated specification before it can be presented for approval.
/// The execution planner remains the final shared check, while this contract
/// deliberately narrows new specifications to numbered top-level milestones.
pub fn validate_format(specification: &str) -> Result<()> {
    let headings = specification
        .lines()
        .filter_map(|line| line.strip_prefix("## ").map(str::trim))
        .collect::<Vec<_>>();
    for required in REQUIRED_SECTIONS {
        let count = headings
            .iter()
            .filter(|heading| **heading == required)
            .count();
        if count != 1 {
            bail!("specification must contain exactly one ## {required} section");
        }
    }

    let steps = steps_lines(specification)?;
    let mut fence = None;
    let mut milestones = 0usize;
    for line in steps {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if fence_delimiter(trimmed).is_some_and(|delimiter| delimiter == open) {
                fence = None;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(delimiter) = fence_delimiter(trimmed) {
            fence = Some(delimiter);
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        if numbered_milestone(line) {
            milestones += 1;
        } else {
            bail!("invalid top-level milestone entry in ## Steps: {line:?}");
        }
    }
    if milestones == 0 {
        bail!("## Steps must contain at least one top-level numbered milestone");
    }
    crate::milestone::plan_from_spec(specification, &[])
        .map(|_| ())
        .map_err(anyhow::Error::msg)
}

fn steps_lines(specification: &str) -> Result<Vec<&str>> {
    let mut found = false;
    let mut lines = Vec::new();
    for line in specification.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if found {
                break;
            }
            if heading.trim() == "Steps" {
                found = true;
            }
            continue;
        }
        if found {
            lines.push(line);
        }
    }
    if found {
        Ok(lines)
    } else {
        bail!("specification is missing ## Steps")
    }
}

fn numbered_milestone(line: &str) -> bool {
    let Some((number, title)) = line.split_once('.') else {
        return false;
    };
    !number.is_empty()
        && number.chars().all(|character| character.is_ascii_digit())
        && title
            .strip_prefix(' ')
            .is_some_and(|title| !title.trim().is_empty())
}

fn fence_delimiter(line: &str) -> Option<char> {
    ["```", "~~~"].into_iter().find_map(|prefix| {
        line.starts_with(prefix)
            .then(|| prefix.chars().next().expect("fence prefix is non-empty"))
    })
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
    use crate::agent::ChatProvider;
    use crate::agent::chat::ScriptedAgent;
    use crate::debate::{Speaker, Transcript};

    const VALID_SPEC: &str = "# Specification\n\n## Goal\n\nShip the change.\n\n## Requirements\n\n- Preserve behavior.\n\n## Acceptance Criteria\n\n- AC-1: The change works.\n\n## Steps\n\n1. Implement the change\n2. Add tests\n\n## Verification\n\n- Run cargo test.";

    // --- task 0004: DP-3 now runs against any chat agent -------------------

    /// The Critic checks the Proposer draft, and its corrected version is what
    /// comes back — fence and all removed.
    #[tokio::test]
    async fn the_critics_corrected_draft_is_the_result() {
        let proposer = ScriptedAgent::new(ChatProvider::Gemini, &[VALID_SPEC]);
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[VALID_SPEC]);

        let manager = crate::task::TaskManager::new();
        let task = manager.create("audit", "specification", "legacy");
        let document = build(
            &proposer,
            &critic,
            &transcript(),
            true,
            &manager.emitter(task.id),
        )
        .await
        .unwrap();

        assert_eq!(document, VALID_SPEC);
        assert!(
            proposer
                .call(0)
                .1
                .last()
                .unwrap()
                .content
                .contains(TEMPLATE)
        );
        assert!(
            critic
                .call(0)
                .1
                .last()
                .unwrap()
                .content
                .contains("Ship the change")
        );
        let stored = manager.get(task.id).unwrap();
        assert!(stored.history.iter().any(|recorded| matches!(
            recorded.event,
            TaskEvent::ProposerStarted {
                stage: AgentStage::Specification,
                ..
            }
        )));
        assert!(stored.history.iter().any(|recorded| matches!(
            recorded.event,
            TaskEvent::CriticCompleted {
                stage: AgentStage::Specification,
                ..
            }
        )));
        assert_eq!(stored.evidence.len(), 2);
        assert!(matches!(
            &stored.evidence[1].payload,
            crate::evidence::EvidencePayload::AgentInteraction {
                stage: AgentStage::Specification,
                role: crate::evidence::EvidenceRole::Critic,
                prompt,
                response: Some(response),
                ..
            } if prompt.contains("Ship the change") && response.contains("Ship the change")
        ));
    }

    /// An unapproved debate must tell the Critic to keep its live objections
    /// in the document, or the implementer builds a design nobody agreed to.
    #[tokio::test]
    async fn an_unapproved_debate_demands_open_risks() {
        let proposer = ScriptedAgent::new(ChatProvider::Gemini, &[VALID_SPEC]);
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[VALID_SPEC]);

        build(
            &proposer,
            &critic,
            &transcript(),
            false,
            &crate::task::Emitter::detached(),
        )
        .await
        .unwrap();

        let request = critic.call(0).1.last().unwrap().content.clone();
        assert!(
            request.contains("WITHOUT agreement"),
            "unexpected: {request}"
        );
        assert!(request.contains("Requirements"));
    }

    fn transcript() -> Transcript {
        let mut t = Transcript::new("credit applications");
        t.push_for_test(Speaker::Proposer, "the agreed design");
        t.push_for_test(Speaker::Critic, "VERDICT: APPROVED");
        t
    }

    #[test]
    fn valid_template_passes_and_plans_as_milestones() {
        validate_format(VALID_SPEC).unwrap();
        assert_eq!(
            crate::milestone::plan_from_spec(VALID_SPEC, &[])
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn heading_or_bullet_milestones_are_rejected_before_approval() {
        for invalid in [
            VALID_SPEC.replace("1. Implement the change", "### 1. Implement the change"),
            VALID_SPEC.replace("1. Implement the change", "- Implement the change"),
        ] {
            assert!(validate_format(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn nested_descriptions_and_fenced_code_remain_valid() {
        let specification = VALID_SPEC.replace(
            "1. Implement the change",
            "1. Implement the change\n   - Preserve compatibility.\n   ```rust\n   let value = 1;\n   ```",
        );
        validate_format(&specification).unwrap();
    }

    #[test]
    fn missing_duplicate_or_empty_steps_are_rejected() {
        assert!(validate_format(&VALID_SPEC.replace("## Steps", "## Work")).is_err());
        assert!(validate_format(&format!("{VALID_SPEC}\n\n## Steps\n\n1. Duplicate")).is_err());
        assert!(
            validate_format(&VALID_SPEC.replace("1. Implement the change\n2. Add tests", "",))
                .is_err()
        );
    }

    #[test]
    fn a_missing_required_section_is_rejected() {
        assert!(validate_format(&VALID_SPEC.replace("## Verification", "## Checks")).is_err());
    }

    #[tokio::test]
    async fn bounded_format_repair_produces_an_approvable_specification() {
        let invalid = VALID_SPEC.replace("1. Implement the change", "### 1. Implement the change");
        let proposer = ScriptedAgent::new(ChatProvider::Gemini, &[VALID_SPEC]);
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[&invalid, VALID_SPEC]);
        let manager = crate::task::TaskManager::new();
        let task = manager.create("audit", "repair", "legacy");

        assert_eq!(
            build(
                &proposer,
                &critic,
                &transcript(),
                true,
                &manager.emitter(task.id),
            )
            .await
            .unwrap(),
            VALID_SPEC
        );
        assert_eq!(manager.get(task.id).unwrap().evidence.len(), 3);
    }

    #[tokio::test]
    async fn failed_bounded_format_repair_never_returns_a_specification() {
        let invalid = VALID_SPEC.replace("1. Implement the change", "### 1. Implement the change");
        let proposer = ScriptedAgent::new(ChatProvider::Gemini, &[VALID_SPEC]);
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[&invalid, &invalid]);

        assert!(
            build(
                &proposer,
                &critic,
                &transcript(),
                true,
                &crate::task::Emitter::detached(),
            )
            .await
            .is_err()
        );
    }

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
