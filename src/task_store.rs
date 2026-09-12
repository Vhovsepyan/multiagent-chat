//! Durable, local task audit storage.
//!
//! The store deliberately owns only orchestration metadata.  Generated project
//! output remains at its configured persistent destination and is never copied
//! into this runtime directory.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::evidence::EvidenceRecord;
use crate::execution_limits::HistoryLimits;
use crate::task::{AuditRedactor, RecordedEvent, Task, TaskId};

const TASKS_DIRECTORY: &str = "tasks";
const SNAPSHOT: &str = "task.json";
const EVENTS: &str = "events.jsonl";
const EVIDENCE: &str = "evidence.jsonl";

/// A checkpoint couples a task-state snapshot to the exact prefix of the two
/// append-only journals which produced it.  Journal entries past this sequence
/// are an uncommitted crash tail, never state to replay onto an older snapshot.
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotFile {
    version: u8,
    committed_sequence: u64,
    task: Task,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskStore {
    root: PathBuf,
    redactor: AuditRedactor,
}

impl TaskStore {
    pub(crate) fn open(root: impl AsRef<Path>, redactor: AuditRedactor) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join(TASKS_DIRECTORY)).with_context(|| {
            format!("could not create durable task directory {}", root.display())
        })?;
        Ok(Self { root, redactor })
    }

    pub(crate) fn persist_new_task(&self, task: &Task) -> Result<()> {
        self.append_json(&self.events_path(task.id), &task.history[0])?;
        self.write_snapshot(task)
    }

    pub(crate) fn persist_event(&self, task: &Task, event: &RecordedEvent) -> Result<()> {
        self.append_json(&self.events_path(task.id), event)?;
        self.write_snapshot(task)
    }

    pub(crate) fn persist_events(&self, task: &Task, events: &[RecordedEvent]) -> Result<()> {
        for event in events {
            self.append_json(&self.events_path(task.id), event)?;
        }
        self.write_snapshot(task)
    }

    pub(crate) fn persist_evidence(&self, task: &Task, evidence: &EvidenceRecord) -> Result<()> {
        self.append_json(&self.evidence_path(task.id), evidence)?;
        self.write_snapshot(task)
    }

    pub(crate) fn load(&self, limits: HistoryLimits) -> Result<Vec<Task>> {
        let mut tasks = Vec::new();
        for entry in fs::read_dir(self.root.join(TASKS_DIRECTORY))? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("skipping unreadable durable task entry: {error}");
                    continue;
                }
            };
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let Ok(id) = entry.file_name().to_string_lossy().parse::<Uuid>() else {
                continue;
            };
            match self.load_one(id, &path, limits) {
                Ok(task) => tasks.push(task),
                Err(error) => eprintln!("skipping corrupt durable task {id}: {error:#}"),
            }
        }
        Ok(tasks)
    }

    fn load_one(&self, id: TaskId, directory: &Path, limits: HistoryLimits) -> Result<Task> {
        let event_path = directory.join(EVENTS);
        if !event_path.is_file() {
            bail!("missing required event audit file");
        }
        let events = self.read_jsonl::<RecordedEvent>(&event_path)?;
        let evidence = self.read_jsonl::<EvidenceRecord>(&directory.join(EVIDENCE))?;
        validate_audit_order(&events, &evidence)?;
        let (mut task, committed_sequence) =
            self.select_snapshot(id, directory, &events, &evidence)?;
        let events: Vec<RecordedEvent> = events
            .into_iter()
            .filter(|record| record.sequence <= committed_sequence)
            .collect();
        let evidence: Vec<EvidenceRecord> = evidence
            .into_iter()
            .filter(|record| record.sequence <= committed_sequence)
            .collect();
        self.reconcile_journals(directory, &events, &evidence)?;
        task.restore_durable_audit(events, evidence, limits);
        Ok(task)
    }

    fn select_snapshot(
        &self,
        id: TaskId,
        directory: &Path,
        events: &[RecordedEvent],
        evidence: &[EvidenceRecord],
    ) -> Result<(Task, u64)> {
        let mut candidates = self.snapshot_candidates(directory)?;
        candidates.sort_by_key(|(sequence, _, _)| *sequence);
        while let Some((committed_sequence, task, _path)) = candidates.pop() {
            let committed_sequence = if committed_sequence == 0 {
                events
                    .iter()
                    .map(|record| record.sequence)
                    .chain(evidence.iter().map(|record| record.sequence))
                    .max()
                    .unwrap_or(0)
            } else {
                committed_sequence
            };
            if task.id == id && audit_prefix_is_complete(committed_sequence, events, evidence) {
                return Ok((task, committed_sequence));
            }
        }
        bail!("no snapshot matches a complete committed audit prefix")
    }

    fn read_jsonl<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<Vec<T>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        let ends_with_newline = bytes.ends_with(b"\n");
        let lines = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .collect::<Vec<_>>();
        let mut records = Vec::new();
        for (index, raw) in lines.iter().enumerate() {
            let is_final = index + 1 == lines.len() && !ends_with_newline;
            let text = std::str::from_utf8(raw)
                .with_context(|| format!("invalid UTF-8 JSONL record at line {}", index + 1))?
                .trim();
            if text.is_empty() {
                continue;
            }
            match serde_json::from_str(text) {
                Ok(record) => records.push(record),
                // A crash during append leaves an unterminated last line. Its
                // prior complete records are still a valid journal prefix.
                Err(_) if is_final => break,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("invalid JSONL record at line {}", index + 1));
                }
            }
        }
        Ok(records)
    }

    /// The normal snapshot and every flushed replacement temporary file are
    /// candidates.  This specifically covers Windows' remove-then-rename
    /// window: a newer temp is selected only when its journal checkpoint is
    /// complete, otherwise the older intact snapshot remains authoritative.
    fn snapshot_candidates(&self, directory: &Path) -> Result<Vec<(u64, Task, PathBuf)>> {
        let paths = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name == SNAPSHOT
                        || (name.starts_with(&format!(".{SNAPSHOT}.")) && name.ends_with(".tmp"))
                })
            })
            .collect::<Vec<_>>();
        let mut snapshots = Vec::new();
        for path in paths {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let snapshot = match serde_json::from_slice::<SnapshotFile>(&bytes) {
                Ok(snapshot) => Some(snapshot),
                // Version 1 snapshots had no checkpoint. They are accepted as
                // a one-time migration only when their complete journal is a
                // contiguous audit prefix.
                Err(_) => serde_json::from_slice::<Task>(&bytes)
                    .ok()
                    .map(|task| SnapshotFile {
                        version: 0,
                        committed_sequence: 0,
                        task,
                    }),
            };
            if let Some(snapshot) = snapshot {
                snapshots.push((snapshot.committed_sequence, snapshot.task, path));
            }
        }
        if snapshots.is_empty() {
            bail!("missing valid task snapshot");
        }
        // A legacy snapshot commits the whole validated journal, calculated
        // after parsing rather than guessed from the task JSON.
        Ok(snapshots)
    }

    fn append_json<T: serde::Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let parent = path.parent().expect("task data path has parent");
        fs::create_dir_all(parent)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }

    /// Recovery is the sole exception to append-only operation.  Once a
    /// checkpoint selects a prefix, replace each journal with that exact valid
    /// prefix so a later append cannot follow a torn or uncommitted tail.
    fn reconcile_journals(
        &self,
        directory: &Path,
        events: &[RecordedEvent],
        evidence: &[EvidenceRecord],
    ) -> Result<()> {
        self.replace_jsonl(&directory.join(EVENTS), events)?;
        self.replace_jsonl(&directory.join(EVIDENCE), evidence)
    }

    fn replace_jsonl<T: serde::Serialize>(&self, destination: &Path, values: &[T]) -> Result<()> {
        let temporary = destination.with_extension(format!("jsonl.{}.tmp", Uuid::new_v4()));
        let mut file = File::create(&temporary)?;
        for value in values {
            serde_json::to_writer(&mut file, value)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, destination)
    }

    fn write_snapshot(&self, task: &Task) -> Result<()> {
        let directory = self.task_directory(task.id);
        fs::create_dir_all(&directory)?;
        let destination = directory.join(SNAPSHOT);
        let temporary = directory.join(format!(".{SNAPSHOT}.{}.tmp", Uuid::new_v4()));
        let mut file = File::create(&temporary)?;
        let mut snapshot = task.durable_snapshot();
        snapshot.sanitize_for_export(&self.redactor);
        serde_json::to_writer(
            &mut file,
            &SnapshotFile {
                version: 1,
                committed_sequence: task.durable_sequence(),
                task: snapshot,
            },
        )?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, &destination)?;
        Ok(())
    }

    fn task_directory(&self, id: TaskId) -> PathBuf {
        self.root.join(TASKS_DIRECTORY).join(id.to_string())
    }
    fn events_path(&self, id: TaskId) -> PathBuf {
        self.task_directory(id).join(EVENTS)
    }
    fn evidence_path(&self, id: TaskId) -> PathBuf {
        self.task_directory(id).join(EVIDENCE)
    }
}

