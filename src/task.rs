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

use serde::Serialize;
use tokio::sync::broadcast;
use uuid::Uuid;

/// How many events we keep for a subscriber that is briefly behind.
///
/// A browser reconnecting mid-debate should not miss turns. If a subscriber
/// falls further behind than this, `recv` reports `Lagged` and the UI can
/// re-fetch the snapshot instead of pretending nothing happened.
pub const EVENT_BUFFER: usize = 256;

/// Identifies one task. `Uuid` so the browser can hold it in a URL.
pub type TaskId = Uuid;

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
    Status { status: TaskStatus },

    /// A debate round began.
    RoundStarted { round: u32, of: u32 },

    /// The Proposer's full turn.
    Proposal { round: u32, text: String },

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
    Spec { markdown: String, path: String },

    /// A chunk of Claude Code's output while it builds.
    Build { chunk: String },

    /// Progress chatter — the grey lines v1 printed via `ui::system`.
    Notice { message: String },

    /// Something the user should notice but that did not stop the run.
    Warning { message: String },

    /// The run ended. `error` is set only when `status` is `Failed`.
    Finished {
        status: TaskStatus,
        error: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

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
    /// Project folder name inside WORKSPACE_ROOT.
    pub project: String,
    pub status: TaskStatus,
    /// Every event so far, so a browser opening late sees the whole debate.
    pub history: Vec<TaskEvent>,
    pub spec: Option<String>,
    pub error: Option<String>,
}

impl Task {
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        project: impl Into<String>,
    ) -> Self {
        Task {
            id: Uuid::new_v4(),
            title: title.into(),
            description: description.into(),
            project: project.into(),
            status: TaskStatus::Created,
            history: Vec::new(),
            spec: None,
            error: None,
        }
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
            TaskEvent::Spec { markdown, .. } => self.spec = Some(markdown.clone()),
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
        }
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

    /// Register a new task and return it.
    pub fn create(
        &self,
        title: impl Into<String>,
        description: impl Into<String>,
        project: impl Into<String>,
    ) -> Task {
        let task = Task::new(title, description, project);
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
        assert_eq!(found.project, "renamer");
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
}
