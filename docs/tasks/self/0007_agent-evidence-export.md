# 0007 — Agent Evidence Export

## Goal

Add a complete, exportable evidence package for each task/run.

The export must make it possible to review:

* which agents were used;
* which models/providers/tools were used;
* what prompts were sent;
* what responses/decisions were produced;
* how the task progressed over time;
* which significant events occurred;
* what the final task state was.

The export must be derived from actual recorded run data.

Do not fabricate missing history.

This task builds directly on the timestamped immutable audit trail introduced in `0006`.

---

## Context

For take-home assignments and agentic-development evaluation, the final source code alone is not enough.

The reviewer may want to understand:

```text
What did the proposer decide?
What did the critic challenge?
What did the worker receive?
Which models were used?
When did each stage happen?
Which decisions changed during the run?
What failed?
What eventually succeeded?
```

The existing UI history is useful for the user, but it is not intended to be the permanent full evidence artifact.

This task introduces a separate export mechanism.

Preserve all guarantees introduced by tasks `0001–0006`.

---

# Required Export

For a task/run, generate an evidence package containing at least:

```text
agent-session.jsonl
DEVELOPMENT_LOG.md
DECISIONS.md
AGENT_USAGE.md
FINAL_REPORT.md
```

A reasonable export structure is:

```text
task-evidence/
├── agent-session.jsonl
├── DEVELOPMENT_LOG.md
├── DECISIONS.md
├── AGENT_USAGE.md
└── FINAL_REPORT.md
```

Optionally package this directory into:

```text
task-evidence.zip
```

if that fits the existing application architecture.

---

# 1. Introduce evidence source-of-truth storage

The evidence export must not be reconstructed only from:

```text
current task state
+
bounded UI logs
```

Those sources are insufficient for a complete development record.

Introduce or extend a dedicated evidence/audit structure that retains the information required for export.

Important distinction:

```text
UI log/history
    → optimized for live display
    → may be bounded/truncated

Evidence history
    → complete for required agent interactions/events
    → used for export
```

Do not break the bounded UI log behavior introduced earlier.

---

# 2. Record agent interactions

For every proposer and critic interaction, retain enough information to export:

* sequence/order;
* timestamp;
* stage;
* role;
* provider;
* model;
* prompt;
* response;
* result status;
* duration, if reliably available.

Conceptually:

```rust
struct AgentInteraction {
    sequence: u64,
    timestamp: DateTime<Utc>,
    stage: AgentStage,
    role: AgentRole,
    provider: String,
    model: String,
    prompt: String,
    response: Option<String>,
    status: InteractionStatus,
    duration_ms: Option<u64>,
}
```

Exact names and types are up to you.

Do not duplicate provider implementation logic into this layer.

---

# 3. Record worker interactions

For coding-agent execution, retain safe evidence about what the worker was asked to do.

At minimum record:

```text
timestamp
sequence
role = worker
tool
model
stage
task/spec/milestone prompt or instruction
result status
safe execution summary
duration where available
```

If worker stdout/stderr is large, do not blindly store unlimited raw output in memory.

Use existing bounded process-output protections.

The export should include useful execution evidence without creating an unbounded-memory vulnerability.

---

# 4. Preserve the exact task-level agent selections

The evidence must use the frozen task selection from `0005`.

Do not determine the model/provider later using current environment defaults.

Example:

```text
Task started with:
Proposer → Gemini / model-A
Critic   → Anthropic / model-B
Worker   → Claude Code / model-C
```

Even if environment configuration later changes, the export must still report:

```text
model-A
model-B
model-C
```

for that run.

---

# 5. Integrate with the audit events from 0006

The evidence export should reuse the existing `RecordedEvent` timeline.

Do not create a second unrelated timestamp/sequence system.

The audit timeline should remain the chronological source of truth for lifecycle events.

Agent interactions may have their own detailed records, but they should correlate cleanly with task event ordering.

Avoid inconsistent clocks or independently generated event sequences where possible.

---

# 6. Generate `agent-session.jsonl`

Generate a JSON Lines file.

Each line must be a valid standalone JSON object.

Do not produce one giant JSON array.

