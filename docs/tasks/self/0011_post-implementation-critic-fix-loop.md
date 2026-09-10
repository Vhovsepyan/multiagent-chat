# 0011 — Post-Implementation Critic Fix Loop

## Goal

After implementation, let the critic review the actual code/result against the approved specification and milestone requirements.

If the critic finds concrete issues, send them back to the worker for a bounded fix cycle.

Preserve all guarantees from tasks `0001–0010`.

## Requirements

### Critic review

After worker implementation and verification, run the configured critic against:

* approved specification;
* current milestone or final task scope;
* relevant implementation diff/files;
* verification results;
* known limitations.

The critic must return structured output:

```text
status: PASS | FIX_REQUIRED

findings:
- requirement reference
- severity
- evidence
- requested correction
```

Do not accept vague prose as the only machine-readable result.

### Fix loop

If `FIX_REQUIRED`:

```text
critic findings
→ worker fixes only those findings
→ verification reruns
→ critic reviews again
```

Use the same task-level worker and critic configuration already stored on the task.

Do not allow an infinite loop.

Use a small configurable maximum, defaulting to something reasonable such as 2 fix iterations.

### Failure behavior

If:

* verification keeps failing;
* critic still returns `FIX_REQUIRED` after the max iterations;
* critic execution fails;
* worker fix execution fails;

mark the milestone/task as requiring failure/human review according to the existing workflow.

Do not claim success.

### Audit / evidence

Record:

* critic implementation review started/completed/failed;
* critic findings;
* fix iteration number;
* worker fix started/completed/failed;
* verification result after each fix;
* final review disposition.

Include this history in the existing evidence export.

Do not expose secrets or unbounded output.

### UI

Show the review/fix state clearly, for example:

```text
Implementation review: FIX_REQUIRED
Fix iteration: 1/2
Final review: PASS
```

Keep milestone status and existing history behavior intact.

## Acceptance Criteria

* Critic reviews actual implementation, not only the design.
* Critic result is structured.
* `PASS` finishes normally.
* `FIX_REQUIRED` triggers worker correction.
* Verification reruns after each fix.
* Critic re-reviews after each fix.
* Maximum iterations prevent infinite loops.
* Unresolved findings prevent successful completion.
* Review/fix events appear in audit/evidence.
* Existing milestone/Git/persistence behavior remains correct.
* Relevant tests are added.
* Final required checks pass.

## Required Tests

Cover at least:

1. critic returns PASS;
2. FIX_REQUIRED → worker fix → PASS;
3. FIX_REQUIRED until max iterations exhausted;
4. worker fix failure;
5. critic failure;
6. verification failure after fix;
7. audit/evidence contains all iterations;
8. configured critic/worker models are respected.

## Out of Scope

Do not implement:

* acceptance-criteria tracking;
* Codex worker;
* take-home assignment mode;
* GitHub publishing.

## Completion

Run targeted tests while developing, then run the full required verification once before completion.

Commit and push to `main`.
