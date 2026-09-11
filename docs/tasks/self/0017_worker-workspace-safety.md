# 0017 — Worker Workspace Safety

## Goal

Strengthen worker workspace isolation before using Multiagent Chat for the real take-home assignment.

Fix two issues:

1. coding workers must not be able to push through inherited Git remotes;
2. Codex must reliably access the approved specification while remaining inside its sandbox model.

Preserve all guarantees from tasks `0001–0016`.

## Requirements

### Disable worker Git remotes

For cloned existing-project workspaces:

* preserve source repository metadata internally if needed;
* remove or disable push-capable remotes inside the worker workspace before any coding agent runs;
* Claude Code and Codex must not be able to publish using inherited `origin`;
* explicit GitHub publishing from `0016` remains the only supported publishing path.

Do not rely only on prompt text such as "do not push".

Do not break baseline/revision tracking used for task diffs.

### New Project

New Project / Take-home Assignment workspaces must continue to start without a remote.

Do not automatically configure one for workers.

### Codex approved-spec access

Codex runs with workspace sandboxing but the approved specification currently lives outside the repository directory.

Make approved-spec access explicit and supported.

Preferred approaches:

* safely expose the artifacts directory using the supported Codex sandbox/additional-directory mechanism; or
* stage a read-only/scoped copy accessible to Codex and remove it afterward.

Do not weaken the sandbox simply to make the file readable.

The evidence-safe `<APPROVED_SPEC_PATH>` behavior must remain intact.

### Worker parity

Both Claude Code and Codex must receive the same logical approved specification and milestone instructions.

Provider-specific filesystem handling may live inside their adapters.

### Audit / security

Do not expose:

* Git credentials;
* credential-helper contents;
* SSH keys;
* authentication tokens;
* unsafe absolute paths in evidence.

## Acceptance Criteria

* Existing-project coding workspaces have no usable push remote.
* Worker cannot push merely because the source repository had authenticated `origin`.
* Explicit `0016` publishing still works from persistent output.
* Codex can reliably read the approved specification.
* Claude Code behavior remains compatible.
* Existing Git baseline/diff behavior remains correct.
* Relevant regression tests pass.

## Required Tests

Cover at least:

1. cloned repository remote is disabled/removed before worker execution;
2. worker-side push attempt cannot use inherited origin;
3. New Project has no worker remote;
4. Codex can access the approved specification through the supported sandbox configuration;
5. task diff/baseline behavior still works;
6. explicit publishing remains independent from worker remotes.

## Out of Scope

Do not implement:

* full container/VM isolation;
* network sandboxing;
* new GitHub authentication;
* cancellation improvements;
* global queue/rate limiting.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
