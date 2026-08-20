//! multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.
//!
//! See plan.md for the full pipeline. Implemented so far: config, choosing the
//! target repo for this run, and the Proposer/Critic debate (Gate 1).

mod api;
mod config;
mod debate;
mod target;
mod ui;

use anyhow::Result;

use crate::api::claude::ClaudeClient;
use crate::api::gemini::GeminiClient;
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

    let proposer = GeminiClient::new(&config)?;
    let critic = ClaudeClient::new(&config)?;

    let outcome = debate::run(&proposer, &critic, &topic, config.max_rounds).await?;

    ui::header("Debate finished");
    ui::system(&format!(
        "{} rounds, {}",
        outcome.rounds_used,
        if outcome.approved {
            "approved"
        } else {
            "no approval"
        }
    ));
    ui::system(&format!(
        "spec will be written to {}",
        target_repo.join("SPEC.md").display()
    ));

    Ok(())
}
