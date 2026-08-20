//! Gate 2: show the finished spec and wait for a human yes or no.
//!
//! This is the last point before the app starts changing files in the target
//! repo, so the default is "no": anything that is not an explicit yes stops the
//! run.

use std::path::Path;

use anyhow::Result;

use crate::ui;

/// Print the spec and ask whether to implement it.
///
/// Returns `false` when the user declines — the caller should stop, not
/// continue with a warning.
pub fn ask(spec: &str, spec_path: &Path, approved_by_critic: bool) -> Result<bool> {
    ui::header("SPEC.md");
    println!("{}", spec.trim());

    println!();
    ui::system(&format!("written to {}", spec_path.display()));

    if !approved_by_critic {
        ui::warn(
            "the debate ended WITHOUT an APPROVED verdict — this spec is the latest state, not an agreed design",
        );
    }

    ui::confirm("Implement this spec with Claude Code?")
}
