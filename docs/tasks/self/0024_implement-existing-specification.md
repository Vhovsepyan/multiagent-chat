# 0024 — Implement Existing Specification

## Goal

Allow a user who already has a specification to skip debate/spec generation and send that existing specification directly to Claude Code or Codex for implementation.

Preserve all guarantees from tasks `0001–0023`.

## New Task Mode

Add a task mode such as:

```text
Implement Existing Specification
```

This mode must bypass:

* proposer debate;
* critic debate;
* specification generation.

It must still use:

* specification validation;
* milestone planning;
* acceptance criteria;
* worker execution;
* verification;
* post-implementation critic review;
* Git milestone commits;
* evidence/audit;
* durable persistence;
* correctness gate;
* rebuild-after-failure.

## Input

Allow the user to provide an existing Markdown specification.

At minimum support:

* pasted specification text.

File import may be added only if it fits cleanly into the existing UI.

## Specification Validation

Reuse the `0022` specification-format validator.

The imported specification must satisfy the same contract as generated specs, including:

* required sections;
* valid `## Steps`;
* parseable milestones;
* acceptance criteria;
* verification section.

Do not create a separate weaker parser.

If invalid, show the structural error before implementation starts.

Do not automatically change technical meaning.

## Approval

The provided specification is user-supplied, so debate approval is not required.

The UI may show:

```text
Validate and build
```

or:

```text
Approve and build
```

After validation succeeds, the specification becomes the authoritative approved specification for the task.

Record that its source is:

```text
user_provided
```

rather than AI-generated.

## Worker Selection

Allow normal worker selection:

```text
Claude Code
or
Codex
```

Reuse existing model/tool selection.

Proposer and critic configuration used for debate are not required for this mode.

The post-implementation critic may still use the configured critic provider/model if the current pipeline requires it.

## Milestones

Generate milestone state directly from the imported `## Steps`.

Example:

```text
Imported specification
→ validate
→ parse milestones
→ build milestone 1
→ verify
→ critic
→ commit
→ milestone 2
...
```

Do not regenerate or rewrite the specification.

## Audit / Evidence

Record clearly:

```text
specification_source = user_provided
```

Evidence must distinguish this from:

```text
specification_source = generated_by_agents
```

Do not invent proposer/debate records for a task that skipped debate.

Record:

* specification imported;
* validation result;
* user initiated build;
* worker/model used;
* normal milestone/verification/critic evidence.

## Durable Persistence

Persist imported specification and source metadata through `0019`.

After restart, the task must remain rebuildable under `0023` if implementation fails.

## Correctness

All `0018` correctness gates remain active.

Imported specifications must not bypass:

* milestone completion requirements;
* acceptance criteria;
* verification;
* critic PASS;
* commit requirements;
* publication preflight.

## UI

Add a clear task creation choice such as:

```text
New Project
Feature
Bug Fix
Take-home Assignment
Implement Existing Specification
```

For this mode show:

* title;
* specification text;
* worker selection;
* relevant critic selection if needed;
* Git mode/output settings.

Do not show proposer configuration when debate is skipped.

## Acceptance Criteria

* user can create a task from an existing specification;
* proposer/debate is not invoked;
* spec generation is not invoked;
* existing spec validator is reused;
* invalid specs are rejected before build;
* valid spec becomes the approved authoritative spec;
* milestones come from the provided `## Steps`;
* Claude Code and Codex both work;
* normal verification/critic/commit pipeline remains active;
* evidence marks the spec as user-provided;
* durable restart works;
* failed implementation can use `0023` rebuild;
* existing normal task modes remain unchanged.

## Required Tests

Cover at least:

1. valid imported specification starts implementation without debate;
2. proposer is never invoked;
3. spec generator is never invoked;
4. invalid `## Steps` is rejected;
5. imported milestones are parsed correctly;
6. exact imported spec is preserved;
7. source metadata is `user_provided`;
8. Codex worker path works;
9. Claude Code worker path works;
10. post-implementation critic still runs;
11. durable reload preserves imported spec;
12. failed build remains rebuildable;
13. existing normal/debate flow still works.

## Out of Scope

Do not implement:

* automatic rewriting of arbitrary specifications;
* debate of imported specifications;
* changing the specification during implementation;
* generic document formats other than supported Markdown;
* full specification editor/versioning.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
