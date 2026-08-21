//! Runs the v1 pipeline in the background on behalf of a web task.
//!
//! Phase 8 reports coarse progress: the status transitions, the spec, and the
//! outcome. The turn-by-turn `Proposal` / `Critique` / `Build` events arrive in
//! Phase 9, when `debate.rs`, `spec.rs` and `implementer.rs` are handed the
//! `Emitter` (DP-9). Until then those stages still print to the terminal, which
//! is harmless — it just means the server console shows the debate.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::api::claude::ClaudeClient;
use crate::api::gemini::GeminiClient;
use crate::spec;
use crate::task::{Emitter, TaskEvent, TaskId, TaskStatus};
use crate::web::AppState;

/// Start the pipeline for `id` and return immediately.
pub fn spawn(state: AppState, id: TaskId, repo: PathBuf) {
    tokio::spawn(async move {
        let emitter = state.manager.emitter(id);

        // One failure path for every stage: whatever went wrong, the task ends
        // Failed with the message attached, instead of hanging forever in
        // whatever state it happened to be in.
        if let Err(error) = run(&state, id, &repo, &emitter).await {
            emitter.emit(TaskEvent::Finished {
                status: TaskStatus::Failed,
                error: Some(format!("{error:#}")),
            });
        }
    });
}

async fn run(state: &AppState, id: TaskId, repo: &Path, emitter: &Emitter) -> Result<()> {
    let config = &state.config;

    let task = match state.manager.get(id) {
        Some(task) => task,
        // Only possible if the task was removed while starting up.
        None => return Ok(()),
    };
    let topic = task.topic();

    let proposer = GeminiClient::new(config)?;
    let critic = ClaudeClient::new(config)?;

    // --- Gate 1: the debate ------------------------------------------------
    emitter.status(TaskStatus::Debating);
    let outcome = crate::debate::run(&proposer, &critic, &topic, config.max_rounds).await?;

    if !outcome.approved {
        emitter.warn("the debate ended without an APPROVED verdict");
        if let Some(reason) = &outcome.last_reason {
            emitter.warn(format!("still unresolved: {reason}"));
        }
    }

    // --- The spec ----------------------------------------------------------
    emitter.status(TaskStatus::GeneratingSpec);
    let document = spec::build(&proposer, &critic, &outcome.transcript, outcome.approved).await?;
    let path = spec::write_to(repo, &document)?;
    emitter.emit(TaskEvent::Spec {
        markdown: document,
        path: path.display().to_string(),
    });

    // --- Gate 2: park until a human answers (DP-11) ------------------------
    emitter.status(TaskStatus::WaitingForApproval);
    let Some(decision) = state.manager.await_decision(id).await else {
        // The task disappeared; nothing left to report it to.
        return Ok(());
    };

    if !decision.approve {
        emitter.notice("rejected — SPEC.md is on disk if you want to edit it and re-run");
        emitter.emit(TaskEvent::Finished {
            status: TaskStatus::Rejected,
            error: None,
        });
        return Ok(());
    }

    // An edited spec replaces the generated one before the build (DP-10).
    if let Some(edited) = decision.spec {
        spec::write_to(repo, &edited)?;
        emitter.notice("using your edited SPEC.md");
    }

    // --- The build ---------------------------------------------------------
    emitter.status(TaskStatus::Implementing);
    crate::implementer::run(config, repo).await?;

    emitter.emit(TaskEvent::Finished {
        status: TaskStatus::Completed,
        error: None,
    });
    Ok(())
}
