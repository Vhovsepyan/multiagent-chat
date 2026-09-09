//! Deterministic evidence records and the task evidence download.
//!
//! Detailed agent interactions are retained separately from the bounded UI log
//! tail. They share the task's audit sequence allocator, so this module does not
//! introduce a competing clock or ordering system. Export is a pure rendering
//! step over a sanitized task snapshot; it never calls an agent.

use std::io::{Cursor, Write};
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use crate::agent::{ChatProvider, CodingTool};
use crate::api::{Message, Role};
use crate::execution_limits::{TRUNCATED, bounded_text};
use crate::task::{AgentStage, RecordedEvent, Task, TaskEvent, TaskStatus};

pub const JSONL_FILENAME: &str = "agent-session.jsonl";
pub const DEVELOPMENT_LOG_FILENAME: &str = "DEVELOPMENT_LOG.md";
pub const DECISIONS_FILENAME: &str = "DECISIONS.md";
pub const AGENT_USAGE_FILENAME: &str = "AGENT_USAGE.md";
pub const FINAL_REPORT_FILENAME: &str = "FINAL_REPORT.md";

/// One prompt or response is bounded independently. Provider replies and
/// process output already have lower transport/capture caps; this additionally
/// protects task memory if a future adapter supplies unexpectedly large text.
pub const EVIDENCE_TEXT_BYTES: usize = 256 * 1024;

pub fn archive_filename(id: crate::task::TaskId) -> String {
    format!("task-{id}-evidence.zip")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    Proposer,
    Critic,
}

impl EvidenceRole {
    fn label(self) -> &'static str {
        match self {
            Self::Proposer => "Proposer",
            Self::Critic => "Critic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Completed,
    Failed,
}

impl EvidenceStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// A detailed evidence record. `sequence` and `timestamp` are assigned by the
/// same locked task allocator used for `RecordedEvent`.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceRecord {
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub payload: EvidencePayload,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidencePayload {
    AgentInteraction {
        stage: AgentStage,
        role: EvidenceRole,
        #[serde(skip_serializing_if = "Option::is_none")]
        round: Option<u32>,
        provider: ChatProvider,
        model: String,
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        response: Option<String>,
        status: EvidenceStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        duration_ms: u64,
        truncated: bool,
    },
    WorkerExecution {
        role: WorkerRole,
        stage: WorkerStage,
        tool: CodingTool,
        model: String,
        instruction: String,
        summary: String,
        status: EvidenceStatus,
        duration_ms: u64,
        truncated: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    Worker,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStage {
    Implementation,
}

impl EvidenceRecord {
    pub(crate) fn new(sequence: u64, timestamp: DateTime<Utc>, payload: EvidencePayload) -> Self {
        Self {
            sequence,
            timestamp,
            payload,
        }
    }

    pub fn truncated(&self) -> bool {
        match &self.payload {
            EvidencePayload::AgentInteraction { truncated, .. }
            | EvidencePayload::WorkerExecution { truncated, .. } => *truncated,
        }
    }

    /// Apply the established audit redactor before the record is retained, then
    /// impose the evidence-specific memory cap. Truncation is always explicit.
    pub(crate) fn sanitized_and_bounded(
        mut self,
        redact: impl Fn(&str) -> String,
        worker_output_truncated: bool,
    ) -> Self {
        fn clean(text: &mut String, truncated: &mut bool, redact: &impl Fn(&str) -> String) {
            let redacted = redact(text);
            if redacted.len() > EVIDENCE_TEXT_BYTES {
                *truncated = true;
            }
            *text = bounded_text(&redacted, EVIDENCE_TEXT_BYTES);
        }

        match &mut self.payload {
            EvidencePayload::AgentInteraction {
                model,
                prompt,
                response,
                error,
                truncated,
                ..
            } => {
                clean(model, truncated, &redact);
                clean(prompt, truncated, &redact);
                if let Some(response) = response {
                    clean(response, truncated, &redact);
                }
                if let Some(error) = error {
                    clean(error, truncated, &redact);
                }
            }
            EvidencePayload::WorkerExecution {
                model,
                instruction,
                summary,
                truncated,
                ..
            } => {
                *truncated |= worker_output_truncated;
                clean(model, truncated, &redact);
                clean(instruction, truncated, &redact);
                clean(summary, truncated, &redact);
            }
        }
        self
    }
}

/// Render exactly what the chat abstraction received, without provider-specific
/// wire formatting. Message order and roles remain explicit and deterministic.
pub fn chat_prompt(system: Option<&str>, messages: &[Message]) -> String {
    let mut prompt = String::new();
    if let Some(system) = system {
        prompt.push_str("System:\n");
        prompt.push_str(system);
        prompt.push_str("\n\n");
    }
    prompt.push_str("Messages:\n");
    for message in messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        prompt.push_str("\n[");
        prompt.push_str(role);
        prompt.push_str("]\n");
        prompt.push_str(&message.content);
        prompt.push('\n');
    }
    prompt
}

/// Safe representation of the request handed to the worker abstraction. The
/// server's absolute temporary paths are deliberately not retained.
pub fn worker_instruction(instructions: &str) -> String {
    format!(
        "Read the authoritative approved specification artifact ({}) and implement it.\n\nTask-specific instructions:\n{instructions}",
        crate::spec::APPROVED_SPEC_FILENAME
    )
}

pub fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug)]
pub struct EvidencePackage {
    pub filename: String,
    pub bytes: Vec<u8>,
    #[cfg(test)]
    pub files: Vec<(String, String)>,
}

