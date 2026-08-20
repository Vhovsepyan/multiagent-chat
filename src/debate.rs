//! The debate loop: Proposer writes, Critic reviews, repeat until APPROVED.
//!
//! DP-1 (decided): state lives in ONE shared `Transcript`. Each model's view is
//! rebuilt from it on every call, with that model's own turns marked as the
//! assistant and the other model's turns as the user. One source of truth means
//! printing the debate and writing SPEC.md later both read the same data.
//!
//! DP-2 (decided): the verdict is found by scanning the critique's lines from
//! the bottom for `VERDICT: APPROVED` / `VERDICT: NEEDS_WORK`, so the Critic may
//! add a closing sentence without breaking the run.

use anyhow::Result;

use crate::api::{Message, claude::ClaudeClient, gemini::GeminiClient};
use crate::ui;

// ---------------------------------------------------------------------------
// System prompts
// ---------------------------------------------------------------------------

const PROPOSER_SYSTEM: &str = "\
You are the Proposer in a two-model design debate. You write concrete, \
buildable solution proposals: the architecture, the main components, the data \
model, and the order of work. Be specific and decide things — name the \
technologies, the tables, the endpoints. Do not hedge or list every option.

A Critic reviews each proposal. When you receive a review, revise your proposal \
to address every point, or explain briefly why a point is wrong. Always reply \
with the FULL revised proposal, not a diff or a list of changes.

Keep it under roughly 600 words.";

const CRITIC_SYSTEM: &str = "\
You are the Critic in a two-model design debate. Review the Proposer's solution \
for correctness, missing requirements, unnecessary complexity, and risk. Be \
direct and specific: quote the part you object to and say what to do instead. \
Do not rewrite the proposal yourself.

If the proposal is genuinely good enough to build from, approve it. Do not \
demand perfection or invent work.

End your reply with exactly one of these lines, on its own line, and nothing \
after it:
VERDICT: APPROVED
VERDICT: NEEDS_WORK";

/// The first thing the Proposer is asked.
const PROPOSER_TASK: &str = "Write a solution proposal for this topic.";

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/// Which model said a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    Proposer,
    Critic,
}

/// One thing that was said, stored exactly as the model wrote it.
#[derive(Debug, Clone)]
pub struct Turn {
    pub speaker: Speaker,
    pub text: String,
}

/// The whole debate. This is the single source of truth (DP-1).
#[derive(Debug, Clone)]
pub struct Transcript {
    pub topic: String,
    pub turns: Vec<Turn>,
}

impl Transcript {
    pub fn new(topic: impl Into<String>) -> Self {
        Transcript {
            topic: topic.into(),
            turns: Vec::new(),
        }
    }

    fn push(&mut self, speaker: Speaker, text: impl Into<String>) {
        self.turns.push(Turn {
            speaker,
            text: text.into(),
        });
    }

    /// The conversation as the Proposer should see it: its own proposals are
    /// the assistant, the Critic's reviews arrive as user messages.
    pub fn for_proposer(&self) -> Vec<Message> {
        let mut messages = vec![Message::user(format!(
            "Topic: {}\n\n{PROPOSER_TASK}",
            self.topic
        ))];

        for turn in &self.turns {
            match turn.speaker {
                Speaker::Proposer => messages.push(Message::assistant(&turn.text)),
                Speaker::Critic => messages.push(Message::user(format!(
                    "The Critic reviewed your proposal:\n\n{}\n\nReply with the full revised proposal.",
                    turn.text
                ))),
            }
        }
        messages
    }

    /// The conversation as the Critic should see it: its own reviews are the
    /// assistant, the Proposer's work arrives as user messages.
    ///
    /// The transcript always starts with a Proposer turn, so this always starts
    /// with a user message — which both APIs require.
    pub fn for_critic(&self) -> Vec<Message> {
        let mut messages = Vec::new();
        let mut seen_proposal = false;

        for turn in &self.turns {
            match turn.speaker {
                Speaker::Proposer => {
                    let text = if seen_proposal {
                        format!("Revised proposal:\n\n{}", turn.text)
                    } else {
                        seen_proposal = true;
                        format!("Topic: {}\n\nProposal:\n\n{}", self.topic, turn.text)
                    };
                    messages.push(Message::user(text));
                }
                Speaker::Critic => messages.push(Message::assistant(&turn.text)),
            }
        }
        messages
    }
}

// ---------------------------------------------------------------------------
// Verdict detection (DP-2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    NeedsWork,
}

/// Strip the decoration a model might wrap the line in, e.g. `**VERDICT: ...**`
/// or `### VERDICT: ...`, so the plain rule still matches.
fn normalize(line: &str) -> String {
    line.trim()
        .trim_matches(|c: char| c == '*' || c == '`' || c == '#' || c == '_' || c.is_whitespace())
        .to_string()
}

