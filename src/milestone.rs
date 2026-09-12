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
    /// The acceptance criteria this milestone is responsible for (task 0012).
    #[serde(default)]
    pub criteria: Vec<String>,
}

/// Build a non-empty, ordered plan from the approved specification. The
/// approved document is authoritative; an absent or empty `## Steps` section
/// is rejected instead of silently becoming one giant implementation step.
pub fn plan_from_spec(
    specification: &str,
    verification: &[VerificationCommand],
) -> Result<Vec<Milestone>, String> {
    let mut in_steps = false;
    let mut fence: Option<char> = None;
    let mut titles = Vec::new();
    for line in specification.lines() {
        let trimmed_start = line.trim_start();
        if let Some(open) = fence {
            if fenced_delimiter(trimmed_start).is_some_and(|delimiter| delimiter == open) {
                fence = None;
            }
            continue;
        }

        if let Some(heading) = line.trim_start().strip_prefix("## ") {
            in_steps = heading.trim().eq_ignore_ascii_case("Steps");
            continue;
        }
        if !in_steps || line.trim().is_empty() {
            continue;
        }

        if let Some(delimiter) = fenced_delimiter(trimmed_start) {
            fence = Some(delimiter);
            continue;
        }

        // Markdown continuation text and nested lists are indented. Milestones
        // are deliberately limited to list entries at the section's left edge.
        if line.starts_with(char::is_whitespace) {
            continue;
        }

        let title = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| {
                let dot = line.find('.')?;
                line[..dot]
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .map(|_| &line[dot + 1..])
            })
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string);

        match title {
            Some(title) => titles.push(title),
            // A Steps section must begin with a real top-level list entry, but
            // later prose belongs to the preceding Markdown list item.
            None if titles.is_empty() => {
                return Err(format!("invalid milestone entry in ## Steps: {line:?}"));
            }
            None => continue,
        }
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
            criteria: Vec::new(),
        })
        .collect())
}

fn fenced_delimiter(line: &str) -> Option<char> {
    ["```", "~~~"].into_iter().find_map(|prefix| {
        line.starts_with(prefix)
            .then(|| prefix.chars().next().unwrap())
    })
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
    fn plans_bullet_steps_in_order() {
        let plan = plan_from_spec("## Steps\n- Add the model\n* Add tests", &[]).unwrap();
        assert_eq!(titles(&plan), ["Add the model", "Add tests"]);
    }

    #[test]
    fn rejects_missing_steps_instead_of_using_a_fallback() {
        assert!(plan_from_spec("## Design\n- one thing", &[]).is_err());
    }

    #[test]
    fn rejects_malformed_steps() {
        assert!(plan_from_spec("## Steps\nnot a list item", &[]).is_err());
    }

    #[test]
    fn ignores_fenced_commands_between_real_steps() {
        let plan = plan_from_spec(
            "## Steps\n1. Change Kafka keying\n```bash\ncargo test\n```\n2. Add regression coverage",
            &[],
        )
        .unwrap();
        assert_eq!(
            titles(&plan),
            ["Change Kafka keying", "Add regression coverage"]
        );
    }

    #[test]
    fn ignores_fenced_yaml_and_nested_content() {
        let plan = plan_from_spec(
            "## Steps\n1. Add CI workflow\n\n   ```yaml\n   jobs:\n     build:\n       steps:\n         - uses: actions/checkout@v4\n           with:\n             fetch-depth: 0\n   ```\n\n2. Add tests",
            &[],
        )
        .unwrap();
        assert_eq!(titles(&plan), ["Add CI workflow", "Add tests"]);
    }

    #[test]
    fn ignores_fenced_json_and_continues_after_the_block() {
        let plan = plan_from_spec(
            "## Steps\n- Add configuration\n  ~~~json\n  {\"workers\": [\"codex\"]}\n  ~~~\n- Add tests",
            &[],
        )
        .unwrap();
        assert_eq!(titles(&plan), ["Add configuration", "Add tests"]);
    }

    #[test]
    fn a_heading_inside_a_fenced_block_does_not_end_steps() {
        let plan = plan_from_spec(
            "## Steps\n1. Add parser\n```markdown\n## This is code\n```\n2. Add tests",
            &[],
        )
        .unwrap();
        assert_eq!(titles(&plan), ["Add parser", "Add tests"]);
    }

    #[test]
    fn ignores_indented_continuations_and_nested_lists() {
        let plan = plan_from_spec(
            "## Steps\n1. Implement parser\n   Explain the parsing rules.\n   - Accept numbered entries\n   - Ignore nested bullets\n2. Add tests",
            &[],
        )
        .unwrap();
        assert_eq!(titles(&plan), ["Implement parser", "Add tests"]);
    }

    #[test]
    fn ignores_explanatory_paragraphs_after_a_step() {
        let plan = plan_from_spec(
            "## Steps\n1. Implement parser\nThis explains why the parser must preserve existing behavior.\n2. Add tests",
            &[],
        )
        .unwrap();
        assert_eq!(titles(&plan), ["Implement parser", "Add tests"]);
    }

    #[test]
    fn next_level_two_section_ends_step_parsing() {
        let plan = plan_from_spec(
            "## Steps\n1. Implement parser\n## Notes\n- This is not a milestone",
            &[],
        )
        .unwrap();
        assert_eq!(titles(&plan), ["Implement parser"]);
    }

    #[test]
    fn rejects_a_steps_section_without_top_level_entries() {
        assert!(
            plan_from_spec(
                "## Steps\n  - Nested entry\n\n  Explanatory continuation",
                &[]
            )
            .is_err()
        );
    }

    fn titles(plan: &[Milestone]) -> Vec<&str> {
        plan.iter()
            .map(|milestone| milestone.title.as_str())
            .collect()
    }
}
