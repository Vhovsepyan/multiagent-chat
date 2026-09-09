# 0005 — Per-Task Agent and Model Selection

## Goal

Allow the user to choose the provider/tool and model independently for each agent role when creating a task:

* proposer;
* critic;
* worker.

The selected configuration must belong to the task/run itself.

Do not rely only on global `.env` configuration after the task has been created.

This task builds on the provider abstractions introduced in task `0004`.

---

## Context

Before this task, the application effectively uses fixed/default agent assignments such as:

```text
Proposer → Gemini
Critic   → Anthropic / Claude
Worker   → Claude Code
```

After this task, the user should be able to choose configurations such as:

```text
Proposer
Provider: Gemini
Model: <selected model>

Critic
Provider: Anthropic
Model: <selected model>

Worker
Tool: Claude Code
Model: <selected model>
```

The architecture should also be ready for adding more worker tools later, especially Codex.

Do NOT add Codex in this task.

Preserve the security, audit, and execution-limit guarantees introduced by tasks `0001–0004`.

---

## Requirements

### 1. Add task-level agent configuration

Introduce a task/run configuration that stores agent selection independently for:

* proposer;
* critic;
* worker.

A possible conceptual model is:

```rust
struct AgentSelection {
    proposer: ChatAgentConfig,
    critic: ChatAgentConfig,
    worker: CodingAgentConfig,
}
```

Conceptually:

```rust
struct ChatAgentConfig {
    provider: ChatProvider,
    model: String,
}

struct CodingAgentConfig {
    tool: CodingTool,
    model: String,
}
```

Exact names and types are up to you.

Important:

* proposer and critic use a provider + model;
* worker uses a tool/provider + model;
* worker tool and model must remain separate concepts.

Do not represent worker configuration using only a single string.

---

## 2. Store the configuration on the task/run

When a task is created, resolve and store the actual agent configuration.

Example:

```text
Task
├── proposer
│   ├── provider: Gemini
│   └── model: gemini-...
│
├── critic
│   ├── provider: Anthropic
│   └── model: claude-...
│
└── worker
    ├── tool: ClaudeCode
    └── model: claude-...
```

The task must retain these values for its complete lifecycle.

Do not re-read model selection from `.env` during later pipeline stages.

Environment configuration should only provide:

* defaults;
* supported provider configuration;
* credentials;
* fallback values when the user did not explicitly choose something.

This is important for reproducibility and auditability.

---

## 3. Update task creation API

Extend the task creation request/API to accept agent selection.

The request should support explicit configuration for:

```text
proposer.provider
proposer.model

critic.provider
critic.model

worker.tool
worker.model
```

Use typed enums where appropriate instead of arbitrary strings for provider/tool identifiers.

Models may remain strings if model names are configuration-driven.

---

## 4. Add backend validation

Validate every requested combination before starting the task.

Examples:

Valid:

```text
Gemini + configured Gemini model
Anthropic + configured Claude model
ClaudeCode + configured worker model
```

Invalid examples:

```text
Gemini provider + Anthropic-only model
Unsupported provider
Unknown worker tool
Empty model when model is required
```

Return clear user-facing validation errors.

Do not silently replace an invalid user selection with another provider/model.

---

## 5. Preserve existing defaults

If the user does not explicitly choose agent configurations, preserve the current default behavior.

Conceptually:

```text
Proposer → configured Gemini default
Critic   → configured Anthropic default
Worker   → configured Claude Code default
```

Existing installations using only environment configuration must continue to work.

Do not require users to configure selections manually for every task.

---

## 6. Use selected agents in the pipeline

Update orchestration so the task-level configuration determines which agent implementation is used.

The pipeline should conceptually do:

```text
Task agent configuration
        ↓
Agent factory/resolver
        ↓
ChatAgent / CodingAgent
        ↓
pipeline execution
```

Do not hard-code:

```rust
GeminiClient::new(...)
ClaudeClient::new(...)
ClaudeCodeAgent::new(...)
```

inside task orchestration for fixed roles.

Use the abstractions introduced in `0004`.

---

## 7. Add an agent factory/resolver

Introduce a centralized mechanism responsible for resolving configuration into actual agent implementations.

Conceptually:

```rust
AgentFactory
    ↓
ChatAgentConfig
    → GeminiAgent
    → AnthropicAgent

CodingAgentConfig
    → ClaudeCodeAgent
```

Avoid provider-specific branching spread across multiple pipeline files.

One central resolver/factory is preferred.

Do not over-engineer plugin discovery or dynamic loading.

---

## 8. Frontend — agent selectors

Update the task creation UI.

Add a dedicated section for agent selection.

Example:

```text
Agents

Proposer
Provider: [ Gemini ▼ ]
Model:    [ gemini-... ▼ ]

Critic
Provider: [ Anthropic ▼ ]
Model:    [ claude-... ▼ ]

Worker
Tool:     [ Claude Code ▼ ]
Model:    [ claude-... ▼ ]
```

The exact visual design should follow the current application style.

---

## 9. Provider-dependent model options

The model selector must depend on the selected provider/tool.

Example:

```text
Provider = Gemini
→ show configured Gemini models only

Provider = Anthropic
→ show configured Anthropic models only
```

For worker:

```text
Tool = Claude Code
→ show supported/configured Claude Code worker models
```

Changing provider/tool should update the model options appropriately.

Do not allow stale invalid model selection to remain after provider changes.

If necessary, reset to the provider's configured default model.

---

## 10. Backend exposes safe agent options

The frontend needs a safe way to know:

* available providers;
* available tools;
* supported/configured model names;
* default selections.

Add or extend a backend endpoint/config response for this purpose.

Example conceptual response:

```json
{
  "chatProviders": {
    "gemini": {
      "models": ["model-a", "model-b"],
      "defaultModel": "model-a"
    },
    "anthropic": {
      "models": ["model-c"],
      "defaultModel": "model-c"
    }
  },
  "codingTools": {
    "claude-code": {
      "models": ["model-c"],
      "defaultModel": "model-c"
    }
  }
}
```

Exact API design is up to you.

Important:

Do NOT expose:

* API keys;
* credentials;
* secret environment variables;
* raw internal config.

Only expose safe metadata needed by the UI.

---

## 11. Show selected agents in task details

Once the task is created, show the resolved configuration in task details.

Example:

```text
Agents

Proposer
Gemini / gemini-...

Critic
Anthropic / claude-...

Worker
Claude Code / claude-...
```

This should reflect the stored task configuration, not the application's current global defaults.

---

## 12. Audit integration

Ensure the task's selected agents are available to the audit/event system.

At minimum the system must be able to determine later:

```text
Role
Provider / Tool
Model
```

Example:

```text
role: proposer
provider: gemini
model: gemini-...
```

```text
role: worker
tool: claude-code
model: claude-...
```

Do not duplicate secrets or credentials into audit records.

A later task will build the full evidence export.

---

## 13. Model configuration strategy

Do not hard-code every known vendor model directly in frontend source code.

Prefer backend/application configuration.

A reasonable approach is:

```text
Environment/configuration
        ↓
Available provider/model definitions
        ↓
Backend safe config endpoint
        ↓
Frontend selectors
```

If the project currently supports only one configured model per provider, the UI may initially display that one model.

The architecture should support multiple configured models without requiring another major refactor.

---

## 14. Failure behavior

If the selected provider/tool cannot be initialized:

* fail clearly;
* record the failure using existing audit mechanisms;
* do not silently switch to another provider;
* do not start the pipeline with a different model.

Examples:

```text
Selected Anthropic configuration is unavailable
```

or:

```text
Selected worker model is not configured
```

Errors shown to the user must not contain credentials or secret config.

---

# Acceptance Criteria

The task is complete when all of the following are true:

* proposer provider can be selected per task;
* proposer model can be selected per task;
* critic provider can be selected per task;
* critic model can be selected per task;
* worker tool can be selected per task;
* worker model can be selected per task;
* the selections are stored on the task/run;
* later pipeline stages use the stored selection;
* environment values remain defaults;
* existing behavior works when no explicit selection is supplied;
* invalid provider/model combinations are rejected;
* frontend shows only valid model options for selected provider/tool;
* task details display the resolved agents/models;
* no secrets are exposed through configuration APIs;
* audit infrastructure can access role/provider/model metadata;
* existing tests pass;
* new tests cover configuration and selection behavior;
* `cargo fmt` passes;
* `cargo clippy` passes;
* frontend formatting/lint/type checks pass;
* complete test suite passes.

---

# Required Tests

Add focused tests for at least the following cases.

### Backend

1. Default configuration creates the same agent roles as before.
2. Explicit proposer selection is stored correctly.
3. Explicit critic selection is stored correctly.
4. Explicit worker selection is stored correctly.
5. Unsupported provider is rejected.
6. Unsupported model/provider combination is rejected.
7. Unsupported worker tool is rejected.
8. Missing required model is rejected.
9. Stored task configuration does not change if global/default config changes later.
10. Safe configuration endpoint does not expose credentials.

### Frontend

Where the existing frontend test setup allows:

1. Agent selectors render.
2. Changing provider changes available models.
3. Invalid stale model is cleared/reset.
4. Submitted task contains selected configuration.
5. Task details show actual configured agents.

---

# Out of Scope

Do NOT implement:

* Codex worker;
* OpenAI/Codex proposer;
* OpenAI/Codex critic;
* remote model discovery;
* querying Gemini/Anthropic APIs for available model lists;
* Git commits;
* milestone implementation;
* GitHub publishing;
* take-home assignment mode;
* development evidence export;
* large UI redesign.

These belong to later tasks.

---

# Backward Compatibility

Existing configuration must continue working.

A user who currently runs the project with environment variables and creates a normal task without changing the new selectors should observe effectively the same behavior as before this task.

Do not require migration of ordinary existing configuration unless strictly necessary.

If persisted task/config structures require compatibility handling, implement it safely and document the decision.

---

# Completion Report

When finished, provide:

1. Files changed.
2. Task/run configuration types introduced.
3. Agent factory/resolver design.
4. Backend API changes.
5. Frontend changes.
6. Validation rules.
7. Default/fallback behavior.
8. How configuration is stored for the whole run.
9. How secrets are prevented from reaching the frontend/audit log.
10. Tests and verification commands executed.
11. Any remaining limitations.

Do not push changes automatically.