Example:

```json
{"sequence":1,"timestamp":"2026-09-09T10:00:00Z","kind":"task_event","event":{"type":"task_started"}}
{"sequence":2,"timestamp":"2026-09-09T10:00:01Z","kind":"agent_interaction","role":"proposer","provider":"gemini","model":"...","stage":"debate","prompt":"...","response":"...","status":"completed"}
```

Exact schema may differ.

Requirements:

* one JSON object per line;
* UTF-8;
* deterministic field serialization where practical;
* valid JSON;
* timestamps serialized consistently;
* safe to parse line by line.

---

# 7. JSONL event types

The JSONL should distinguish records clearly.

For example:

```text
kind = task_event
kind = agent_interaction
kind = worker_execution
kind = verification
```

Do not require consumers to infer the record type from arbitrary fields.

---

# 8. Generate `DEVELOPMENT_LOG.md`

Generate a human-readable chronological development log.

Example structure:

```markdown
# Development Log

## 2026-09-09

### 10:12 UTC — Task started

### 10:13 UTC — Proposer started
Provider: Gemini
Model: ...

### 10:14 UTC — Proposer completed

### 10:15 UTC — Critic started
Provider: Anthropic
Model: ...

...
```

The log must come from real recorded timestamps.

Do not invent intermediate events.

---

# 9. Development log content

Include significant events such as:

* task creation/start;
* proposer execution;
* critic execution;
* specification generation;
* specification editing;
* approval/rejection;
* worker execution;
* verification;
* failure;
* cancellation;
* completion.

Do not dump every repetitive low-level process line into the human-readable development log.

Keep it useful for review.

---

# 10. Generate `AGENT_USAGE.md`

Create a concise document describing the agents actually used.

Example:

```markdown
# Agent Usage

## Proposer

Provider: Google Gemini
Model: ...
Purpose: Generate implementation and architecture proposals.

## Critic

Provider: Anthropic
Model: ...
Purpose: Challenge the proposed design and identify risks.

## Worker

Tool: Claude Code
Model: ...
Purpose: Implement the approved specification.
```

Use the actual task-level configuration.

---

# 11. Tool vs model distinction

For the worker, preserve:

```text
tool
model
```

as separate concepts.

Correct:

```text
Tool: Claude Code
Model: Claude ...
```

Incorrect:

```text
Model: Claude Code
```

This distinction will be required later when Codex is added.

---

# 12. Generate `DECISIONS.md`

Create a human-readable engineering decision log.

Use actual recorded proposer/critic/spec information.

A useful structure is:

```markdown
# Decisions

## Decision 1 — Use PostgreSQL for persistence

Decision:
...

Rationale:
...

Alternatives considered:
...

Critic concerns:
...

Resolution:
...
```

Not every proposer sentence is a decision.

Extract only meaningful engineering decisions.

---

# 13. No invented decisions

If the evidence only shows:

```text
Decision: use PostgreSQL
```

but contains no recorded rationale, do NOT invent:

```text
because PostgreSQL offers superior transactional guarantees...
```

Instead write:

```text
Rationale: Not explicitly recorded.
```

The exported decision log must remain honest.

---

# 14. Generate `FINAL_REPORT.md`

Generate a summary of the completed task/run.

Include at minimum:

```text
Task title
Task type
Task description/objective
Start time
End time
Final status
Agents used
Specification status
Worker result
Verification result
Known errors/failures
Output/workspace location where safe
```

---

# 15. Known limitations

The final report must honestly expose unresolved issues.

If the worker succeeded but verification failed:

```text
Final status: Verification failed
```

Do not claim:

```text
Project successfully completed
```

simply because code was generated.

Similarly, if a task failed halfway through, the export should still be available.

---

# 16. Export failed tasks too

Evidence export must work for:

* completed tasks;
* failed tasks;
* rejected tasks where meaningful evidence exists;
* cancelled tasks where meaningful evidence exists.

Do not restrict export only to successful runs.

Failed runs may be especially valuable for debugging and evaluation.

---

# 17. Export action

Add a user-triggered export action.

For example:

```text
[ Export Evidence ]
```

in task details.

