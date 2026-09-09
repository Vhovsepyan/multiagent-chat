# 0008 — Milestone-Based Worker Execution

## Goal

Change worker execution from one large implementation step into sequential milestones.

Each milestone must:

* have a clear objective;
* be executed separately;
* be verified before the next milestone starts;
* have its own status and timestamps;
* appear in task history/evidence.

Preserve all guarantees from tasks `0001–0007`.

---

## Requirements

### Milestone model

Add a structured milestone representation containing at least:

* id/order;
* title;
* objective;
* status;
* related verification instructions;
* started timestamp;
* completed timestamp;
* worker result summary.

Suggested statuses:

```text
pending
running
passed
failed
cancelled
```

---

### Milestone planning

After the specification is approved, create an ordered milestone plan.

Example:

```text
1. Project bootstrap
2. Database/model layer
3. Core business flow
4. Concurrency handling
5. Realtime updates
6. Tests/documentation
```

The exact milestones depend on the task.

Do not accept an empty or invalid milestone plan.

Do not silently fall back to one giant implementation step.

---

### Execution

Execute milestones sequentially.

For each milestone:

```text
record start
→ invoke configured worker
→ verify milestone
→ record result
→ continue only if successful
```

Only one write-capable worker may modify the same workspace at a time.

Use the task-level worker configuration introduced earlier.

---

### Worker context

The worker should receive:

* approved specification;
* current milestone;
* relevant acceptance/details for that milestone;
* necessary repository context.

Do not ask the worker to implement future milestones early.

---

### Verification

Run appropriate verification after every milestone.

Reuse existing verification mechanisms where possible.

A milestone must not become `passed` only because the worker claims success.

Verification result must determine whether execution can continue.

---

### Failure behavior

If a milestone fails:

* mark it failed;
* record evidence;
* stop later milestones;
* do not report the task as successfully completed.

Do not introduce unlimited retries.

Recovery/fix loops belong to a later task.

---

### Cancellation

If task execution is cancelled or aborted:

* stop future milestones;
* preserve already completed milestone state;
* mark active milestone appropriately;
* record audit/evidence events.

---

### Audit / evidence

Record milestone lifecycle events in the existing timestamped audit/evidence infrastructure.

At minimum:

```text
milestone_started
milestone_completed
milestone_failed
milestone_cancelled
```

Include:

* milestone id;
* title;
* worker tool/model where useful;
* verification result.

Do not expose secrets.

---

### UI

Show milestone progress in task details.

Example:

```text
1. Bootstrap              PASS
2. Persistence            PASS
3. Registration           RUNNING
4. Concurrency            PENDING
5. Realtime               PENDING
```

Keep existing task history working.

---

## Acceptance Criteria

* Approved specification produces an ordered milestone plan.
* Milestones have stable IDs/order and statuses.
* Worker executes one milestone at a time.
* Each milestone is verified before continuing.
* Failed milestone stops subsequent execution.
* Cancellation stops future milestones.
* Milestone events appear in audit/evidence.
* UI displays milestone progress.
* Existing task types continue working.
* Existing execution/security limits remain intact.
* Relevant tests are added.
* `cargo fmt`, `cargo clippy`, frontend checks, and full tests pass.

---

## Required Tests

Cover at least:

1. milestone plan creation;
2. sequential execution;
3. successful milestone progression;
4. failed milestone stops later milestones;
5. cancellation stops later milestones;
6. milestone timestamps/statuses;
7. audit/evidence integration;
8. task-level worker selection is respected.

---

## Out of Scope

Do not implement:

* automatic Git commits;
* worker retry/fix loop;
* acceptance-criteria tracking;
* Codex worker;
* GitHub publishing;
* take-home assignment mode.

---

## Completion Report

Report:

* files changed;
* milestone data model;
* planning approach;
* execution flow;
* failure/cancellation behavior;
* UI changes;
* tests and verification;
* remaining limitations.