/// Build all five required files and package only those files into an in-memory
/// ZIP. File names, ordering, timestamps, and permissions are fixed so repeated
/// exports of a stable snapshot are byte-for-byte deterministic.
pub fn export(task: &Task) -> Result<EvidencePackage> {
    let files = vec![
        (JSONL_FILENAME.to_string(), jsonl(task)?),
        (DEVELOPMENT_LOG_FILENAME.to_string(), development_log(task)),
        (DECISIONS_FILENAME.to_string(), decisions(task)),
        (AGENT_USAGE_FILENAME.to_string(), agent_usage(task)),
        (FINAL_REPORT_FILENAME.to_string(), final_report(task)),
    ];

    let cursor = Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for (name, content) in &files {
        archive
            .start_file(name, options)
            .with_context(|| format!("could not create evidence entry {name}"))?;
        archive
            .write_all(content.as_bytes())
            .with_context(|| format!("could not write evidence entry {name}"))?;
    }
    let bytes = archive
        .finish()
        .context("could not finish evidence archive")?
        .into_inner();

    Ok(EvidencePackage {
        filename: archive_filename(task.id),
        bytes,
        #[cfg(test)]
        files,
    })
}

#[derive(Serialize)]
struct TaskEventLine<'a> {
    sequence: u64,
    timestamp: &'a DateTime<Utc>,
    kind: &'static str,
    event: &'a TaskEvent,
}

#[derive(Serialize)]
struct VerificationLine<'a> {
    sequence: u64,
    timestamp: &'a DateTime<Utc>,
    kind: &'static str,
    command: &'a str,
    success: bool,
    output: &'a str,
    truncated: bool,
}

fn jsonl(task: &Task) -> Result<String> {
    let mut lines = Vec::<(u64, String)>::new();
    for recorded in &task.history {
        let line = match &recorded.event {
            TaskEvent::Verification { result } => serde_json::to_string(&VerificationLine {
                sequence: recorded.sequence,
                timestamp: &recorded.timestamp,
                kind: "verification",
                command: &result.command,
                success: result.success,
                output: &result.output,
                truncated: result.output.contains(TRUNCATED),
            }),
            _ => serde_json::to_string(&TaskEventLine {
                sequence: recorded.sequence,
                timestamp: &recorded.timestamp,
                kind: "task_event",
                event: &recorded.event,
            }),
        }
        .context("could not serialize evidence JSONL record")?;
        lines.push((recorded.sequence, line));
    }
    for record in &task.evidence {
        lines.push((
            record.sequence,
            serde_json::to_string(record).context("could not serialize interaction evidence")?,
        ));
    }
    lines.sort_unstable_by_key(|(sequence, _)| *sequence);
    let mut jsonl = lines
        .into_iter()
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    if !jsonl.is_empty() {
        jsonl.push('\n');
    }
    Ok(jsonl)
}

fn development_log(task: &Task) -> String {
    let mut entries = Vec::<(u64, DateTime<Utc>, String, Vec<String>)>::new();
    for event in &task.history {
        if let Some((title, details)) = describe_event(event) {
            entries.push((event.sequence, event.timestamp, title, details));
        }
    }
    for record in &task.evidence {
        let (title, details) = describe_evidence(record);
        entries.push((record.sequence, record.timestamp, title, details));
    }
    entries.sort_unstable_by_key(|(sequence, _, _, _)| *sequence);

    let mut markdown = String::from("# Development Log\n\n");
    for (sequence, timestamp, title, details) in entries {
        markdown.push_str(&format!(
            "## {} — {}\n\nSequence: {}\n\n",
            timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            title,
            sequence
        ));
        for detail in details {
            markdown.push_str(&detail);
            markdown.push_str("\n\n");
        }
    }
    markdown
}