The exact UI design should follow the current application style.

Do not automatically export every task unless there is a strong architectural reason.

---

# 18. Backend endpoint

Add an appropriate backend endpoint.

Conceptually:

```text
GET /api/tasks/{task_id}/evidence
```

or:

```text
POST /api/tasks/{task_id}/export
```

Choose the design that fits existing conventions.

The endpoint must:

* validate task existence;
* generate export safely;
* return/download a safe artifact;
* avoid path traversal;
* avoid arbitrary user-supplied output paths.

---

# 19. Export location

If evidence is generated on disk, store it under an application-controlled directory.

For example conceptually:

```text
.runtime/
  evidence/
    <task-id>/
```

or another existing runtime/artifact location.

Do not write into arbitrary paths based on unsanitized task titles.

Use task IDs or safely sanitized identifiers.

---

# 20. Zip export

If generating ZIP archives:

* use a safe archive library;
* preserve UTF-8 filenames;
* do not include files outside the evidence directory;
* do not follow unsafe symlinks;
* do not include project secrets accidentally.

The archive should contain only intended evidence files.

---

# 21. Secret redaction

This is critical.

The evidence must never export:

* Gemini API key;
* Anthropic API key;
* authorization headers;
* bearer tokens;
* GitHub credentials;
* cloud credentials;
* database passwords;
* `.env` secret values;
* secret-bearing CLI arguments.

Reuse the redaction logic introduced in earlier audit/security tasks.

Do not create a new weaker redaction implementation.

---

# 22. Prompt redaction

Prompts themselves may accidentally contain credentials.

Before storing/exporting prompts, apply the same secret-redaction mechanism.

Example:

Input:

```text
Use API key sk-secret-value to call ...
```

Export:

```text
Use API key [REDACTED] to call ...
```

Do not assume prompts are safe simply because they came from an agent workflow.

---

# 23. Response redaction

Agent responses and execution summaries must also be sanitized.

A provider may echo secrets from:

* prompts;
* environment;
* error messages;
* tool output.

All exported evidence content must pass through the established redaction layer.

---

# 24. Memory safety

Do not create unlimited in-memory retention of:

```text
worker stdout
worker stderr
large generated files
huge prompt transcripts
```

Respect the execution/output constraints introduced in `0002`.

If content exceeds safe limits, preserve:

```text
truncated: true
```

or another explicit indication.

Never silently pretend truncated evidence is complete.

---

# 25. Evidence completeness indicator

If any record had to be truncated due to safety/resource limits, surface it.

For example:

```json
{
  "truncated": true
}
```

and in `FINAL_REPORT.md`:

```text
Evidence note:
Some worker output was truncated due to configured output limits.
```

The reviewer should know that the transcript is partial.

---

# 26. Evidence generation must be deterministic from stored data

Repeated export of the same completed task should produce semantically equivalent content.

Do not call an LLM during export just to summarize the evidence unless strictly necessary.

Prefer deterministic generation from recorded evidence.

Especially:

```text
DEVELOPMENT_LOG.md
AGENT_USAGE.md
FINAL_REPORT.md
```

should not require another model invocation.

---

# 27. Decision-log generation

`DECISIONS.md` may use structured information already recorded from proposer/critic/spec.

Prefer deterministic extraction.

Do not add a hidden extra LLM call merely to "make the document nicer".

If robust deterministic decision extraction is not currently possible, generate a conservative decision log based on explicitly recorded decision/spec sections.

Document the limitation.

---

# 28. Audit the export action

Record that evidence was exported.

For example:

```text
evidence_exported
```

with:

* timestamp;
* task id;
* artifact name/reference.

Do not include secrets or full filesystem details if not appropriate.

Avoid creating a recursive problem where exporting evidence modifies the evidence endlessly.

Choose clear semantics.

A reasonable approach:

```text
snapshot evidence
↓
generate archive
↓
record EvidenceExported event
```

and document that the export event is visible only in subsequent exports.

Alternatively include it deliberately with a two-phase process.

Choose one consistent behavior and test it.

---

# 29. Existing APIs

Do not break:

* task creation;
* task details;
* SSE;
* approval;
* worker execution;
* verification.

Evidence export should be additive.

---

# 30. Frontend

Add a simple export control.

Example:

```text
Task Actions

[ Export Evidence ]
```

When clicked:

```text
download/open generated evidence archive
```

or another clear existing artifact flow.

Show useful errors if generation fails.

Do not expose internal server paths unnecessarily.

---

# Suggested JSONL Shape

A reasonable generic shape:

```json
{
  "sequence": 12,
  "timestamp": "2026-09-09T10:24:51.412Z",
  "kind": "agent_interaction",
  "stage": "critic",
  "role": "critic",
  "provider": "anthropic",
  "model": "claude-...",
  "status": "completed",
  "prompt": "...",
  "response": "...",
  "duration_ms": 14321,
  "truncated": false
}
```

Task-event example:

```json
{
  "sequence": 13,
  "timestamp": "2026-09-09T10:25:10.000Z",
  "kind": "task_event",
  "event": {
    "type": "spec_generated"
  }
}
```

Exact schema may differ.

Keep it stable and documented.

---

# Acceptance Criteria

The task is complete when:

* evidence can be exported for a task/run;
* export works for successful tasks;
* export works for failed tasks;
* export contains `agent-session.jsonl`;
* export contains `DEVELOPMENT_LOG.md`;
* export contains `DECISIONS.md`;
* export contains `AGENT_USAGE.md`;
* export contains `FINAL_REPORT.md`;
* JSONL is valid one-object-per-line JSON;
* actual task timestamps are used;
* actual frozen provider/model/tool selections are used;
* proposer prompts/responses are available;
* critic prompts/responses are available;
* worker instructions/results are represented safely;
* significant lifecycle events from `0006` are represented;
* UI bounded logs remain separate from evidence retention;
* evidence truncation is explicit where applicable;
* secrets are redacted;
* prompts are redacted;
* responses are redacted;
* failed/error messages are redacted;
* export paths are safe;
* user can trigger export from the UI;
* no new LLM call is required simply to generate the export;
* existing task flow continues to work;
* tests pass;
* `cargo fmt` passes;
* `cargo clippy` passes;
* frontend checks pass where applicable.

---

# Required Tests

Add focused tests covering at least the following.

## JSONL validity

Export a representative task.

Read `agent-session.jsonl`.

Verify every line can be parsed independently as JSON.

---

## Chronological ordering

Verify exported records preserve task/audit ordering.

---

## Agent configuration

Create a task with explicit selections.

Verify export reports the exact stored:

```text
proposer provider/model
critic provider/model
worker tool/model
```

---

## Prompt/response export

Verify representative proposer and critic prompt/response records appear.

---

## Failed task export

Create/simulate a failed task.

Verify evidence can still be exported and the final report says it failed.

---

## Redaction

Inject representative sensitive values into:

* prompt;
* response;
* error;
* worker output.

Verify none appears in exported files.

---

## Truncation

Where large content is bounded, verify the export marks truncation explicitly.

---

## Export path safety

Verify task titles or malformed IDs cannot cause path traversal.

---

## Repeat export

Export the same stable task twice.

Verify semantic evidence remains consistent.

---

## Frontend

Where current test infrastructure allows:

* export button/action is present;
* correct task ID is used;
* errors are surfaced.

---

# Out of Scope

Do NOT implement:

* milestone execution;
* Git commits after milestones;
* persistent New Project output;
* post-implementation critic fix loop;
* acceptance-criteria tracking;
* automatic project README generation;
* take-home assignment mode;
* Codex worker;
* GitHub publishing.

Those belong to later tasks.

---

# Completion Report

When finished, provide:

1. Files changed.
2. Evidence storage/data model.
3. How agent prompts/responses are captured.
4. How worker evidence is captured.
5. Export directory/archive structure.
6. JSONL schema.
7. How each Markdown file is generated.
8. Redaction strategy.
9. Truncation/resource-limit strategy.
10. Backend endpoint/action.
11. Frontend changes.
12. Export audit behavior.
13. Tests added.
14. Verification commands/results.
15. Remaining limitations.

Do not push changes automatically.
