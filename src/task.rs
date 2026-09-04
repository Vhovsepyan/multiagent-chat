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

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

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
    /// SPEC.md exists and is on disk, gate not yet answered.
    WaitingForApproval,
    /// Claude Code is running in the target repo.
    Implementing,
    /// Finished successfully.
    Completed,
    /// The human declined at the gate. Not an error — SPEC.md is still there.
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

/// Everything worth telling a watcher about, as it happens.
///
/// `#[serde(tag = "type")]` puts a discriminator in the JSON, so the browser
/// gets `{"type":"proposal","round":1,"text":"..."}` and can switch on `type`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEvent {
    /// The task moved to a new stage. Drives the pipeline timeline.
    Status {
        status: TaskStatus,
    },

    /// A debate round began.
    RoundStarted {
        round: u32,
        of: u32,
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

    /// SPEC.md is written. `markdown` is the document, `path` where it landed.
    Spec {
        markdown: String,
        path: String,
    },

    /// The exact specification accepted at Gate 2. This becomes the task's
    /// authoritative specification for the UI, implementation, and replay.
    SpecApproved {
        markdown: String,
    },

    Inspection {
        profile: ProjectProfile,
        source_revision: Option<String>,
    },

    Verification {
        result: VerificationResult,
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
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// The human's answer at Gate 2 (DP-10).
///
/// `spec` carries an edited document. When present it replaces SPEC.md before
/// the build starts, so editing and approving are one atomic action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub approve: bool,
    #[serde(default)]
    pub spec: Option<String>,
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
    pub profile: Option<ProjectProfile>,
    pub result: Option<TaskResult>,
    pub status: TaskStatus,
    /// Every event so far, so a browser opening late sees the whole debate.
    pub history: Vec<TaskEvent>,
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
            profile: None,
            result: None,
            status: TaskStatus::Created,
            history: Vec::new(),
            spec: None,
            error: None,
            decision: None,
        }
    }

    pub fn from_request(request: TaskRequest) -> Result<Self, String> {
        request.validate()?;
        Ok(Task {
            id: Uuid::new_v4(),
            title: request.title.trim().to_string(),
            description: request.description.trim().to_string(),
            kind: request.kind,
            project_id: request.project_id,
            technology: request.technology,
            output: request.output,
            profile: None,
            result: None,
            status: TaskStatus::Created,
            history: Vec::new(),
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

    /// Fold an event into the task, so `history` and the summary fields agree.
    pub fn apply(&mut self, event: &TaskEvent) {
        match event {
            TaskEvent::Status { status } => self.status = *status,
            TaskEvent::Spec { markdown, .. } | TaskEvent::SpecApproved { markdown } => {
                self.spec = Some(markdown.clone());
            }
            TaskEvent::Inspection { profile, .. } => self.profile = Some(profile.clone()),
            TaskEvent::Result { result } => self.result = Some(result.clone()),
            TaskEvent::Finished { status, error } => {
                self.status = *status;
                self.error = error.clone();
            }
            _ => {}
        }
        self.history.push(event.clone());
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
        self.inner.record(self.id, &event);
        let _ = self.inner.tx.send((self.id, event));
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
    tasks: RwLock<HashMap<TaskId, Task>>,
    tx: broadcast::Sender<(TaskId, TaskEvent)>,
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
            tasks: RwLock::new(HashMap::new()),
            tx,
            gates: RwLock::new(HashMap::new()),
        }
    }

    /// The waker for a task, created on first use.
    fn gate(&self, id: TaskId) -> Arc<Notify> {
        let mut gates = self.gates.write().expect("gate registry lock poisoned");
        Arc::clone(gates.entry(id).or_insert_with(|| Arc::new(Notify::new())))
    }

    /// Fold an event into the stored task, if that task still exists.
    fn record(&self, id: TaskId, event: &TaskEvent) {
        let mut tasks = self.tasks.write().expect("task registry lock poisoned");
        if let Some(task) = tasks.get_mut(&id) {
            task.apply(event);
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
        let task = Task::from_request(TaskRequest {
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
        })
        .expect("legacy task input is valid");
        self.insert(task)
    }

    pub fn create_from_request(&self, request: TaskRequest) -> Result<Task, String> {
        let task = Task::from_request(request)?;
        Ok(self.insert(task))
    }

    fn insert(&self, task: Task) -> Task {
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
    /// Returns `false` if there is no such task. On approval, the generated or
    /// edited text is recorded through `SpecApproved` before the gate wakes.
    pub fn decide(&self, id: TaskId, mut decision: Decision) -> bool {
        let approved_event = {
            let mut tasks = self
                .inner
                .tasks
                .write()
                .expect("task registry lock poisoned");
            match tasks.get_mut(&id) {
                Some(task) => {
                    let event = if decision.approve {
                        decision.spec = decision.spec.or_else(|| task.spec.clone());
                        decision.spec.clone().map(|markdown| {
                            let event = TaskEvent::SpecApproved { markdown };
                            task.apply(&event);
                            event
                        })
                    } else {
                        decision.spec = None;
                        None
                    };
                    task.decision = Some(decision);
                    event
                }
                None => return false,
            }
        };
        if let Some(event) = approved_event {
            let _ = self.inner.tx.send((id, event));
        }
        // notify_one, NOT notify_waiters: notify_one stores a permit if nobody
        // is parked yet, so an answer that arrives before the pipeline reaches
        // the gate is still delivered. notify_waiters would drop it silently.
        self.inner.gate(id).notify_one();
        true
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
    pub fn subscribe(&self) -> broadcast::Receiver<(TaskId, TaskEvent)> {
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
    fn approval_without_edits_promotes_the_generated_spec() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
        let emitter = manager.emitter(task.id);
        emitter.emit(TaskEvent::Spec {
            markdown: "generated".into(),
            path: "SPEC.md".into(),
        });

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
            Some(TaskEvent::SpecApproved { markdown }) if markdown == "generated"
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
    }

    #[test]
    fn rejection_keeps_the_generated_spec_without_approving_it() {
        let manager = TaskManager::new();
        let task = manager.create("t", "d", "p");
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
                .any(|event| matches!(event, TaskEvent::SpecApproved { .. }))
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
            event,
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
            first.recv().await.unwrap().1,
            TaskEvent::Notice { .. }
        ));
        assert!(matches!(
            second.recv().await.unwrap().1,
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
        };
        assert!(request.validate().unwrap_err().contains("title"));
        let request = TaskRequest {
            title: "Title".into(),
            ..request
        };
        assert!(request.validate().unwrap_err().contains("description"));
    }
}
