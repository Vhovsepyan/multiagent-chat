//! Acceptance criteria derived from the approved specification (task 0012).
//!
//! A milestone plan says what the run will DO. Acceptance criteria say what the
//! run must SATISFY, and they are tracked independently: a criterion becomes
//! `passed` only when verification actually supports it and no critic finding is
//! still open against it. The worker saying it implemented something is never
//! enough.
//!
//! Generation is deterministic, so the same approved specification always yields
//! the same ids in the same order for the lifetime of a run.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::execution_limits::bounded_text;
use crate::milestone::Milestone;

/// One criterion description is bounded like every other stored model text.
pub const CRITERION_TEXT_BYTES: usize = 2 * 1024;

/// Evidence is a CONCISE reference to what verified a criterion — never a copy
/// of the verification log, which already lives in the task result.
pub const EVIDENCE_TEXT_BYTES: usize = 512;

/// A specification that yields more criteria than this is not trackable.
pub const MAX_CRITERIA: usize = 200;

/// The section an approved specification may use to state criteria directly.
const CRITERIA_HEADING: &str = "acceptance criteria";

/// The section every approved specification has, used when it states none.
const STEPS_HEADING: &str = "steps";

/// The independent source that supports a criterion's state. Keeping this
/// explicit prevents a review result from being mistaken for an automatic test,
/// and vice versa, in the task UI and exported evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionEvidenceKind {
    AutomaticVerification,
    ImplementationReview,
}

impl CriterionEvidenceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::AutomaticVerification => "Automatic verification",
            Self::ImplementationReview => "Implementation review",
        }
    }
}

/// A concise, typed reference to evidence. Its text is bounded; detailed
/// command output remains only in the task result/evidence export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionEvidence {
    pub kind: CriterionEvidenceKind,
    pub summary: String,
}

impl CriterionEvidence {
    pub fn automatic_verification(summary: &str) -> Self {
        Self {
            kind: CriterionEvidenceKind::AutomaticVerification,
            summary: evidence_line(summary),
        }
    }

    pub fn implementation_review(summary: &str) -> Self {
        Self {
            kind: CriterionEvidenceKind::ImplementationReview,
            summary: evidence_line(summary),
        }
    }

    pub fn display(&self) -> String {
        format!("{}: {}", self.kind.label(), self.summary)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    /// Generated, not yet worked on.
    Pending,
    /// The milestone responsible for it reported completion — not proof.
    Implemented,
    /// Supported by actual verification, with no open finding against it.
    Passed,
    /// Verification failed, or a critic finding is open against it.
    Failed,
    /// No milestone in this run is responsible for it.
    Deferred,
}

impl CriterionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Implemented => "IMPLEMENTED",
            Self::Passed => "PASSED",
            Self::Failed => "FAILED",
            Self::Deferred => "DEFERRED",
        }
    }

    /// Whether this status still needs work before the task is complete.
    pub fn is_outstanding(self) -> bool {
        !matches!(self, Self::Passed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// Stable for the lifetime of the run, e.g. `AC-003`.
    pub id: String,
    pub description: String,
    pub status: CriterionStatus,
    /// The milestones responsible for satisfying it. Empty means no milestone
    /// covers it, which is why such a criterion is `Deferred` rather than lost.
    pub milestones: Vec<String>,
    /// Concise references to what supports its current state.
    pub evidence: Vec<CriterionEvidence>,
    /// Critic findings that must be resolved before it can pass (task 0011).
    pub blocking_findings: Vec<String>,
}

impl AcceptanceCriterion {
    /// Whether a passing verification may move this criterion to `Passed`.
    pub fn may_pass(&self) -> bool {
        self.blocking_findings.is_empty() && !self.milestones.is_empty()
    }
}

/// Build the criteria for one run and tell each milestone which it owns.
///
/// An explicit `## Acceptance criteria` section wins, because it is what the
/// specification actually promised. Otherwise the `## Steps` list — the same
/// authoritative list the milestone plan comes from — is used, which keeps
/// every step traceable even when the specification states no criteria.
pub fn plan(
    specification: &str,
    milestones: &mut [Milestone],
) -> Result<Vec<AcceptanceCriterion>, String> {
    let (items, from_steps) = match section_items(specification, CRITERIA_HEADING) {
        items if !items.is_empty() => (items, false),
        _ => (section_items(specification, STEPS_HEADING), true),
    };
    if items.is_empty() {
        return Err(
            "approved specification states no acceptance criteria and no ## Steps to derive them from"
                .into(),
        );
    }
    if items.len() > MAX_CRITERIA {
        return Err(format!(
            "approved specification yields more than {MAX_CRITERIA} acceptance criteria"
        ));
    }

    let mut criteria = Vec::with_capacity(items.len());
    for (index, description) in items.into_iter().enumerate() {
        // Derived from the steps: criterion N is exactly milestone N, which is
        // the same list in the same order. Stated separately: matched on the
        // words they share, and left unmapped when nothing matches well.
        let owner = if from_steps {
            milestones.get(index).map(|milestone| milestone.id.clone())
        } else {
            best_milestone(&description, milestones)
        };
        let status = match &owner {
            Some(_) => CriterionStatus::Pending,
            None => CriterionStatus::Deferred,
        };
        criteria.push(AcceptanceCriterion {
            id: format!("AC-{:03}", index + 1),
            description: bounded_text(&description, CRITERION_TEXT_BYTES),
            status,
            milestones: owner.into_iter().collect(),
            evidence: Vec::new(),
            blocking_findings: Vec::new(),
        });
    }

    for milestone in milestones.iter_mut() {
        milestone.criteria = criteria
            .iter()
            .filter(|criterion| criterion.milestones.iter().any(|id| id == &milestone.id))
            .map(|criterion| criterion.id.clone())
            .collect();
    }
    Ok(criteria)
}

/// The criteria one milestone is responsible for.
pub fn owned_by<'a>(
    criteria: &'a [AcceptanceCriterion],
    milestone_id: &str,
) -> Vec<&'a AcceptanceCriterion> {
    criteria
        .iter()
        .filter(|criterion| criterion.milestones.iter().any(|id| id == milestone_id))
        .collect()
}

