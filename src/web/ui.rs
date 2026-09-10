//! Server-rendered production UI and HTML SSE fragments.

use std::convert::Infallible;

use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};

use crate::agent::{
    AgentSelection, AgentSelectionRequest, ChatAgentRequest, ChatProvider, CodingAgentRequest,
    CodingTool,
};
use crate::milestone::MilestoneStatus;
use crate::project::{Project, ProjectSource};
use crate::task::{
    Decision, OutputTarget, PersistenceStatus, ProjectPersistence, RecordedEvent, Task, TaskEvent,
    TaskId, TaskKind, TaskRequest, TaskStatus,
};
use crate::technology::TechStack;
use crate::web::{AppState, pipeline};

fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn timeline_html(status: TaskStatus) -> String {
    let steps = [
        ("Debate", TaskStatus::Debating),
        ("Spec", TaskStatus::GeneratingSpec),
        ("Approval", TaskStatus::WaitingForApproval),
        ("Build", TaskStatus::Implementing),
    ];
    let rank = |value| match value {
        TaskStatus::Created => 0,
        TaskStatus::Debating => 1,
        TaskStatus::GeneratingSpec => 2,
        TaskStatus::WaitingForApproval => 3,
        TaskStatus::Implementing => 4,
        TaskStatus::Completed
        | TaskStatus::Rejected
        | TaskStatus::Failed
        | TaskStatus::Cancelled => 5,
    };
    let mut html = String::from(r#"<div class="timeline">"#);
    for (label, step) in steps {
        let class = if rank(step) < rank(status) {
            "step done"
        } else if step == status {
            "step active"
        } else {
            "step"
        };
        html.push_str(&format!(r#"<span class="{class}">{label}</span>"#));
    }
    let (class, label) = match status {
        TaskStatus::Completed => ("step done", "Completed"),
        TaskStatus::Rejected => ("step bad", "Rejected"),
        TaskStatus::Failed => ("step bad", "Failed"),
        TaskStatus::Cancelled => ("step bad", "Cancelled"),
        _ => ("step", "Done"),
    };
    html.push_str(&format!(r#"<span class="{class}">{label}</span></div>"#));
    html
}

/// Requirement 11: what this task actually runs, read from the task itself
/// rather than from the current global defaults.
fn agents_html(agents: &AgentSelection, git_mode: crate::git::GitMode) -> String {
    let row = |role: &str, who: &str, model: &str| {
        format!(
            r#"<div class="agent"><span class="role">{role}</span><span class="who">{}</span><code>{}</code></div>"#,
            esc(who),
            esc(model)
        )
    };
    let git_row = format!(
        r#"<div class="agent"><span class="role">Git</span><span class="who">{}</span></div>"#,
        esc(git_mode.label())
    );
    format!(
        r#"<div class="card"><h2 class="section">Agents</h2><div class="agents">{}{}{}{}</div></div>"#,
        row(
            "Proposer",
            agents.proposer.provider.label(),
            &agents.proposer.model
        ),
        row(
            "Critic",
            agents.critic.provider.label(),
            &agents.critic.model
        ),
        row("Worker", agents.worker.tool.label(), &agents.worker.model),
        // Task 0009: the run's Git behavior is part of what it actually did.
        git_row
    )
}

fn actions_html(id: TaskId) -> String {
    format!(
        r#"<div class="card task-actions"><h2 class="section">Task Actions</h2><a class="button-link" href="/api/tasks/{id}/evidence" download>Export Evidence</a><div class="hint">Downloads a redacted ZIP containing JSONL and human-readable run records.</div></div>"#
    )
}

/// The milestone card. Its inner `#milestones` region is replaced on every
/// milestone event (task 0008 follow-up), so the badges track task state live
/// instead of only accumulating text.
fn milestones_html(task: &Task) -> String {
    format!(
        r#"<div class="card"><h2 class="section">Milestones</h2><div id="milestones" hx-swap="innerHTML">{}</div></div>"#,
        milestone_list_html(&task.milestones)
    )
}

/// Rendered from current task state, never from one event in isolation.
fn milestone_list_html(milestones: &[crate::milestone::Milestone]) -> String {
    if milestones.is_empty() {
        return r#"<div class="hint">The ordered milestone plan will appear after approval.</div>"#
            .into();
    }
    let rows = milestones
        .iter()
        .map(|milestone| {
            let class = match milestone.status {
                MilestoneStatus::Passed => "ok",
                MilestoneStatus::Failed | MilestoneStatus::Cancelled => "err",
                MilestoneStatus::Running => "active",
                MilestoneStatus::Pending => "pending",
            };
            let commit = milestone
                .commit
                .as_ref()
                .map(|commit| {
                    format!(
                        r#"<div class="hint">Commit: <code>{}</code> · {}</div>"#,
                        esc(&commit.short_sha),
                        esc(&commit.message)
                    )
                })
                .unwrap_or_default();
            // Task 0011: what the critic made of the implemented milestone.
            let review = milestone
                .review
                .as_ref()
                .map(|review| {
                    let class = if review.status.is_pass() { "ok" } else { "err" };
                    let findings = review
                        .findings
                        .iter()
                        .map(|finding| {
                            format!(
                                "<li>[{}] {} — {}</li>",
                                esc(finding.severity.label()),
                                esc(&finding.requirement),
                                esc(&finding.correction)
                            )
                        })
                        .collect::<String>();
                    format!(
                        r#"<div class="hint">Implementation review: <span class="milestone-status {class}">{}</span> · fix iteration {}/{}</div>{}"#,
                        esc(review.status.label()),
                        review.iterations_used,
                        review.max_iterations,
                        if findings.is_empty() {
                            String::new()
                        } else {
                            format!("<ul class=\"hint\">{findings}</ul>")
                        }
                    )
                })
                .unwrap_or_default();
            format!(
                r#"<li><span class="milestone-status {class}">{}</span> <strong>{}. {}</strong><div class="hint">{}</div>{commit}{review}</li>"#,
                milestone.status.label(),
                milestone.order,
                esc(&milestone.title),
                esc(&milestone.objective)
            )
        })
        .collect::<String>();
    format!(r#"<ol class="milestones">{rows}</ol>"#)
}

/// The output card. Shown only for tasks that have an output target of their
/// own — Feature and Bug Fix inherit the registered project's, so the option
/// does not apply to them (task 0010).
fn output_html(task: &Task) -> String {
    format!(
        r#"<div class="card"><h2 class="section">Output</h2><div id="output-summary" hx-swap="innerHTML">{}</div></div>"#,
        output_state_html(task.output, task.persistence.as_ref())
    )
}

/// Rendered from current task state, so a reload and a live update agree.
fn output_state_html(
    output: Option<OutputTarget>,
    persistence: Option<&ProjectPersistence>,
) -> String {
    let Some(output) = output else {
        return String::new();
    };
    let mut html = format!(
        r#"<div class="agent"><span class="role">Mode</span><span class="who">{}</span></div>"#,
        esc(output.label())
    );
    match persistence {
        None if output.is_persistent() => html.push_str(
            r#"<div class="hint">The finished project is kept after the run; nothing has been written yet.</div>"#,
        ),
        None => html.push_str(
            r#"<div class="hint">The temporary workspace is removed after the run; review the result below.</div>"#,
        ),
        Some(persistence) => {
            let (class, detail) = match persistence.status {
                PersistenceStatus::Persisted => (
                    "ok",
                    format!(
                        "Kept at <code>{}</code>{}",
                        esc(&persistence.destination),
                        match (&persistence.git, &persistence.git_warning) {
                            (Some(git), _) =>
                                format!(" · Git history preserved: {} commit(s)", git.commits),
                            // The project was published; only reporting failed.
                            (None, Some(warning)) =>
                                format!(" · repository details unavailable: {}", esc(warning)),
                            (None, None) => String::new(),
                        }
                    ),
                ),
                PersistenceStatus::Failed => (
                    "err",
                    format!(
                        "Not kept at <code>{}</code> · {}",
                        esc(&persistence.destination),
                        esc(persistence.error.as_deref().unwrap_or("no detail recorded"))
                    ),
                ),
                PersistenceStatus::Started => (
                    "active",
                    format!("Writing to <code>{}</code>…", esc(&persistence.destination)),
                ),
            };
            html.push_str(&format!(
                r#"<div class="agent"><span class="milestone-status {class}">{}</span><span class="who">{detail}</span></div>"#,
                persistence.status.label()
            ));
        }
    }
    html
}

fn gate_html(id: TaskId, spec: &str) -> String {
    format!(
        r#"<div class="card"><h2 class="section">Specification — your call</h2>
<form hx-post="/ui/tasks/{id}/approve" hx-swap="none">
<textarea class="spec-edit" name="spec">{}</textarea>
<div class="hint">Edit freely — the approved text is what gets built.</div>
<button type="submit" name="approve" value="true">Approve &amp; Build</button>
<button type="submit" name="approve" value="false" class="danger">Reject</button>
</form></div>"#,
        esc(spec)
    )
}

fn event_html(
    id: TaskId,
    recorded: &RecordedEvent,
    agents: &AgentSelection,
) -> Option<(&'static str, String)> {
    let result = match &recorded.event {
        TaskEvent::TaskCreated { kind } => Some((
            "debate",
            format!(
                r#"<div class="notice">Task created · {}</div>"#,
                kind.label()
            ),
        )),
        TaskEvent::TaskStarted => {
            Some(("debate", r#"<div class="notice">Task started</div>"#.into()))
        }
        TaskEvent::Status { status } => Some(("status", timeline_html(*status))),
        TaskEvent::RoundStarted { round, of } => Some((
            "debate",
            format!(r#"<h2 class="section">Round {round} of {of}</h2>"#),
        )),
        TaskEvent::ProposerStarted {
            stage,
            round,
            provider,
            model,
        }
        | TaskEvent::ProposerCompleted {
            stage,
            round,
            provider,
            model,
        } => Some((
            "debate",
            format!(
                r#"<div class="notice">Proposer {} · {:?}{} · {} / {}</div>"#,
                if matches!(&recorded.event, TaskEvent::ProposerStarted { .. }) {
                    "started"
                } else {
                    "completed"
                },
                stage,
                round
                    .map(|round| format!(" round {round}"))
                    .unwrap_or_default(),
                esc(provider.label()),
                esc(model)
            ),
        )),
        TaskEvent::ProposerFailed {
            stage,
            round,
            provider,
            model,
            error,
        } => Some((
            "debate",
            format!(
                r#"<div class="notice err">Proposer failed · {:?}{} · {} / {} · {}</div>"#,
                stage,
                round
                    .map(|round| format!(" round {round}"))
                    .unwrap_or_default(),
                esc(provider.label()),
                esc(model),
                esc(error)
            ),
        )),
        TaskEvent::CriticStarted {
            stage,
            round,
            provider,
            model,
        }
        | TaskEvent::CriticCompleted {
            stage,
            round,
            provider,
            model,
        } => Some((
            "debate",
            format!(
                r#"<div class="notice">Critic {} · {:?}{} · {} / {}</div>"#,
                if matches!(&recorded.event, TaskEvent::CriticStarted { .. }) {
                    "started"
                } else {
                    "completed"
                },
                stage,
                round
                    .map(|round| format!(" round {round}"))
                    .unwrap_or_default(),
                esc(provider.label()),
                esc(model)
            ),
        )),
        TaskEvent::CriticFailed {
            stage,
            round,
            provider,
            model,
            error,
        } => Some((
            "debate",
            format!(
                r#"<div class="notice err">Critic failed · {:?}{} · {} / {} · {}</div>"#,
                stage,
                round
                    .map(|round| format!(" round {round}"))
                    .unwrap_or_default(),
                esc(provider.label()),
                esc(model),
                esc(error)
            ),
        )),
        TaskEvent::Proposal { text, .. } => Some((
            "debate",
            format!(
                r#"<div class="turn proposer"><h3>Proposer · {}</h3><pre>{}</pre></div>"#,
                esc(agents.proposer.provider.label()),
                esc(text)
            ),
        )),
        TaskEvent::Critique {
            text,
            verdict,
            reason,
            ..
        } => {
            let badge = verdict
                .as_deref()
                .map(|value| {
                    let label = if value == "approved" {
                        "VERDICT: APPROVED"
                    } else {
                        "VERDICT: NEEDS_WORK"
                    };
                    format!(
                        r#"<div class="verdict {value}">{label} {}</div>"#,
                        esc(reason.as_deref().unwrap_or(""))
                    )
                })
                .unwrap_or_default();
            Some((
                "debate",
                format!(
                    r#"<div class="turn critic"><h3>Critic · {}</h3><pre>{}</pre>{badge}</div>"#,
                    esc(agents.critic.provider.label()),
                    esc(text)
                ),
            ))
        }
        TaskEvent::Spec { markdown, .. } => Some(("spec", gate_html(id, markdown))),
        TaskEvent::SpecApproved { markdown } => Some((
            "spec",
            format!(
                r#"<div class="card"><h2 class="section">Specification</h2><div class="spec-body">{}</div></div>"#,
                esc(markdown)
            ),
        )),
        TaskEvent::SpecGenerated => Some((
            "debate",
            r#"<div class="notice">Specification generated</div>"#.into(),
        )),
        TaskEvent::SpecUpdated => Some((
            "debate",
            r#"<div class="notice">Specification updated at approval</div>"#.into(),
        )),
        TaskEvent::SpecRejected => Some((
            "debate",
            r#"<div class="notice warn">Specification rejected</div>"#.into(),
        )),
        TaskEvent::AgentsSelected { agents } => Some((
            "debate",
            format!(
                r#"<div class="notice">Agents · Proposer {} · Critic {} · Worker {}</div>"#,
                esc(&format!(
                    "{} {}",
                    agents.proposer.provider.label(),
                    agents.proposer.model
                )),
                esc(&format!(
                    "{} {}",
                    agents.critic.provider.label(),
                    agents.critic.model
                )),
                esc(&format!(
                    "{} {}",
                    agents.worker.tool.label(),
                    agents.worker.model
                ))
            ),
        )),
        TaskEvent::Inspection {
            profile,
            source_revision,
        } => Some((
            "debate",
            format!(
                r#"<div class="notice">Detected <strong>{}</strong>{}</div>"#,
                esc(&format!("{:?}", profile.stack)),
                source_revision
                    .as_deref()
                    .map(|revision| format!(" at <code>{}</code>", esc(revision)))
                    .unwrap_or_default()
            ),
        )),
        TaskEvent::Verification { result } => Some((
            "build",
            format!(
                r#"<div>{}: {}</div>"#,
                esc(&result.command),
                if result.success { "passed" } else { "failed" }
            ),
        )),
        TaskEvent::VerificationStarted { commands } => Some((
            "build",
            format!("<div>Verification started · {commands} command(s)</div>"),
        )),
        TaskEvent::VerificationCompleted { commands } => Some((
            "build",
            format!("<div>Verification completed · {commands} command(s)</div>"),
        )),
        TaskEvent::VerificationFailed { command, error } => Some((
            "build",
            format!(
                r#"<div class="notice err">Verification failed{} · {}</div>"#,
                command
                    .as_deref()
                    .map(|command| format!(" · {}", esc(command)))
                    .unwrap_or_default(),
                esc(error)
            ),
        )),
        TaskEvent::WorkerStarted { tool, model } | TaskEvent::WorkerCompleted { tool, model } => {
            Some((
                "build",
                format!(
                    "<div>Worker {} · {} / {}</div>",
                    if matches!(&recorded.event, TaskEvent::WorkerStarted { .. }) {
                        "started"
                    } else {
                        "completed"
                    },
                    esc(tool.label()),
                    esc(model)
                ),
            ))
        }
        TaskEvent::WorkerFailed { tool, model, error } => Some((
            "build",
            format!(
                r#"<div class="notice err">Worker failed · {} / {} · {}</div>"#,
                esc(tool.label()),
                esc(model),
                esc(error)
            ),
        )),
        TaskEvent::WorkerCancelled { tool, model } => Some((
            "build",
            format!(
                r#"<div class="notice warn">Worker cancelled · {} / {}</div>"#,
                esc(tool.label()),
                esc(model)
            ),
        )),
        TaskEvent::MilestonePlanCreated { milestones } => Some((
            "build",
            format!(
                r#"<div class="notice ok">Milestone plan created · {} milestones</div>"#,
                milestones.len()
            ),
        )),
        TaskEvent::MilestoneStarted { order, title, .. } => Some((
            "build",
            format!(
                r#"<div class="notice">Milestone {order} started · {}</div>"#,
                esc(title)
            ),
        )),
        TaskEvent::MilestoneCompleted { order, title, .. } => Some((
            "build",
            format!(
                r#"<div class="notice ok">Milestone {order} passed · {}</div>"#,
                esc(title)
            ),
        )),
        TaskEvent::MilestoneFailed {
            order,
            title,
            error,
            ..
        } => Some((
            "build",
            format!(
                r#"<div class="notice err">Milestone {order} failed · {} · {}</div>"#,
                esc(title),
                esc(error)
            ),
        )),
        TaskEvent::MilestoneCancelled {
            order,
            title,
            reason,
            ..
        } => Some((
            "build",
            format!(
                r#"<div class="notice warn">Milestone {order} cancelled · {} · {}</div>"#,
                esc(title),
                esc(reason)
            ),
        )),
        TaskEvent::MilestoneCommitCreated {
            order,
            title,
            commit,
            ..
        } => Some((
            "build",
            format!(
                r#"<div class="notice ok">Milestone {order} committed · {} · <code>{}</code></div>"#,
                esc(title),
                esc(&commit.short_sha)
            ),
        )),
        TaskEvent::ImplementationReviewStarted {
            order,
            iteration,
            of,
            ..
        } => Some((
            "build",
            format!(
                r#"<div class="notice">Implementation review started · milestone {order} · round {} of up to {}</div>"#,
                iteration + 1,
                of + 1
            ),
        )),
        TaskEvent::ImplementationReviewCompleted {
            order,
            iteration,
            of,
            status,
            findings,
            ..
        } => {
            let class = if status.is_pass() { "ok" } else { "warn" };
            let detail = findings
                .iter()
                .map(|finding| {
                    format!(
                        r#"<li>[{}] {} — {}</li>"#,
                        esc(finding.severity.label()),
                        esc(&finding.requirement),
                        esc(&finding.correction)
                    )
                })
                .collect::<String>();
            let list = if detail.is_empty() {
                String::new()
            } else {
                format!("<ul>{detail}</ul>")
            };
            Some((
                "build",
                format!(
                    r#"<div class="notice {class}">Implementation review: {} · milestone {order} · fix iteration {iteration}/{of}</div>{list}"#,
                    esc(status.label())
                ),
            ))
        }
        TaskEvent::ImplementationReviewFailed { order, error, .. } => Some((
            "build",
            format!(
                r#"<div class="notice err">Implementation review failed · milestone {order} · {}</div>"#,
                esc(error)
            ),
        )),
        TaskEvent::FixStarted {
            order,
            iteration,
            of,
            tool,
            model,
            ..
        } => Some((
            "build",
            format!(
                r#"<div class="notice">Fix iteration {iteration}/{of} started · milestone {order} · {} / {}</div>"#,
                esc(tool.label()),
                esc(model)
            ),
        )),
        TaskEvent::FixCompleted {
            order,
            iteration,
            of,
            ..
        } => Some((
            "build",
            format!(
                r#"<div class="notice ok">Fix iteration {iteration}/{of} completed · milestone {order}</div>"#
            ),
        )),
        TaskEvent::FixFailed {
            order,
            iteration,
            of,
            error,
            ..
        } => Some((
            "build",
            format!(
                r#"<div class="notice err">Fix iteration {iteration}/{of} failed · milestone {order} · {}</div>"#,
                esc(error)
            ),
        )),
        TaskEvent::ProjectPersistenceStarted { destination } => Some((
            "build",
            format!(
                r#"<div class="notice">Keeping the project at <code>{}</code></div>"#,
                esc(destination)
            ),
        )),
        TaskEvent::ProjectPersisted {
            destination,
            git,
            git_warning,
        } => Some((
            "build",
            format!(
                r#"<div class="notice ok">Project kept at <code>{}</code>{}</div>"#,
                esc(destination),
                match (git, git_warning) {
                    (Some(git), _) => format!(" · {} commit(s) preserved", git.commits),
                    (None, Some(_)) => " · repository details unavailable".to_string(),
                    (None, None) => String::new(),
                }
            ),
        )),
        TaskEvent::ProjectPersistenceFailed { destination, error } => Some((
            "build",
            format!(
                r#"<div class="notice err">Could not keep the project at <code>{}</code> · {}</div>"#,
                esc(destination),
                esc(error)
            ),
        )),
        TaskEvent::Result { result } => {
            Some(("build", format!("<pre>{}</pre>", esc(&result.diff))))
        }
        TaskEvent::Build { chunk } => Some(("build", format!("<div>{}</div>", esc(chunk)))),
        TaskEvent::Notice { message } => Some((
            "debate",
            format!(r#"<div class="notice">{}</div>"#, esc(message)),
        )),
        TaskEvent::Warning { message } => Some((
            "debate",
            format!(r#"<div class="notice warn">{}</div>"#, esc(message)),
        )),
        TaskEvent::Finished { status, error } => {
            let (class, text) = match status {
                TaskStatus::Completed => (
                    "ok",
                    "Completed — implementation and verification finished.".into(),
                ),
                TaskStatus::Rejected => (
                    "warn",
                    "Rejected. No repository changes were published.".into(),
                ),
                TaskStatus::Cancelled => (
                    "warn",
                    "Cancelled. Completed milestones were preserved.".into(),
                ),
                _ => (
                    "bad",
                    format!("Failed: {}", esc(error.as_deref().unwrap_or("unknown"))),
                ),
            };
            Some((
                "done",
                format!(r#"<div class="done-banner {class}">{text}</div>"#),
            ))
        }
        TaskEvent::TaskCompleted
        | TaskEvent::TaskFailed { .. }
        | TaskEvent::TaskCancelled
        | TaskEvent::EvidenceExported { .. } => None,
    };
    result.map(|(slot, html)| (slot, format!("{}{}", timestamp_html(recorded), html)))
}

fn timestamp_html(recorded: &RecordedEvent) -> String {
    let machine = recorded
        .timestamp
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let readable = recorded.timestamp.format("%Y-%m-%d %H:%M:%S%.3f UTC");
    format!(
        r#"<time class="event-time" datetime="{}" data-sequence="{}">{}</time>"#,
        esc(&machine),
        recorded.sequence,
        readable
    )
}

/// The task state a live update is rendered against.
///
/// Live updates are rendered from CURRENT task state rather than from one event
/// in isolation, so a reconnecting browser and a live one show the same thing.
struct RenderState<'a> {
    spec: Option<&'a str>,
    agents: &'a AgentSelection,
    milestones: &'a [crate::milestone::Milestone],
    output: Option<OutputTarget>,
    persistence: Option<&'a ProjectPersistence>,
}

impl<'a> RenderState<'a> {
    fn of(task: &'a Task) -> Self {
        Self {
            spec: task.spec.as_deref(),
            agents: &task.agents,
            milestones: &task.milestones,
            output: task.output,
            persistence: task.persistence.as_ref(),
        }
    }
}

/// A single domain event may affect several independent live UI regions.
///
/// `milestones` is the task's CURRENT plan state, so a milestone event refreshes
/// the visible pending/running/passed/failed/cancelled badges instead of only
/// appending a line of text (task 0008 follow-up); persistence events refresh
/// the output card the same way (task 0010). The extra updates travel as
/// out-of-band swaps on the same SSE message, so ordering is unchanged.
fn event_updates(
    id: TaskId,
    recorded: &RecordedEvent,
    state: &RenderState<'_>,
) -> Vec<(&'static str, String)> {
    let Some((name, html)) = event_html(id, recorded, state.agents) else {
        return Vec::new();
    };
    let output = || output_state_html(state.output, state.persistence);
    if let TaskEvent::Finished { status, .. } = &recorded.event {
        return vec![
            ("status", timeline_html(*status)),
            (name, html),
            ("spec", spec_readonly_html(state.spec)),
            ("milestones", milestone_list_html(state.milestones)),
            ("output-summary", output()),
        ];
    }
    if matches!(
        recorded.event,
        TaskEvent::ProjectPersistenceStarted { .. }
            | TaskEvent::ProjectPersisted { .. }
            | TaskEvent::ProjectPersistenceFailed { .. }
    ) {
        return vec![(name, html), ("output-summary", output())];
    }
    if matches!(
        recorded.event,
        TaskEvent::MilestonePlanCreated { .. }
            | TaskEvent::MilestoneStarted { .. }
            | TaskEvent::MilestoneCompleted { .. }
            | TaskEvent::MilestoneFailed { .. }
            | TaskEvent::MilestoneCancelled { .. }
            | TaskEvent::MilestoneCommitCreated { .. }
            | TaskEvent::ImplementationReviewCompleted { .. }
            | TaskEvent::FixCompleted { .. }
    ) {
        return vec![
            (name, html),
            ("milestones", milestone_list_html(state.milestones)),
        ];
    }
    vec![(name, html)]
}

fn live_event(updates: Vec<(&'static str, String)>) -> Option<Event> {
    let mut updates = updates.into_iter();
    let (name, mut html) = updates.next()?;
    for (target, fragment) in updates {
        html.push_str(&format!(
            r#"<div id="{target}" hx-swap-oob="innerHTML">{fragment}</div>"#
        ));
    }
    Some(Event::default().event(name).data(html))
}

pub async fn projects(State(state): State<AppState>) -> Html<String> {
    let projects = state.projects.list();
    let mut html = String::new();
    if projects.is_empty() {
        html.push_str(r#"<option value="">no repositories registered yet</option>"#);
    }
    for project in projects {
        html.push_str(&format!(
            r#"<option value="{}">{}</option>"#,
            project.id,
            esc(&project.name)
        ));
    }
    Html(html)
}

#[derive(Debug, Deserialize)]
pub struct RegisterProjectForm {
    pub name: String,
    pub repository: String,
    #[serde(default = "default_branch")]
    pub default_branch: String,
}

fn default_branch() -> String {
    "main".into()
}

pub async fn register_project(
    State(state): State<AppState>,
    Form(form): Form<RegisterProjectForm>,
) -> Response {
    let result = ProjectSource::github(&form.repository)
        .and_then(|source| Project::new(&form.name, source, &form.default_branch))
        .and_then(|project| state.projects.register(project));
    match result {
        Ok(project) => Html(format!(
            r#"<div class="notice ok">Registered {}. Reload the project list to select it.</div>"#,
            esc(&project.name)
        ))
        .into_response(),
        Err(error) => error_fragment(&error.to_string()),
    }
}

/// HTML forms are flat, so the agent selection arrives as six separate fields
/// rather than the nested `agents` object the JSON API takes (task 0005).
#[derive(Debug, Deserialize)]
pub struct CreateForm {
    pub kind: TaskKind,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub project_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub technology: Option<TechStack>,
    #[serde(default)]
    pub output: Option<OutputTarget>,
    /// The persistent destination folder name (task 0010). A browser submits an
    /// untouched text input as an empty string, which means "not chosen".
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub proposer_provider: Option<String>,
    #[serde(default)]
    pub proposer_model: Option<String>,
    #[serde(default)]
    pub critic_provider: Option<String>,
    #[serde(default)]
    pub critic_model: Option<String>,
    #[serde(default)]
    pub worker_tool: Option<String>,
    #[serde(default)]
    pub worker_model: Option<String>,
    /// Whether verified milestones are committed (task 0009). An unset select
    /// means the safe default: no commits.
    #[serde(default)]
    pub git_mode: Option<String>,
}

/// A browser submits an unset `<select>` as an empty string. That is "not
/// chosen", not "chosen as empty", so it becomes `None` here and the catalogue
/// fills in the default. The JSON API keeps the stricter reading, where an
/// explicit empty model is an error.
fn chosen(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

impl CreateForm {
    fn agents(&self) -> Result<Option<AgentSelectionRequest>, String> {
        let proposer_provider = parse_provider("proposer", chosen(&self.proposer_provider))?;
        let critic_provider = parse_provider("critic", chosen(&self.critic_provider))?;
        let worker_tool = match chosen(&self.worker_tool) {
            None => None,
            Some(value) => Some(
                CodingTool::from_id(value)
                    .ok_or_else(|| format!("{value:?} is not a supported worker tool"))?,
            ),
        };
        let request = AgentSelectionRequest {
            proposer: chat_request(proposer_provider, chosen(&self.proposer_model)),
            critic: chat_request(critic_provider, chosen(&self.critic_model)),
            worker: match (worker_tool, chosen(&self.worker_model)) {
                (None, None) => None,
                (tool, model) => Some(CodingAgentRequest {
                    tool,
                    model: model.map(str::to_string),
                }),
            },
        };
        Ok((!request.is_empty()).then_some(request))
    }
}

fn parse_provider(role: &str, value: Option<&str>) -> Result<Option<ChatProvider>, String> {
    match value {
        None => Ok(None),
        Some(value) => ChatProvider::from_id(value)
            .map(Some)
            .ok_or_else(|| format!("{value:?} is not a supported {role} provider")),
    }
}

fn chat_request(provider: Option<ChatProvider>, model: Option<&str>) -> Option<ChatAgentRequest> {
    match (provider, model) {
        (None, None) => None,
        (provider, model) => Some(ChatAgentRequest {
            provider,
            model: model.map(str::to_string),
        }),
    }
}

pub async fn create(State(state): State<AppState>, Form(form): Form<CreateForm>) -> Response {
    let agents = match form.agents() {
        Ok(agents) => agents,
        Err(error) => return error_fragment(&error),
    };
    let git_mode = match chosen(&form.git_mode) {
        None => None,
        Some(value) => match crate::git::GitMode::from_id(value) {
            Some(mode) => Some(mode),
            None => return error_fragment(&format!("{value:?} is not a supported Git mode")),
        },
    };
    let request = TaskRequest {
        kind: form.kind,
        title: form.title,
        description: form.description,
        project_id: form.project_id,
        technology: form.technology,
        output: form.output,
        destination: chosen(&form.destination).map(str::to_string),
        agents,
        git_mode,
    };
    if let Err(error) = request.validate() {
        return error_fragment(&error);
    }
    if let Some(project_id) = request.project_id
        && state.projects.get(project_id).is_none()
    {
        return error_fragment("Select a registered project.");
    }
    let agents = match state.catalogue.resolve(request.agents.as_ref()) {
        Ok(agents) => agents,
        Err(error) => return error_fragment(&error),
    };
    let task = match state.manager.create_from_request(request, agents) {
        Ok(task) => task,
        Err(error) => return error_fragment(&error),
    };
    pipeline::spawn(state, task.id);
    let mut headers = HeaderMap::new();
    headers.insert(
        "HX-Redirect",
        format!("/task/{}", task.id).parse().expect("valid header"),
    );
    (headers, Html(String::new())).into_response()
}

fn error_fragment(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            r#"<div class="notice err" id="create-error">{}</div>"#,
            esc(message)
        )),
    )
        .into_response()
}

pub async fn task_page(State(state): State<AppState>, Path(id): Path<TaskId>) -> Response {
    let Some(task) = state.manager.get(id) else {
        return (StatusCode::NOT_FOUND, Html("<h1>No such task</h1>")).into_response();
    };
    let mut debate = String::new();
    let mut spec = String::new();
    let mut build = if task.discarded_log_events > 0 {
        format!(
            "<div class=\"notice\">[output truncated: history limit exceeded; {} older log events discarded]</div>",
            task.discarded_log_events
        )
    } else {
        String::new()
    };
    let mut done = String::new();
    let render = RenderState::of(&task);
    for event in task.display_history() {
        // The page renders the milestone and output cards from state below, so
        // their slots are ignored while replaying history.
        for (slot, html) in event_updates(id, event, &render) {
            match slot {
                "debate" => debate.push_str(&html),
                "spec" => spec = html,
                "build" => build.push_str(&html),
                "done" => done = html,
                _ => {}
            }
        }
    }
    if task.status != TaskStatus::WaitingForApproval {
        spec = spec_readonly(&task);
    }
    let project_name = task
        .project_id
        .and_then(|project_id| state.projects.get(project_id))
        .map(|project| project.name)
        .unwrap_or_else(|| "New project".into());
    let agents = agents_html(&task.agents, task.git_mode);
    let output = if task.output.is_some() {
        output_html(&task)
    } else {
        String::new()
    };
    Html(page_html(
        &task,
        &project_name,
        &agents,
        &output,
        &debate,
        &spec,
        &build,
        &done,
    ))
    .into_response()
}

fn spec_readonly(task: &Task) -> String {
    spec_readonly_html(task.spec.as_deref())
}

fn spec_readonly_html(spec: Option<&str>) -> String {
    spec.map(|spec| format!(r#"<div class="card"><h2 class="section">Specification</h2><div class="spec-body">{}</div></div>"#, esc(spec))).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn page_html(
    task: &Task,
    project: &str,
    agents: &str,
    output: &str,
    debate: &str,
    spec: &str,
    build: &str,
    done: &str,
) -> String {
    let actions = actions_html(task.id);
    let milestones = milestones_html(task);
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title} — multiagent-chat</title><link rel="stylesheet" href="/static/style.css"><script src="/static/vendor/htmx.min.js"></script><script src="/static/vendor/sse.js"></script></head><body><div class="wrap" hx-ext="sse" sse-connect="/ui/tasks/{id}/stream"><header class="top"><h1>{title}</h1><span class="sub"><a href="/">&larr; new task</a> · {kind} · <code>{project}</code></span></header><div id="timeline" sse-swap="status" hx-swap="innerHTML">{timeline}</div><div id="done" sse-swap="done" hx-swap="innerHTML">{done}</div>{agents}{output}{actions}{milestones}<div id="spec" sse-swap="spec" hx-swap="innerHTML">{spec}</div><h2 class="section">Debate</h2><div id="debate" sse-swap="debate" hx-swap="beforeend">{debate}</div><h2 class="section">Implementation / Verification / Result</h2><div id="terminal" class="terminal" sse-swap="build" hx-swap="beforeend">{build}</div></div></body></html>"##,
        id = task.id,
        title = esc(&task.title),
        kind = task.kind.label(),
        project = esc(project),
        timeline = timeline_html(task.status)
    )
}

pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let manager = state.manager.clone();
    let events = BroadcastStream::new(state.manager.subscribe()).filter_map(move |received| {
        let updates = match received {
            Ok((event_id, event)) if event_id == id => {
                let task = manager.get(id);
                // A task that vanished mid-stream still renders, with the
                // compiled defaults standing in for state we can no longer read.
                let fallback = AgentSelection::compiled_defaults();
                let render = match &task {
                    Some(task) => RenderState::of(task),
                    None => RenderState {
                        spec: None,
                        agents: &fallback,
                        milestones: &[],
                        output: None,
                        persistence: None,
                    },
                };
                event_updates(id, &event, &render)
            }
            Ok(_) => return None,
            Err(BroadcastStreamRecvError::Lagged(_)) => vec![(
                "debate",
                r#"<div class="notice warn">Some output was dropped — reload to catch up.</div>"#
                    .into(),
            )],
        };
        live_event(updates).map(Ok)
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
pub struct ApproveForm {
    pub approve: String,
    #[serde(default)]
    pub spec: Option<String>,
}

pub async fn approve(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
    Form(form): Form<ApproveForm>,
) -> Response {
    let approve = form.approve == "true";
    let spec = if approve { form.spec } else { None };
    if let Err(error) = state.manager.decide_checked(id, Decision { approve, spec }) {
        let status = match error {
            crate::task::DecisionError::NotFound => StatusCode::NOT_FOUND,
            crate::task::DecisionError::NotWaiting => StatusCode::CONFLICT,
            crate::task::DecisionError::InvalidSpec => StatusCode::BAD_REQUEST,
        };
        return (status, Html(esc(&error.to_string()))).into_response();
    }
    let body = state
        .manager
        .get(id)
        .map(|task| spec_readonly(&task))
        .unwrap_or_default();
    Html(body).into_response()
}
