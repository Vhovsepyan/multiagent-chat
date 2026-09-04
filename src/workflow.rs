//! Task-kind-specific agent instructions, separate from common behavior.

use crate::task::TaskKind;
use crate::technology::ProjectProfile;

const COMMON: &str = "Preserve unrelated behavior, follow repository instructions, avoid secrets, and produce a concrete buildable proposal.";

pub fn design_context(kind: TaskKind, profile: &ProjectProfile, inspection: &str) -> String {
    format!(
        "Task workflow: {}\nTechnology profile: {:?}\n\n{}\n\nRepository context:\n{}",
        kind_instruction(kind),
        profile,
        COMMON,
        inspection
    )
}

pub fn implementation_prompt(kind: TaskKind, profile: &ProjectProfile) -> String {
    format!(
        "Read SPEC.md and implement the approved {} workflow. Follow repository instructions and existing conventions. Use the {:?} project profile to choose repository-defined build and test commands. Do not commit, push, or modify unrelated behavior. Summarize completed work and limitations.",
        kind.label(),
        profile.stack
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
