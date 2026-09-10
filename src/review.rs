//! Post-implementation critic review and its structured result (task 0011).
//!
//! The debate critic reviews a DESIGN. This module is the same critic reviewing
//! what was actually built: the approved specification and the milestone scope
//! on one side, the real diff and verification results on the other.
//!
//! The result has to be machine-readable, because the pipeline branches on it.
//! Prose alone is rejected: `parse` accepts exactly one JSON object, and a
//! `FIX_REQUIRED` without concrete findings is an invalid review rather than an
//! excuse to keep going.

use serde::{Deserialize, Serialize};

use crate::execution_limits::bounded_text;
use crate::milestone::Milestone;
use crate::task::TaskKind;
use crate::verification::VerificationResult;

/// How much of the implementation diff the critic is shown. The full diff is
/// capped at the Git output budget, which is far more than a prompt should
/// carry, so it is bounded again here and the cut is marked explicitly.
pub const REVIEW_DIFF_BYTES: usize = 32 * 1024;

/// How much of one verification command's output travels with the review.
pub const REVIEW_OUTPUT_BYTES: usize = 4 * 1024;

/// Per-field bound on what the critic sends back, so one malformed reply cannot
/// grow task memory or the evidence export without limit.
pub const FINDING_TEXT_BYTES: usize = 2 * 1024;

/// More findings than this is not a review the worker can act on.
pub const MAX_FINDINGS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Pass,
    FixRequired,
}

impl ReviewStatus {
    /// The wire spelling from the task specification, used in prompts and UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::FixRequired => "FIX_REQUIRED",
        }
    }

    pub fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Blocker,
    Major,
    Minor,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Blocker => "blocker",
            Self::Major => "major",
            Self::Minor => "minor",
        }
    }

    /// Models spell severities in their own words. An unrecognized one becomes
    /// `Major` rather than failing a review that is otherwise well formed: the
    /// finding still has to be fixed, only its label was unusual.
    fn from_label(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "blocker" | "critical" | "high" | "severe" => Self::Blocker,
            "minor" | "low" | "nit" | "trivial" | "suggestion" => Self::Minor,
            _ => Self::Major,
        }
    }
}

/// One concrete defect the critic wants corrected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Which requirement, milestone objective or verification it comes from.
    pub requirement: String,
    pub severity: Severity,
    /// What in the implementation shows the problem.
    pub evidence: String,
    /// What the worker must change.
    pub correction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub status: ReviewStatus,
    pub findings: Vec<Finding>,
}

