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
use crate::execution_limits::{HistoryLimits, bounded_text};
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
    Feature,
    BugFix,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::NewProject => "new project",
            Self::Feature => "feature",
            Self::BugFix => "bug fix",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputTarget {
    ReviewableResult,
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
    /// Per-role agent choice (task 0005). Absent means "the configured
    /// defaults", which is what every pre-0005 client sends.
    #[serde(default)]
    pub agents: Option<AgentSelectionRequest>,
}

impl TaskRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("title cannot be empty".into());
        }
        if self.description.trim().is_empty() {
            return Err("description cannot be empty".into());
        }
        match self.kind {
            TaskKind::NewProject => {
                if self.project_id.is_some() {
                    return Err("new_project must not reference an existing project".into());
                }
                if self.technology.is_none() {
                    return Err("new_project requires a technology".into());
                }
                if self.output.is_none() {
                    return Err("new_project requires output configuration".into());
                }
            }
            TaskKind::Feature | TaskKind::BugFix => {
                if self.project_id.is_none() {
                    return Err(format!(
                        "{} requires a registered project",
                        self.kind.label()
                    ));
                }
                if self.technology.is_some() || self.output.is_some() {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub source_revision: Option<String>,
    pub verification: Vec<VerificationResult>,
    pub diff: String,
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
}

impl TaskStatus {
    /// True once nothing further will happen on its own.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Rejected | TaskStatus::Failed
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
            Self::Build { chunk } => clean(chunk),
            Self::Notice { message } | Self::Warning { message } => clean(message),
            Self::Finished { error, .. } => {
                if let Some(error) = error {
                    clean(error);
                }
            }
            Self::TaskFailed { error } => clean(error),
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
struct AuditRedactor {
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

    fn redact(&self, text: &str) -> String {
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
        "access_token=",
        "access-token=",
        "authorization=",
        "authorization:",
        "password=",
        "secret=",
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
    line.to_string()
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
    /// The agents this run uses, resolved once at creation and never re-read
    /// from the environment afterwards (task 0005).
    pub agents: AgentSelection,
    pub profile: Option<ProjectProfile>,
    pub result: Option<TaskResult>,
    pub status: TaskStatus,
    /// Significant events are append-only for the lifetime of this task.
    pub history: Vec<RecordedEvent>,
    /// Repetitive UI output remains bounded independently from the audit log.
    pub log_tail: Vec<RecordedEvent>,
    pub discarded_log_events: usize,
    #[serde(skip)]
    history_limits: HistoryLimits,
    #[serde(skip)]
    next_event_sequence: u64,
    pub spec: Option<String>,
    pub error: Option<String>,
    /// Set once the human answers Gate 2 (DP-11).
    pub decision: Option<Decision>,
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
            agents: AgentSelection::compiled_defaults(),
            profile: None,
            result: None,
            status: TaskStatus::Created,
            history: Vec::new(),
            log_tail: Vec::new(),
            discarded_log_events: 0,
            history_limits: HistoryLimits::default(),
            next_event_sequence: 1,
            spec: None,
            error: None,
            decision: None,
        }
    }

    /// `agents` is resolved by the caller against the `AgentCatalogue`, so the
    /// domain never has to reach for the environment (task 0005).
    pub fn from_request(request: TaskRequest, agents: AgentSelection) -> Result<Self, String> {
        request.validate()?;
        Ok(Task {
            id: Uuid::new_v4(),
            title: request.title.trim().to_string(),
            description: request.description.trim().to_string(),
            kind: request.kind,
            project_id: request.project_id,
            technology: request.technology,
            output: request.output,
            agents,
            profile: None,
            result: None,
            status: TaskStatus::Created,
            history: Vec::new(),
            log_tail: Vec::new(),
            discarded_log_events: 0,
            history_limits: HistoryLimits::default(),
            next_event_sequence: 1,
            spec: None,
            error: None,
            decision: None,
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
            _ => {}
        }
        let recorded = RecordedEvent {
            sequence: self.next_event_sequence,
            timestamp: DateTime::<Utc>::from(std::time::SystemTime::now()),
            event,
        };
        self.next_event_sequence = self
            .next_event_sequence
            .checked_add(1)
            .expect("task event sequence exhausted");
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
                agents: None,
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
        let created = TaskEvent::TaskCreated { kind: task.kind }
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
    fn only_completed_rejected_and_failed_are_terminal() {
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Rejected.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());

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
            agents: None,
        };
        assert!(valid.validate().is_ok());

        let mut missing_stack = valid.clone();
        missing_stack.technology = None;
        assert!(missing_stack.validate().unwrap_err().contains("technology"));
        let mut with_project = valid;
        with_project.project_id = Some(Uuid::new_v4());
        assert!(with_project.validate().is_err());
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
                agents: None,
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
            agents: None,
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
