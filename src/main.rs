//! multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.
//!
//! See plan.md for the full pipeline. Implemented so far: config loading and
//! choosing the target repo for this run.

mod api;
mod config;
mod target;
mod ui;

use anyhow::Result;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    ui::header("multiagent-chat v0.1.0");

    let config = Config::load()?;
    ui::system(&format!(
        "proposer {} | critic {} | max {} rounds",
        config.gemini_model, config.critic_model, config.max_rounds
    ));
    ui::system(&format!("workspace: {}", config.workspace_root.display()));
    println!();

    let topic = ui::prompt("Topic", "")?;
    let target_repo = target::resolve(&config, &topic)?;

    println!();
    ui::success("ready.");
    ui::system(&format!("topic : {topic}"));
    ui::system(&format!(
        "spec  : {}",
        target_repo.join("SPEC.md").display()
    ));

    Ok(())
}
