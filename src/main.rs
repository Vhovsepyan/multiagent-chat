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
mod task;
mod ui;
mod web;

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

    let config = Config::load()?;

    // DP-12: the terminal pipeline stays the default until Phase 10 has a page
    // worth serving; --web opts in.
    if args.web {
        return web::serve(config).await;
    }

    ui::header(concat!("multiagent-chat v", env!("CARGO_PKG_VERSION")));
    if args.implement_only {
        ui::system(&format!("implementer {}", config.implementer_model));
    } else {
        ui::system(&format!(
            "proposer {} | critic {} | max {} rounds",
            config.gemini_model, config.critic_model, config.max_rounds
        ));
    }
    ui::system(&format!("workspace: {}", config.workspace_root.display()));
    println!();

    // The CLI has no web watchers, so every stage gets an emitter wired to
    // nothing (DP-9). The terminal output is unchanged from v1.
    let emitter = task::Emitter::detached();

    // Both routes have to end up with a repo, the spec text, and how much we
    // trust it, so that Gate 2 below is identical either way.
    let (target_repo, document, approved, reason) = if args.implement_only {
        let repo = target::resolve_existing(&config, args.topic.as_deref())?;
        let document = spec::read_from(&repo)?;
        ui::system("using the SPEC.md already in this project — no debate this run");
        (repo, document, true, None)
    } else {
        let topic = match args.topic {
            Some(topic) => {
                ui::system(&format!("Topic: {topic}"));
                topic
            }
            None => ui::prompt("Topic", "")?,
        };
        let repo = target::resolve(&config, &topic)?;

        let proposer = GeminiClient::new(&config)?;
        let critic = ClaudeClient::new(&config)?;

        // Gate 1: the debate runs until APPROVED or max rounds.
        let outcome = debate::run(&proposer, &critic, &topic, config.max_rounds, &emitter).await?;
        ui::system(&format!(
            "debate finished after {} round(s)",
            outcome.rounds_used
        ));

        // The spec is built from the transcript either way; `approved` only
        // changes how loudly we warn about it.
        let document = spec::build(
            &proposer,
            &critic,
            &outcome.transcript,
            outcome.approved,
            &emitter,
        )
        .await?;
        spec::write_to(&repo, &document)?;

        (repo, document, outcome.approved, outcome.last_reason)
    };

    let spec_path = target_repo.join(spec::SPEC_FILENAME);

    // Gate 2: nothing touches the repo unless a human says yes.
    if !approve::ask(&document, &spec_path, approved, reason.as_deref())? {
        ui::system("stopped. SPEC.md is on disk if you want to edit it and re-run.");
        return Ok(());
    }

    ui::success("approved.");

    // Phase 5: hand it to Claude Code inside the target repo.
    implementer::run(&config, &target_repo, &emitter).await?;

    Ok(())
}