fn validate_audit_order(events: &[RecordedEvent], evidence: &[EvidenceRecord]) -> Result<()> {
    let mut seen = HashSet::new();
    let mut last_event = 0;
    for event in events {
        if event.sequence == 0 || event.sequence <= last_event || !seen.insert(event.sequence) {
            bail!("event audit sequences are not strictly increasing");
        }
        last_event = event.sequence;
    }
    let mut last_evidence = 0;
    for record in evidence {
        if record.sequence == 0 || record.sequence <= last_evidence || !seen.insert(record.sequence)
        {
            bail!("evidence audit sequences are invalid or overlap events");
        }
        last_evidence = record.sequence;
    }
    Ok(())
}

fn audit_prefix_is_complete(
    committed_sequence: u64,
    events: &[RecordedEvent],
    evidence: &[EvidenceRecord],
) -> bool {
    let mut sequences = events
        .iter()
        .map(|record| record.sequence)
        .chain(evidence.iter().map(|record| record.sequence))
        .filter(|sequence| *sequence <= committed_sequence)
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    sequences.len() == committed_sequence as usize
        && sequences.iter().copied().eq(1..=committed_sequence)
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    if destination.exists() {
        // Windows' standard rename cannot replace an existing destination.
        // The fully flushed temporary copy remains until the replacement step.
        fs::remove_file(destination)?;
    }
    fs::rename(temporary, destination).with_context(|| {
        format!(
            "could not atomically install task snapshot {}",
            destination.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acceptance::{AcceptanceCriterion, CriterionStatus};
    use crate::agent::{
        AgentSelection, ChatAgentConfig, ChatProvider, CodingAgentConfig, CodingTool,
    };
    use crate::evidence::{EvidencePayload, EvidenceStatus, WorkerRole, WorkerStage};
    use crate::git::RepositoryStatus;
    use crate::milestone::{Milestone, MilestoneStatus};
    use crate::task::{
        GitHubPublication, OutputTarget, TaskEvent, TaskKind, TaskManager, TaskRequest, TaskStatus,
    };

    fn root(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("multiagent-chat-store-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn new_manager(path: &Path) -> TaskManager {
        TaskManager::with_durable_history_limits_and_secrets(
            HistoryLimits::default(),
            ["durable-secret-token".to_string()],
            path,
        )
        .unwrap()
    }

    fn task_directory(root: &Path, id: TaskId) -> PathBuf {
        root.join(TASKS_DIRECTORY).join(id.to_string())
    }

    fn worker_evidence() -> EvidencePayload {
        EvidencePayload::WorkerExecution {
            role: WorkerRole::Worker,
            stage: WorkerStage::Implementation,
            milestone_id: None,
            milestone_title: None,
            tool: CodingTool::Codex,
            model: "model".into(),
            instruction: "safe evidence".into(),
            summary: "completed".into(),
            status: EvidenceStatus::Completed,
            duration_ms: 1,
            truncated: false,
        }
    }

    #[test]
    fn restores_completed_state_audit_evidence_and_next_sequence() {
        let root = root("completed");
        let manager = new_manager(&root);
        let task = manager.create("durable", "task state", "legacy");
        let emitter = manager.emitter(task.id);
        emitter.status(TaskStatus::WaitingForApproval);
        emitter.emit(TaskEvent::MilestonePlanCreated {
            milestones: vec![Milestone {
                id: "M-001".into(),
                order: 1,
                title: "durable milestone".into(),
                objective: "retain state".into(),
                verification_instructions: vec!["cargo test".into()],
                status: MilestoneStatus::Pending,
                started_at: None,
                completed_at: None,
                worker_result_summary: None,
                commit: None,
                review: None,
                criteria: vec!["AC-001".into()],
            }],
        });
        emitter.emit(TaskEvent::AcceptanceCriteriaGenerated {
            criteria: vec![AcceptanceCriterion {
                id: "AC-001".into(),
                description: "state survives".into(),
                status: CriterionStatus::Pending,
                milestones: vec!["M-001".into()],
                evidence: Vec::new(),
                blocking_findings: Vec::new(),
            }],
        });
        emitter.emit(TaskEvent::ProjectPersisted {
            destination: "durable-project".into(),
            git: Some(RepositoryStatus {
                branch: Some("main".into()),
                head_sha: Some("abc123".into()),
                commits: 1,
                has_remote: false,
            }),
            git_warning: None,
            source_repository: Some("owner/repository".into()),
        });
        emitter.emit(TaskEvent::GitHubPublishCompleted {
            publication: GitHubPublication {
                repository: "owner/repository".into(),
                branch: "main".into(),
                commit_sha: "abc123".into(),
                published_at: chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()),
            },
        });
        emitter.record_evidence(worker_evidence());
        emitter.emit(TaskEvent::TaskCompleted);
        let before = manager.get(task.id).unwrap();
        let audit = before
            .display_history()
            .into_iter()
            .map(|item| item.sequence)
            .collect::<Vec<_>>();
        drop(manager);

        let restored = new_manager(&root);
        let task = restored.get(task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(
            task.display_history()
                .into_iter()
                .map(|item| item.sequence)
                .collect::<Vec<_>>(),
            audit
        );
        assert_eq!(task.evidence.len(), 1);
        assert_eq!(task.milestones[0].id, "M-001");
        assert_eq!(task.acceptance[0].id, "AC-001");
        assert_eq!(
            task.persistence.as_ref().unwrap().destination,
            "durable-project"
        );
        assert_eq!(
            task.github_publication.as_ref().unwrap().repository,
            "owner/repository"
        );
        restored.emitter(task.id).notice("after restart");
        let after = restored.get(task.id).unwrap();
        assert_eq!(
            after.display_history().last().unwrap().sequence,
            audit.last().unwrap() + 1
        );
        let export =
            crate::evidence::export(&restored.evidence_snapshot(task.id).unwrap()).unwrap();
        assert_eq!(export.files.len(), 5);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restores_frozen_openai_selection_without_any_provider_configuration() {
        let root = root("openai-selection");
        let manager = new_manager(&root);
        let task = manager
            .create_from_request(
                TaskRequest {
                    kind: TaskKind::NewProject,
                    title: "OpenAI durable selection".into(),
                    description: "Keep the resolved provider and model".into(),
                    project_id: None,
                    technology: Some(crate::technology::TechStack::Rust),
                    output: Some(OutputTarget::ReviewableResult),
                    destination: None,
                    agents: None,
                    git_mode: None,
                },
                AgentSelection {
                    proposer: ChatAgentConfig::new(ChatProvider::OpenAI, "gpt-5.6-sol"),
                    critic: ChatAgentConfig::new(ChatProvider::Anthropic, "claude-sonnet-5"),
                    worker: CodingAgentConfig::new(CodingTool::Codex, "gpt-5.3-codex"),
                },
            )
            .unwrap();
        drop(manager);

        let restored = new_manager(&root);
        let restored_task = restored.get(task.id).unwrap();
        assert_eq!(restored_task.agents.proposer.provider, ChatProvider::OpenAI);
        assert_eq!(restored_task.agents.proposer.model, "gpt-5.6-sol");
        assert_eq!(
            restored_task.agents.critic.provider,
            ChatProvider::Anthropic
        );
        assert_eq!(restored_task.agents.worker.tool, CodingTool::Codex);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preserves_waiting_gate_and_safely_fails_interrupted_execution() {
        let root = root("recovery");
        let manager = new_manager(&root);
        let waiting = manager.create("waiting", "approval", "legacy");
        manager
            .emitter(waiting.id)
            .status(TaskStatus::WaitingForApproval);
        let running = manager.create("running", "work", "legacy");
        manager.emitter(running.id).status(TaskStatus::Implementing);
        drop(manager);

        let restored = new_manager(&root);
        assert_eq!(
            restored.get(waiting.id).unwrap().status,
            TaskStatus::WaitingForApproval
        );
        let interrupted = restored.get(running.id).unwrap();
        assert_eq!(interrupted.status, TaskStatus::Failed);
        assert!(interrupted.error.unwrap().contains("application restart"));
        assert!(interrupted.history.iter().any(|event| matches!(
            event.event,
            TaskEvent::Finished {
                status: TaskStatus::Failed,
                ..
            }
        )));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corruption_is_isolated_and_secret_values_never_reach_disk() {
        let root = root("safety");
        let manager = new_manager(&root);
        let good = manager.create(
            "safe",
            "durable-secret-token https://user:git-secret@example.test/repository",
            "legacy",
        );
        manager
            .emitter(good.id)
            .notice("authorization: durable-secret-token");
        let bad = root.join(TASKS_DIRECTORY).join(Uuid::new_v4().to_string());
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join(SNAPSHOT), "not json").unwrap();
        drop(manager);
        let task_dir = root.join(TASKS_DIRECTORY).join(good.id.to_string());
        let contents = [task_dir.join(EVENTS), task_dir.join(SNAPSHOT)]
            .into_iter()
            .map(fs::read_to_string)
            .collect::<std::result::Result<String, _>>()
            .unwrap();
        assert!(!contents.contains("durable-secret-token"));
        assert!(!contents.contains("git-secret"));
        let restored = new_manager(&root);
        assert!(restored.get(good.id).is_some());
        assert_eq!(restored.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_snapshot_discards_uncommitted_event_tail_and_reuses_its_sequence() {
        let root = root("stale-snapshot");
        let manager = new_manager(&root);
        let task = manager.create("checkpoint", "event", "legacy");
        let directory = task_directory(&root, task.id);
        let old_snapshot = fs::read(directory.join(SNAPSHOT)).unwrap();
        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        fs::write(directory.join(SNAPSHOT), old_snapshot).unwrap();
        drop(manager);

        let restored = new_manager(&root);
        assert_eq!(restored.get(task.id).unwrap().status, TaskStatus::Created);
        restored.emitter(task.id).notice("new committed event");
        let restored = new_manager(&root);
        let task = restored.get(task.id).unwrap();
        assert_eq!(
            task.display_history()
                .into_iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn newer_temporary_snapshot_wins_when_its_checkpoint_is_complete() {
        let root = root("temporary-snapshot");
        let manager = new_manager(&root);
        let task = manager.create("checkpoint", "temp", "legacy");
        let directory = task_directory(&root, task.id);
        let old_snapshot = fs::read(directory.join(SNAPSHOT)).unwrap();
        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        let newer_snapshot = fs::read(directory.join(SNAPSHOT)).unwrap();
        fs::write(
            directory.join(format!(".{SNAPSHOT}.replacement.tmp")),
            newer_snapshot,
        )
        .unwrap();
        fs::write(directory.join(SNAPSHOT), old_snapshot).unwrap();
        drop(manager);

        let restored = new_manager(&root);
        assert_eq!(
            restored.get(task.id).unwrap().status,
            TaskStatus::WaitingForApproval
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn torn_final_journal_records_are_dropped_without_losing_committed_state() {
        let root = root("torn-journal");
        let manager = new_manager(&root);
        let task = manager.create("torn", "journal", "legacy");
        let directory = task_directory(&root, task.id);
        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        manager.emitter(task.id).record_evidence(worker_evidence());
        fs::OpenOptions::new()
            .append(true)
            .open(directory.join(EVENTS))
            .unwrap()
            .write_all(b"{\"sequence\":")
            .unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(directory.join(EVIDENCE))
            .unwrap()
            .write_all(b"{\"sequence\":")
            .unwrap();
        drop(manager);

        let restored = new_manager(&root);
        let task = restored.get(task.id).unwrap();
        assert_eq!(task.status, TaskStatus::WaitingForApproval);
        assert_eq!(task.evidence.len(), 1);
        restored.emitter(task.id).notice("after torn tails");
        assert_eq!(
            restored
                .get(task.id)
                .unwrap()
                .display_history()
                .last()
                .unwrap()
                .sequence,
            4
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_middle_journal_record_skips_only_that_task() {
        let root = root("middle-corruption");
        let manager = new_manager(&root);
        let task = manager.create("corrupt", "middle", "legacy");
        let directory = task_directory(&root, task.id);
        let created = fs::read(directory.join(EVENTS)).unwrap();
        fs::write(
            directory.join(EVENTS),
            [created, b"not json\n".to_vec(), b"{}\n".to_vec()].concat(),
        )
        .unwrap();
        drop(manager);

        assert!(new_manager(&root).get(task.id).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
