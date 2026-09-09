//! Task-kind-specific agent instructions, separate from common behavior.

use crate::milestone::Milestone;
use crate::task::TaskKind;
use crate::technology::ProjectProfile;

const COMMON: &str = "Preserve unrelated behavior, follow repository instructions, avoid secrets, and produce a concrete buildable proposal.";

/// The terminal pipeline has no task kind, so it keeps the instruction v1 used.
pub const CLI_INSTRUCTIONS: &str = "Do not commit or push.";

pub fn design_context(kind: TaskKind, profile: &ProjectProfile, inspection: &str) -> String {
    format!(
        "Task workflow: {}\nTechnology profile: {:?}\n\n{}\n\nRepository context:\n{}",
        kind_instruction(kind),
        profile,
        COMMON,
        inspection
    )
}

/// Instructions shared by every worker run, whatever its scope.
fn worker_conventions(profile: &ProjectProfile) -> String {
    format!(
        "Follow repository instructions and existing conventions. Use the {:?} project profile to choose repository-defined build and test commands. Do not commit, push, or modify unrelated behavior.",
        profile.stack
    )
}

/// The authoritative instruction for ONE milestone (task 0008).
///
/// The approved specification travels with the request as background context;
/// this text alone decides what the worker changes now. It therefore names only
/// the current milestone and never asks for the specification's remaining
/// steps — a second, broader instruction would contradict it.
pub fn milestone_prompt(
    kind: TaskKind,
    profile: &ProjectProfile,
    milestone: &Milestone,
    total: usize,
    repository_context: &str,
) -> String {
    format!(
        "Implement only milestone {order} of {total} ({id}) of the approved {kind} workflow.\n\
         Title: {title}\n\
         Objective: {objective}\n\
         Verification for this milestone: {checks}\n\n\
         The approved specification artifact is background context for the whole task; \
         this milestone instruction is authoritative for what to change in this run. \
         Implement nothing from a later milestone: do not start, scaffold, or stub work \
         that belongs to one, and do not continue into the specification's remaining steps.\n\n\
         {conventions} Summarize what this milestone changed and any limitation.\n\n\
         Repository context:\n{repository_context}",
        order = milestone.order,
        id = milestone.id,
        kind = kind.label(),
        title = milestone.title,
        objective = milestone.objective,
        checks = milestone.verification_instructions.join("; "),
        conventions = worker_conventions(profile),
    )
}

fn kind_instruction(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::NewProject => {
            "Design a new application from the stated requirements and selected stack. Do not assume Rust or add unrelated infrastructure."
        }
        TaskKind::Feature => {
            "Inspect the existing architecture and task-relevant code. Propose the smallest compatible feature change and regression coverage; do not recreate the application."
        }
        TaskKind::BugFix => {
            "Understand or reproduce the failure, identify root cause and blast radius, then propose the smallest correct fix with regression coverage."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::technology::TechStack;

    fn milestone(order: u32, title: &str) -> Milestone {
        Milestone {
            id: format!("m{order}"),
            order,
            title: title.into(),
            objective: format!("{title} objective"),
            verification_instructions: vec!["cargo test".into()],
            status: crate::milestone::MilestoneStatus::Pending,
            started_at: None,
            completed_at: None,
            worker_result_summary: None,
        }
    }

    /// Task 0008: the instruction a worker receives for a milestone must scope
    /// the run to that milestone — never to the whole Steps list.
    #[test]
    fn a_milestone_prompt_scopes_the_run_to_one_milestone() {
        let profile = ProjectProfile::selected(TechStack::Rust);
        let second = milestone(2, "Persistence layer");

        let prompt = milestone_prompt(TaskKind::NewProject, &profile, &second, 5, "src/");

        assert!(
            prompt.contains("Implement only milestone 2 of 5 (m2)"),
            "{prompt}"
        );
        assert!(prompt.contains("Persistence layer"));
        assert!(prompt.contains("Implement nothing from a later milestone"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("src/"));
    }

    /// The full text handed to the coding agent — common prompt plus milestone
    /// instruction — must not tell it to work through the specification steps.
    #[test]
    fn the_generated_milestone_worker_prompt_never_orders_the_whole_steps_section() {
        let profile = ProjectProfile::selected(TechStack::Rust);
        let first = milestone(1, "Project bootstrap");
        let instructions = milestone_prompt(TaskKind::NewProject, &profile, &first, 4, "empty");

        let sent = crate::implementer::prompt(
            std::path::Path::new("/tmp/task/artifacts/approved-spec.md"),
            &instructions,
        );

        for forbidden in [
            "Work through the Steps",
            "Steps section",
            "implement the specification in full",
        ] {
            assert!(
                !sent.contains(forbidden),
                "worker prompt still says {forbidden:?}: {sent}"
            );
        }
        // Milestone 1 is named; nothing invites work on milestones 2..4.
        assert!(sent.contains("Implement only milestone 1 of 4 (m1)"));
        assert!(sent.contains("do not continue into the specification's remaining steps"));
        assert!(sent.contains("background context"));
    }

    #[test]
    fn task_kinds_receive_meaningfully_different_instructions() {
        let profile = ProjectProfile::selected(TechStack::Rust);
        let new = design_context(TaskKind::NewProject, &profile, "none");
        let feature = design_context(TaskKind::Feature, &profile, "src/");
        let bug = design_context(TaskKind::BugFix, &profile, "tests/");
        assert!(new.contains("new application"));
        assert!(feature.contains("existing architecture"));
        assert!(bug.contains("root cause"));
        assert_ne!(new, feature);
        assert_ne!(feature, bug);
    }
}
