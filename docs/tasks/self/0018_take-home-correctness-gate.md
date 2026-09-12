# 0018 — Take-Home Correctness Gate

## Goal

Prevent a Take-home Assignment from being finalized, persisted, or published while required acceptance criteria or milestone state remain inconsistent.

Preserve all guarantees from tasks `0001–0017`.

## Requirements

### Acceptance status ordering

Do not mark acceptance criteria fully `PASSED` before all required milestone finalization steps succeed.

For commit-per-milestone mode:

```text
worker
→ verification
→ critic PASS
→ milestone commit succeeds
→ related acceptance criteria become PASSED
→ milestone completes
```

If commit creation fails:

* milestone fails;
* related criteria must not remain falsely `PASSED`.

### Take-home completion gate

Before a Take-home Assignment becomes `Completed`, require all mandatory acceptance criteria to be successfully resolved.

A required criterion must not remain:

* pending;
* implemented but unverified;
* failed;
* deferred.

If optional criteria are supported explicitly, only those may remain deferred.

### Persistence gate

Persistent project finalization must happen only after the take-home correctness gate passes.

Incomplete required criteria must block persistence.

### Publication gate

GitHub publication must re-check the same correctness gate.

Publishing must be rejected if required acceptance criteria are not fully passed.

### Milestone consistency

A Take-home Assignment must not report overall success when:

* any milestone failed;
* any milestone remains incomplete;
* a critic review remains unresolved;
* required verification is incomplete.

### Documentation / evidence

Ensure these all reflect the same final truth:

* task status;
* completion checklist;
* `FINAL_REPORT.md`;
* `NEXT_STEPS.md`;
* acceptance-criteria summary;
* publication preflight.

Do not allow contradictory states such as:

```text
Task: Completed
AC-004: Failed
```

### Audit

Record a clear event/reason when finalization is blocked because required criteria are incomplete.

Do not expose secrets.

## Acceptance Criteria

* Criterion PASS is applied only after required milestone finalization succeeds.
* Commit failure cannot leave related criteria falsely passed.
* Take-home completion requires all mandatory criteria to pass.
* Deferred required criteria block completion.
* Failed/pending/unverified criteria block persistence.
* Incomplete take-home state blocks GitHub publication.
* Milestone/task/checklist/evidence status remain consistent.
* Existing non-take-home flows remain compatible.
* Relevant tests pass.

## Required Tests

Cover at least:

1. critic PASS + commit failure does not leave criteria passed;
2. all required criteria passed allows completion;
3. deferred required criterion blocks completion;
4. failed criterion blocks persistence;
5. pending/unverified criterion blocks completion;
6. failed/incomplete milestone blocks completion;
7. publication preflight rejects incomplete take-home state;
8. final report/checklist matches the blocked state.

## Out of Scope

Do not implement:

* new acceptance-criteria generation;
* automatic requirement relaxation;
* durable task persistence;
* worker sandboxing changes.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
