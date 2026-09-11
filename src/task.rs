//! Phase 7: the domain model for one orchestration run.
//!
//! A `Task` is one trip through the pipeline: debate, spec, approval, build.
//! v1 did all of that in a straight line inside `main`, printing as it went.
//! For the web UI the same work has to be observable from outside, so every
//! interesting moment becomes a `TaskEvent` that anyone can subscribe to.
//!
//! DP-9 (decided 2026-08-21): the pipeline stages do not reach for a global
//! channel. Each is handed an `&Emitter` and calls `emit` on it. That keeps the
//! wiring explicit, lets tests pass a throwaway emitter, and means nothing in
//! `debate.rs` / `spec.rs` / `implementer.rs` needs to know a web server exists.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

use crate::agent::{AgentSelection, AgentSelectionRequest};
use crate::evidence::{EvidencePayload, EvidenceRecord};
use crate::execution_limits::{HistoryLimits, bounded_text};
use crate::git::{GitMode, MilestoneCommit};
use crate::milestone::{Milestone, MilestoneStatus};
use crate::project::ProjectId;
use crate::technology::{ProjectProfile, TechStack};
use crate::verification::VerificationResult;

/// How many events we keep for a subscriber that is briefly behind.
///
/// A browser reconnecting mid-debate should not miss turns. If a subscriber
/// falls further behind than this, `recv` reports `Lagged` and the UI can
/// re-fetch the snapshot instead of pretending nothing happened.
pub const EVENT_BUFFER: usize = 256;

/// Identifies one task. `Uuid` so the browser can hold it in a URL.
pub type TaskId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    NewProject,
    TakeHomeAssignment,
    Feature,
    BugFix,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::NewProject => "new project",
            Self::TakeHomeAssignment => "take-home assignment",
            Self::Feature => "feature",
            Self::BugFix => "bug fix",
        }
    }

    /// Both of these kinds create an application in a fresh task workspace.
    /// Take-home work deliberately reuses the New Project execution path.
    pub fn creates_new_project(self) -> bool {
        matches!(self, Self::NewProject | Self::TakeHomeAssignment)
    }

    pub fn is_take_home_assignment(self) -> bool {
        matches!(self, Self::TakeHomeAssignment)
    }
}

/// What a New Project run leaves behind (task 0010).
///
/// The default keeps the behavior every task had before persistent output
/// existed: the generated project is reviewed from the task result and the
/// temporary workspace is thrown away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTarget {
    #[default]
    ReviewableResult,
    /// Keep the finished project in a folder inside the server's configured
    /// persistent output root. `TaskRequest::destination` names that folder.
    PersistentLocalProject,
}