fn describe_event(recorded: &RecordedEvent) -> Option<(String, Vec<String>)> {
    let event = &recorded.event;
    let (title, details) = match event {
        TaskEvent::TaskCreated { kind } => (
            "Task created".into(),
            vec![format!("Task type: {}", kind.label())],
        ),
        TaskEvent::TaskStarted => ("Task started".into(), vec![]),
        TaskEvent::Status { status } => (
            "Task status changed".into(),
            vec![format!("Status: {}", status_label(*status))],
        ),
        TaskEvent::RoundStarted { round, of } => (
            format!("Debate round {round} started"),
            vec![format!("Configured rounds: {of}")],
        ),
        TaskEvent::ProposerStarted {
            stage,
            round,
            provider,
            model,
        } => agent_lifecycle("Proposer", "started", *stage, *round, *provider, model),
        TaskEvent::ProposerCompleted {
            stage,
            round,
            provider,
            model,
        } => agent_lifecycle("Proposer", "completed", *stage, *round, *provider, model),
        TaskEvent::ProposerFailed {
            stage,
            round,
            provider,
            model,
            error,
        } => {
            let (title, mut details) =
                agent_lifecycle("Proposer", "failed", *stage, *round, *provider, model);
            details.push(format!("Error: {}", markdown_inline(error)));
            (title, details)
        }
        TaskEvent::CriticStarted {
            stage,
            round,
            provider,
            model,
        } => agent_lifecycle("Critic", "started", *stage, *round, *provider, model),
        TaskEvent::CriticCompleted {
            stage,
            round,
            provider,
            model,
        } => agent_lifecycle("Critic", "completed", *stage, *round, *provider, model),
        TaskEvent::CriticFailed {
            stage,
            round,
            provider,
            model,
            error,
        } => {
            let (title, mut details) =
                agent_lifecycle("Critic", "failed", *stage, *round, *provider, model);
            details.push(format!("Error: {}", markdown_inline(error)));
            (title, details)
        }
        TaskEvent::Proposal { round, .. } => (format!("Proposal recorded (round {round})"), vec![]),
        TaskEvent::Critique {
            round,
            verdict,
            reason,
            ..
        } => {
            let mut details = Vec::new();
            if let Some(verdict) = verdict {
                details.push(format!("Verdict: {}", markdown_inline(verdict)));
            }
            if let Some(reason) = reason {
                details.push(format!("Reason: {}", markdown_inline(reason)));
            }
            (format!("Critique recorded (round {round})"), details)
        }
        TaskEvent::Spec { .. } => ("Specification stored".into(), vec![]),
        TaskEvent::SpecApproved { .. } => ("Specification approved".into(), vec![]),
        TaskEvent::SpecGenerated => ("Specification generated".into(), vec![]),
        TaskEvent::SpecUpdated => ("Specification edited by user".into(), vec![]),
        TaskEvent::SpecRejected => ("Specification rejected".into(), vec![]),
        TaskEvent::AgentsSelected { agents } => (
            "Task agent selection recorded".into(),
            agents
                .audit_rows()
                .into_iter()
                .map(|(role, provider, model)| {
                    format!(
                        "{}: {} / {}",
                        title_case(role),
                        provider,
                        markdown_inline(model)
                    )
                })
                .collect(),
        ),
        TaskEvent::Inspection {
            source_revision, ..
        } => (
            "Project inspection completed".into(),
            source_revision
                .as_ref()
                .map(|revision| format!("Source revision: {}", markdown_inline(revision)))
                .into_iter()
                .collect(),
        ),
        TaskEvent::Verification { result } => (
            "Verification command completed".into(),
            vec![
                format!("Command: `{}`", markdown_inline(&result.command)),
                format!("Success: {}", result.success),
            ],
        ),
        TaskEvent::VerificationStarted { commands } => (
            "Verification started".into(),
            vec![format!("Commands planned: {commands}")],
        ),
        TaskEvent::VerificationCompleted { commands } => (
            "Verification completed".into(),
            vec![format!("Commands run: {commands}")],
        ),
        TaskEvent::VerificationFailed { command, error } => {
            let mut details = command
                .as_ref()
                .map(|command| format!("Command: `{}`", markdown_inline(command)))
                .into_iter()
                .collect::<Vec<_>>();
            details.push(format!("Error: {}", markdown_inline(error)));
            ("Verification failed".into(), details)
        }
        TaskEvent::WorkerStarted { tool, model } => worker_lifecycle("started", *tool, model),
        TaskEvent::WorkerCompleted { tool, model } => worker_lifecycle("completed", *tool, model),
        TaskEvent::WorkerFailed { tool, model, error } => {
            let (title, mut details) = worker_lifecycle("failed", *tool, model);
            details.push(format!("Error: {}", markdown_inline(error)));
            (title, details)
        }
        TaskEvent::WorkerCancelled { tool, model } => worker_lifecycle("cancelled", *tool, model),
        TaskEvent::Result { .. } => ("Task result captured".into(), vec![]),
        TaskEvent::Finished { status, error } => {
            let mut details = vec![format!("Status: {}", status_label(*status))];
            if let Some(error) = error {
                details.push(format!("Error: {}", markdown_inline(error)));
            }
            ("Task run finished".into(), details)
        }
        TaskEvent::TaskCompleted => ("Task completed".into(), vec![]),
        TaskEvent::TaskFailed { error } => (
            "Task failed".into(),
            vec![format!("Error: {}", markdown_inline(error))],
        ),
        TaskEvent::TaskCancelled => ("Task cancelled".into(), vec![]),
        TaskEvent::EvidenceExported { artifact } => (
            "Evidence exported".into(),
            vec![format!("Artifact: `{}`", markdown_inline(artifact))],
        ),
        TaskEvent::Build { .. } | TaskEvent::Notice { .. } | TaskEvent::Warning { .. } => {
            return None;
        }
    };
    Some((title, details))
}