/// Scan from the bottom for the verdict line. Returns `None` if the Critic
/// forgot to include one.
pub fn find_verdict(critique: &str) -> Option<Verdict> {
    for line in critique.lines().rev() {
        match normalize(line).as_str() {
            "VERDICT: APPROVED" => return Some(Verdict::Approved),
            "VERDICT: NEEDS_WORK" => return Some(Verdict::NeedsWork),
            _ => continue,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// What the debate produced.
pub struct Outcome {
    /// Phase 4 (`spec.rs`) turns this into SPEC.md.
    #[allow(dead_code)]
    pub transcript: Transcript,
    pub approved: bool,
    pub rounds_used: u32,
}

/// Run the debate until the Critic approves or `max_rounds` is reached.
///
/// Gate 1 from plan.md: this returns either way; it is the caller's job to warn
/// the user when `approved` is false.
pub async fn run(
    proposer: &GeminiClient,
    critic: &ClaudeClient,
    topic: &str,
    max_rounds: u32,
) -> Result<Outcome> {
    let mut transcript = Transcript::new(topic);
    let mut approved = false;
    let mut rounds_used = 0;

    for round in 1..=max_rounds {
        rounds_used = round;
        ui::header(&format!("Round {round} of {max_rounds}"));

        ui::system("waiting for the Proposer...");
        let proposal = proposer
            .send(Some(PROPOSER_SYSTEM), &transcript.for_proposer())
            .await?;
        ui::proposer(&proposal);
        transcript.push(Speaker::Proposer, proposal);

        ui::system("waiting for the Critic...");
        let critique = critic
            .send(Some(CRITIC_SYSTEM), &transcript.for_critic())
            .await?;
        ui::critic(&critique);
        transcript.push(Speaker::Critic, &critique);

        match find_verdict(&critique) {
            Some(Verdict::Approved) => {
                ui::success("VERDICT: APPROVED");
                approved = true;
                break;
            }
            Some(Verdict::NeedsWork) => {
                ui::system("verdict: NEEDS_WORK — sending the review back to the Proposer");
            }
            None => {
                // Treating a missing verdict as approval would end the debate on
                // a formatting slip, so we keep going instead.
                ui::warn("the Critic gave no verdict line — treating it as NEEDS_WORK");
            }
        }
    }

    if !approved {
        ui::warn(&format!(
            "stopped after {rounds_used} rounds without an APPROVED verdict — the state below is the latest, not an agreed design"
        ));
    }

    Ok(Outcome {
        transcript,
        approved,
        rounds_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Role;

    fn transcript_with(turns: &[(Speaker, &str)]) -> Transcript {
        let mut t = Transcript::new("credit applications");
        for (speaker, text) in turns {
            t.push(*speaker, *text);
        }
        t
    }

    // --- DP-2: verdict detection -------------------------------------------

    #[test]
    fn finds_a_plain_verdict() {
        assert_eq!(
            find_verdict("Looks fine.\n\nVERDICT: APPROVED"),
            Some(Verdict::Approved)
        );
        assert_eq!(
            find_verdict("Several problems.\nVERDICT: NEEDS_WORK"),
            Some(Verdict::NeedsWork)
        );
    }

    #[test]
    fn survives_text_after_the_verdict() {
        let critique = "Good enough.\n\nVERDICT: APPROVED\n\nHope this helps!";
        assert_eq!(find_verdict(critique), Some(Verdict::Approved));
    }

    #[test]
    fn survives_markdown_decoration() {
        assert_eq!(
            find_verdict("**VERDICT: APPROVED**"),
            Some(Verdict::Approved)
        );
        assert_eq!(
            find_verdict("### VERDICT: NEEDS_WORK"),
            Some(Verdict::NeedsWork)
        );
        assert_eq!(find_verdict("`VERDICT: APPROVED`"), Some(Verdict::Approved));
    }

    #[test]
    fn takes_the_last_verdict_when_the_word_appears_earlier() {
        let critique = "I will end with VERDICT: APPROVED if this is fixed.\n\
                        The schema is wrong.\n\
                        VERDICT: NEEDS_WORK";
        assert_eq!(find_verdict(critique), Some(Verdict::NeedsWork));
    }

    #[test]
    fn reports_a_missing_verdict() {
        assert_eq!(find_verdict("I like it, ship it."), None);
        assert_eq!(find_verdict(""), None);
    }

    // --- DP-1: transcript views --------------------------------------------

    #[test]
    fn proposer_view_starts_with_the_topic() {
        let t = Transcript::new("credit applications");
        let view = t.for_proposer();

        assert_eq!(view.len(), 1);
        assert_eq!(view[0].role, Role::User);
        assert!(view[0].content.contains("credit applications"));
        assert!(view[0].content.contains(PROPOSER_TASK));
    }

    #[test]
    fn proposer_view_marks_its_own_turns_as_assistant() {
        let t = transcript_with(&[
            (Speaker::Proposer, "proposal one"),
            (Speaker::Critic, "not good enough"),
        ]);
        let view = t.for_proposer();

        let roles: Vec<Role> = view.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::User]);
        assert_eq!(view[1].content, "proposal one");
        assert!(view[2].content.contains("not good enough"));
    }

    #[test]
    fn critic_view_flips_the_roles() {
        let t = transcript_with(&[
            (Speaker::Proposer, "proposal one"),
            (Speaker::Critic, "not good enough"),
            (Speaker::Proposer, "proposal two"),
        ]);
        let view = t.for_critic();

        let roles: Vec<Role> = view.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::User]);
        assert!(view[0].content.contains("credit applications"));
        assert!(view[0].content.contains("proposal one"));
        assert_eq!(view[1].content, "not good enough");
        assert!(view[2].content.starts_with("Revised proposal:"));
    }

    /// Both APIs require the conversation to start with a user message and
    /// alternate. A drift here would only show up as a 400 at runtime.
    #[test]
    fn both_views_alternate_starting_with_user() {
        let t = transcript_with(&[
            (Speaker::Proposer, "p1"),
            (Speaker::Critic, "c1"),
            (Speaker::Proposer, "p2"),
            (Speaker::Critic, "c2"),
        ]);

        for view in [t.for_proposer(), t.for_critic()] {
            assert_eq!(view[0].role, Role::User);
            for pair in view.windows(2) {
                assert_ne!(pair[0].role, pair[1].role, "roles must alternate");
            }
        }
    }
}