impl OutputTarget {
    pub const ALL: [OutputTarget; 2] = [
        OutputTarget::ReviewableResult,
        OutputTarget::PersistentLocalProject,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::ReviewableResult => "reviewable_result",
            Self::PersistentLocalProject => "persistent_local_project",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ReviewableResult => "Temporary review result",
            Self::PersistentLocalProject => "Persistent local project",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.id() == value)
    }

    pub fn is_persistent(self) -> bool {
        matches!(self, Self::PersistentLocalProject)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRequest {
    pub kind: TaskKind,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub technology: Option<TechStack>,
    #[serde(default)]
    pub output: Option<OutputTarget>,
    /// The folder name a persistent New Project is written to, inside the
    /// server's configured output root (task 0010). Never a path.
    #[serde(default)]
    pub destination: Option<String>,
    /// Per-role agent choice (task 0005). Absent means "the configured
    /// defaults", which is what every pre-0005 client sends.
    #[serde(default)]
    pub agents: Option<AgentSelectionRequest>,
    /// Whether verified milestones are committed (task 0009). Absent means the
    /// previous behavior: no commits.
    #[serde(default)]
    pub git_mode: Option<GitMode>,
}

impl TaskRequest {
    /// Take-home work always has a durable deliverable. The client may omit the
    /// output value because the UI supplies this default, but the stored task
    /// never does.
    pub fn effective_output(&self) -> Option<OutputTarget> {
        match self.kind {
            TaskKind::TakeHomeAssignment => {
                self.output.or(Some(OutputTarget::PersistentLocalProject))
            }
            _ => self.output,
        }
    }

    /// A take-home repository should show its gradual, verified development
    /// history by default. Other task kinds retain their established default.
    pub fn effective_git_mode(&self) -> GitMode {
        match self.kind {
            TaskKind::TakeHomeAssignment => self.git_mode.unwrap_or(GitMode::CommitPerMilestone),
            _ => self.git_mode.unwrap_or_default(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("title cannot be empty".into());
        }
        if self.description.trim().is_empty() {
            return Err("description cannot be empty".into());
        }
        match self.kind {
            TaskKind::NewProject | TaskKind::TakeHomeAssignment => {
                if self.project_id.is_some() {
                    return Err(format!(
                        "{} must not reference an existing project",
                        self.kind.label()
                    ));
                }
                if self.technology.is_none() {
                    return Err(format!("{} requires a technology", self.kind.label()));
                }
                match self.effective_output() {
                    None => {
                        return Err(format!(
                            "{} requires output configuration",
                            self.kind.label()
                        ));
                    }
                    // Task 0010: the destination is part of choosing persistent
                    // output, so its syntax is checked with the rest of the
                    // request rather than half-way through the run.
                    Some(OutputTarget::PersistentLocalProject) => {
                        let destination = self.destination.as_deref().unwrap_or_default();
                        crate::persistence::validate_name(destination)
                            .map_err(|error| error.to_string())?;
                    }
                    Some(OutputTarget::ReviewableResult) => {
                        if self.destination.is_some() {
                            return Err(
                                "a temporary review result has no destination folder".into()
                            );
                        }
                    }
                }
                if self.kind.is_take_home_assignment()
                    && self.effective_output() != Some(OutputTarget::PersistentLocalProject)
                {
                    return Err("take-home assignment requires persistent output".into());
                }
            }
            TaskKind::Feature | TaskKind::BugFix => {
                if self.project_id.is_none() {
                    return Err(format!(
                        "{} requires a registered project",
                        self.kind.label()
                    ));
                }
                if self.technology.is_some() || self.output.is_some() || self.destination.is_some()
                {
                    return Err(format!(
                        "{} uses the registered project's detected technology and output",
                        self.kind.label()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// How far persistent output got (task 0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceStatus {
    Started,
    Persisted,
    Failed,
}

impl PersistenceStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Started => "persisting",
            Self::Persisted => "persisted",
            Self::Failed => "failed",
        }
    }
}

/// The safe persistent-output metadata retained on the task (task 0010).
///
/// `destination` is the user's own chosen folder; no temporary workspace path
/// ever reaches this structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPersistence {
    pub mode: OutputTarget,
    pub status: PersistenceStatus,
    pub destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<crate::git::RepositoryStatus>,
    /// Why repository metadata is missing from a SUCCESSFUL persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Safe metadata recorded after an explicit GitHub publication succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPublication {
    pub repository: String,
    pub branch: String,
    pub commit_sha: String,
    pub published_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub source_revision: Option<String>,
    pub verification: Vec<VerificationResult>,
    pub diff: String,
}

/// A take-home delivery checklist derived from recorded task state. It is
/// intentionally evidence-facing: an item is never marked complete merely
/// because this task kind selected the corresponding feature by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompletionChecklist {
    pub implementation_complete: bool,
    pub verification_complete: bool,
    pub acceptance_criteria_reviewed: bool,
    pub final_critic_review_complete: bool,
    pub documentation_generated: bool,
    pub evidence_export_available: bool,
    pub git_history_available: bool,
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// Where a task currently is (DP-7).
///
/// `Serialize` renders these as `"debating"`, `"waiting_for_approval"` and so
/// on, which is what the browser will switch the timeline UI on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Accepted, nothing started yet.
    Created,
    /// Proposer and Critic are arguing.
    Debating,
    /// The debate is over; the spec is being drafted and checked.
    GeneratingSpec,
    /// The generated specification is in task state; gate not yet answered.
    WaitingForApproval,
    /// Claude Code is running in the target repo.
    Implementing,
    /// Finished successfully.
    Completed,
    /// The human declined at the gate. Not an error; task state keeps the spec.
    Rejected,
    /// Something went wrong; `Task::error` says what.
    Failed,
    /// Execution was cancelled before all milestones completed.
    Cancelled,
}

impl TaskStatus {
    /// True once nothing further will happen on its own.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed
                | TaskStatus::Rejected
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        )
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Which part of a run caused a chat-agent call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStage {
    Debate,
    Specification,
    /// The critic reviewing what was actually built (task 0011).
    ImplementationReview,
}

/// Everything worth telling a watcher about, as it happens.
///
/// `#[serde(tag = "type")]` puts a discriminator in the JSON, so the browser
/// gets `{"type":"proposal","round":1,"text":"..."}` and can switch on `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEvent {
    TaskCreated {
        kind: TaskKind,
        /// Frozen at task creation so the audit states whether a newly created
        /// project is review-only or must be retained after the run.
        output: Option<OutputTarget>,
        /// Frozen task-level Git behavior. It describes local milestone
        /// commits only; no event or mode can initiate a push.
        git_mode: GitMode,
    },
    TaskStarted,

    /// The task moved to a new stage. Drives the pipeline timeline.
    Status {
        status: TaskStatus,
    },

    /// A debate round began.
    RoundStarted {
        round: u32,
        of: u32,
    },

    ProposerStarted {
        stage: AgentStage,
        round: Option<u32>,
        provider: crate::agent::ChatProvider,
        model: String,
    },
    ProposerCompleted {
        stage: AgentStage,
        round: Option<u32>,
        provider: crate::agent::ChatProvider,
        model: String,
    },
    ProposerFailed {
        stage: AgentStage,
        round: Option<u32>,
        provider: crate::agent::ChatProvider,
        model: String,
        error: String,
    },

    CriticStarted {
        stage: AgentStage,
        round: Option<u32>,
        provider: crate::agent::ChatProvider,
        model: String,
    },
    CriticCompleted {
        stage: AgentStage,
        round: Option<u32>,
        provider: crate::agent::ChatProvider,
        model: String,
    },
    CriticFailed {
        stage: AgentStage,
        round: Option<u32>,
        provider: crate::agent::ChatProvider,
        model: String,
        error: String,
    },

    /// The Proposer's full turn.
    Proposal {
        round: u32,
        text: String,
    },

    /// The Critic's full turn, with its verdict pulled out so the UI can
    /// highlight it without re-parsing the prose.
    Critique {
        round: u32,
        text: String,
        /// "approved" / "needs_work", or absent if the Critic gave none.
        verdict: Option<String>,
        reason: Option<String>,
    },

    /// Specification generated. `path` is its intended orchestration artifact
    /// location, not a file in the user's repository.
    Spec {
        markdown: String,
        path: String,
    },

    /// The exact specification accepted at Gate 2. This becomes the task's
    /// authoritative specification for the UI, implementation, and replay.
    SpecApproved {
        markdown: String,
    },
    SpecGenerated,
    SpecUpdated,
    SpecRejected,

    /// The agents this run will use, published once before the debate starts
    /// so the event stream carries role/provider/model too (task 0005).
    AgentsSelected {
        agents: AgentSelection,
    },

    MilestonePlanCreated {
        milestones: Vec<Milestone>,
    },
    MilestoneStarted {
        id: String,
        order: u32,
        title: String,
        worker_tool: crate::agent::CodingTool,
        worker_model: String,
    },
    MilestoneCompleted {
        id: String,
        order: u32,
        title: String,
        verification: Vec<VerificationResult>,
        worker_result_summary: String,
    },
    MilestoneFailed {
        id: String,
        order: u32,
        title: String,
        verification: Vec<VerificationResult>,
        worker_result_summary: Option<String>,
        error: String,
    },
    MilestoneCancelled {
        id: String,
        order: u32,
        title: String,
        reason: String,
    },
    /// A verified milestone was recorded as one commit (task 0009). Carries
    /// only milestone identity and commit metadata — never a path or remote.
    MilestoneCommitCreated {
        id: String,
        order: u32,
        title: String,
        commit: MilestoneCommit,
    },

    /// Task 0012: the acceptance criteria this run must satisfy, generated
    /// from the approved specification before any milestone executes.
    AcceptanceCriteriaGenerated {
        criteria: Vec<crate::acceptance::AcceptanceCriterion>,
    },
    /// One criterion changed state. `evidence` retains which independent source
    /// supports it, never a copy of a verification log.
    AcceptanceCriterionUpdated {
        id: String,
        status: crate::acceptance::CriterionStatus,
        evidence: Option<crate::acceptance::CriterionEvidence>,
        /// A critic finding that now blocks this criterion from passing.
        blocking_finding: Option<String>,
        /// Set when a passing review cleared the findings against it.
        findings_cleared: bool,
    },

    /// Task 0011: the critic reviewing the implemented milestone, and the
    /// bounded fix cycle its findings drive. `iteration` is 0 for the first
    /// review and counts the fixes applied before each later one.
    ImplementationReviewStarted {
        milestone_id: String,
        order: u32,
        iteration: u32,
        of: u32,
        provider: crate::agent::ChatProvider,
        model: String,
    },
    ImplementationReviewCompleted {
        milestone_id: String,
        order: u32,
        iteration: u32,
        of: u32,
        status: crate::review::ReviewStatus,
        findings: Vec<crate::review::Finding>,
    },
    ImplementationReviewFailed {
        milestone_id: String,
        order: u32,
        iteration: u32,
        of: u32,
        provider: crate::agent::ChatProvider,
        model: String,
        error: String,
    },
    FixStarted {
        milestone_id: String,
        order: u32,
        iteration: u32,
        of: u32,
        tool: crate::agent::CodingTool,
        model: String,
    },
    FixCompleted {
        milestone_id: String,
        order: u32,
        iteration: u32,
        of: u32,
        tool: crate::agent::CodingTool,
        model: String,
    },
    FixFailed {
        milestone_id: String,
        order: u32,
        iteration: u32,
        of: u32,
        tool: crate::agent::CodingTool,
        model: String,
        error: String,
    },

    /// Task 0013: submission documentation written into the finished project.
    /// Paths are project-relative; `preserved` names documentation that already
    /// existed and was therefore left exactly as it was.
    SubmissionDocumentationGenerated {
        written: Vec<String>,
        preserved: Vec<String>,
    },

    /// Persistent New Project output (task 0010). `destination` is always the
    /// user's chosen folder, never an internal temporary path.
    ProjectPersistenceStarted {
        destination: String,
    },
    ProjectPersisted {
        destination: String,
        git: Option<crate::git::RepositoryStatus>,
        /// Present when the project reached its destination but its repository
        /// could not be inspected. Persistence still succeeded.
        git_warning: Option<String>,
    },
    ProjectPersistenceFailed {
        destination: String,
        error: String,
    },

    /// Explicit, user-confirmed publication lifecycle. These events contain
    /// repository identity and commit metadata only; never credentials or
    /// local filesystem paths.
    GitHubPublishStarted {
        repository: String,
        branch: String,
        commit_sha: String,
    },
    GitHubPublishCompleted {
        publication: GitHubPublication,
    },
    GitHubPublishFailed {
        repository: Option<String>,
        branch: Option<String>,
        error: String,
    },

    Inspection {
        profile: ProjectProfile,
        source_revision: Option<String>,
    },

    Verification {
        result: VerificationResult,
    },
    VerificationStarted {
        commands: usize,
    },
    VerificationCompleted {
        commands: usize,
    },
    VerificationFailed {
        command: Option<String>,
        error: String,
    },

    WorkerStarted {
        tool: crate::agent::CodingTool,
        model: String,
    },
    WorkerCompleted {
        tool: crate::agent::CodingTool,
        model: String,
    },
    WorkerFailed {
        tool: crate::agent::CodingTool,
        model: String,
        error: String,
    },
    WorkerCancelled {
        tool: crate::agent::CodingTool,
        model: String,
    },

    Result {
        result: TaskResult,
    },

    /// A chunk of Claude Code's output while it builds.
    Build {
        chunk: String,
    },

    /// Progress chatter — the grey lines v1 printed via `ui::system`.
    Notice {
        message: String,
    },

    /// Something the user should notice but that did not stop the run.
    Warning {
        message: String,
    },

    /// The run ended. `error` is set only when `status` is `Failed`.
    Finished {
        status: TaskStatus,
        error: Option<String>,
    },
    TaskCompleted,
    TaskFailed {
        error: String,
    },
    TaskCancelled,
    EvidenceExported {
        artifact: String,
    },
}

impl TaskEvent {
    fn log_text(&self) -> Option<&str> {
        match self {
            Self::Build { chunk } => Some(chunk),
            Self::Notice { message } | Self::Warning { message } => Some(message),
            _ => None,
        }
    }

    fn bounded(mut self, limits: HistoryLimits) -> Self {
        match &mut self {
            Self::Build { chunk } => *chunk = bounded_text(chunk, limits.event_bytes),
            Self::Notice { message } | Self::Warning { message } => {
                *message = bounded_text(message, limits.event_bytes)
            }
            _ => {}
        }
        self
    }

    fn sanitized(mut self, redactor: &AuditRedactor) -> Self {
        let clean = |text: &mut String| *text = redactor.redact(text);
        match &mut self {
            Self::Proposal { text, .. } => clean(text),
            Self::Critique { text, reason, .. } => {
                clean(text);
                if let Some(reason) = reason {
                    clean(reason);
                }
            }
            Self::Spec { markdown, path } => {
                clean(markdown);
                clean(path);
            }
            Self::SpecApproved { markdown } => clean(markdown),
            Self::GitHubPublishFailed { error, .. } => clean(error),
            Self::GitHubPublishStarted { .. } | Self::GitHubPublishCompleted { .. } => {}
            Self::ProposerStarted { model, .. }
            | Self::ProposerCompleted { model, .. }
            | Self::CriticStarted { model, .. }
            | Self::CriticCompleted { model, .. }
            | Self::WorkerStarted { model, .. }
            | Self::WorkerCompleted { model, .. }
            | Self::WorkerCancelled { model, .. } => clean(model),
            Self::ProposerFailed { model, error, .. }
            | Self::CriticFailed { model, error, .. }
            | Self::WorkerFailed { model, error, .. } => {
                clean(model);
                clean(error);
            }
            Self::Inspection {
                source_revision, ..
            } => {
                if let Some(source_revision) = source_revision {
                    clean(source_revision);
                }
            }
            Self::Verification { result } => {
                clean(&mut result.command);
                clean(&mut result.output);
            }
            Self::VerificationFailed { command, error } => {
                if let Some(command) = command {
                    clean(command);
                }
                clean(error);
            }
            Self::Result { result } => {
                if let Some(source_revision) = &mut result.source_revision {
                    clean(source_revision);
                }
                clean(&mut result.diff);
                for verification in &mut result.verification {
                    clean(&mut verification.command);
                    clean(&mut verification.output);
                }
            }
            Self::MilestonePlanCreated { milestones } => {
                for milestone in milestones {
                    clean(&mut milestone.title);
                    clean(&mut milestone.objective);
                    for instruction in &mut milestone.verification_instructions {
                        clean(instruction);
                    }
                    if let Some(summary) = &mut milestone.worker_result_summary {
                        clean(summary);
                    }
                }
            }
            Self::MilestoneStarted {
                id,
                title,
                worker_model,
                ..
            } => {
                clean(id);
                clean(title);
                clean(worker_model);
            }
            Self::MilestoneCompleted {
                id,
                title,
                verification,
                worker_result_summary,
                ..
            } => {
                clean(id);
                clean(title);
                clean(worker_result_summary);
                for result in verification {
                    clean(&mut result.command);
                    clean(&mut result.output);
                }
            }
            Self::MilestoneFailed {
                id,
                title,
                verification,
                worker_result_summary,
                error,
                ..
            } => {
                clean(id);
                clean(title);
                if let Some(summary) = worker_result_summary {
                    clean(summary);
                }
                clean(error);
                for result in verification {
                    clean(&mut result.command);
                    clean(&mut result.output);
                }
            }
            Self::MilestoneCancelled {
                id, title, reason, ..
            } => {
                clean(id);
                clean(title);
                clean(reason);
            }
            Self::MilestoneCommitCreated {
                id, title, commit, ..
            } => {
                clean(id);
                clean(title);
                clean(&mut commit.message);
            }
            Self::AcceptanceCriteriaGenerated { criteria } => {
                for criterion in criteria {
                    clean(&mut criterion.id);
                    clean(&mut criterion.description);
                    for evidence in &mut criterion.evidence {
                        clean(&mut evidence.summary);
                    }
                    for finding in &mut criterion.blocking_findings {
                        clean(finding);
                    }
                }
            }
            Self::AcceptanceCriterionUpdated {
                id,
                evidence,
                blocking_finding,
                ..
            } => {
                clean(id);
                if let Some(evidence) = evidence {
                    clean(&mut evidence.summary);
                }
                if let Some(finding) = blocking_finding {
                    clean(finding);
                }
            }
            Self::ImplementationReviewStarted {
                milestone_id,
                model,
                ..
            } => {
                clean(milestone_id);
                clean(model);
            }
            Self::ImplementationReviewCompleted {
                milestone_id,
                findings,
                ..
            } => {
                clean(milestone_id);
                for finding in findings {
                    clean(&mut finding.requirement);
                    clean(&mut finding.evidence);
                    clean(&mut finding.correction);
                }
            }
            Self::ImplementationReviewFailed {
                milestone_id,
                model,
                error,
                ..
            } => {
                clean(milestone_id);
                clean(model);
                clean(error);
            }
            Self::FixStarted {
                milestone_id,
                model,
                ..
            }
            | Self::FixCompleted {
                milestone_id,
                model,
                ..
            } => {
                clean(milestone_id);
                clean(model);
            }
            Self::FixFailed {
                milestone_id,
                model,
                error,
                ..
            } => {
                clean(milestone_id);
                clean(model);
                clean(error);
            }
            Self::SubmissionDocumentationGenerated { written, preserved } => {
                for file in written.iter_mut().chain(preserved.iter_mut()) {
                    clean(file);
                }
            }
            Self::ProjectPersistenceStarted { destination } => clean(destination),
            Self::ProjectPersisted {
                destination,
                git_warning,
                ..
            } => {
                clean(destination);
                if let Some(git_warning) = git_warning {
                    clean(git_warning);
                }
            }
            Self::ProjectPersistenceFailed { destination, error } => {
                clean(destination);
                clean(error);
            }
            Self::Build { chunk } => clean(chunk),
            Self::Notice { message } | Self::Warning { message } => clean(message),
            Self::Finished { error, .. } => {
                if let Some(error) = error {
                    clean(error);
                }
            }
            Self::TaskFailed { error } => clean(error),
            Self::EvidenceExported { artifact } => clean(artifact),
            Self::AgentsSelected { agents } => {
                clean(&mut agents.proposer.model);
                clean(&mut agents.critic.model);
                clean(&mut agents.worker.model);
            }
            Self::TaskCreated { .. }
            | Self::TaskStarted
            | Self::Status { .. }
            | Self::RoundStarted { .. }
            | Self::SpecGenerated
            | Self::SpecUpdated
            | Self::SpecRejected
            | Self::VerificationStarted { .. }
            | Self::VerificationCompleted { .. }
            | Self::TaskCompleted
            | Self::TaskCancelled => {}
        }
        self
    }
}

/// Backend-authored metadata shared by every stored and streamed event.
#[derive(Debug, Clone, Serialize)]
pub struct RecordedEvent {
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub event: TaskEvent,
}

/// Values and common credential syntax removed before an event enters task
/// state or the live stream. Debug output intentionally never exposes values.
#[derive(Clone, Default)]
pub(crate) struct AuditRedactor {
    values: Vec<String>,
}

impl std::fmt::Debug for AuditRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditRedactor")
            .field("values", &format_args!("<{} redacted>", self.values.len()))
            .finish()
    }
}