fn agent_lifecycle(
    role: &str,
    action: &str,
    stage: AgentStage,
    round: Option<u32>,
    provider: ChatProvider,
    model: &str,
) -> (String, Vec<String>) {
    let mut details = vec![
        format!("Stage: {}", stage_label(stage)),
        format!("Provider: {}", provider.label()),
        format!("Model: `{}`", markdown_inline(model)),
    ];
    if let Some(round) = round {
        details.insert(1, format!("Round: {round}"));
    }
    (format!("{role} {action}"), details)
}

fn worker_lifecycle(action: &str, tool: CodingTool, model: &str) -> (String, Vec<String>) {
    (
        format!("Worker {action}"),
        vec![
            format!("Tool: {}", tool.label()),
            format!("Model: `{}`", markdown_inline(model)),
        ],
    )
}

fn describe_evidence(record: &EvidenceRecord) -> (String, Vec<String>) {
    match &record.payload {
        EvidencePayload::AgentInteraction {
            stage,
            role,
            round,
            provider,
            model,
            status,
            duration_ms,
            truncated,
            ..
        } => {
            let mut details = vec![
                format!("Stage: {}", stage_label(*stage)),
                format!("Provider: {}", provider.label()),
                format!("Model: `{}`", markdown_inline(model)),
                format!("Duration: {duration_ms} ms"),
            ];
            if let Some(round) = round {
                details.insert(1, format!("Round: {round}"));
            }
            if *truncated {
                details.push("Evidence truncated: true".into());
            }
            (
                format!("{} interaction {}", role.label(), status.label()),
                details,
            )
        }
        EvidencePayload::WorkerExecution {
            tool,
            model,
            status,
            duration_ms,
            truncated,
            ..
        } => {
            let mut details = vec![
                format!("Tool: {}", tool.label()),
                format!("Model: `{}`", markdown_inline(model)),
                format!("Duration: {duration_ms} ms"),
            ];
            if *truncated {
                details.push("Evidence truncated: true".into());
            }
            (
                format!("Worker execution evidence {}", status.label()),
                details,
            )
        }
    }
}

fn agent_usage(task: &Task) -> String {
    format!(
        "# Agent Usage\n\n## Proposer\n\nProvider: {}\n\nModel: `{}`\n\nPurpose: Generate implementation and architecture proposals and draft the specification.\n\n## Critic\n\nProvider: {}\n\nModel: `{}`\n\nPurpose: Review proposals, identify risks, and check the generated specification.\n\n## Worker\n\nTool: {}\n\nModel: `{}`\n\nPurpose: Implement the user-approved specification in the isolated task workspace.\n",
        task.agents.proposer.provider.label(),
        markdown_inline(&task.agents.proposer.model),
        task.agents.critic.provider.label(),
        markdown_inline(&task.agents.critic.model),
        task.agents.worker.tool.label(),
        markdown_inline(&task.agents.worker.model),
    )
}