/// Criterion ids mentioned in free text, such as a critic finding.
///
/// Accepts `AC-4`, `AC-004` and `ac-004`, and reports them in the canonical
/// `AC-004` spelling so a finding can be matched to what it is about.
pub fn referenced_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found: Vec<String> = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(offset) = lower[from..].find("ac-") {
        let start = from + offset + "ac-".len();
        let digits = bytes[start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        from = start + digits.max(1);
        if digits == 0 || digits > 3 {
            continue;
        }
        let number: u32 = text[start..start + digits].parse().unwrap_or(0);
        if number == 0 {
            continue;
        }
        let id = format!("AC-{number:03}");
        if !found.contains(&id) {
            found.push(id);
        }
    }
    found
}

/// A concise, bounded evidence line. Verification logs stay in the task result.
pub fn evidence_line(text: &str) -> String {
    bounded_text(text.trim(), EVIDENCE_TEXT_BYTES)
}

/// List items under a `## <heading>` section, ignoring prose lines.
fn section_items(specification: &str, heading: &str) -> Vec<String> {
    let mut inside = false;
    let mut items = Vec::new();
    for line in specification.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("## ") {
            inside = title.trim().eq_ignore_ascii_case(heading);
            continue;
        }
        if !inside || trimmed.is_empty() {
            continue;
        }
        if let Some(item) = list_item(trimmed) {
            items.push(item.to_string());
        }
    }
    items
}

/// `- item`, `* item` or `3. item`; anything else is prose, not a criterion.
fn list_item(line: &str) -> Option<&str> {
    let item = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| {
            let dot = line.find('.')?;
            line[..dot].trim().parse::<u32>().ok()?;
            Some(&line[dot + 1..])
        })?
        .trim();
    (!item.is_empty()).then_some(item)
}

/// Words worth matching on: short and structural words match everything.
fn significant_words(text: &str) -> BTreeSet<String> {
    const NOISE: [&str; 16] = [
        "the", "and", "for", "with", "that", "this", "must", "should", "when", "then", "from",
        "into", "each", "every", "which", "their",
    ];
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.len() >= 4)
        .map(str::to_ascii_lowercase)
        .filter(|word| !NOISE.contains(&word.as_str()))
        .collect()
}