impl AuditRedactor {
    fn new(values: impl IntoIterator<Item = String>) -> Self {
        Self {
            values: values
                .into_iter()
                // Real provider credentials are long. Ignoring tiny test or
                // placeholder values avoids corrupting ordinary words such as
                // "test" while generic KEY=/Bearer syntax is still covered.
                .filter(|value| value.len() >= 8)
                .collect(),
        }
    }

    pub(crate) fn redact(&self, text: &str) -> String {
        let mut redacted = text.to_string();
        for value in &self.values {
            redacted = redacted.replace(value, "[REDACTED]");
        }
        redacted
            .split_inclusive('\n')
            .map(redact_credential_line)
            .collect()
    }
}

fn redact_credential_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let assignment_markers = [
        "api_key=",
        "api-key=",
        "api key ",
        "api key:",
        "access_token=",
        "access-token=",
        "auth_token=",
        "github_token=",
        "gh_token=",
        "authorization=",
        "authorization:",
        "password=",
        "password:",
        "secret=",
        "secret:",
        "database_url=",
        "database-url=",
        "aws_secret_access_key=",
        "google_application_credentials=",
    ];
    if let Some(index) = assignment_markers
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
    {
        let newline = if line.ends_with('\n') { "\n" } else { "" };
        return format!("{}[REDACTED]{newline}", &line[..index]);
    }
    if lower.contains("bearer ") {
        let newline = if line.ends_with('\n') { "\n" } else { "" };
        return format!("[REDACTED authorization]{newline}");
    }
    redact_token_prefixes(line)
}

fn redact_token_prefixes(line: &str) -> String {
    let mut redacted = line.to_string();
    for prefix in ["sk-", "ghp_", "github_pat_", "AIza"] {
        let mut search_from = 0;
        while let Some(relative) = redacted[search_from..].find(prefix) {
            let start = search_from + relative;
            let end = redacted[start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '\"' | '\'' | '`' | ','))
                .map(|offset| start + offset)
                .unwrap_or(redacted.len());
            // Avoid turning ordinary short prose fragments into secrets.
            if end.saturating_sub(start) >= 12 {
                redacted.replace_range(start..end, "[REDACTED]");
                search_from = start + "[REDACTED]".len();
            } else {
                search_from = end;
            }
        }
    }
    redacted
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// The human's answer at Gate 2 (DP-10).
///
/// `spec` carries an edited document. When present it becomes the artifact before
/// the build starts, so editing and approving are one atomic action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub approve: bool,
    #[serde(default)]
    pub spec: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionError {
    NotFound,
    NotWaiting,
    InvalidSpec,
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotFound => "unknown task",
            Self::NotWaiting => "task is not waiting for an unanswered approval",
            Self::InvalidSpec => "approval requires a non-empty specification",
        })
    }
}

/// One orchestration run, and enough history to render the page on a fresh
/// load or a reconnect.
///
/// DP-8: `title` and `description` are captured separately in the UI and joined
/// into one topic string for the models — see `Task::topic`.
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub description: String,
    pub kind: TaskKind,
    pub project_id: Option<ProjectId>,
    pub technology: Option<TechStack>,
    pub output: Option<OutputTarget>,
    /// The persistent destination folder name this run asked for (task 0010).
    pub destination: Option<String>,
    /// How persistent output went, once it has been attempted (task 0010).
    pub persistence: Option<ProjectPersistence>,
    /// Publication metadata appears only after the explicit GitHub action
    /// succeeds. It is safe to expose in API/UI snapshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_publication: Option<GitHubPublication>,
    /// The agents this run uses, resolved once at creation and never re-read
    /// from the environment afterwards (task 0005).
    pub agents: AgentSelection,
    pub profile: Option<ProjectProfile>,
    pub result: Option<TaskResult>,
    pub status: TaskStatus,
    /// Significant events are append-only for the lifetime of this task.
    pub history: Vec<RecordedEvent>,
    /// Detailed agent/worker evidence is durable for this in-memory task but is
    /// deliberately omitted from ordinary task snapshots and the live UI.
    #[serde(skip)]
    pub evidence: Vec<EvidenceRecord>,
    /// Repetitive UI output remains bounded independently from the audit log.
    pub log_tail: Vec<RecordedEvent>,
    pub discarded_log_events: usize,
    #[serde(skip)]
    history_limits: HistoryLimits,
    #[serde(skip)]
    next_event_sequence: u64,
    #[serde(skip)]
    worker_output_truncated: bool,
    pub spec: Option<String>,
    pub error: Option<String>,
    /// Set once the human answers Gate 2 (DP-11).
    pub decision: Option<Decision>,
    /// Ordered approved-spec execution plan and live milestone state.
    pub milestones: Vec<Milestone>,
    /// What this run must satisfy, and how far each criterion has got (0012).
    pub acceptance: Vec<crate::acceptance::AcceptanceCriterion>,
    /// The Git behavior chosen for this run, frozen at creation (task 0009).
    pub git_mode: GitMode,
    /// Present only for Take-home Assignment tasks. It is recomputed from the
    /// same task state shown elsewhere, so UI, API snapshots and evidence do
    /// not drift into a hard-coded success summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_checklist: Option<CompletionChecklist>,
    #[serde(skip)]
    cancelled: bool,
}

