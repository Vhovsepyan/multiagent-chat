# 0006 — Timestamped Immutable Audit Events

## Goal

Make the task history suitable for a chronological development audit.

Every significant task event must be recorded with:

* a stable sequence number;
* a UTC timestamp;
* the event type/payload;
* its task/run association.

The audit history must be append-only during a run.

This task prepares the system for the later evidence-export and development-log features.

---

## Context

MadDevs requires evidence that development happened progressively over time.

They explicitly care about:

* timestamps;
* gradual execution;
* agent activity;
* development history;
* reproducibility.

The application already records task events, but the event history now needs stronger guarantees.

After this task, it should be possible to reconstruct a task chronologically without relying on UI timing or current task state.

Preserve all guarantees introduced in tasks `0001–0005`.

---

# Requirements

## 1. Introduce a recorded-event envelope

Do not add timestamp fields independently to every `TaskEvent` variant.

Introduce a common wrapper.

Conceptually:

```rust
struct RecordedEvent {
    sequence: u64,
    timestamp: DateTime<Utc>,
    event: TaskEvent,
}
```

Exact names may differ.

The important properties are:

```text
RecordedEvent
├── sequence
├── timestamp
└── event
```

---

## 2. Sequence number

Every event within a task/run must receive a monotonically increasing sequence number.

Example:

```text
1
2
3
4
5
...
```

Requirements:

* sequence starts from a deterministic value;
* sequence is unique within one task/run;
* sequence increases in event-recording order;
* sequence must not be reused;
* sequence must not depend on frontend behavior.

The exact starting value may be `0` or `1`, but it must be consistent.

Prefer `1` unless existing conventions suggest otherwise.

---

## 3. Timestamp

Each recorded event must have a UTC timestamp.

Use an unambiguous machine-readable representation.

Preferred serialization:

```text
RFC3339
```

Example:

```text
2026-09-09T10:24:51.412Z
```

Do not store only local time.

Do not depend on the browser clock.

The backend should be the source of truth.

---

## 4. Append-only audit behavior

Historical audit events must not be mutated to represent later state.

For example:

Do NOT do:

```text
event #12:
status = running

later mutate same event:
status = completed
```

Instead:

```text
#12 worker_started
#13 worker_completed
```

Current task state may continue to be mutable separately.

Audit history must represent what happened over time.

---

## 5. Record lifecycle events

Ensure significant lifecycle transitions are represented in the audit trail.

At minimum cover events around:

### Task

```text
task_created
task_started
task_completed
task_failed
task_cancelled
```

Use only events that make sense in the current workflow.

Do not fabricate a state transition just to satisfy this list.

---

## 6. Proposer events

Record meaningful proposer transitions.

At minimum:

```text
proposer_started
proposer_completed
proposer_failed
```

Where appropriate, include safe metadata such as:

```text
provider
model
```

Use the actual task-level selection from task `0005`.

Do not record credentials.

---

## 7. Critic events

Record meaningful critic transitions.

At minimum:

```text
critic_started
critic_completed
critic_failed
```

Include safe role/provider/model metadata where appropriate.

---

## 8. Specification events

Record important specification lifecycle events.

Examples:

```text
spec_generated
spec_updated
spec_approved
spec_rejected
```

Only include states supported by the current workflow.

If the current system has a human approval gate, preserve and audit it.

---

## 9. Worker events

Record worker lifecycle transitions.

At minimum:

```text
worker_started
worker_completed
worker_failed
worker_cancelled
```

Include safe metadata such as:

```text
worker tool
worker model
```

Do not record secrets.

---

## 10. Verification events

Record verification lifecycle where it exists.

At minimum:

```text
verification_started
verification_completed
verification_failed
```

If verification currently produces structured results, associate the event safely with that result.

Do not duplicate huge logs unnecessarily in the event payload.

---

## 11. Failure correctness

Do not emit success events before the corresponding operation actually succeeds.

Example:

Wrong:

```text
record worker_completed
↓
run worker
↓
worker fails
```

Correct:

```text
record worker_started
↓
run worker
↓
worker succeeds
↓
record worker_completed
```

If an operation fails:

```text
started
↓
failed
```

must be visible.

---

## 12. Cancellation correctness

Cancellation must produce an explicit chronological record.

Example:

```text
worker_started
task_cancellation_requested
worker_cancelled
task_cancelled
```

Exact event structure may vary according to existing architecture.

Do not report cancellation as a normal failure unless that is already an intentional product distinction.

---

## 13. Centralize event recording

Avoid manually calculating:

```rust
sequence += 1;
timestamp = Utc::now();
```

in many unrelated pipeline locations.

Prefer a centralized task/audit method.

Conceptually:

```rust
task.record_event(TaskEvent::WorkerStarted { ... });
```

The recording mechanism should automatically assign:

```text
sequence
timestamp
```

This prevents inconsistent behavior.

---

## 14. Thread/concurrency safety

Event recording must remain correct when asynchronous pipeline operations occur.

Sequence numbers must not collide.

Events must not be lost because two async paths tried to record simultaneously.

Use the synchronization primitives already appropriate for the task model.

Do not introduce unnecessary global locking.

Ordering only needs to be guaranteed within the same task/run.

---

## 15. Existing task history API

Update existing APIs that expose task events so they include the recorded-event metadata.

Conceptual response:

```json
{
  "sequence": 12,
  "timestamp": "2026-09-09T10:24:51.412Z",
  "event": {
    "type": "critic_completed"
  }
}
```

