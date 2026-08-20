//! multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.
//!
//! See plan.md for the full pipeline. Implemented so far: config, choosing the
//! target repo, the Proposer/Critic debate (Gate 1), SPEC.md, and the human
//! approval gate (Gate 2). Phase 5 adds the implementer.

mod api;
mod approve;
mod cli;
mod config;
mod debate;
mod implementer;
mod spec;
mod target;
mod ui;

use anyhow::Result;

use crate::api::claude::ClaudeClient;
use crate::api::gemini::GeminiClient;
use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    // --help / --version print and exit before anything else happens.
    let Some(args) = cli::parse()? else {
        return Ok(());
    };

    ui::header(concat!("multiagent-chat v", env!("CARGO_PKG_VERSION")));

    let config = Config::load()?;
    ui::system(&format!(
        "proposer {} | critic {} | max {} rounds",
        config.gemini_model, config.critic_model, config.max_rounds
    ));
    ui::system(&format!("workspace: {}", config.workspace_root.display()));
    println!();

    let topic = match args.topic {
        Some(topic) => {
            ui::system(&format!("Topic: {topic}"));
            topic
        }
        None => ui::prompt("Topic", "")?,
    };
    let target_repo = target::resolve(&config, &topic)?;

    let proposer = GeminiClient::new(&config)?;
    let critic = ClaudeClient::new(&config)?;

    // Gate 1: the debate runs until APPROVED or max rounds.
    let outcome = debate::run(&proposer, &critic, &topic, config.max_rounds).await?;

    ui::system(&format!(
        "debate finished after {} round(s)",
        outcome.rounds_used
    ));

    // The spec is built from the transcript either way; `approved` only changes
    // how loudly we warn about it.
    let document = spec::build(&proposer, &critic, &outcome.transcript).await?;
    let spec_path = spec::write_to(&target_repo, &document)?;

    // Gate 2: nothing touches the repo unless a human says yes.
    if !approve::ask(&document, &spec_path, outcome.approved)? {
        ui::system("stopped. SPEC.md is on disk if you want to edit it and re-run.");
        return Ok(());
    }

    ui::success("approved.");

    // Phase 5: hand it to Claude Code inside the target repo.
    implementer::run(&config, &target_repo).await?;

    Ok(())
}