/// The milestone a stated criterion most plausibly belongs to.
///
/// Deliberately conservative: without a clear overlap the criterion stays
/// unmapped and visible, rather than being attached to the wrong milestone and
/// passing on that milestone's verification.
fn best_milestone(description: &str, milestones: &[Milestone]) -> Option<String> {
    const MINIMUM_OVERLAP: usize = 2;
    let wanted = significant_words(description);
    milestones
        .iter()
        .map(|milestone| {
            let words = significant_words(&format!("{} {}", milestone.title, milestone.objective));
            (wanted.intersection(&words).count(), milestone)
        })
        .filter(|(score, _)| *score >= MINIMUM_OVERLAP)
        // Ties go to the earliest milestone, so mapping is order-stable.
        .max_by_key(|(score, milestone)| (*score, std::cmp::Reverse(milestone.order)))
        .map(|(_, milestone)| milestone.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::milestone::plan_from_spec;

    const SPEC: &str = "## Problem\nEvents need managing.\n\n\
         ## Steps\n\
         1. Create events\n\
         2. Prevent duplicate events\n\
         3. Deliver reminders\n\n\
         ## Out of scope\nNothing.";

    fn milestones(specification: &str) -> Vec<Milestone> {
        plan_from_spec(specification, &[]).unwrap()
    }

    /// Required test 1 and 2: criteria come from the approved specification,
    /// with stable ids in document order.
    #[test]
    fn criteria_are_generated_from_the_specification_in_stable_order() {
        let mut first = milestones(SPEC);
        let criteria = plan(SPEC, &mut first).unwrap();

        assert_eq!(
            criteria
                .iter()
                .map(|criterion| criterion.id.as_str())
                .collect::<Vec<_>>(),
            ["AC-001", "AC-002", "AC-003"]
        );
        assert_eq!(criteria[1].description, "Prevent duplicate events");
        assert!(
            criteria
                .iter()
                .all(|criterion| criterion.status == CriterionStatus::Pending)
        );

        // The same document always produces the same plan.
        let mut second = milestones(SPEC);
        assert_eq!(plan(SPEC, &mut second).unwrap(), criteria);
    }

    /// Required test 3: milestones and criteria reference each other.
    #[test]
    fn steps_map_one_to_one_onto_their_milestones() {
        let mut plan_milestones = milestones(SPEC);
        let criteria = plan(SPEC, &mut plan_milestones).unwrap();

        assert_eq!(criteria[0].milestones, vec!["m1".to_string()]);
        assert_eq!(criteria[2].milestones, vec!["m3".to_string()]);
        assert_eq!(plan_milestones[0].criteria, vec!["AC-001".to_string()]);
        assert_eq!(plan_milestones[2].criteria, vec!["AC-003".to_string()]);
        assert_eq!(owned_by(&criteria, "m2")[0].id, "AC-002");
        assert!(owned_by(&criteria, "unknown").is_empty());
    }

    /// A stated criteria section wins over the steps, and what it states is
    /// matched to the milestone that actually covers it.
    #[test]
    fn stated_criteria_are_matched_to_the_milestone_that_covers_them() {
        let specification = "## Steps\n\
             1. Build the reminder scheduler\n\
             2. Build the duplicate registration guard\n\n\
             ## Acceptance criteria\n\
             - Duplicate registration is rejected with a clear error\n\
             - Reminder scheduler delivers on time\n";
        let mut plan_milestones = milestones(specification);
        let criteria = plan(specification, &mut plan_milestones).unwrap();

        assert_eq!(criteria.len(), 2, "the stated section wins over the steps");
        assert_eq!(criteria[0].id, "AC-001");
        assert_eq!(criteria[0].milestones, vec!["m2".to_string()]);
        assert_eq!(criteria[1].milestones, vec!["m1".to_string()]);
        assert_eq!(plan_milestones[0].criteria, vec!["AC-002".to_string()]);
    }

    /// Required test 7: a requirement no milestone covers is never dropped.
    #[test]
    fn unmapped_criteria_are_deferred_and_stay_visible() {
        let specification = "## Steps\n\
             1. Build the invoice importer\n\n\
             ## Acceptance criteria\n\
             - The invoice importer accepts CSV files\n\
             - Published dashboards refresh within five seconds\n";
        let mut plan_milestones = milestones(specification);
        let criteria = plan(specification, &mut plan_milestones).unwrap();

        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[0].status, CriterionStatus::Pending);
        assert_eq!(criteria[1].status, CriterionStatus::Deferred);
        assert!(criteria[1].milestones.is_empty());
        assert!(!criteria[1].may_pass(), "nothing can verify it");
        assert_eq!(plan_milestones[0].criteria, vec!["AC-001".to_string()]);
    }

    #[test]
    fn a_specification_without_criteria_or_steps_is_refused() {
        let error = plan("## Problem\nNothing listed.", &mut []).unwrap_err();
        assert!(error.contains("no acceptance criteria"), "{error}");
        let many = format!(
            "## Steps\n{}",
            (0..=MAX_CRITERIA)
                .map(|index| format!("{}. Step {index}\n", index + 1))
                .collect::<String>()
        );
        assert!(plan(&many, &mut []).unwrap_err().contains("more than"));
    }

    #[test]
    fn criterion_references_are_found_in_free_text() {
        assert_eq!(
            referenced_ids("AC-004 and ac-5 both regress; see AC-004 again"),
            ["AC-004".to_string(), "AC-005".to_string()]
        );
        assert!(referenced_ids("no criterion here").is_empty());
        assert!(referenced_ids("AC- and AC-0 and AC-1234").is_empty());
    }

    #[test]
    fn evidence_stays_short_enough_to_never_carry_a_log() {
        let line = evidence_line(&format!("  cargo test passed {}  ", "x".repeat(4096)));
        assert!(line.len() <= EVIDENCE_TEXT_BYTES);
        assert!(line.starts_with("cargo test passed"));
        assert!(line.ends_with(crate::execution_limits::TRUNCATED));
    }
}
