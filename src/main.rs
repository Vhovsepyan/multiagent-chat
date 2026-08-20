//! multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.
//!
//! See plan.md for the full pipeline. Right now this is Phase 0: it only proves
//! that the config loads and the terminal colors work.

mod config;
mod ui;

use anyhow::Result;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    ui::header("multiagent-chat v0.1.0");

    let config = Config::load()?;

    ui::system(&format!("proposer model : {}", config.gemini_model));
    ui::system(&format!("critic model   : {}", config.critic_model));
    ui::system(&format!("max rounds     : {}", config.max_rounds));
    ui::system(&format!(
        "target repo    : {}",
        config.target_repo_path.display()
    ));

    ui::success("\nconfig loaded — ready for Phase 1.");
    Ok(())
}
