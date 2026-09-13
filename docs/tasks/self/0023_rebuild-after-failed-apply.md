# 0023 — Rebuild After Failed Apply

## Goal

When a task already has an approved specification but worker/build execution fails, allow the user to click **Approve and build** again and continue building the same task.

Do not restart debate or regenerate the specification.

Preserve all guarantees from tasks `0001–0022`.

## Example

Current behavior:

```text
Debate
→ Spec generated
→ Human approves
→ Build starts
→ Codex fails
→ Task = Failed
→ no way to continue
```

Required behavior:

```text
Debate
→ Spec generated
→ Human approves
→ Build starts
→ Codex fails
→ Task = Failed
→ "Approve and build" available again
→ click
→ continue build from first incomplete milestone
```

## Retry Eligibility

Allow rebuild only when:

* task has an approved specification;
* failure happened after approval during build/execution;
* workspace still exists;
* task is not already completed;
* task is not currently running;
* Git/workspace state is safe enough to continue.

Do not allow rebuild for failures during:

* debate;
* specification generation;
* specification validation;
* pre-approval state.

## Approved Specification

Reuse the exact existing approved specification.

Do not:

* regenerate it;
* rerun proposer;
* rerun critic debate;
* ask for approval again internally;
* change acceptance criteria.

The existing approved specification remains authoritative.

## Agent Configuration

Reuse the same frozen:

* proposer;
* critic;
* worker;
* provider;
* model selections.

Do not silently change agents/models during rebuild.

## Resume Point

Resume from the first milestone that is not successfully completed.

Example:

```text
Milestone 1 ✅
Milestone 2 ✅
Milestone 3 ❌
Milestone 4 pending
```

Clicking **Approve and build** again starts from:

```text
Milestone 3
```

Milestones 1 and 2 must not run again.

## Partial Failed-Milestone Work

A worker may modify files before exiting unsuccessfully.

Do not automatically discard that work.

Before retry:

* validate workspace exists;
* validate Git repository state;
* reject unsafe merge/rebase/conflict states;
* preserve existing partial changes;
* let the worker inspect the current workspace before continuing.

Do not use:

```text
git reset --hard
git clean
```

or other destructive recovery.

The retry worker prompt should make clear that a previous build attempt may have left partial work and it must inspect the current state before changing files.

## UI

For eligible failed tasks, show the **Approve and build** button again.

The button may use the existing visual design.

Internally, if the specification is already approved, clicking it means:

```text
retry existing approved build
```

not:

```text
approve specification again
```

Do not create duplicate approval events.

## API / State Transition

Support a safe transition such as:

```text
Failed
→ Implementing
```

only through the rebuild eligibility checks.

Do not make arbitrary failed tasks mutable.

The existing normal approval flow must remain unchanged.

## Audit / Evidence

Keep all previous failure history.

Append new events such as:

```text
build_retry_requested
build_retry_started
```

Include:

* previous failure reason;
* retry timestamp;
* resume milestone.

Do not delete or overwrite:

* previous worker evidence;
* previous failure event;
* previous verification results.

## Durable State

The retry state must work with `0019` durable persistence.

After application restart, an eligible failed task must still be able to show **Approve and build** and resume.

## Correctness

All existing `0018` correctness gates still apply.

A retry does not make acceptance criteria pass automatically.

GitHub publishing remains blocked until the task actually completes successfully.

## Acceptance Criteria

* failed post-approval tasks can show Approve and build again;
* clicking it does not rerun debate/spec generation;
* approved spec is reused unchanged;
* frozen agents/models remain unchanged;
* completed milestones are not rerun;
* retry starts from first incomplete/failed milestone;
* partial worker changes are preserved;
* unsafe Git state blocks retry;
* previous failure evidence remains;
* retry events are appended;
* no duplicate approval event is created;
* durable restart preserves rebuild ability;
* completed tasks cannot rebuild;
* pre-approval failures cannot use this path;
* normal first-time approval behavior remains unchanged.

## Required Tests

Cover at least:

1. approved task + worker failure → Approve and build appears again;
2. retry does not invoke proposer/spec generation again;
3. retry reuses exact approved spec;
4. milestones 1–2 passed, milestone 3 failed → retry begins at milestone 3;
5. completed milestones are not rerun;
6. partial failed-milestone changes remain in workspace;
7. unsafe merge/rebase/conflict state blocks rebuild;
8. previous failure events remain in audit/evidence;
9. rebuild appends retry event;
10. no duplicate specification-approved event;
11. failed eligible task remains retryable after server restart;
12. completed task does not expose rebuild;
13. failure before approval does not expose rebuild.

## Out of Scope

Do not implement:

* automatic retries;
* retry backoff;
* changing models/providers during retry;
* editing approved specification;
* retrying debate/spec-generation failures;
* destructive workspace rollback;
* full generic retry framework.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
