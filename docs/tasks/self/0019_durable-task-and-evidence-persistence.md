# 0019 — Durable Task and Evidence Persistence

## Goal

Persist task state, audit history, evidence, approval state, milestones, acceptance criteria, documentation metadata, persistence metadata, and publication metadata so application restart does not destroy the take-home assignment history.

Use a simple durable local storage design.

Do not introduce a database unless clearly necessary.

Preserve all guarantees from tasks `0001–0018`.

## Requirements

### Storage layout

Persist each task under an application-controlled runtime directory.

For example:

```text
.runtime/
└── tasks/
    └── <task-id>/
        ├── task.json
        ├── events.jsonl
        └── evidence.jsonl
```

Exact structure may differ.

Use task IDs, not task titles, for filesystem paths.

### Persist task state

Persist enough state to restore:

* task id/type/title/description;
* frozen proposer/critic/worker configuration;
* task status;
* specification/approval state;
* milestones and statuses;
* acceptance criteria;
* Git mode;
* output/persistence configuration and result;
* critic/fix-loop state where needed;
* generated documentation metadata;
* publication metadata;
* completion-checklist inputs.

### Audit events

Persist `RecordedEvent` history durably.

Preserve:

* sequence;
* timestamp;
* event payload;
* ordering.

After restart, new events must continue from the next sequence without duplication.

### Evidence

Persist detailed evidence records needed for:

* `agent-session.jsonl`;
* `DEVELOPMENT_LOG.md`;
* `DECISIONS.md`;
* `AGENT_USAGE.md`;
* `FINAL_REPORT.md`.

Evidence export must work after restart.

### Approval state

If a task is waiting for human approval when the server stops, restore it as waiting for approval.

Do not automatically approve or continue it.

### Interrupted active tasks

If the process stops while a task is actively running:

* do not mark it completed;
* do not automatically restart the worker;
* restore it as interrupted/recovery-required, or another explicit safe failure state;
* preserve all completed audit/evidence up to the interruption.

### Atomic task snapshots

Task-state snapshots must be written atomically enough that a crash does not destroy the only usable copy.

Prefer:

```text
write temporary file
→ flush
→ atomic rename/replace
```

or equivalent safe behavior.

### Append-only history

Events and evidence should use append-only persistence where practical.

Do not rewrite the full event/evidence history on every new record unless there is a strong reason.

### Startup recovery

At application startup:

* discover persisted task directories;
* validate persisted state;
* restore tasks into `TaskManager`;
* rebuild correct next sequence numbers;
* safely report/skip corrupted task entries instead of crashing the whole application.

### Persistence timing

Durable state must be updated after meaningful state changes, including at least:

* task creation;
* task status changes;
* specification approval/rejection;
* milestone changes;
* acceptance-criteria changes;
* audit events;
* evidence records;
* persistence result;
* publication result.

Do not wait until task completion before persisting everything.

### Security

Reuse existing redaction rules.

Never persist:

* API keys;
* authorization headers;
* bearer tokens;
* Git credentials;
* cloud credentials;
* secret environment values.

Persisted task state must contain only safe provider/model metadata, not credentials.

### Existing project artifacts

Do not duplicate entire generated repositories into task-state persistence.

Persistent project output remains managed by the existing persistence workflow.

Store references/metadata only where appropriate.

### Cleanup

Do not automatically delete completed task history/evidence.

Retention policy is out of scope.

## Acceptance Criteria

* Completed tasks survive application restart.
* Waiting-for-approval tasks restore correctly.
* Interrupted active tasks restore safely and are not auto-resumed.
* Audit sequence/timestamps survive exactly.
* New event sequences continue without duplication.
* Evidence export works after restart.
* Frozen agent configuration survives restart.
* Milestones and acceptance criteria survive restart.
* persistence/publication metadata survives restart.
* corrupted task storage does not crash application startup.
* secrets are absent from durable task files.
* existing API/UI behavior remains compatible.
* relevant tests pass.

## Required Tests

Cover at least:

1. create task → persist → reconstruct manager;
2. completed task survives restart;
3. waiting-for-approval task restores;
4. running task becomes interrupted/recovery-required;
5. audit history and ordering survive;
6. next sequence continues correctly after restart;
7. evidence export works after reload;
8. milestones/criteria survive;
9. publication/persistence metadata survives;
10. corrupt task storage is isolated;
11. secrets are not written to durable storage.

## Out of Scope

Do not implement:

* PostgreSQL or another database;
* distributed/multi-node coordination;
* automatic worker resume after crash;
* cloud storage;
* retention/garbage collection;
* global task queue.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
