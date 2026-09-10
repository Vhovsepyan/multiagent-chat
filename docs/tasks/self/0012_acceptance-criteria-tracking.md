# 0012 — Acceptance Criteria Tracking

## Goal

Turn the approved specification into explicit, traceable acceptance criteria and track each criterion through implementation, verification, critic review, and final task status.

Preserve all guarantees from tasks `0001–0011`.

## Requirements

### Acceptance criteria model

Create stable criterion IDs, for example:

```text
AC-001
AC-002
AC-003
```

Each criterion should contain at least:

* id;
* description;
* status;
* related milestone(s);
* verification evidence where available.

Suggested statuses:

```text
pending
implemented
passed
failed
deferred
```

Do not mark a criterion `passed` only because the worker says it was implemented.

### Generation

Generate acceptance criteria from the approved specification before milestone execution.

The generated criteria must be stored on the task/run and remain stable for that run.

Do not silently discard requirements that do not map cleanly to a milestone.

### Milestone mapping

Each milestone should reference the acceptance criteria it is responsible for.

Example:

```text
Milestone 3
→ AC-004
→ AC-005
→ AC-006
```

### Verification

Verification results must update criterion state/evidence where appropriate.

A criterion should become `passed` only when supported by actual verification or equivalent existing evidence.

If verification fails, affected criteria must remain failed/unverified.

### Critic integration

Post-implementation critic findings from `0011` should reference acceptance-criterion IDs where possible.

Example:

```text
AC-008
Severity: high
Evidence: ...
Correction: ...
```

A critic finding against a criterion must prevent that criterion from being reported as passed until resolved.

### Final traceability

At the end of a task, it should be possible to answer:

```text
What was required?
Which milestone implemented it?
How was it verified?
Did it pass?
What remains incomplete?
```

### Audit / evidence

Acceptance-criteria state changes should appear in existing audit/evidence infrastructure.

Evidence export and final report should contain a concise acceptance-criteria summary.

Do not duplicate large verification logs.

### UI

Show acceptance criteria in task details.

Example:

```text
AC-001 Event creation             PASS
AC-002 Duplicate prevention       PASS
AC-003 Reminder delivery          FAILED
AC-004 Realtime updates           PENDING
```

Keep milestone and review UI intact.

## Acceptance Criteria

* Approved specs produce stable acceptance criteria.
* Criteria are stored per task/run.
* Milestones reference relevant criteria.
* Verification updates criterion status.
* Critic findings can reference criteria.
* Unresolved findings prevent false PASS state.
* Failed/deferred criteria remain visible.
* Final report/evidence contains criterion status.
* UI shows criterion progress.
* Existing task types and workflow remain compatible.
* Relevant tests are added.
* Required project checks pass.

## Required Tests

Cover at least:

1. criteria generated from approved spec;
2. stable IDs/order;
3. criteria mapped to milestones;
4. successful verification marks relevant criteria passed;
5. failed verification does not mark criteria passed;
6. critic finding keeps criterion unresolved until fixed;
7. deferred/incomplete criteria remain visible;
8. evidence/final report includes criteria summary.

## Out of Scope

Do not implement:

* automatic test generation for every criterion;
* Codex worker;
* take-home assignment mode;
* GitHub publishing.

## Completion

Run targeted tests while developing and full required verification before completion.

Commit and push to `main`.