Exact JSON structure should follow existing serialization conventions.

Preserve frontend compatibility where practical.

---

## 16. Frontend task history

Update the task history UI to use recorded timestamps.

Display timestamps in a readable way.

The backend timestamp remains the source of truth.

Frontend may convert UTC to local display time if that matches existing application behavior.

Do not reorder events by browser-rendering time.

Use sequence/order from the backend.

---

## 17. Preserve bounded UI history behavior

If the current application intentionally limits large/repetitive UI logs, that behavior may remain.

This task does NOT yet require keeping the entire raw agent session in memory forever.

However:

```text
Recorded task events
```

and:

```text
temporary UI rendering/log truncation
```

should remain conceptually separate.

Do not silently mutate old audit events merely to keep the UI small.

A later task will introduce full evidence export.

---

## 18. Safe event metadata

Audit events may contain safe metadata such as:

```text
role
provider
model
stage
result
error category
duration if already available
```

Do NOT record:

* API keys;
* access tokens;
* authorization headers;
* environment secrets;
* database credentials;
* private credential-file contents;
* raw secret-bearing command-line arguments.

Reuse redaction protections from earlier tasks.

---

## 19. Error messages

If failure messages are stored in task events, apply existing sanitization/redaction rules.

Do not record:

```text
ANTHROPIC_API_KEY=...
```

or equivalent secrets.

If a provider returns a secret-bearing error body, sanitize it before persistence/audit.

---

## 20. Backward compatibility

Existing task flows must continue to work:

```text
New Project
Feature
Bug Fix
```

Task creation, debate, approval, worker execution, verification, and cancellation must continue functioning.

Do not redesign the pipeline for milestone execution yet.

That belongs to task `0008`.

---

# Suggested Data Model

A reasonable architecture is:

```rust
pub struct RecordedEvent {
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub event: TaskEvent,
}
```

Task:

```rust
pub struct Task {
    ...
    events: Vec<RecordedEvent>,
    next_event_sequence: u64,
}
```

Conceptually:

```rust
impl Task {
    pub fn record_event(&mut self, event: TaskEvent) {
        let recorded = RecordedEvent {
            sequence: self.next_event_sequence,
            timestamp: Utc::now(),
            event,
        };

        self.next_event_sequence += 1;
        self.events.push(recorded);
    }
}
```

This is conceptual only.

Choose an implementation consistent with current synchronization and ownership design.

---

# Acceptance Criteria

The task is complete when:

* every significant task event has a sequence number;
* every significant task event has a UTC timestamp;
* sequence numbers are monotonically increasing within a task;
* event history is append-only;
* proposer lifecycle is timestamped;
* critic lifecycle is timestamped;
* specification/approval lifecycle is timestamped where supported;
* worker lifecycle is timestamped;
* verification lifecycle is timestamped;
* cancellation/failure events are represented correctly;
* provider/model metadata uses the actual task-level selection;
* secrets are not written into audit events;
* backend task APIs expose sequence/timestamp information;
* frontend task history still works;
* frontend displays event time appropriately;
* current task types continue to work;
* existing security and execution-limit behavior remains intact;
* existing tests pass;
* new audit-event tests are added;
* `cargo fmt` passes;
* `cargo clippy` passes;
* frontend checks pass where applicable;
* full test suite passes.

---

# Required Tests

Add focused tests covering at least the following.

## Sequence ordering

Record multiple events.

Verify:

```text
event[0].sequence < event[1].sequence < event[2].sequence
```

No duplicates.

---

## Timestamp presence

Verify every recorded event has a serialized UTC timestamp.

---

## Append-only behavior

Record an event.

Record later events.

Verify the original event remains unchanged.

---

## Lifecycle order

Representative successful flow should produce logical ordering such as:

```text
task_started
proposer_started
proposer_completed
critic_started
critic_completed
...
```

Exact event list depends on current architecture.

---

## Failure flow

Simulate an agent/worker failure.

Verify:

```text
started
failed
```

and no false completion event exists.

---

## Cancellation flow

Verify cancellation produces explicit audit events and does not emit normal successful completion.

---

## Agent metadata

Verify stored proposer/critic/worker metadata matches the task's frozen agent selection from `0005`.

---

## Secret redaction

Attempt to record or surface a representative sensitive value through an error/result.

Verify it does not appear in serialized task history.

---

## API serialization

Verify task-history JSON contains:

```text
sequence
timestamp
event
```

with a stable shape.

---

## Frontend

Where current test infrastructure allows:

* history renders events;
* timestamps are visible;
* event ordering follows backend order.

---

# Out of Scope

Do NOT implement:

* full `agent-session.jsonl` export;
* `DEVELOPMENT_LOG.md`;
* `DECISIONS.md`;
* evidence archive download;
* milestone execution;
* milestone Git commits;
* post-implementation critic loop;
* acceptance-criteria tracking;
* Codex worker;
* GitHub publishing;
* take-home assignment mode.

Those belong to later tasks.

---

# Completion Report

When finished, provide:

1. Files changed.
2. Recorded-event data model.
3. How sequence numbers are generated.
4. Timestamp representation and serialization.
5. Lifecycle events added/updated.
6. How concurrency safety is handled.
7. How agent provider/model metadata is recorded.
8. How secret redaction is preserved.
9. Backend/API changes.
10. Frontend changes.
11. Tests added or modified.
12. Verification commands and results.
13. Remaining limitations.

Do not push changes automatically.
