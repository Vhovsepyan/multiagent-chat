# 0004 — Agent Provider Abstraction

## Goal

Decouple the proposer, critic, and worker roles from concrete AI provider implementations.

Currently the orchestration is coupled to specific implementations:

* proposer → Gemini
* critic → Anthropic / Claude
* worker → Claude Code

After this task, orchestration must depend on generic agent abstractions instead of concrete providers.

The current runtime behavior must remain the same after the refactoring.

---

## Context

Later tasks will add:

* per-task provider/model selection;
* different models for proposer, critic, and worker;
* Codex as an alternative worker.

This task only prepares the architecture for those capabilities.

Do not implement those later features yet.

Preserve all security, audit, and execution-limit guarantees introduced by tasks `0001–0003`.

---

## Requirements

### 1. Introduce a Chat Agent abstraction

Create an abstraction for agents used for conversational reasoning, currently:

* proposer;
* critic.

A possible conceptual API is:

```rust
trait ChatAgent {
    async fn complete(
        &self,
        request: AgentRequest,
    ) -> Result<AgentResponse>;
}
```

Exact names and types are up to you.

The abstraction should expose enough metadata to identify:

* provider;
* model.

Provider-specific details must stay inside provider adapters.

---

### 2. Adapt Gemini

The existing Gemini implementation must implement the new `ChatAgent` abstraction.

Existing Gemini behavior must continue to work.

Do not move Gemini-specific logic into the orchestration layer.

---

### 3. Adapt Anthropic / Claude

The existing Anthropic/Claude implementation must also implement the new `ChatAgent` abstraction.

Existing critic behavior must continue to work.

Do not move Anthropic-specific logic into the orchestration layer.

---

### 4. Refactor proposer / critic orchestration

The debate/orchestration code must no longer depend directly on concrete types such as:

```rust
GeminiClient
ClaudeClient
```

Instead, it should receive generic `ChatAgent` implementations.

Conceptually:

```text
Proposer
    ↓
ChatAgent
    ↓
Gemini / Claude / future provider

Critic
    ↓
ChatAgent
    ↓
Gemini / Claude / future provider
```

The orchestration layer should care about the agent role, not the provider implementation.

---

### 5. Introduce a Coding Agent abstraction

Worker agents have different behavior from normal chat agents because they can:

* work with files;
* run commands;
* modify repositories;
* execute tests.

Therefore introduce a separate abstraction for coding agents.

Conceptually:

```rust
trait CodingAgent {
    async fn execute(
        &self,
        request: CodingTaskRequest,
    ) -> Result<CodingTaskResult>;
}
```

Exact names and types are up to you.

---

### 6. Adapt Claude Code worker

Move the existing Claude Code worker behind the new `CodingAgent` abstraction.

Conceptually:

```text
Worker
    ↓
CodingAgent
    ↓
ClaudeCodeAgent
```

Do not change the current Claude Code execution behavior unless necessary for the abstraction.

Existing:

* timeout handling;
* cancellation;
* process cleanup;
* execution limits;
* security restrictions;
* workspace restrictions

must continue to work.

---

### 7. Avoid provider-specific branching

Do not introduce logic such as:

```rust
if provider == "gemini" {
    ...
} else if provider == "anthropic" {
    ...
}
```

throughout the pipeline.

Provider-specific behavior should remain inside provider adapters/factories.

The pipeline should work against the abstractions.

---

### 8. Preserve current defaults

After this task the application's behavior should remain effectively:

```text
Proposer → Gemini
Critic   → Claude / Anthropic
Worker   → Claude Code
```

The purpose of this task is architectural decoupling, not changing user-facing behavior.

Environment configuration should continue to work.

---

## Acceptance Criteria

The task is complete when:

* Gemini implements the chat-agent abstraction.
* Anthropic/Claude implements the chat-agent abstraction.
* Claude Code implements the coding-agent abstraction.
* proposer orchestration does not depend directly on `GeminiClient`.
* critic orchestration does not depend directly on `ClaudeClient`.
* worker orchestration does not depend directly on Claude Code implementation details.
* existing application behavior remains unchanged.
* existing security protections remain intact.
* execution timeout/cancellation behavior remains intact.
* existing tests pass.
* appropriate tests for the new abstractions are added.
* `cargo fmt` passes.
* `cargo clippy` passes.
* the complete test suite passes.

---

## Out of Scope

Do NOT implement:

* UI model selectors;
* task-level model selection;
* task-level provider selection;
* Codex worker;
* GitHub publishing;
* milestone execution;
* changes to the task workflow unrelated to this refactoring.

These belong to later tasks.

---

## Completion Report

When finished, provide a summary containing:

1. Files changed.
2. Agent abstractions introduced.
3. How Gemini was adapted.
4. How Anthropic/Claude was adapted.
5. How Claude Code was adapted.
6. Important architectural decisions.
7. Tests and verification commands executed.
8. Any remaining limitations.

Do not push changes automatically.