fn decisions(task: &Task) -> String {
    let mut markdown = String::from("# Decisions\n\n");
    let source = task.spec.as_deref();
    let mut recorded = false;
    if let Some(spec) = source {
        for heading in ["Agreed solution", "Architecture"] {
            if let Some(body) = markdown_section(spec, heading)
                && !body.trim().is_empty()
            {
                recorded = true;
                markdown.push_str(&format!(
                    "## {}\n\nDecision:\n\n{}\n\nRationale: Not explicitly recorded separately from the specification.\n\n",
                    heading, body
                ));
            }
        }
    }
    if !recorded {
        markdown.push_str("No explicit engineering decisions were recorded in a generated or approved specification.\n\n");
    }

    let concerns = task
        .history
        .iter()
        .filter_map(|recorded| match &recorded.event {
            TaskEvent::Critique {
                verdict: Some(verdict),
                reason: Some(reason),
                ..
            } if verdict == "needs_work" => Some(reason.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    markdown.push_str("## Critic concerns\n\n");
    if concerns.is_empty() {
        markdown.push_str("No explicit unresolved critic concern was recorded.\n");
    } else {
        for concern in concerns {
            markdown.push_str("- ");
            markdown.push_str(&markdown_inline(concern));
            markdown.push('\n');
        }
        markdown.push_str("\nResolution: Refer to the later recorded proposal/specification; no separate rationale is inferred.\n");
    }
    markdown
}

fn final_report(task: &Task) -> String {
    let started = event_time(task, |event| matches!(event, TaskEvent::TaskStarted));
    let ended = task
        .history
        .iter()
        .rev()
        .find(|recorded| {
            matches!(
                recorded.event,
                TaskEvent::TaskCompleted
                    | TaskEvent::TaskFailed { .. }
                    | TaskEvent::TaskCancelled
                    | TaskEvent::Finished { .. }
            )
        })
        .map(|event| event.timestamp);
    let specification = if task
        .decision
        .as_ref()
        .is_some_and(|decision| decision.approve)
    {
        "Approved"
    } else if task.status == TaskStatus::Rejected {
        "Rejected"
    } else if task.spec.is_some() {
        "Generated; approval not recorded"
    } else {
        "Not generated"
    };
    let worker = task
        .evidence
        .iter()
        .rev()
        .find_map(|record| {
            if let EvidencePayload::WorkerExecution {
                status, summary, ..
            } = &record.payload
            {
                Some(format!(
                    "{} — {}",
                    title_case(status.label()),
                    markdown_inline(summary)
                ))
            } else {
                None
            }
        })
        .or_else(|| {
            task.history
                .iter()
                .rev()
                .find_map(|recorded| match &recorded.event {
                    TaskEvent::WorkerCancelled { .. } => Some("Cancelled".into()),
                    TaskEvent::WorkerFailed { error, .. } => {
                        Some(format!("Failed — {}", markdown_inline(error)))
                    }
                    TaskEvent::WorkerCompleted { .. } => Some("Completed".into()),
                    TaskEvent::WorkerStarted { .. } => Some("Started; no result recorded".into()),
                    _ => None,
                })
        });
    let verification = if task
        .history
        .iter()
        .any(|recorded| matches!(recorded.event, TaskEvent::VerificationFailed { .. }))
    {
        "Failed"
    } else if task
        .history
        .iter()
        .any(|recorded| matches!(recorded.event, TaskEvent::VerificationCompleted { .. }))
    {
        "Completed successfully"
    } else if task
        .history
        .iter()
        .any(|recorded| matches!(recorded.event, TaskEvent::VerificationStarted { .. }))
    {
        "Started; no completion recorded"
    } else {
        "Not run"
    };
    let errors = known_errors(task);
    let truncated = task.evidence.iter().any(EvidenceRecord::truncated)
        || task
            .history
            .iter()
            .any(|recorded| event_contains_truncation(&recorded.event));

    let mut markdown = format!(
        "# Final Report\n\nTask title: {}\n\nTask type: {}\n\nTask description/objective:\n\n{}\n\nStart time: {}\n\nEnd time: {}\n\nFinal status: {}\n\nAgents used: Proposer {} / `{}`; Critic {} / `{}`; Worker {} / `{}`\n\nSpecification status: {}\n\nWorker result: {}\n\nVerification result: {}\n\nOutput/workspace: The temporary workspace is not exported. Any reviewable diff and verification result are retained in task result evidence.\n\n## Known errors/failures\n\n",
        markdown_inline(&task.title),
        task.kind.label(),
        markdown_quote(&task.description),
        format_time(started),
        format_time(ended),
        final_status(task),
        task.agents.proposer.provider.label(),
        markdown_inline(&task.agents.proposer.model),
        task.agents.critic.provider.label(),
        markdown_inline(&task.agents.critic.model),
        task.agents.worker.tool.label(),
        markdown_inline(&task.agents.worker.model),
        specification,
        worker
            .as_deref()
            .unwrap_or("Not run or no worker result recorded"),
        verification,
    );
    if errors.is_empty() {
        markdown.push_str("None recorded.\n");
    } else {
        for error in errors {
            markdown.push_str("- ");
            markdown.push_str(&markdown_inline(&error));
            markdown.push('\n');
        }
    }
    markdown.push_str("\n## Evidence note\n\n");
    if truncated {
        markdown.push_str("Some evidence was truncated due to configured safety/resource limits. Records identify truncation explicitly.\n");
    } else {
        markdown.push_str("No retained evidence record reports truncation. Raw worker stdout/stderr is intentionally not part of the durable evidence transcript; bounded execution summaries and lifecycle events are retained instead.\n");
    }
    markdown
}

fn known_errors(task: &Task) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(error) = &task.error {
        errors.push(error.clone());
    }
    for recorded in &task.history {
        let error = match &recorded.event {
            TaskEvent::ProposerFailed { error, .. }
            | TaskEvent::CriticFailed { error, .. }
            | TaskEvent::WorkerFailed { error, .. }
            | TaskEvent::VerificationFailed { error, .. }
            | TaskEvent::TaskFailed { error } => Some(error),
            TaskEvent::Finished {
                error: Some(error), ..
            } => Some(error),
            _ => None,
        };
        if let Some(error) = error
            && !errors.contains(error)
        {
            errors.push(error.clone());
        }
    }
    errors
}

fn event_contains_truncation(event: &TaskEvent) -> bool {
    match event {
        TaskEvent::Proposal { text, .. } | TaskEvent::Critique { text, .. } => {
            text.contains(TRUNCATED)
        }
        TaskEvent::Spec { markdown, .. } | TaskEvent::SpecApproved { markdown } => {
            markdown.contains(TRUNCATED)
        }
        TaskEvent::Verification { result } => result.output.contains(TRUNCATED),
        TaskEvent::Result { result } => {
            result.diff.contains(TRUNCATED)
                || result
                    .verification
                    .iter()
                    .any(|verification| verification.output.contains(TRUNCATED))
        }
        _ => false,
    }
}

fn event_time(task: &Task, predicate: impl Fn(&TaskEvent) -> bool) -> Option<DateTime<Utc>> {
    task.history
        .iter()
        .find(|recorded| predicate(&recorded.event))
        .map(|recorded| recorded.timestamp)
}

fn format_time(time: Option<DateTime<Utc>>) -> String {
    time.map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(|| "Not recorded".into())
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Created => "Created",
        TaskStatus::Debating => "Debating",
        TaskStatus::GeneratingSpec => "Generating specification",
        TaskStatus::WaitingForApproval => "Waiting for approval",
        TaskStatus::Implementing => "Implementing",
        TaskStatus::Completed => "Completed",
        TaskStatus::Rejected => "Rejected",
        TaskStatus::Failed => "Failed",
    }
}

fn final_status(task: &Task) -> &'static str {
    if task
        .history
        .iter()
        .any(|recorded| matches!(recorded.event, TaskEvent::TaskCancelled))
    {
        "Cancelled"
    } else {
        status_label(task.status)
    }
}

