# Restart-Safe Existing-Project Rebuild

## Context

Task 0023 added rebuild/resume support for failed approved builds.

A failed build can be resumed using:

`Approve and build again`

Passed milestones remain completed and execution resumes from the first incomplete milestone.

However, existing-project execution still depends on the in-memory project registry.

## Problem

Task state is durable, but registered projects are currently held by the in-memory `ProjectStore`.

Consider:

1. user registers repository A
2. task is created against repository A
3. specification is approved
4. implementation partially succeeds
5. a later milestone fails
6. task/workspace state is persisted
7. multiagent-chat server restarts
8. task is restored
9. `ProjectStore` is empty
10. user clicks `Approve and build again`

The task still contains `project_id`, but that ID can no longer be resolved.

The rebuild therefore fails even though the task and retained workspace are otherwise recoverable.

## Goal

An already-created existing-project task must contain enough durable repository identity to resume safely after a server restart.

Execution of persisted tasks must not depend exclusively on the original in-memory project registration.

## Requirements

### 1. Freeze repository identity at task creation

When creating a task for a registered existing project, copy the required repository execution information into durable task state.

Store only non-secret information required to reproduce the task source, for example:

- normalized repository identity
- repository clone/source URL
- base/default branch where relevant
- project display name if useful
- source revision/baseline information where appropriate

The exact data model is an implementation decision.

The important rule is:

> after task creation, task execution must not require the original mutable ProjectStore entry to still exist.

### 2. Project registration is configuration, task source is historical input

Project registration may be used while creating a new task.

After creation, the task must represent the repository source that was selected at that moment.

Later changes to the project registry must not silently change an already-created task.

For example:

1. task A is created from repository `owner/repo`
2. project registration is removed or changed
3. task A must still represent the original source

### 3. Restart-safe rebuild

For a persisted failed approved task:

1. restore durable task state
2. reopen/reuse retained workspace when valid
3. retain passed milestones
4. identify first non-passed milestone
5. resume implementation from there
6. do not rerun proposer/debate
7. do not regenerate the specification
8. do not require another approval
9. do not require the original ProjectStore entry

### 4. Preserve source revision safety

Do not silently rebuild against a newer arbitrary repository state.

Preserve the source/baseline guarantees already used by existing-project tasks.

If the retained workspace is reused, ensure it belongs to the expected task/repository.

If reconstruction is necessary, use durable task source metadata.

Do not silently change the task source to the latest upstream revision.

### 5. Legacy persisted tasks

Existing persisted tasks created before this change may not contain frozen repository metadata.

Handle them safely.

Allowed behavior:

- resolve through ProjectStore when the original registration is still available

Otherwise:

- fail clearly with an actionable error explaining that the historical repository identity cannot safely be reconstructed

Do not guess repository information.

### 6. Secrets

Do not serialize credentials into durable task state.

Do not persist:

- GitHub tokens
- API tokens
- passwords
- authenticated remote URLs containing credentials

Continue obtaining authentication from the normal execution environment.

### 7. Keep ProjectStore scope limited

Do not make the complete project registry durable unless architecture analysis shows it is truly necessary.

Prefer immutable/frozen execution inputs on the task.

This makes tasks reproducible independently from later registry mutations.

## Tests

Add tests covering at minimum:

1. create task from registered project
2. persist task
3. remove/reconstruct ProjectStore
4. restore task
5. resume failed approved build successfully
6. passed milestones remain passed
7. execution begins at first incomplete milestone
8. proposer/debate are not rerun
9. approval is not requested again
10. repository identity remains the original one
11. later project re-registration does not alter an existing task
12. legacy task fallback works when ProjectStore entry exists
13. legacy task fails safely when source cannot be reconstructed
14. no credentials are persisted
15. source revision guarantees remain intact

Prefer a restart-style integration test rather than only testing serialization structures.

## Validation

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test