impl Task {
    #[cfg(test)]
    fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        _legacy_project: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            description: description.into(),
            kind: TaskKind::NewProject,
            project_id: None,
            technology: Some(TechStack::Rust),
            output: Some(OutputTarget::ReviewableResult),
            destination: None,
            persistence: None,
            github_publication: None,
            agents: AgentSelection::compiled_defaults(),
            profile: None,
            result: None,
            status: TaskStatus::Created,
            history: Vec::new(),
            evidence: Vec::new(),
            log_tail: Vec::new(),
            discarded_log_events: 0,
            history_limits: HistoryLimits::default(),
            next_event_sequence: 1,
            worker_output_truncated: false,
            spec: None,
            error: None,
            decision: None,
            milestones: Vec::new(),
            acceptance: Vec::new(),
            git_mode: GitMode::None,
            completion_checklist: None,
            cancelled: false,
        }
    }

    /// `agents` is resolved by the caller against the `AgentCatalogue`, so the
    /// domain never has to reach for the environment (task 0005).
    pub fn from_request(request: TaskRequest, agents: AgentSelection) -> Result<Self, String> {
        request.validate()?;
        let output = request.effective_output();
        let git_mode = request.effective_git_mode();
        let completion_checklist =
            request
                .kind
                .is_take_home_assignment()
                .then_some(CompletionChecklist {
                    implementation_complete: false,
                    verification_complete: false,
                    acceptance_criteria_reviewed: false,
                    final_critic_review_complete: false,
                    documentation_generated: false,
                    evidence_export_available: false,
                    git_history_available: false,
                });
        Ok(Task {
            id: Uuid::new_v4(),
            title: request.title.trim().to_string(),
            description: request.description.trim().to_string(),
            kind: request.kind,
            project_id: request.project_id,
            technology: request.technology,
            output,
            destination: request
                .destination
                .map(|destination| destination.trim().to_string()),
            persistence: None,
            github_publication: None,
            agents,
            git_mode,
            profile: None,
            result: None,
            status: TaskStatus::Created,
            history: Vec::new(),
            evidence: Vec::new(),
            log_tail: Vec::new(),
            discarded_log_events: 0,
            history_limits: HistoryLimits::default(),
            next_event_sequence: 1,
            worker_output_truncated: false,
            spec: None,
            error: None,
            decision: None,
            milestones: Vec::new(),
            acceptance: Vec::new(),
            completion_checklist,
            cancelled: false,
        })
    }

    /// The public take-home checklist for this task. Non-take-home tasks have
    /// no implied delivery checklist and therefore return `None`.
    pub fn take_home_completion_checklist(&self) -> Option<CompletionChecklist> {
        self.kind.is_take_home_assignment().then(|| {
            let implementation_complete = !self.milestones.is_empty()
                && self
                    .milestones
                    .iter()
                    .all(|milestone| milestone.status == MilestoneStatus::Passed);
            let verification_complete = implementation_complete
                && self.history.iter().any(|recorded| {
                    matches!(recorded.event, TaskEvent::VerificationCompleted { .. })
                })
                && !self
                    .history
                    .iter()
                    .any(|recorded| matches!(recorded.event, TaskEvent::VerificationFailed { .. }));
            let acceptance_criteria_reviewed = !self.acceptance.is_empty()
                && self.acceptance.iter().all(|criterion| {
                    !matches!(
                        criterion.status,
                        crate::acceptance::CriterionStatus::Pending
                            | crate::acceptance::CriterionStatus::Implemented
                    )
                });
            let final_critic_review_complete = implementation_complete
                && self.milestones.iter().all(|milestone| {
                    milestone
                        .review
                        .as_ref()
                        .is_some_and(|review| review.status.is_pass())
                });
            let documentation_generated = self.history.iter().any(|recorded| {
                matches!(
                    recorded.event,
                    TaskEvent::SubmissionDocumentationGenerated { .. }
                )
            });
            let evidence_export_available =
                self.status == TaskStatus::Completed && self.result.is_some();
            let git_history_available = self.persistence.as_ref().is_some_and(|persistence| {
                persistence.status == PersistenceStatus::Persisted
                    && persistence.git.as_ref().is_some_and(|git| git.commits > 0)
            });
            CompletionChecklist {
                implementation_complete,
                verification_complete,
                acceptance_criteria_reviewed,
                final_critic_review_complete,
                documentation_generated,
                evidence_export_available,
                git_history_available,
            }
        })
    }

    /// What the models are actually asked about (DP-8).
    ///
    /// The title alone is usually too thin to design from, and the description
    /// alone loses the headline, so they are joined rather than picked between.
    pub fn topic(&self) -> String {
        if self.description.trim().is_empty() {
            self.title.clone()
        } else {
            format!("{}\n\n{}", self.title.trim(), self.description.trim())
        }
    }

    /// Fold an event into the task, assigning its immutable audit envelope.
    pub fn apply(&mut self, event: &TaskEvent) {
        self.record_event(event.clone());
    }

    fn record_event(&mut self, event: TaskEvent) -> RecordedEvent {
        let timestamp = DateTime::<Utc>::from(std::time::SystemTime::now());
        match event {
            TaskEvent::Status { status } => self.status = status,
            TaskEvent::Spec { ref markdown, .. } | TaskEvent::SpecApproved { ref markdown } => {
                self.spec = Some(markdown.clone());
            }
            TaskEvent::Inspection { ref profile, .. } => self.profile = Some(profile.clone()),
            TaskEvent::Result { ref result } => self.result = Some(result.clone()),
            TaskEvent::Finished { status, ref error } => {
                self.status = status;
                self.error = error.clone();
            }
            TaskEvent::TaskCompleted => {
                self.status = TaskStatus::Completed;
                self.error = None;
            }
            TaskEvent::TaskFailed { ref error } => {
                self.status = TaskStatus::Failed;
                self.error = Some(error.clone());
            }
            TaskEvent::TaskCancelled => {
                self.status = TaskStatus::Cancelled;
                self.cancelled = true;
                self.error = None;
                for milestone in &mut self.milestones {
                    if milestone.status == MilestoneStatus::Running {
                        milestone.status = MilestoneStatus::Cancelled;
                        milestone.completed_at = Some(timestamp);
                    }
                }
            }
            TaskEvent::MilestonePlanCreated { ref milestones } => {
                self.milestones = milestones.clone();
            }
            TaskEvent::MilestoneStarted { ref id, .. } => {
                if let Some(milestone) = self.milestones.iter_mut().find(|item| item.id == *id) {
                    milestone.status = MilestoneStatus::Running;
                    milestone.started_at = Some(timestamp);
                }
            }
            TaskEvent::MilestoneCompleted {
                ref id,
                ref worker_result_summary,
                ..
            } => {
                if let Some(milestone) = self.milestones.iter_mut().find(|item| item.id == *id) {
                    milestone.status = MilestoneStatus::Passed;
                    milestone.completed_at = Some(timestamp);
                    milestone.worker_result_summary = Some(worker_result_summary.clone());
                }
            }
            TaskEvent::MilestoneFailed {
                ref id,
                ref worker_result_summary,
                ..
            } => {
                if let Some(milestone) = self.milestones.iter_mut().find(|item| item.id == *id) {
                    milestone.status = MilestoneStatus::Failed;
                    milestone.completed_at = Some(timestamp);
                    milestone.worker_result_summary = worker_result_summary.clone();
                }
            }
            TaskEvent::MilestoneCancelled { ref id, .. } => {
                if let Some(milestone) = self.milestones.iter_mut().find(|item| item.id == *id) {
                    milestone.status = MilestoneStatus::Cancelled;
                    milestone.completed_at = Some(timestamp);
                }
            }
            // Task 0012: criterion state is rebuilt from its own events, so a
            // snapshot can never disagree with the audit about what passed.
            TaskEvent::AcceptanceCriteriaGenerated { ref criteria } => {
                self.acceptance = criteria.clone();
            }
            TaskEvent::AcceptanceCriterionUpdated {
                ref id,
                status,
                ref evidence,
                ref blocking_finding,
                findings_cleared,
            } => {
                if let Some(criterion) = self.acceptance.iter_mut().find(|item| item.id == *id) {
                    criterion.status = status;
                    if findings_cleared {
                        criterion.blocking_findings.clear();
                    }
                    if let Some(finding) = blocking_finding
                        && !criterion.blocking_findings.contains(finding)
                    {
                        criterion.blocking_findings.push(finding.clone());
                    }
                    if let Some(evidence) = evidence
                        && !criterion.evidence.contains(evidence)
                    {
                        criterion.evidence.push(evidence.clone());
                    }
                }
            }
            // Task 0011: the milestone carries the latest critic disposition,
            // so the UI and the export never re-derive it from raw events.
            TaskEvent::ImplementationReviewCompleted {
                ref milestone_id,
                iteration,
                of,
                status,
                ref findings,
                ..
            } => {
                if let Some(milestone) = self
                    .milestones
                    .iter_mut()
                    .find(|item| item.id == *milestone_id)
                {
                    milestone.review = Some(crate::review::MilestoneReview {
                        status,
                        iterations_used: iteration,
                        max_iterations: of,
                        findings: findings.clone(),
                    });
                }
            }
            // Task 0010: persistent-output state is rebuilt from its events, so
            // a snapshot and the audit log can never disagree about it.
            TaskEvent::ProjectPersistenceStarted { ref destination } => {
                self.persistence = Some(ProjectPersistence {
                    mode: OutputTarget::PersistentLocalProject,
                    status: PersistenceStatus::Started,
                    destination: destination.clone(),
                    git: None,
                    git_warning: None,
                    error: None,
                });
            }
            TaskEvent::ProjectPersisted {
                ref destination,
                ref git,
                ref git_warning,
            } => {
                self.persistence = Some(ProjectPersistence {
                    mode: OutputTarget::PersistentLocalProject,
                    status: PersistenceStatus::Persisted,
                    destination: destination.clone(),
                    git: git.clone(),
                    git_warning: git_warning.clone(),
                    error: None,
                });
            }
            TaskEvent::ProjectPersistenceFailed {
                ref destination,
                ref error,
            } => {
                self.persistence = Some(ProjectPersistence {
                    mode: OutputTarget::PersistentLocalProject,
                    status: PersistenceStatus::Failed,
                    destination: destination.clone(),
                    git: None,
                    git_warning: None,
                    error: Some(error.clone()),
                });
            }
            TaskEvent::GitHubPublishCompleted { ref publication } => {
                self.github_publication = Some(publication.clone());
            }
            TaskEvent::MilestoneCommitCreated {
                ref id, ref commit, ..
            } => {
                if let Some(milestone) = self.milestones.iter_mut().find(|item| item.id == *id) {
                    milestone.commit = Some(commit.clone());
                }
            }
            _ => {}
        }
        self.completion_checklist = self.take_home_completion_checklist();
        if matches!(&event, TaskEvent::Build { chunk } if chunk.contains(crate::execution_limits::TRUNCATED))
        {
            self.worker_output_truncated = true;
        }
        let (sequence, _) = self.next_audit_metadata();
        let recorded = RecordedEvent {
            sequence,
            timestamp,
            event,
        };
        if recorded.event.log_text().is_some() {
            self.log_tail.push(recorded.clone());
        } else {
            self.history.push(recorded.clone());
        }
        let (mut count, mut bytes) = self
            .log_tail
            .iter()
            .filter_map(|recorded| recorded.event.log_text())
            .fold((0, 0), |(count, bytes), text| {
                (count + 1, bytes + text.len())
            });
        while count > self.history_limits.log_events || bytes > self.history_limits.log_bytes {
            let Some(index) = self
                .log_tail
                .iter()
                .position(|recorded| recorded.event.log_text().is_some())
            else {
                break;
            };
            bytes -= self.log_tail[index]
                .event
                .log_text()
                .expect("log event")
                .len();
            self.log_tail.remove(index);
            count -= 1;
            self.discarded_log_events += 1;
        }
        recorded
    }

    fn record_evidence(
        &mut self,
        payload: EvidencePayload,
        redactor: &AuditRedactor,
    ) -> EvidenceRecord {
        let (sequence, timestamp) = self.next_audit_metadata();
        let record = EvidenceRecord::new(sequence, timestamp, payload)
            .sanitized_and_bounded(|text| redactor.redact(text), self.worker_output_truncated);
        self.evidence.push(record.clone());
        record
    }

    fn next_audit_metadata(&mut self) -> (u64, DateTime<Utc>) {
        let sequence = self.next_event_sequence;
        self.next_event_sequence = self
            .next_event_sequence
            .checked_add(1)
            .expect("task audit sequence exhausted");
        (
            sequence,
            DateTime::<Utc>::from(std::time::SystemTime::now()),
        )
    }

    fn sanitize_for_export(&mut self, redactor: &AuditRedactor) {
        let clean = |text: &mut String| *text = redactor.redact(text);
        clean(&mut self.title);
        clean(&mut self.description);
        clean(&mut self.agents.proposer.model);
        clean(&mut self.agents.critic.model);
        clean(&mut self.agents.worker.model);
        if let Some(spec) = &mut self.spec {
            clean(spec);
        }
        if let Some(error) = &mut self.error {
            clean(error);
        }
        if let Some(destination) = &mut self.destination {
            clean(destination);
        }
        if let Some(persistence) = &mut self.persistence {
            clean(&mut persistence.destination);
            if let Some(warning) = &mut persistence.git_warning {
                clean(warning);
            }
            if let Some(error) = &mut persistence.error {
                clean(error);
            }
        }
        if let Some(decision) = &mut self.decision
            && let Some(spec) = &mut decision.spec
        {
            clean(spec);
        }
        if let Some(result) = &mut self.result {
            if let Some(revision) = &mut result.source_revision {
                clean(revision);
            }
            clean(&mut result.diff);
            for verification in &mut result.verification {
                clean(&mut verification.command);
                clean(&mut verification.output);
            }
        }
        for criterion in &mut self.acceptance {
            clean(&mut criterion.id);
            clean(&mut criterion.description);
            for evidence in &mut criterion.evidence {
                clean(&mut evidence.summary);
            }
            for finding in &mut criterion.blocking_findings {
                clean(finding);
            }
        }
        for milestone in &mut self.milestones {
            clean(&mut milestone.id);
            clean(&mut milestone.title);
            clean(&mut milestone.objective);
            for instruction in &mut milestone.verification_instructions {
                clean(instruction);
            }
            if let Some(summary) = &mut milestone.worker_result_summary {
                clean(summary);
            }
            for criterion in &mut milestone.criteria {
                clean(criterion);
            }
            if let Some(review) = &mut milestone.review {
                for finding in &mut review.findings {
                    clean(&mut finding.requirement);
                    clean(&mut finding.evidence);
                    clean(&mut finding.correction);
                }
            }
        }
    }

    /// Significant events followed by the bounded UI log tail, in backend
    /// recording order. This is a snapshot iterator; it never changes storage.
    pub fn display_history(&self) -> Vec<&RecordedEvent> {
        let mut events = self
            .history
            .iter()
            .chain(self.log_tail.iter())
            .collect::<Vec<_>>();
        events.sort_unstable_by_key(|event| event.sequence);
        events
    }
}