fn stage_label(stage: AgentStage) -> &'static str {
    match stage {
        AgentStage::Debate => "debate",
        AgentStage::Specification => "specification",
    }
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn markdown_inline(value: &str) -> String {
    value.replace(['\r', '\n'], " ").replace('`', "\\`")
}

fn markdown_quote(value: &str) -> String {
    value
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn markdown_section(markdown: &str, name: &str) -> Option<String> {
    let heading = format!("## {name}");
    let start = markdown
        .lines()
        .enumerate()
        .find(|(_, line)| line.trim().eq_ignore_ascii_case(&heading))?
        .0;
    let lines = markdown.lines().collect::<Vec<_>>();
    let body_start = start + 1;
    let body_end = lines[body_start..]
        .iter()
        .position(|line| line.trim_start().starts_with("## "))
        .map(|offset| body_start + offset)
        .unwrap_or(lines.len());
    Some(lines[body_start..body_end].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentSelection, ChatAgentConfig, CodingAgentConfig};
    use crate::execution_limits::HistoryLimits;
    use crate::task::{OutputTarget, TaskKind, TaskManager, TaskRequest};
    use crate::technology::TechStack;

    fn selection() -> AgentSelection {
        AgentSelection {
            proposer: ChatAgentConfig::new(ChatProvider::Anthropic, "proposal-model-v1"),
            critic: ChatAgentConfig::new(ChatProvider::Gemini, "critic-model-v2"),
            worker: CodingAgentConfig::new(CodingTool::ClaudeCode, "worker-model-v3"),
        }
    }

    fn task_with_evidence(secret: &str) -> (TaskManager, crate::task::TaskId) {
        let manager = TaskManager::with_history_limits_and_secrets(
            HistoryLimits::default(),
            [secret.to_string()],
        );
        let agents = selection();
        let task = manager
            .create_from_request(
                TaskRequest {
                    kind: TaskKind::NewProject,
                    title: "Evidence task".into(),
                    description: "Review the actual run".into(),
                    project_id: None,
                    technology: Some(TechStack::Rust),
                    output: Some(OutputTarget::ReviewableResult),
                    agents: None,
                },
                agents.clone(),
            )
            .unwrap();
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::TaskStarted);
        emitter.emit(TaskEvent::AgentsSelected { agents });
        emitter.emit(TaskEvent::ProposerStarted {
            stage: AgentStage::Debate,
            round: Some(1),
            provider: ChatProvider::Anthropic,
            model: "proposal-model-v1".into(),
        });
        emitter.record_evidence(EvidencePayload::AgentInteraction {
            stage: AgentStage::Debate,
            role: EvidenceRole::Proposer,
            round: Some(1),
            provider: ChatProvider::Anthropic,
            model: "proposal-model-v1".into(),
            prompt: format!(
                "Review design safely.\nUse API key sk-secret-value-long and {secret} in prompt"
            ),
            response: Some(format!("response echoed {secret}")),
            status: EvidenceStatus::Completed,
            error: None,
            duration_ms: 42,
            truncated: false,
        });
        emitter.emit(TaskEvent::ProposerCompleted {
            stage: AgentStage::Debate,
            round: Some(1),
            provider: ChatProvider::Anthropic,
            model: "proposal-model-v1".into(),
        });
        emitter.emit(TaskEvent::CriticStarted {
            stage: AgentStage::Debate,
            round: Some(1),
            provider: ChatProvider::Gemini,
            model: "critic-model-v2".into(),
        });
        emitter.record_evidence(EvidencePayload::AgentInteraction {
            stage: AgentStage::Debate,
            role: EvidenceRole::Critic,
            round: Some(1),
            provider: ChatProvider::Gemini,
            model: "critic-model-v2".into(),
            prompt: "Review proposal".into(),
            response: Some("REASON: sound\nVERDICT: APPROVED".into()),
            status: EvidenceStatus::Completed,
            error: None,
            duration_ms: 17,
            truncated: false,
        });
        emitter.emit(TaskEvent::CriticCompleted {
            stage: AgentStage::Debate,
            round: Some(1),
            provider: ChatProvider::Gemini,
            model: "critic-model-v2".into(),
        });
        emitter.emit(TaskEvent::Critique {
            round: 1,
            text: "REASON: sound\nVERDICT: APPROVED".into(),
            verdict: Some("approved".into()),
            reason: Some("sound".into()),
        });
        emitter.emit(TaskEvent::SpecApproved {
            markdown: "## Problem\nNeed evidence.\n\n## Agreed solution\nStore actual interactions.\n\n## Architecture\nUse the task audit sequence.\n\n## Steps\n1. Export.\n\n## Out of scope\nNone.\n\n## Open risks\nNone."
                .into(),
        });
        emitter.emit(TaskEvent::WorkerStarted {
            tool: CodingTool::ClaudeCode,
            model: "worker-model-v3".into(),
        });
        emitter.emit(TaskEvent::Build {
            chunk: "password=worker-secret-must-not-export".into(),
        });
        emitter.record_evidence(EvidencePayload::WorkerExecution {
            role: WorkerRole::Worker,
            stage: WorkerStage::Implementation,
            tool: CodingTool::ClaudeCode,
            model: "worker-model-v3".into(),
            instruction: "Implement approved-spec.md".into(),
            summary: "Worker completed successfully".into(),
            status: EvidenceStatus::Completed,
            duration_ms: 99,
            truncated: false,
        });
        emitter.emit(TaskEvent::WorkerCompleted {
            tool: CodingTool::ClaudeCode,
            model: "worker-model-v3".into(),
        });
        emitter.emit(TaskEvent::VerificationStarted { commands: 1 });
        emitter.emit(TaskEvent::Verification {
            result: crate::verification::VerificationResult {
                command: "cargo test".into(),
                success: true,
                output: "all tests passed".into(),
            },
        });
        emitter.emit(TaskEvent::VerificationCompleted { commands: 1 });
        emitter.emit(TaskEvent::TaskCompleted);
        (manager, task.id)
    }

    fn file<'a>(package: &'a EvidencePackage, name: &str) -> &'a str {
        package
            .files
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, content)| content.as_str())
            .unwrap()
    }

    #[test]
    fn jsonl_is_independently_valid_ordered_and_uses_frozen_agents() {
        let (manager, id) = task_with_evidence("configured-secret-value");
        let task = manager.evidence_snapshot(id).unwrap();
        let package = export(&task).unwrap();
        let lines = file(&package, JSONL_FILENAME)
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();

        let sequences = lines
            .iter()
            .map(|line| line["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
        let timestamps = lines
            .iter()
            .map(|line| DateTime::parse_from_rfc3339(line["timestamp"].as_str().unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert!(timestamps.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(lines.iter().any(|line| line["kind"] == "task_event"));
        assert!(lines.iter().any(|line| line["kind"] == "verification"));
        assert!(lines.iter().any(|line| {
            line["kind"] == "agent_interaction"
                && line["provider"] == "anthropic"
                && line["model"] == "proposal-model-v1"
                && line["prompt"]
                    .as_str()
                    .unwrap()
                    .contains("Review design safely")
                && line["response"]
                    .as_str()
                    .unwrap()
                    .contains("response echoed")
        }));
        assert!(lines.iter().any(|line| {
            line["kind"] == "worker_execution"
                && line["tool"] == "claude_code"
                && line["model"] == "worker-model-v3"
        }));

        let usage = file(&package, AGENT_USAGE_FILENAME);
        assert!(usage.contains("proposal-model-v1"));
        assert!(usage.contains("critic-model-v2"));
        assert!(usage.contains("Tool: Claude Code"));
        assert!(usage.contains("worker-model-v3"));
    }

    #[test]
    fn archive_contains_only_the_five_required_utf8_files() {
        let (manager, id) = task_with_evidence("configured-secret-value");
        let package = export(&manager.evidence_snapshot(id).unwrap()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(package.bytes)).unwrap();
        assert_eq!(archive.len(), 5);
        let expected = [
            JSONL_FILENAME,
            DEVELOPMENT_LOG_FILENAME,
            DECISIONS_FILENAME,
            AGENT_USAGE_FILENAME,
            FINAL_REPORT_FILENAME,
        ];
        for (index, expected_name) in expected.into_iter().enumerate() {
            let mut entry = archive.by_index(index).unwrap();
            assert_eq!(entry.name(), expected_name);
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
            assert!(!text.is_empty());
        }
    }

    #[test]
    fn all_exported_content_is_redacted_and_worker_logs_stay_separate() {
        let secret = "configured-secret-value";
        let (manager, id) = task_with_evidence(secret);
        manager
            .emitter(id)
            .record_evidence(EvidencePayload::AgentInteraction {
                stage: AgentStage::Debate,
                role: EvidenceRole::Critic,
                round: Some(2),
                provider: ChatProvider::Gemini,
                model: "critic-model-v2".into(),
                prompt: "retry".into(),
                response: None,
                status: EvidenceStatus::Failed,
                error: Some("Authorization: Bearer error-token-must-not-export".into()),
                duration_ms: 3,
                truncated: false,
            });
        let package = export(&manager.evidence_snapshot(id).unwrap()).unwrap();
        for (name, content) in &package.files {
            assert!(!content.contains(secret), "secret leaked in {name}");
            assert!(
                !content.contains("sk-secret-value-long"),
                "inline key leaked in {name}"
            );
            assert!(
                !content.contains("worker-secret-must-not-export"),
                "bounded UI worker log leaked in {name}"
            );
            assert!(
                !content.contains("error-token-must-not-export"),
                "error credential leaked in {name}"
            );
        }
        assert!(file(&package, JSONL_FILENAME).contains("[REDACTED]"));
    }

    #[test]
    fn detailed_evidence_is_not_exposed_in_normal_task_json_or_ui_logs() {
        let (manager, id) = task_with_evidence("configured-secret-value");
        let task = manager.get(id).unwrap();
        let ordinary_json = serde_json::to_value(&task).unwrap();
        assert!(ordinary_json.get("evidence").is_none());
        assert!(task.log_tail.iter().all(|recorded| {
            matches!(
                recorded.event,
                TaskEvent::Build { .. } | TaskEvent::Notice { .. } | TaskEvent::Warning { .. }
            )
        }));
        assert!(!task.evidence.is_empty());
    }

    #[test]
    fn truncation_is_explicit_in_jsonl_and_the_final_report() {
        let manager = TaskManager::new();
        let task = manager.create("large", "evidence", "legacy");
        manager
            .emitter(task.id)
            .record_evidence(EvidencePayload::AgentInteraction {
                stage: AgentStage::Debate,
                role: EvidenceRole::Proposer,
                round: Some(1),
                provider: ChatProvider::Gemini,
                model: "model".into(),
                prompt: "x".repeat(EVIDENCE_TEXT_BYTES + 100),
                response: Some("ok".into()),
                status: EvidenceStatus::Completed,
                error: None,
                duration_ms: 1,
                truncated: false,
            });
        manager.emitter(task.id).emit(TaskEvent::Build {
            chunk: TRUNCATED.into(),
        });
        manager
            .emitter(task.id)
            .record_evidence(EvidencePayload::WorkerExecution {
                role: WorkerRole::Worker,
                stage: WorkerStage::Implementation,
                tool: CodingTool::ClaudeCode,
                model: "worker".into(),
                instruction: "implement".into(),
                summary: "completed".into(),
                status: EvidenceStatus::Completed,
                duration_ms: 1,
                truncated: false,
            });
        let package = export(&manager.evidence_snapshot(task.id).unwrap()).unwrap();
        assert!(file(&package, JSONL_FILENAME).contains("\"truncated\":true"));
        assert!(file(&package, JSONL_FILENAME).contains(TRUNCATED));
        assert!(file(&package, FINAL_REPORT_FILENAME).contains("Some evidence was truncated"));
    }

    #[test]
    fn failed_tasks_export_honestly() {
        let manager = TaskManager::new();
        let task = manager.create("failed", "objective", "legacy");
        manager.emitter(task.id).emit(TaskEvent::TaskFailed {
            error: "verification failed".into(),
        });
        let package = export(&manager.evidence_snapshot(task.id).unwrap()).unwrap();
        let report = file(&package, FINAL_REPORT_FILENAME);
        assert!(report.contains("Final status: Failed"));
        assert!(report.contains("verification failed"));
    }

    #[test]
    fn cancelled_tasks_export_honestly_without_a_cancelled_status_variant() {
        let manager = TaskManager::new();
        let task = manager.create("cancelled", "objective", "legacy");
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::WorkerStarted {
            tool: task.agents.worker.tool,
            model: task.agents.worker.model.clone(),
        });
        emitter.emit(TaskEvent::WorkerCancelled {
            tool: task.agents.worker.tool,
            model: task.agents.worker.model.clone(),
        });
        emitter.emit(TaskEvent::TaskCancelled);
        let package = export(&manager.evidence_snapshot(task.id).unwrap()).unwrap();
        let report = file(&package, FINAL_REPORT_FILENAME);
        assert!(report.contains("Final status: Cancelled"));
        assert!(report.contains("Worker result: Cancelled"));
    }

    #[test]
    fn repeat_export_is_deterministic_and_the_audit_event_is_idempotent() {
        let (manager, id) = task_with_evidence("configured-secret-value");
        let first = export(&manager.evidence_snapshot(id).unwrap()).unwrap();
        let second = export(&manager.evidence_snapshot(id).unwrap()).unwrap();
        assert_eq!(first.bytes, second.bytes);
        assert_eq!(first.files, second.files);
        let stored = manager.get(id).unwrap();
        assert_eq!(
            stored
                .history
                .iter()
                .filter(|recorded| { matches!(recorded.event, TaskEvent::EvidenceExported { .. }) })
                .count(),
            1
        );
    }

    #[test]
    fn titles_cannot_control_the_archive_or_entry_paths() {
        let manager = TaskManager::new();
        let task = manager.create("../../escape", "path test", "legacy");
        let package = export(&manager.evidence_snapshot(task.id).unwrap()).unwrap();
        assert_eq!(package.filename, format!("task-{}-evidence.zip", task.id));
        assert!(!package.filename.contains(".."));
        assert!(
            package
                .files
                .iter()
                .all(|(name, _)| !name.contains('/') && !name.contains('\\'))
        );
    }
}
