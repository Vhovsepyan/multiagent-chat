//! Deterministic milestone planning for an approved task specification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::git::MilestoneCommit;
use crate::verification::VerificationCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Cancelled,
}

impl MilestoneStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub order: u32,
    pub title: String,
    pub objective: String,
    pub verification_instructions: Vec<String>,
    pub status: MilestoneStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub worker_result_summary: Option<String>,
    /// Set when this milestone was committed (task 0009). Absent means no
    /// commit was requested, or none was needed.
    pub commit: Option<MilestoneCommit>,
    /// The critic's disposition of the implemented milestone (task 0011).
    /// Absent until the implementation review has produced a result.
    pub review: Option<crate::review::MilestoneReview>,
}

/// Build a non-empty, ordered plan from the approved specification. The
/// approved document is authoritative; an absent or empty `## Steps` section
/// is rejected instead of silently becoming one giant implementation step.
pub fn plan_from_spec(
    specification: &str,
    verification: &[VerificationCommand],
) -> Result<Vec<Milestone>, String> {
    let mut in_steps = false;
    let mut titles = Vec::new();
    for line in specification.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_steps = trimmed.eq_ignore_ascii_case("## Steps");
            continue;
        }
        if !in_steps || trimmed.is_empty() {
            continue;
        }
        let title = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| {
                let dot = trimmed.find('.')?;
                trimmed[..dot]
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .map(|_| &trimmed[dot + 1..])
            })
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .ok_or_else(|| format!("invalid milestone entry in ## Steps: {trimmed:?}"))?;
        titles.push(title.to_string());
    }
    if titles.is_empty() {
        return Err("approved specification must contain a non-empty ## Steps section".into());
    }

    let checks = verification.iter().map(display_command).collect::<Vec<_>>();
    let checks = if checks.is_empty() {
        vec!["No automatic verification commands detected".into()]
    } else {
        checks
    };
    Ok(titles
        .into_iter()
        .enumerate()
        .map(|(index, title)| Milestone {
            id: format!("m{}", index + 1),
            order: u32::try_from(index + 1).expect("milestone count fits u32"),
            objective: title.clone(),
            title,
            verification_instructions: checks.clone(),
            status: MilestoneStatus::Pending,
            started_at: None,
            completed_at: None,
            worker_result_summary: None,
            commit: None,
            review: None,
        })
        .collect())
}

pub fn display_command(command: &VerificationCommand) -> String {
    std::iter::once(command.program.as_str())
        .chain(command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_numbered_steps_in_order() {
        let plan = plan_from_spec(
            "# Spec\n\n## Steps\n1. Add the model\n2. Add tests\n\n## Notes\nignore",
            &[],
        )
        .unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].id, "m1");
        assert_eq!(plan[1].title, "Add tests");
    }

    #[test]
    fn rejects_missing_steps_instead_of_using_a_fallback() {
        assert!(plan_from_spec("## Design\n- one thing", &[]).is_err());
    }

    #[test]
    fn rejects_malformed_steps() {
        assert!(plan_from_spec("## Steps\nnot a list item", &[]).is_err());
    }
}