impl Review {
    /// The findings as one instruction block for the worker.
    pub fn findings_text(&self) -> String {
        self.findings
            .iter()
            .enumerate()
            .map(|(index, finding)| {
                format!(
                    "{}. [{}] Requirement: {}\n   Evidence: {}\n   Required correction: {}",
                    index + 1,
                    finding.severity.label(),
                    finding.requirement,
                    finding.evidence,
                    finding.correction
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What one milestone review carried, retained on the milestone itself so the
/// UI and the evidence export read the same disposition (task 0011).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneReview {
    pub status: ReviewStatus,
    /// Fix iterations actually run before this result.
    pub iterations_used: u32,
    /// The configured maximum for this run.
    pub max_iterations: u32,
    /// Outstanding findings from the most recent review.
    pub findings: Vec<Finding>,
}

impl MilestoneReview {
    /// One line for a UI badge or a report row.
    pub fn summary(&self) -> String {
        format!(
            "{} after {} of {} fix iteration(s)",
            self.status.label(),
            self.iterations_used,
            self.max_iterations
        )
    }
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

pub const REVIEW_SYSTEM: &str = "\
You are the Critic reviewing an IMPLEMENTATION, not a design. You are given the \
approved specification, the scope of one milestone, the code that was actually \
written, and the verification results. Decide whether this milestone was really \
implemented as approved.

Reply with ONE JSON object and nothing else — no prose before or after it, no \
code fence:

{\"status\": \"PASS\" | \"FIX_REQUIRED\", \"findings\": [{\"requirement\": \"...\", \
\"severity\": \"blocker\" | \"major\" | \"minor\", \"evidence\": \"...\", \
\"correction\": \"...\"}]}

Rules:
- Answer PASS with an empty findings list when the milestone objective is met \
and nothing concrete is wrong.
- Answer FIX_REQUIRED only for concrete, evidenced defects in THIS milestone's \
scope. Every finding must name the requirement it comes from, quote or point at \
the evidence in the diff or verification output, and state the correction.
- FIX_REQUIRED with no findings is invalid. If you cannot point at a defect, the \
answer is PASS.
- Do not request work belonging to a later milestone, speculative hardening, \
refactors, or style preferences.
- Do not rewrite the code yourself.";

/// Everything the critic is shown about one implemented milestone.
pub struct ReviewRequest<'a> {
    pub kind: TaskKind,
    pub milestone: &'a Milestone,
    pub total: usize,
    pub approved_spec: &'a str,
    /// The change the run has produced so far, already bounded.
    pub diff: &'a str,
    pub verification: &'a [VerificationResult],
    /// What the worker reported, which is the known-limitations input.
    pub worker_summary: &'a str,
    /// 0 for the first review, then the number of fixes already applied.
    pub iteration: u32,
    pub max_iterations: u32,
}

impl ReviewRequest<'_> {
    /// The single user message the critic answers.
    pub fn message(&self) -> String {
        let verification = if self.verification.is_empty() {
            "No automatic verification commands were available.".to_string()
        } else {
            self.verification
                .iter()
                .map(|result| {
                    format!(
                        "- `{}` — {}\n{}",
                        result.command,
                        if result.success { "passed" } else { "FAILED" },
                        bounded_text(&result.output, REVIEW_OUTPUT_BYTES)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "Task workflow: {kind}\n\
             Milestone under review: {order} of {total} ({id})\n\
             Title: {title}\n\
             Objective: {objective}\n\
             Verification planned for this milestone: {checks}\n\
             Review round: {round} (up to {max} fix iteration(s) are available)\n\n\
             Approved specification:\n{spec}\n\n\
             Worker report and known limitations:\n{summary}\n\n\
             Verification results:\n{verification}\n\n\
             Implementation produced so far:\n{diff}",
            kind = self.kind.label(),
            order = self.milestone.order,
            total = self.total,
            id = self.milestone.id,
            title = self.milestone.title,
            objective = self.milestone.objective,
            checks = self.milestone.verification_instructions.join("; "),
            round = self.iteration + 1,
            max = self.max_iterations,
            spec = self.approved_spec,
            summary = self.worker_summary,
            diff = self.diff,
        )
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WireReview {
    status: String,
    #[serde(default)]
    findings: Vec<WireFinding>,
}

#[derive(Deserialize)]
struct WireFinding {
    #[serde(default, alias = "requirement_reference", alias = "reference")]
    requirement: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    evidence: String,
    #[serde(
        default,
        alias = "requested_correction",
        alias = "fix",
        alias = "correction_requested"
    )]
    correction: String,
}

/// Read the critic's structured answer, or say why it is not one.
///
/// Vague prose is not a result: without a parseable object carrying a known
/// status, this fails, and the caller treats the review as failed rather than
/// guessing what the critic meant.
pub fn parse(reply: &str) -> Result<Review, String> {
    let object = extract_object(reply)
        .ok_or_else(|| "the review reply contained no JSON object".to_string())?;
    let wire: WireReview = serde_json::from_str(object)
        .map_err(|error| format!("the review reply was not valid review JSON: {error}"))?;
    let status = match normalize_status(&wire.status).as_str() {
        "PASS" => ReviewStatus::Pass,
        "FIX_REQUIRED" => ReviewStatus::FixRequired,
        other => {
            return Err(format!(
                "the review reply carried an unknown status {other:?}; expected PASS or FIX_REQUIRED"
            ));
        }
    };
    let findings = wire
        .findings
        .into_iter()
        .filter(|finding| {
            // A finding the worker cannot act on is not a finding.
            !finding.requirement.trim().is_empty() || !finding.correction.trim().is_empty()
        })
        .take(MAX_FINDINGS)
        .map(|finding| Finding {
            requirement: bound(&finding.requirement, "unspecified requirement"),
            severity: Severity::from_label(&finding.severity),
            evidence: bound(&finding.evidence, "no evidence recorded"),
            correction: bound(&finding.correction, "no correction recorded"),
        })
        .collect::<Vec<_>>();
    if status == ReviewStatus::FixRequired && findings.is_empty() {
        return Err("the review reported FIX_REQUIRED without any actionable finding".to_string());
    }
    Ok(Review { status, findings })
}

fn bound(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return fallback.to_string();
    }
    bounded_text(value, FINDING_TEXT_BYTES)
}

fn normalize_status(status: &str) -> String {
    status
        .trim()
        .trim_matches(|character: char| {
            character == '*' || character == '`' || character == '"' || character == '.'
        })
        .replace([' ', '-'], "_")
        .to_ascii_uppercase()
}

/// The outermost JSON object in the reply.
///
/// Models wrap answers in ```json fences or add a closing sentence despite
/// being told not to, so the object is located rather than assumed to be the
/// whole reply. Anything outside it is ignored, never treated as a result.
fn extract_object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (start < end).then(|| &reply[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milestone::MilestoneStatus;

    fn milestone() -> Milestone {
        Milestone {
            id: "m2".into(),
            order: 2,
            title: "Persistence layer".into(),
            objective: "Store invoices durably".into(),
            verification_instructions: vec!["cargo test".into()],
            status: MilestoneStatus::Running,
            started_at: None,
            completed_at: None,
            worker_result_summary: None,
            commit: None,
            review: None,
        }
    }

    #[test]
    fn a_pass_review_needs_no_findings() {
        let review = parse(r#"{"status": "PASS", "findings": []}"#).unwrap();
        assert_eq!(review.status, ReviewStatus::Pass);
        assert!(review.findings.is_empty());
        assert!(review.status.is_pass());
    }

    #[test]
    fn findings_are_read_in_full_with_their_severity() {
        let review = parse(
            r#"Here is my review:
```json
{"status":"fix_required","findings":[
  {"requirement":"Spec step 2","severity":"Blocker","evidence":"no repository module","correction":"add the repository"},
  {"requirement":"Milestone objective","severity":"nit","evidence":"typo","correction":"rename it"}
]}
```
That is all."#,
        )
        .unwrap();

        assert_eq!(review.status, ReviewStatus::FixRequired);
        assert_eq!(review.findings.len(), 2);
        assert_eq!(review.findings[0].severity, Severity::Blocker);
        assert_eq!(review.findings[0].requirement, "Spec step 2");
        assert_eq!(review.findings[1].severity, Severity::Minor);
        let text = review.findings_text();
        assert!(
            text.contains("[blocker] Requirement: Spec step 2"),
            "{text}"
        );
        assert!(
            text.contains("Required correction: add the repository"),
            "{text}"
        );
    }

    /// Requirement: prose alone is never a machine-readable result.
    #[test]
    fn vague_prose_is_not_a_review() {
        for reply in [
            "Looks good to me overall, ship it.",
            "VERDICT: APPROVED",
            "",
            "{ not json at all }",
            r#"{"status": "MAYBE", "findings": []}"#,
        ] {
            assert!(parse(reply).is_err(), "must be rejected: {reply:?}");
        }
    }

    /// A demand for fixes that names nothing to fix cannot drive a fix loop.
    #[test]
    fn fix_required_without_findings_is_rejected() {
        let error = parse(r#"{"status":"FIX_REQUIRED","findings":[]}"#).unwrap_err();
        assert!(error.contains("without any actionable finding"), "{error}");
        let error =
            parse(r#"{"status":"FIX_REQUIRED","findings":[{"evidence":"  "}]}"#).unwrap_err();
        assert!(error.contains("without any actionable finding"), "{error}");
    }

    #[test]
    fn oversized_and_missing_fields_stay_bounded_and_explicit() {
        let long = "x".repeat(FINDING_TEXT_BYTES * 2);
        let review = parse(&format!(
            r#"{{"status":"FIX_REQUIRED","findings":[{{"requirement":"{long}","correction":"fix it"}}]}}"#
        ))
        .unwrap();
        let finding = &review.findings[0];
        assert!(finding.requirement.len() <= FINDING_TEXT_BYTES);
        assert_eq!(finding.evidence, "no evidence recorded");
        assert_eq!(finding.severity, Severity::Major, "unknown severity");
    }

    #[test]
    fn the_review_request_carries_scope_evidence_and_verification() {
        let milestone = milestone();
        let verification = [VerificationResult {
            command: "cargo test".into(),
            success: false,
            output: "test failed".into(),
        }];
        let message = ReviewRequest {
            kind: TaskKind::Feature,
            milestone: &milestone,
            total: 4,
            approved_spec: "## Steps\n1. Store invoices",
            diff: "+fn store() {}",
            verification: &verification,
            worker_summary: "Worker completed; storage is in memory only.",
            iteration: 1,
            max_iterations: 2,
        }
        .message();

        assert!(
            message.contains("Milestone under review: 2 of 4 (m2)"),
            "{message}"
        );
        assert!(message.contains("Store invoices durably"), "{message}");
        assert!(message.contains("## Steps"), "{message}");
        assert!(message.contains("+fn store() {}"), "{message}");
        assert!(message.contains("`cargo test` — FAILED"), "{message}");
        assert!(message.contains("storage is in memory only"), "{message}");
        assert!(message.contains("Review round: 2"), "{message}");
    }

    #[test]
    fn a_review_summary_reports_the_iterations_it_used() {
        let review = MilestoneReview {
            status: ReviewStatus::Pass,
            iterations_used: 1,
            max_iterations: 2,
            findings: Vec::new(),
        };
        assert_eq!(review.summary(), "PASS after 1 of 2 fix iteration(s)");
    }
}
