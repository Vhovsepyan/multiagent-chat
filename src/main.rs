//! multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.
//!
//! See plan.md for the full pipeline. Implemented so far: config, choosing the
//! target repo, the Proposer/Critic debate (Gate 1), SPEC.md, and the human
//! approval gate (Gate 2). Phase 5 adds the implementer.

mod acceptance;
mod agent;
mod api;
mod approve;
mod cli;
mod config;
mod debate;
mod evidence;
mod execution_limits;
mod git;
mod implementer;
mod inspection;
mod milestone;
mod persistence;
mod process_environment;
#[cfg(windows)]
mod process_job;
mod process_runner;
mod project;
mod repository_file;
mod review;
mod review_baseline;
#[cfg(test)]
mod safety_tests;
mod spec;
mod submission;
mod target;
mod task;
mod technology;
mod ui;
mod verification;
mod web;
mod workflow;
mod workspace;

use anyhow::Result;

use crate::agent::{AgentCatalogue, CodingTaskRequest};
use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    // --help / --version print and exit before anything else happens.
    let Some(args) = cli::parse()? else {
        return Ok(());
    };

    let config = Config::load()?;

    // DP-12: since Phase 10 the browser UI is the default. --cli, --topic and
    // --implement-only all opt back into the terminal pipeline.
    if !args.wants_cli() {
        return web::serve(config).await;
    }

    // Task 0005: the CLI has no selection UI, so it runs the configured
    // defaults — resolved once here and then used for the whole run.
    //
    // The worker is needed on every path; the chat roles only when there is a
    // debate, so `--implement-only` still runs on an installation that
    // configures no chat provider at all.
    let catalogue = AgentCatalogue::from_config(&config);
    let worker_selection = catalogue.default_worker().map_err(anyhow::Error::msg)?;
    let worker = agent::coding_agent(&worker_selection, &config)?;
    let chat_selection = if args.implement_only {
        None
    } else {
        Some(catalogue.default_chat_pair().map_err(anyhow::Error::msg)?)
    };

    ui::header(concat!("multiagent-chat v", env!("CARGO_PKG_VERSION")));
    match &chat_selection {
        None => ui::system(&format!("implementer {}", worker_selection.model)),
        Some((proposer, critic)) => ui::system(&format!(
            "proposer {} {} | critic {} {} | max {} rounds",
            proposer.provider, proposer.model, critic.provider, critic.model, config.max_rounds
        )),
    }
    if let Some(root) = &config.workspace_root {
        ui::system(&format!("legacy CLI workspace: {}", root.display()));
    }
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

        let (proposer_selection, critic_selection) =
            chat_selection.expect("a debate run resolved its chat agents");
        let proposer = agent::chat_agent(&proposer_selection, &config)?;
        let critic = agent::chat_agent(&critic_selection, &config)?;

        // Gate 1: the debate runs until APPROVED or max rounds.
        let outcome = debate::run(
            proposer.as_ref(),
            critic.as_ref(),
            &topic,
            config.max_rounds,
            &emitter,
        )
        .await?;
        ui::system(&format!(
            "debate finished after {} round(s)",
            outcome.rounds_used
        ));

        // The spec is built from the transcript either way; `approved` only
        // changes how loudly we warn about it.
        let document = spec::build(
            proposer.as_ref(),
            critic.as_ref(),
            &outcome.transcript,
            outcome.approved,
            &emitter,
        )
        .await?;

        (repo, document, outcome.approved, outcome.last_reason)
    };

    // Both generated and legacy imported specifications are snapshotted outside
    // the project. The project-owned SPEC.md is only ever read.
    let spec_path = spec::write_cli_artifact(&document)?;

    // Gate 2: nothing touches the repo unless a human says yes.
    if !approve::ask(&document, &spec_path, approved, reason.as_deref())? {
        ui::system("stopped. The external specification artifact remains at the path shown above.");
        return Ok(());
    }

    ui::success("approved.");

    // Phase 5: hand it to the Worker agent inside the target repo.
    worker
        .execute(
            CodingTaskRequest {
                workspace: &target_repo,
                spec_path: &spec_path,
                instructions: workflow::CLI_INSTRUCTIONS,
            },
            &emitter,
        )
        .await?;

    Ok(())
}