// ---------------------------------------------------------------------------
// Emitter (DP-9)
// ---------------------------------------------------------------------------

/// The handle a pipeline stage uses to report what it is doing.
///
/// Cloning is cheap — it is an `Arc` plus an id, so each stage can hold its own.
#[derive(Debug, Clone)]
pub struct Emitter {
    id: TaskId,
    inner: Arc<Inner>,
}

impl Emitter {
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Record an event and publish it, in that order.
    ///
    /// Recording first matters: a browser that fetches the snapshot immediately
    /// after seeing an event must never find a task that has not caught up yet.
    ///
    /// A send failure only means nobody is subscribed right now, which is normal
    /// (the CLI has no subscribers at all). It must never abort the pipeline, so
    /// the result is deliberately discarded.
    pub fn emit(&self, event: TaskEvent) {
        self.inner.record_and_publish(self.id, event);
    }

    pub fn status(&self, status: TaskStatus) {
        self.emit(TaskEvent::Status { status });
    }

    pub fn notice(&self, message: impl Into<String>) {
        self.emit(TaskEvent::Notice {
            message: message.into(),
        });
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.emit(TaskEvent::Warning {
            message: message.into(),
        });
    }

    /// Retain detailed export evidence without publishing it to the bounded UI
    /// stream. Ordering still comes from the task's shared audit allocator.
    pub fn record_evidence(&self, payload: EvidencePayload) {
        self.inner.record_evidence(self.id, payload);
    }

