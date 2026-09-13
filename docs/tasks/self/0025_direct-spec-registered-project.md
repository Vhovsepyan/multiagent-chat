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