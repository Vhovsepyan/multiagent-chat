# 0020 — OpenAI Chat Provider and Expanded Model Catalog

## Goal

Add OpenAI as a `ChatProvider` so OpenAI models can be selected independently as:

* proposer;
* critic.

Also expand the configurable chat-model catalog for OpenAI, Anthropic, and Gemini.

Preserve all guarantees from tasks `0001–0019`.

## Requirements

### OpenAI provider

Extend the existing chat-provider abstraction:

```text
ChatAgent
├── GeminiAgent
├── AnthropicAgent
└── OpenAIAgent
```

Use `OpenAI` as the internal provider name.

Do not add OpenAI-specific branching throughout orchestration.

OpenAI must work anywhere the existing `ChatAgent` abstraction is used.

### OpenAI API

Add safe configuration for:

```text
OPENAI_API_KEY
OPENAI_BASE_URL
OPENAI_MODEL
OPENAI_MODELS
```

Use the official OpenAI API and the existing HTTP/client architecture.

Prefer the Responses API unless the existing abstraction provides a strong technical reason to use another supported endpoint.

Never expose `OPENAI_API_KEY` through:

* frontend configuration;
* audit events;
* durable task storage;
* evidence exports;
* logs/errors.

### OpenAI model catalog

Support configurable OpenAI models.

Initial defaults/options should include current general-purpose models such as:

```text
gpt-6-astra
gpt-5.6-sol
gpt-5.6-terra
gpt-5.6-luna
```

Do not make the application architecture depend on this fixed list.

`OPENAI_MODELS` must allow model configuration without code changes.

Do not mix Codex worker models into the chat-provider catalog.

### Anthropic catalog

Expand configurable Anthropic choices with current active general-purpose models, for example:

```text
claude-opus-5
claude-sonnet-5
claude-fable-5
claude-opus-4-8
claude-sonnet-4-6
claude-haiku-4-5-20251001
```

Keep model lists configuration-driven.

Do not restore retired models as defaults.

### Gemini catalog

Expand configurable Gemini choices with current general-purpose models, for example:

```text
gemini-3.8-flash
gemini-3.7-flash
gemini-3.6-flash
gemini-3.5-flash
gemini-3.5-flash-lite
gemini-3.1-pro-preview
```

Do not include image/audio/embedding-specific models in proposer/critic selection.

### Provider availability

OpenAI should be available only when its credentials are configured:

```text
OPENAI_API_KEY present
→ OpenAI available

OPENAI_API_KEY absent
→ OpenAI not offered
```

The application must still start normally without OpenAI configured.

This behavior must remain consistent with Gemini and Anthropic availability.

### Task-level selection

Support mixed configurations such as:

```text
Proposer
OpenAI / gpt-5.6-sol

Critic
Anthropic / claude-sonnet-5

Worker
Codex / configured Codex model
```

and:

```text
Proposer
Gemini / gemini-3.8-flash

Critic
OpenAI / gpt-6-astra

Worker
Claude Code / configured Claude model
```

Selections must remain frozen on the task/run and survive durable persistence/restart.

### UI

Add OpenAI to the existing proposer/critic provider selector.

Changing provider must update its valid model choices.

Do not hard-code provider credentials or secret configuration in frontend code.

### Audit / evidence / persistence

Existing infrastructure must correctly record:

```text
role
provider = openai
model
```

OpenAI selections must survive durable task restoration from `0019`.

Evidence export must report the actual frozen provider/model.

### Error handling

Safely handle:

* missing OpenAI credentials;
* invalid model;
* timeout;
* HTTP/API error;
* malformed response.

Do not silently fall back to Gemini or Anthropic.

Apply existing secret redaction.

## Acceptance Criteria

* OpenAI can be proposer.
* OpenAI can be critic.
* Gemini and Anthropic remain compatible.
* OpenAI uses `ChatAgent`.
* Mixed proposer/critic providers work.
* OpenAI is unavailable when not configured.
* model lists for all chat providers are configuration-driven.
* model validation works per provider.
* frozen OpenAI selections survive application restart.
* audit/evidence correctly identifies OpenAI/model.
* credentials never appear in UI, durable storage, logs, or evidence.
* worker selection remains unaffected.
* relevant tests pass.

## Required Tests

Cover at least:

1. OpenAI available with configured API key;
2. OpenAI absent without API key;
3. OpenAI proposer call;
4. OpenAI critic call;
5. mixed-provider proposer/critic task;
6. OpenAI model validation;
7. frozen OpenAI selection survives durable reload;
8. OpenAI error/secret redaction;
9. UI provider/model selection;
10. evidence reports correct OpenAI model;
11. Gemini/Anthropic regression coverage.

## Out of Scope

Do not implement:

* OpenAI as a coding worker;
* ChatGPT web-session/cookie authentication;
* browser automation against ChatGPT;
* automatic provider model discovery;
* image/audio/realtime models;
* new orchestration roles.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