    /// An emitter attached to no task, for tests and for CLI code paths that do
    /// not care. `emit` stays harmless: nothing is recorded, nobody listens.
    pub fn detached() -> Self {
        Emitter {
            id: Uuid::new_v4(),
            inner: Arc::new(Inner::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// TaskManager
// ---------------------------------------------------------------------------

/// The shared state behind every handle. Kept private so nothing outside this
/// module can hold the lock or reach the channel directly.
#[derive(Debug)]
struct Inner {
    history_limits: HistoryLimits,
    tasks: RwLock<HashMap<TaskId, Task>>,
    tx: broadcast::Sender<(TaskId, RecordedEvent)>,
    redactor: AuditRedactor,
    /// One waker per task, used to unpark a pipeline sitting at Gate 2.
    gates: RwLock<HashMap<TaskId, Arc<Notify>>>,
}

impl Inner {
    fn new() -> Self {
        // The receiver returned here is dropped straight away. That is fine:
        // `broadcast::Sender::send` works with no receivers, it just reports
        // that nobody heard it.
        let (tx, _rx) = broadcast::channel(EVENT_BUFFER);
        Inner {
            history_limits: HistoryLimits::default(),
            tasks: RwLock::new(HashMap::new()),
            tx,
            redactor: AuditRedactor::default(),
            gates: RwLock::new(HashMap::new()),
        }
    }

    /// The waker for a task, created on first use.
    fn gate(&self, id: TaskId) -> Arc<Notify> {
        let mut gates = self.gates.write().expect("gate registry lock poisoned");
        Arc::clone(gates.entry(id).or_insert_with(|| Arc::new(Notify::new())))
    }

    /// Assign sequence/timestamp, store, and publish while holding one lock.
    /// This makes recording order and broadcast order identical for a task.
    fn record_and_publish(&self, id: TaskId, event: TaskEvent) {
        let mut tasks = self.tasks.write().expect("task registry lock poisoned");
        if let Some(task) = tasks.get_mut(&id) {
            let event = event.sanitized(&self.redactor).bounded(self.history_limits);
            let recorded = task.record_event(event);
            let _ = self.tx.send((id, recorded));
        }
    }

    fn record_evidence(&self, id: TaskId, payload: EvidencePayload) {
        let mut tasks = self.tasks.write().expect("task registry lock poisoned");
        if let Some(task) = tasks.get_mut(&id) {
            task.record_evidence(payload, &self.redactor);
        }
    }
}

/// The registry of tasks and the one channel every watcher subscribes to.
///
/// Cloning a `TaskManager` shares the same state — it is an `Arc` inside — so
/// axum can hand a clone to every request handler.
#[derive(Debug, Clone)]
pub struct TaskManager {
    inner: Arc<Inner>,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    pub fn with_history_limits(history_limits: HistoryLimits) -> Self {
        Self::with_history_limits_and_secrets(history_limits, std::iter::empty::<String>())
    }

    pub fn with_history_limits_and_secrets(
        history_limits: HistoryLimits,
        secrets: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut inner = Inner::new();
        inner.history_limits = history_limits;
        inner.redactor = AuditRedactor::new(secrets);
        Self {
            inner: Arc::new(inner),
        }
    }
    pub fn new() -> Self {
        TaskManager {
            inner: Arc::new(Inner::new()),
        }
    }

    /// Legacy helper retained for internal v2 tests and CLI-era call sites.
    /// Production handlers use `create_from_request` and never accept paths.
    pub fn create(
        &self,
        title: impl Into<String>,
        description: impl Into<String>,
        _project: impl Into<String>,
    ) -> Task {
        let description = description.into();
        let task = Task::from_request(
            TaskRequest {
                kind: TaskKind::NewProject,
                title: title.into(),
                description: if description.trim().is_empty() {
                    "Legacy task".into()
                } else {
                    description
                },
                project_id: None,
                technology: Some(TechStack::Rust),
                output: Some(OutputTarget::ReviewableResult),
                destination: None,
                agents: None,
                git_mode: None,
            },
            AgentSelection::compiled_defaults(),
        )
        .expect("legacy task input is valid");
        self.insert(task)
    }

    /// `agents` comes from `AgentCatalogue::resolve`, so the stored task is
    /// already frozen against later configuration changes (task 0005).
    pub fn create_from_request(
        &self,
        request: TaskRequest,
        agents: AgentSelection,
    ) -> Result<Task, String> {
        let task = Task::from_request(request, agents)?;
        Ok(self.insert(task))
    }

    fn insert(&self, mut task: Task) -> Task {
        task.history_limits = self.inner.history_limits;
        let created = TaskEvent::TaskCreated {
            kind: task.kind,
            output: task.output,
            git_mode: task.git_mode,
        }
        .sanitized(&self.inner.redactor)
        .bounded(self.inner.history_limits);
        task.record_event(created);
        let mut tasks = self
            .inner
            .tasks
            .write()
            .expect("task registry lock poisoned");
        tasks.insert(task.id, task.clone());
        task
    }

    /// A snapshot of one task. Cloned, so the caller never holds the lock.
    pub fn get(&self, id: TaskId) -> Option<Task> {
        let tasks = self
            .inner
            .tasks
            .read()
            .expect("task registry lock poisoned");
        tasks.get(&id).cloned()
    }

    /// A fully sanitized snapshot to render an evidence archive from.
    ///
    /// Reading is side-effect free: the audit event belongs to a SUCCESSFUL
    /// export, so it is recorded afterwards by `record_evidence_export`. A
    /// failed archive generation therefore leaves no trace of a successful one.
    pub fn evidence_snapshot(&self, id: TaskId) -> Option<Task> {
        let tasks = self
            .inner
            .tasks
            .read()
            .expect("task registry lock poisoned");
        let mut snapshot = tasks.get(&id)?.clone();
        snapshot.sanitize_for_export(&self.inner.redactor);
        Some(snapshot)
    }

    /// Record that an evidence archive was successfully generated.
    ///
    /// Idempotent: the event is recorded at most once per task, so repeated
    /// downloads of a finished task keep producing the same archive. As the
    /// event is recorded after the archive it describes, it becomes visible in
    /// the NEXT export, never in the one that produced it.
    ///
    /// Returns whether this call was the one that recorded the event.
    pub fn record_evidence_export(&self, id: TaskId) -> bool {
        let mut tasks = self
            .inner
            .tasks
            .write()
            .expect("task registry lock poisoned");
        let Some(task) = tasks.get_mut(&id) else {
            return false;
        };
        if task
            .history
            .iter()
            .any(|recorded| matches!(recorded.event, TaskEvent::EvidenceExported { .. }))
        {
            return false;
        }
        let event = TaskEvent::EvidenceExported {
            artifact: crate::evidence::archive_filename(id),
        }
        .sanitized(&self.inner.redactor)
        .bounded(self.inner.history_limits);
        let recorded = task.record_event(event);
        let _ = self.inner.tx.send((id, recorded));
        true
    }

    /// Snapshots of every task.
    pub fn list(&self) -> Vec<Task> {
        let tasks = self
            .inner
            .tasks
            .read()
            .expect("task registry lock poisoned");
        tasks.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        let tasks = self
            .inner
            .tasks
            .read()
            .expect("task registry lock poisoned");
        tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The handle to give a pipeline stage for this task (DP-9).
    pub fn emitter(&self, id: TaskId) -> Emitter {
        Emitter {
            id,
            inner: Arc::clone(&self.inner),
        }
    }

    /// Record the human's Gate 2 answer and wake the waiting pipeline (DP-11).
    ///
    /// Test convenience wrapper; invalid state/input also returns `false`.
    #[cfg(test)]
    pub fn decide(&self, id: TaskId, decision: Decision) -> bool {
        self.decide_checked(id, decision).is_ok()
    }

    /// Validate and consume the approval gate under the same write lock.
    pub fn decide_checked(&self, id: TaskId, mut decision: Decision) -> Result<(), DecisionError> {
        {
            let mut tasks = self
                .inner
                .tasks
                .write()
                .expect("task registry lock poisoned");
            match tasks.get_mut(&id) {
                Some(task) => {
                    if task.status != TaskStatus::WaitingForApproval || task.decision.is_some() {
                        return Err(DecisionError::NotWaiting);
                    }
                    let mut events = Vec::new();
                    if decision.approve {
                        let previous_spec = task.spec.clone();
                        decision.spec = decision.spec.or_else(|| task.spec.clone());
                        if decision
                            .spec
                            .as_ref()
                            .is_none_or(|text| text.trim().is_empty())
                        {
                            return Err(DecisionError::InvalidSpec);
                        }
                        decision.spec = decision
                            .spec
                            .map(|markdown| self.inner.redactor.redact(&markdown));
                        if decision.spec != previous_spec {
                            events.push(task.record_event(TaskEvent::SpecUpdated));
                        }
                        let markdown = decision.spec.clone().expect("validated specification");
                        let event = TaskEvent::SpecApproved { markdown }
                            .sanitized(&self.inner.redactor)
                            .bounded(self.inner.history_limits);
                        events.push(task.record_event(event));
                    } else {
                        decision.spec = None;
                        events.push(task.record_event(TaskEvent::SpecRejected));
                    }
                    task.decision = Some(decision);
                    for event in &events {
                        let _ = self.inner.tx.send((id, event.clone()));
                    }
                }
                None => return Err(DecisionError::NotFound),
            }
        };
        // notify_one, NOT notify_waiters: notify_one stores a permit if nobody
        // is parked yet, so an answer that arrives before the pipeline reaches
        // the gate is still delivered. notify_waiters would drop it silently.
        self.inner.gate(id).notify_one();
        Ok(())
    }

    /// The Gate 2 answer, if one has been given.
    pub fn decision(&self, id: TaskId) -> Option<Decision> {
        let tasks = self
            .inner
            .tasks
            .read()
            .expect("task registry lock poisoned");
        tasks.get(&id).and_then(|task| task.decision.clone())
    }

    /// The specification accepted at Gate 2, if approval has completed.
    pub fn approved_spec(&self, id: TaskId) -> Option<String> {
        let tasks = self
            .inner
            .tasks
            .read()
            .expect("task registry lock poisoned");
        let task = tasks.get(&id)?;
        task.decision
            .as_ref()
            .filter(|decision| decision.approve)
            .and(task.spec.clone())
    }

    pub fn cancel(&self, id: TaskId) -> bool {
        let mut tasks = self
            .inner
            .tasks
            .write()
            .expect("task registry lock poisoned");
        let cancelled = if let Some(task) = tasks.get_mut(&id) {
            if task.status.is_terminal() {
                false
            } else {
                let event = TaskEvent::TaskCancelled
                    .sanitized(&self.inner.redactor)
                    .bounded(self.inner.history_limits);
                let recorded = task.record_event(event);
                let _ = self.inner.tx.send((id, recorded));
                true
            }
        } else {
            false
        };
        drop(tasks);
        if cancelled {
            self.inner.gate(id).notify_one();
        }
        cancelled
    }

    pub fn is_cancelled(&self, id: TaskId) -> bool {
        self.get(id)
            .is_some_and(|task| task.cancelled || task.status == TaskStatus::Cancelled)
    }

    /// Park until the human answers Gate 2.
    ///
    /// The state is checked BEFORE awaiting, which together with `notify_one`'s
    /// stored permit closes the missed-wakeup window in both directions: an
    /// answer that lands before we park is seen by the check, and one that lands
    /// between the check and the park is held as a permit.
    ///
    /// `None` means the task disappeared, which should not happen in practice.
    pub async fn await_decision(&self, id: TaskId) -> Option<Decision> {
        let gate = self.inner.gate(id);
        loop {
            if let Some(decision) = self.decision(id) {
                return Some(decision);
            }
            if self.is_cancelled(id) {
                return None;
            }
            // Bail out if the task disappeared, rather than parking forever.
            self.get(id)?;
            gate.notified().await;
        }
    }

    /// Listen to every task's events. Each subscriber gets its own copy.
    ///
    /// Subscribing only sees events sent from now on, which is exactly why
    /// `Task::history` exists — the browser loads the snapshot first, then
    /// subscribes for the rest.
    pub fn subscribe(&self) -> broadcast::Receiver<(TaskId, RecordedEvent)> {
        self.inner.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_joins_title_and_description() {
        let task = Task::new("Credit applications", "Must support PDF upload.", "credit");
        assert_eq!(
            task.topic(),
            "Credit applications\n\nMust support PDF upload."
        );
    }

    #[test]
    fn topic_falls_back_to_the_title_alone() {
        let task = Task::new("Credit applications", "   ", "credit");
        assert_eq!(task.topic(), "Credit applications");
    }

    #[test]
    fn a_new_task_starts_created_and_empty() {
        let task = Task::new("t", "d", "p");
        assert_eq!(task.status, TaskStatus::Created);
        assert!(task.history.is_empty());
        assert!(task.spec.is_none());
    }

    #[test]
    fn applying_a_status_event_moves_the_task() {
        let mut task = Task::new("t", "d", "p");
        task.apply(&TaskEvent::Status {
            status: TaskStatus::Debating,
        });

        assert_eq!(task.status, TaskStatus::Debating);
        assert_eq!(task.history.len(), 1);
    }

    #[test]
    fn applying_a_spec_event_stores_the_document() {
        let mut task = Task::new("t", "d", "p");
        task.apply(&TaskEvent::Spec {
            markdown: "## Problem".into(),
            path: "C:/x/SPEC.md".into(),
        });

        assert_eq!(task.spec.as_deref(), Some("## Problem"));
    }

    #[test]
    fn recorded_events_have_stable_sequence_utc_time_and_append_only_history() {
        let manager = TaskManager::new();
        let task = manager.create("audit", "events", "legacy");
        let emitter = manager.emitter(task.id);
        let first = manager.get(task.id).unwrap().history[0].clone();

        emitter.status(TaskStatus::Debating);
        emitter.emit(TaskEvent::RoundStarted { round: 1, of: 2 });

        let stored = manager.get(task.id).unwrap();
        assert_eq!(stored.history.len(), 3);
        assert_eq!(
            stored
                .history
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            serde_json::to_value(&stored.history[0]).unwrap(),
            serde_json::to_value(first).unwrap(),
            "later records must not mutate an earlier envelope"
        );
        for recorded in &stored.history {
            assert_eq!(recorded.timestamp.timezone(), Utc);
            let json = serde_json::to_value(recorded).unwrap();
            let timestamp = json["timestamp"].as_str().unwrap();
            assert!(timestamp.ends_with('Z'), "not UTC RFC3339: {timestamp}");
            assert!(json["event"]["type"].is_string());
        }
    }

    #[test]
    fn concurrent_recording_assigns_unique_ordered_sequences() {
        let manager = TaskManager::new();
        let task = manager.create("audit", "concurrency", "legacy");
        let handles = (0..16)
            .map(|round| {
                let emitter = manager.emitter(task.id);
                std::thread::spawn(move || {
                    emitter.emit(TaskEvent::RoundStarted { round, of: 16 });
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let sequences = manager
            .get(task.id)
            .unwrap()
            .history
            .into_iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>();
        assert_eq!(sequences, (1..=17).collect::<Vec<_>>());
    }

    #[test]
    fn cancellation_events_are_chronological_and_never_claim_success() {
        let manager = TaskManager::new();
        let task = manager.create("audit", "cancel", "legacy");
        let emitter = manager.emitter(task.id);
        let worker = task.agents.worker.clone();
        emitter.emit(TaskEvent::WorkerStarted {
            tool: worker.tool,
            model: worker.model.clone(),
        });
        emitter.emit(TaskEvent::WorkerCancelled {
            tool: worker.tool,
            model: worker.model,
        });
        emitter.emit(TaskEvent::TaskCancelled);

        let stored = manager.get(task.id).unwrap();
        let kinds = stored
            .history
            .iter()
            .map(|recorded| serde_json::to_value(&recorded.event).unwrap()["type"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            &kinds[1..],
            ["worker_started", "worker_cancelled", "task_cancelled"]
        );
        assert!(!kinds.iter().any(|kind| kind == "task_completed"));
    }

    #[test]
    fn milestone_events_update_state_with_ordered_timestamps_and_redaction() {
        let manager = TaskManager::with_history_limits_and_secrets(
            HistoryLimits::default(),
            ["milestone-secret-value".to_string()],
        );
        let task = manager.create("audit", "milestones", "legacy");
        let plan = crate::milestone::plan_from_spec("## Steps\n1. First\n2. Second", &[]).unwrap();
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::MilestonePlanCreated {
            milestones: plan.clone(),
        });
        emitter.emit(TaskEvent::MilestoneStarted {
            id: plan[0].id.clone(),
            order: 1,
            title: "First milestone-secret-value".into(),
            worker_tool: crate::agent::CodingTool::ClaudeCode,
            worker_model: "worker-model".into(),
        });
        emitter.emit(TaskEvent::MilestoneCompleted {
            id: plan[0].id.clone(),
            order: 1,
            title: "First".into(),
            verification: Vec::new(),
            worker_result_summary: "done".into(),
        });
        let stored = manager.get(task.id).unwrap();
        assert_eq!(stored.milestones[0].status, MilestoneStatus::Passed);
        assert!(stored.milestones[0].started_at <= stored.milestones[0].completed_at);
        assert_eq!(stored.milestones[1].status, MilestoneStatus::Pending);
        let json = serde_json::to_string(&stored).unwrap();
        assert!(!json.contains("milestone-secret-value"));
        assert!(
            stored
                .history
                .windows(2)
                .all(|events| events[0].sequence < events[1].sequence)
        );
    }

    #[test]
    fn cancellation_preserves_completed_milestones_and_cancels_future_work() {
        let manager = TaskManager::new();
        let task = manager.create("audit", "milestones", "legacy");
        let plan = crate::milestone::plan_from_spec("## Steps\n1. First\n2. Second", &[]).unwrap();
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::MilestonePlanCreated {
            milestones: plan.clone(),
        });
        emitter.emit(TaskEvent::MilestoneStarted {
            id: "m1".into(),
            order: 1,
            title: "First".into(),
            worker_tool: crate::agent::CodingTool::ClaudeCode,
            worker_model: "worker".into(),
        });
        emitter.emit(TaskEvent::MilestoneCompleted {
            id: "m1".into(),
            order: 1,
            title: "First".into(),
            verification: Vec::new(),
            worker_result_summary: "done".into(),
        });
        assert!(manager.cancel(task.id));
        emitter.emit(TaskEvent::MilestoneCancelled {
            id: "m2".into(),
            order: 2,
            title: "Second".into(),
            reason: "task cancelled before milestone start".into(),
        });
        let stored = manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Cancelled);
        assert_eq!(stored.milestones[0].status, MilestoneStatus::Passed);
        assert_eq!(stored.milestones[1].status, MilestoneStatus::Cancelled);
        assert!(
            !stored
                .history
                .iter()
                .any(|event| matches!(event.event, TaskEvent::TaskCompleted))
        );
    }

    #[test]
    fn milestone_failure_stops_before_later_milestones() {
        let manager = TaskManager::new();
        let task = manager.create("audit", "milestones", "legacy");
        let plan = crate::milestone::plan_from_spec("## Steps\n1. First\n2. Later", &[]).unwrap();
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::MilestonePlanCreated { milestones: plan });
        emitter.emit(TaskEvent::MilestoneStarted {
            id: "m1".into(),
            order: 1,
            title: "First".into(),
            worker_tool: crate::agent::CodingTool::ClaudeCode,
            worker_model: "worker".into(),
        });
        emitter.emit(TaskEvent::MilestoneFailed {
            id: "m1".into(),
            order: 1,
            title: "First".into(),
            verification: vec![VerificationResult {
                command: "cargo test".into(),
                success: false,
                output: "failure".into(),
            }],
            worker_result_summary: Some("worker finished".into()),
            error: "verification failed".into(),
        });
        let stored = manager.get(task.id).unwrap();
        assert_eq!(stored.milestones[0].status, MilestoneStatus::Failed);
        assert_eq!(stored.milestones[1].status, MilestoneStatus::Pending);
        assert!(!stored.history.iter().any(|event| matches!(
            event.event,
            TaskEvent::MilestoneStarted { ref id, .. } if id == "m2"
        )));
    }

    #[test]
    fn event_recording_redacts_known_and_structured_credentials() {
        let secret = "anthropic-live-secret-value";
        let manager = TaskManager::with_history_limits_and_secrets(
            HistoryLimits::default(),
            [secret.to_string()],
        );
        let task = manager.create("audit", "redaction", "legacy");
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::TaskFailed {
            error: format!("provider echoed {secret}"),
        });
        emitter.emit(TaskEvent::Result {
            result: TaskResult {
                source_revision: None,
                verification: vec![VerificationResult {
                    command: "probe".into(),
                    success: false,
                    output: "ANTHROPIC_API_KEY=another-secret".into(),
                }],
                diff: "Authorization: Bearer hidden-token".into(),
            },
        });

        let json = serde_json::to_string(&manager.get(task.id).unwrap()).unwrap();
        assert!(!json.contains(secret));
        assert!(!json.contains("another-secret"));
        assert!(!json.contains("hidden-token"));
        assert!(json.contains("REDACTED"));
    }

    #[test]
    fn approval_without_edits_promotes_the_generated_spec() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::Spec {
            markdown: "generated".into(),
            path: "SPEC.md".into(),
        });

        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        assert!(manager.decide(
            task.id,
            Decision {
                approve: true,
                spec: None,
            },
        ));

        assert_eq!(manager.approved_spec(task.id).as_deref(), Some("generated"));
        assert_eq!(
            manager.decision(task.id).unwrap().spec.as_deref(),
            Some("generated")
        );
        assert!(matches!(
            manager.get(task.id).unwrap().history.last(),
            Some(RecordedEvent { event: TaskEvent::SpecApproved { markdown }, .. }) if markdown == "generated"
        ));
    }

    #[test]
    fn edited_approval_replaces_the_authoritative_task_spec() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        manager.emitter(task.id).emit(TaskEvent::Spec {
            markdown: "generated".into(),
            path: "SPEC.md".into(),
        });

        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        manager.decide(
            task.id,
            Decision {
                approve: true,
                spec: Some("edited and approved".into()),
            },
        );

        let stored = manager.get(task.id).unwrap();
        assert_eq!(stored.spec.as_deref(), Some("edited and approved"));
        assert_eq!(manager.approved_spec(task.id), stored.spec);
        let updated = stored
            .history
            .iter()
            .position(|event| matches!(event.event, TaskEvent::SpecUpdated))
            .unwrap();
        let approved = stored
            .history
            .iter()
            .position(|event| matches!(event.event, TaskEvent::SpecApproved { .. }))
            .unwrap();
        assert!(updated < approved);
    }

    #[test]
    fn rejection_keeps_the_generated_spec_without_approving_it() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        manager.emitter(task.id).emit(TaskEvent::Spec {
            markdown: "generated".into(),
            path: "SPEC.md".into(),
        });

        manager.decide(
            task.id,
            Decision {
                approve: false,
                spec: Some("textarea must be ignored".into()),
            },
        );

        let stored = manager.get(task.id).unwrap();
        assert_eq!(stored.spec.as_deref(), Some("generated"));
        assert!(manager.approved_spec(task.id).is_none());
        assert!(manager.decision(task.id).unwrap().spec.is_none());
        assert!(
            !stored
                .history
                .iter()
                .any(|event| matches!(event.event, TaskEvent::SpecApproved { .. }))
        );
        assert!(
            stored
                .history
                .iter()
                .any(|event| matches!(event.event, TaskEvent::SpecRejected))
        );
    }

    #[test]
    fn finishing_records_the_error() {
        let mut task = Task::new("t", "d", "p");
        task.apply(&TaskEvent::Finished {
            status: TaskStatus::Failed,
            error: Some("boom".into()),
        });

        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.error.as_deref(), Some("boom"));
        assert!(task.status.is_terminal());
    }

    #[test]
    fn approval_gate_rejects_invalid_states_and_duplicate_decisions() {
        let manager = TaskManager::new();
        for status in [
            TaskStatus::Created,
            TaskStatus::Debating,
            TaskStatus::Implementing,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Rejected,
        ] {
            let task = manager.create("task", "description", "legacy");
            manager.emitter(task.id).status(status);
            let before = manager.get(task.id).unwrap();
            assert_eq!(
                manager.decide_checked(
                    task.id,
                    Decision {
                        approve: true,
                        spec: Some("injected".into()),
                    }
                ),
                Err(DecisionError::NotWaiting)
            );
            let after = manager.get(task.id).unwrap();
            assert_eq!(after.spec, before.spec);
            assert!(after.decision.is_none());
            assert_eq!(after.history.len(), before.history.len());
        }
        let task = manager.create("task", "description", "legacy");
        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        assert_eq!(
            manager.decide_checked(
                task.id,
                Decision {
                    approve: true,
                    spec: None
                }
            ),
            Err(DecisionError::InvalidSpec)
        );
        assert_eq!(
            manager.decide_checked(
                task.id,
                Decision {
                    approve: true,
                    spec: Some("  ".into())
                }
            ),
            Err(DecisionError::InvalidSpec)
        );
        manager
            .decide_checked(
                task.id,
                Decision {
                    approve: true,
                    spec: Some("approved".into()),
                },
            )
            .unwrap();
        assert_eq!(
            manager.decide_checked(
                task.id,
                Decision {
                    approve: false,
                    spec: None
                }
            ),
            Err(DecisionError::NotWaiting)
        );
        assert_eq!(manager.approved_spec(task.id).as_deref(), Some("approved"));
    }

    #[test]
    fn simultaneous_decisions_only_accept_one_specification() {
        let manager = TaskManager::new();
        let task = manager.create("task", "description", "legacy");
        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|text| {
                let manager = manager.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    manager.decide_checked(
                        task.id,
                        Decision {
                            approve: true,
                            spec: Some(text.into()),
                        },
                    )
                })
            })
            .collect();
        let accepted = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(accepted, 1);
        let task = manager.get(task.id).unwrap();
        assert_eq!(
            task.history
                .iter()
                .filter(|event| matches!(event.event, TaskEvent::SpecApproved { .. }))
                .count(),
            1
        );
        assert_eq!(task.spec, task.decision.unwrap().spec);
    }

    #[test]
    fn successful_rejected_failed_and_cancelled_are_terminal() {
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Rejected.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());

        assert!(!TaskStatus::Created.is_terminal());
        assert!(!TaskStatus::Debating.is_terminal());
        assert!(!TaskStatus::GeneratingSpec.is_terminal());
        assert!(!TaskStatus::WaitingForApproval.is_terminal());
        assert!(!TaskStatus::Implementing.is_terminal());
    }

    /// The browser switches on these strings, so they are part of the contract.
    #[test]
    fn events_serialize_with_a_type_discriminator() {
        let json = serde_json::to_value(TaskEvent::Proposal {
            round: 1,
            text: "hi".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "proposal");
        assert_eq!(json["round"], 1);

        let json = serde_json::to_value(TaskEvent::Status {
            status: TaskStatus::WaitingForApproval,
        })
        .unwrap();
        assert_eq!(json["type"], "status");
        assert_eq!(json["status"], "waiting_for_approval");
    }

    #[test]
    fn a_detached_emitter_never_panics_without_subscribers() {
        let emitter = Emitter::detached();
        emitter.notice("nobody is listening");
        emitter.status(TaskStatus::Debating);
    }

    // --- TaskManager --------------------------------------------------------

    #[test]
    fn created_tasks_are_retrievable() {
        let manager = TaskManager::new();
        let task = manager.create("Renamer", "search and replace", "renamer");

        let found = manager.get(task.id).expect("task should be stored");
        assert_eq!(found.title, "Renamer");
        assert_eq!(found.kind, TaskKind::NewProject);
        assert!(found.project_id.is_none());
        assert_eq!(manager.len(), 1);
        assert!(manager.get(Uuid::new_v4()).is_none());
    }

    /// Phase 7's done-condition: the full run, Created through Completed.
    #[test]
    fn a_task_walks_the_whole_pipeline() {
        let manager = TaskManager::new();
        let task = manager.create("Renamer", "search and replace", "renamer");
        let emitter = manager.emitter(task.id);

        assert_eq!(manager.get(task.id).unwrap().status, TaskStatus::Created);

        emitter.status(TaskStatus::Debating);
        emitter.emit(TaskEvent::RoundStarted { round: 1, of: 5 });
        emitter.emit(TaskEvent::Proposal {
            round: 1,
            text: "use Rust".into(),
        });
        emitter.emit(TaskEvent::Critique {
            round: 1,
            text: "fine\nVERDICT: APPROVED".into(),
            verdict: Some("approved".into()),
            reason: Some("buildable".into()),
        });
        assert_eq!(manager.get(task.id).unwrap().status, TaskStatus::Debating);

        emitter.status(TaskStatus::GeneratingSpec);
        emitter.emit(TaskEvent::Spec {
            markdown: "## Problem".into(),
            path: "C:/x/SPEC.md".into(),
        });

        emitter.status(TaskStatus::WaitingForApproval);
        emitter.status(TaskStatus::Implementing);
        emitter.emit(TaskEvent::Build {
            chunk: "compiling...".into(),
        });
        emitter.emit(TaskEvent::Finished {
            status: TaskStatus::Completed,
            error: None,
        });

        let done = manager.get(task.id).unwrap();
        assert_eq!(done.status, TaskStatus::Completed);
        assert!(done.status.is_terminal());
        assert_eq!(done.spec.as_deref(), Some("## Problem"));
        assert!(done.error.is_none());
        // Every event above is replayable for a browser that arrives late.
        assert_eq!(done.history.len(), 10);
    }

    #[test]
    fn rejecting_is_terminal_but_keeps_the_spec_and_sets_no_error() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        let emitter = manager.emitter(task.id);

        emitter.emit(TaskEvent::Spec {
            markdown: "## Problem".into(),
            path: "C:/x/SPEC.md".into(),
        });
        emitter.emit(TaskEvent::Finished {
            status: TaskStatus::Rejected,
            error: None,
        });

        let done = manager.get(task.id).unwrap();
        assert_eq!(done.status, TaskStatus::Rejected);
        assert!(done.status.is_terminal());
        assert!(done.error.is_none(), "a rejection is not a failure");
        assert!(done.spec.is_some(), "the spec survives so it can be re-run");
    }

    #[test]
    fn a_failure_records_why() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        manager.emitter(task.id).emit(TaskEvent::Finished {
            status: TaskStatus::Failed,
            error: Some("Anthropic API 401".into()),
        });

        let done = manager.get(task.id).unwrap();
        assert_eq!(done.status, TaskStatus::Failed);
        assert_eq!(done.error.as_deref(), Some("Anthropic API 401"));
    }

    #[tokio::test]
    async fn subscribers_receive_events_tagged_with_the_task() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        let mut rx = manager.subscribe();

        manager.emitter(task.id).status(TaskStatus::Debating);

        let (id, event) = rx.recv().await.expect("event should arrive");
        assert_eq!(id, task.id);
        assert!(matches!(
            event.event,
            TaskEvent::Status {
                status: TaskStatus::Debating
            }
        ));
    }

    /// Two browser tabs on the same task must both see everything.
    #[tokio::test]
    async fn every_subscriber_gets_its_own_copy() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        let mut first = manager.subscribe();
        let mut second = manager.subscribe();

        manager.emitter(task.id).notice("hello");

        assert!(matches!(
            first.recv().await.unwrap().1.event,
            TaskEvent::Notice { .. }
        ));
        assert!(matches!(
            second.recv().await.unwrap().1.event,
            TaskEvent::Notice { .. }
        ));
    }

    /// The snapshot must already include an event by the time it is broadcast,
    /// or a browser could fetch state that is behind what it just saw.
    #[tokio::test]
    async fn the_snapshot_is_current_when_the_event_arrives() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        let mut rx = manager.subscribe();

        manager.emitter(task.id).status(TaskStatus::Implementing);
        rx.recv().await.unwrap();

        assert_eq!(
            manager.get(task.id).unwrap().status,
            TaskStatus::Implementing
        );
    }

    /// Cloning the manager must share state, since axum hands a clone to every
    /// handler.
    #[test]
    fn clones_share_the_same_registry() {
        let manager = TaskManager::new();
        let task = manager.clone().create("t", "d", "p");

        assert!(manager.get(task.id).is_some());
        assert_eq!(manager.list().len(), 1);
    }

    #[test]
    fn validates_new_project_inputs() {
        let valid = TaskRequest {
            kind: TaskKind::NewProject,
            title: "Service".into(),
            description: "Build a service".into(),
            project_id: None,
            technology: Some(TechStack::Python),
            output: Some(OutputTarget::ReviewableResult),
            destination: None,
            agents: None,
            git_mode: None,
        };
        assert!(valid.validate().is_ok());
        let ordinary =
            Task::from_request(valid.clone(), AgentSelection::compiled_defaults()).unwrap();
        assert_eq!(ordinary.output, Some(OutputTarget::ReviewableResult));
        assert_eq!(ordinary.git_mode, GitMode::None);

        let mut missing_stack = valid.clone();
        missing_stack.technology = None;
        assert!(missing_stack.validate().unwrap_err().contains("technology"));
        let mut with_project = valid;
        with_project.project_id = Some(Uuid::new_v4());
        assert!(with_project.validate().is_err());
    }

    #[test]
    fn take_home_assignment_defaults_to_persistence_and_milestone_commits() {
        let request = TaskRequest {
            kind: TaskKind::TakeHomeAssignment,
            title: "Candidate portal".into(),
            description: "Build the requested assignment".into(),
            project_id: None,
            technology: Some(TechStack::TypeScriptNode),
            // The browser may omit these defaults; the domain is authoritative.
            output: None,
            destination: Some("candidate-portal".into()),
            agents: None,
            git_mode: None,
        };

        let task = Task::from_request(request, AgentSelection::compiled_defaults()).unwrap();

        assert_eq!(task.kind, TaskKind::TakeHomeAssignment);
        assert_eq!(task.output, Some(OutputTarget::PersistentLocalProject));
        assert_eq!(task.destination.as_deref(), Some("candidate-portal"));
        assert_eq!(task.git_mode, GitMode::CommitPerMilestone);
        assert!(task.completion_checklist.is_some());
    }

    #[test]
    fn take_home_assignment_requires_a_safe_persistent_destination() {
        let base = TaskRequest {
            kind: TaskKind::TakeHomeAssignment,
            title: "Candidate portal".into(),
            description: "Build the requested assignment".into(),
            project_id: None,
            technology: Some(TechStack::Python),
            output: None,
            destination: None,
            agents: None,
            git_mode: None,
        };
        assert!(base.validate().unwrap_err().contains("destination"));

        let temporary = TaskRequest {
            output: Some(OutputTarget::ReviewableResult),
            ..base
        };
        assert!(
            temporary
                .validate()
                .unwrap_err()
                .contains("persistent output")
        );
    }

    #[test]
    fn take_home_completion_checklist_reflects_real_events() {
        let task = Task::from_request(
            TaskRequest {
                kind: TaskKind::TakeHomeAssignment,
                title: "Candidate portal".into(),
                description: "Build the requested assignment".into(),
                project_id: None,
                technology: Some(TechStack::Rust),
                output: None,
                destination: Some("candidate-portal".into()),
                agents: None,
                git_mode: None,
            },
            AgentSelection::compiled_defaults(),
        )
        .unwrap();
        let manager = TaskManager::new();
        let task = manager.insert(task);
        let emitter = manager.emitter(task.id);
        let milestone = Milestone {
            id: "m1".into(),
            order: 1,
            title: "Build portal".into(),
            objective: "Build portal".into(),
            verification_instructions: vec!["cargo test".into()],
            status: MilestoneStatus::Pending,
            started_at: None,
            completed_at: None,
            worker_result_summary: None,
            commit: None,
            review: None,
            criteria: vec!["AC-001".into()],
        };
        emitter.emit(TaskEvent::MilestonePlanCreated {
            milestones: vec![milestone],
        });
        emitter.emit(TaskEvent::MilestoneCompleted {
            id: "m1".into(),
            order: 1,
            title: "Build portal".into(),
            verification: vec![],
            worker_result_summary: "implemented".into(),
        });
        emitter.emit(TaskEvent::VerificationCompleted { commands: 1 });
        emitter.emit(TaskEvent::AcceptanceCriteriaGenerated {
            criteria: vec![crate::acceptance::AcceptanceCriterion {
                id: "AC-001".into(),
                description: "Portal works".into(),
                status: crate::acceptance::CriterionStatus::Passed,
                milestones: vec!["m1".into()],
                evidence: vec![],
                blocking_findings: vec![],
            }],
        });
        emitter.emit(TaskEvent::ImplementationReviewCompleted {
            milestone_id: "m1".into(),
            order: 1,
            iteration: 0,
            of: 1,
            status: crate::review::ReviewStatus::Pass,
            findings: vec![],
        });
        emitter.emit(TaskEvent::SubmissionDocumentationGenerated {
            written: vec!["README.md".into()],
            preserved: vec![],
        });
        emitter.emit(TaskEvent::Result {
            result: TaskResult {
                source_revision: None,
                verification: vec![],
                diff: "diff".into(),
            },
        });
        emitter.emit(TaskEvent::ProjectPersisted {
            destination: "candidate-portal".into(),
            git: Some(crate::git::RepositoryStatus {
                branch: Some("main".into()),
                head_sha: Some("abc".into()),
                commits: 1,
                has_remote: false,
            }),
            git_warning: None,
        });
        emitter.emit(TaskEvent::Finished {
            status: TaskStatus::Completed,
            error: None,
        });

        let checklist = manager
            .get(task.id)
            .unwrap()
            .completion_checklist
            .expect("take-home checklist");
        assert!(checklist.implementation_complete);
        assert!(checklist.verification_complete);
        assert!(checklist.acceptance_criteria_reviewed);
        assert!(checklist.final_critic_review_complete);
        assert!(checklist.documentation_generated);
        assert!(checklist.evidence_export_available);
        assert!(checklist.git_history_available);
    }

    #[test]
    fn feature_and_bug_fix_require_only_a_project() {
        for kind in [TaskKind::Feature, TaskKind::BugFix] {
            let mut request = TaskRequest {
                kind,
                title: "Change".into(),
                description: "Make the requested change".into(),
                project_id: Some(Uuid::new_v4()),
                technology: None,
                output: None,
                destination: None,
                agents: None,
                git_mode: None,
            };
            assert!(request.validate().is_ok());
            request.project_id = None;
            assert!(
                request
                    .validate()
                    .unwrap_err()
                    .contains("registered project")
            );
        }
    }

    #[test]
    fn all_task_kinds_require_title_and_description() {
        let request = TaskRequest {
            kind: TaskKind::NewProject,
            title: "".into(),
            description: "".into(),
            project_id: None,
            technology: Some(TechStack::Custom),
            output: Some(OutputTarget::ReviewableResult),
            destination: None,
            agents: None,
            git_mode: None,
        };
        assert!(request.validate().unwrap_err().contains("title"));
        let request = TaskRequest {
            title: "Title".into(),
            ..request
        };
        assert!(request.validate().unwrap_err().contains("description"));
    }
}
#[cfg(test)]
mod limit_tests {
    use super::*;
    #[test]
    fn log_bounds_preserve_all_lifecycle_events_and_authoritative_spec() {
        let manager = TaskManager::with_history_limits(HistoryLimits {
            event_bytes: 64,
            log_events: 3,
            log_bytes: 100,
        });
        let task = manager.create("test", "description", "legacy");
        let emitter = manager.emitter(task.id);
        let lifecycle = [
            TaskEvent::Status {
                status: TaskStatus::Created,
            },
            TaskEvent::Proposal {
                round: 1,
                text: "full proposal".repeat(50),
            },
            TaskEvent::Critique {
                round: 1,
                text: "critique".into(),
                verdict: None,
                reason: None,
            },
            TaskEvent::Spec {
                markdown: "exact specification".repeat(50),
                path: "artifacts/approved-spec.md".into(),
            },
            TaskEvent::Status {
                status: TaskStatus::WaitingForApproval,
            },
        ];
        for event in &lifecycle {
            emitter.emit(event.clone());
        }
        manager
            .decide_checked(
                task.id,
                Decision {
                    approve: true,
                    spec: None,
                },
            )
            .unwrap();
        emitter.status(TaskStatus::Implementing);
        for index in 0..1000 {
            emitter.emit(TaskEvent::Build {
                chunk: format!("log {index} {}", "🦀".repeat(50)),
            });
            emitter.notice("ordinary chatter");
        }
        emitter.emit(TaskEvent::Verification {
            result: VerificationResult {
                command: "test".into(),
                success: false,
                output: "useful failure".into(),
            },
        });
        emitter.emit(TaskEvent::Result {
            result: TaskResult {
                source_revision: None,
                verification: vec![],
                diff: "+partial changes".into(),
            },
        });
        emitter.emit(TaskEvent::Finished {
            status: TaskStatus::Failed,
            error: Some("failure details".into()),
        });
        let stored = manager.get(task.id).unwrap();
        let logs: Vec<_> = stored
            .log_tail
            .iter()
            .filter_map(|event| event.event.log_text())
            .collect();
        assert!(logs.len() <= 3 && logs.iter().map(|text| text.len()).sum::<usize>() <= 100);
        assert!(logs.iter().all(|text| text.len() <= 64));
        assert!(stored.discarded_log_events > 0);
        assert_eq!(stored.spec, Some("exact specification".repeat(50)));
        assert_eq!(stored.error.as_deref(), Some("failure details"));
        assert_eq!(stored.result.unwrap().diff, "+partial changes");
        // Every lifecycle event, including approval and final result, survived.
        assert_eq!(
            stored
                .history
                .iter()
                .filter(|event| event.event.log_text().is_none())
                .count(),
            lifecycle.len() + 6
        );
    }

    #[tokio::test]
    async fn oversized_log_is_bounded_before_broadcast_as_well_as_storage() {
        let manager = TaskManager::with_history_limits(HistoryLimits {
            event_bytes: 64,
            log_events: 3,
            log_bytes: 192,
        });
        let task = manager.create("test", "description", "legacy");
        let mut subscriber = manager.subscribe();
        manager.emitter(task.id).emit(TaskEvent::Build {
            chunk: "x".repeat(10000),
        });
        let (_, event) = subscriber.recv().await.unwrap();
        let text = event.event.log_text().unwrap();
        assert_eq!(text.len(), 64);
        assert!(text.ends_with(crate::execution_limits::TRUNCATED));
        assert_eq!(
            manager.get(task.id).unwrap().log_tail[0].event.log_text(),
            Some(text)
        );
    }
}
