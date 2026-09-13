# Direct Specification for Registered Projects

## Context

Task `0024_implement-existing-specification.md` added the ability to provide an existing specification directly to the implementation worker, bypassing proposer/debate/spec generation.

That flow must support both:

1. a new project
2. an already registered existing project

The registered-project path is currently broken.

## Problem

For an `ImplementExistingSpecification` task with `project_id`, request validation accepts the task without `technology`.

However, the pipeline still treats every `ImplementExistingSpecification` task as a new-project task.

It therefore enters the new-project preparation branch and requires `technology`, causing the task to fail before implementation.

The problem is broader than one match arm: code currently uses `TaskKind::ImplementExistingSpecification` and/or `creates_new_project()` in places where the correct behavior depends on the task target.

## Goal

Make direct-specification tasks correctly support registered existing projects while preserving the new-project direct-specification behavior introduced by 0024.

## Requirements

### 1. Model target semantics correctly

Do not determine new-project vs existing-project behavior only from `TaskKind`.

The following are different:

- `ImplementExistingSpecification` without `project_id`
    - creates a new project

- `ImplementExistingSpecification` with `project_id`
    - targets an existing registered project

Centralize this distinction in task-level logic.

Avoid scattering checks such as:

```rust
task.kind == TaskKind::ImplementExistingSpecification
```

The task target is determined from the complete `Task`, not from its kind in
isolation. `Task::targets_new_project()` returns true only when the task kind
supports creating a project and `project_id` is absent. Therefore:

- `ImplementExistingSpecification` with `project_id = None` targets a new,
  empty project and retains 0024's technology and output requirements.
- `ImplementExistingSpecification` with `project_id = Some(...)` targets the
  registered project and must use that project's source metadata and detected
  profile.

### 2. Registered-project preparation and inspection

For a registered-project direct specification, the pipeline must:

1. load the registered `Project`;
2. prepare its isolated task workspace through the existing-project provider
   path, using the registered source and default branch;
3. preserve worker remote isolation from 0017;
4. inspect the prepared repository and detect its technology/profile;
5. record and carry the prepared source revision as the task baseline.

The registered-project form does not require `technology`, `output`, or
`destination`; those values come from the registered-project workflow and its
normal result handling. A missing registered project remains an error.

### 3. Direct specification execution

The supplied Markdown is validated with the existing 0022 format validator
and remains the authoritative approved specification. It is not rewritten,
regenerated, or passed through a proposer/debate/specification-generation
stage. The task records its source as `user_provided`.

Both target modes continue through the ordinary implementation pipeline after
target preparation:

```text
validate supplied specification
  → prepare target workspace
  → inspect registered repository when applicable
  → parse milestones and acceptance criteria
  → run worker
  → verify
  → run post-implementation critic/fix loop
  → commit according to the task Git mode
  → capture result, evidence, and persistence
```

The new-project direct-specification path still requires its selected
technology and output configuration, creates an isolated empty workspace, and
does not perform existing-repository inspection.

### 4. Rebuild and prior guarantees

The implementation must preserve 0023 behavior. A failed approved build may
be rebuilt only when its retained workspace is still safe and available. A
rebuild reuses the exact approved specification, frozen agents, detected
profile, source baseline, completed milestones, and partial work. It must not
rerun proposer/debate/spec generation or request approval a second time.

Existing-project remotes remain removed from worker-visible Git configuration;
publication and source identity safeguards remain outside that workspace.

### 5. Regression coverage

Tests must cover both direct-specification target modes and prove that:

- a valid direct specification without `project_id` keeps the new-project
  behavior;
- a valid direct specification with a registered `project_id` does not require
  technology or output fields;
- the registered source is prepared and inspected in an isolated workspace;
- the detected profile and source revision are propagated;
- the supplied specification is preserved as the authoritative text;
- proposer, debate, and specification generation are skipped;
- the normal worker, verification, post-implementation critic, and commit
  pipeline remains available;
- existing-project remote isolation remains intact;
- an approved failed implementation remains compatible with 0023 rebuild.

Use local or scripted workers/providers in tests; normal tests must not call
external model APIs or publish to a remote repository.

### 6. Validation and scope

Before completion, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Task 0025 changes only direct-specification target semantics and their
coverage/documentation. Restart-safe persistence of registered projects or
other project metadata belongs to task 0026 and is explicitly out of scope.
