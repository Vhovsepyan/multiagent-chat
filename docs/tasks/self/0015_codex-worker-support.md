# 0015 — Codex Worker Support

## Goal

Add Codex as an alternative coding worker alongside Claude Code.

Reuse the existing `CodingAgent` abstraction and task-level worker selection.

Preserve all guarantees from tasks `0001–0014`.

## Requirements

### Worker tools

Support at least:

```text
Claude Code
Codex
```

The user must be able to choose the worker per task.

### Architecture

Do not add Codex-specific branching throughout the pipeline.

Use the existing worker abstraction:

```text
CodingAgent
├── ClaudeCodeAgent
└── CodexAgent
```

Provider/tool-specific behavior should stay inside the adapter/resolver layer.

### Codex execution

Implement Codex execution using the same safe process model as Claude Code.

Respect:

* workspace boundaries;
* timeout;
* cancellation;
* process-tree cleanup;
* output limits;
* environment filtering;
* secret redaction;
* execution concurrency limits.

Do not bypass existing safeguards.

### Worker input

Codex must receive the same task context required by the current coding-agent contract:

* approved specification;
* current milestone;
* acceptance criteria;
* critic findings during fix iterations;
* verification expectations.

Keep provider-specific prompt shaping inside the Codex adapter where practical.

### Model selection

Expose Codex models through the existing worker tool/model selection flow.

Do not hard-code secrets in the frontend.

If model discovery is not reliably available, use configured allowed/default models.

### Result normalization

Codex execution must return the existing common coding-agent result type.

The rest of the pipeline must not need to know whether Claude Code or Codex executed the work.

### Audit / evidence

Record the actual worker:

* tool;
* model;
* start/end;
* result;
* bounded output metadata.

Evidence export must identify Codex correctly when used.

### UI

Add Codex to the worker selector.

Changing worker tool should update the available worker-model choices.

Keep Claude Code behavior unchanged.

## Acceptance Criteria

* Codex can be selected as worker.
* Claude Code still works.
* Both use the same `CodingAgent` abstraction.
* Milestone execution works with Codex.
* Critic fix loops work with Codex.
* Existing execution limits/cancellation apply to Codex.
* Audit/evidence reports actual Codex tool/model.
* Invalid Codex configuration fails clearly.
* Relevant tests are added.
* Required project checks pass.

## Required Tests

Cover at least:

1. Codex worker selection;
2. Claude Code selection still works;
3. Codex model validation;
4. Codex process execution uses existing limits;
5. milestone execution with Codex;
6. critic fix loop with Codex;
7. audit/evidence reports correct tool/model;
8. failure/cancellation behavior.

## Out of Scope

Do not implement:

* Codex as proposer/critic;
* OpenAI chat-provider support;
* GitHub publishing;
* cloud execution.

## Completion

Run targeted tests while developing and final required verification before completion.

Commit and push to `main`.
