# multiagent-chat

A Rust web application for repository-backed, multi-agent software engineering. A proposer agent designs a solution, a critic agent reviews it, the application produces an editable specification, and a worker agent implements the user-approved result in an isolated task workspace.

The proposer, critic, and worker are selected per task. Proposer and critic each use a configured chat provider and one of its configured models (Gemini, Anthropic, or OpenAI); the worker uses a configured coding tool and model (Claude Code or Codex). The resolved choice is stored on the task, so a run is not affected by later configuration changes. With no explicit choice, a task uses the existing defaults — proposer Gemini, critic Anthropic, worker Claude Code. See [Agent selection](#agent-selection).

Multiagent Chat supports four task kinds:

- New Project for a selected technology stack.
- Take-home Assignment for a persistent, evidence-backed evaluation project.
- Feature for a registered repository.
- Bug Fix for a registered repository.

GitHub public repositories are the initial existing-project source. Each task uses a separate disposable server-side workspace; a Project never represents a persistent local directory.

## Requirements

- Rust stable with `cargo`.
- Git on `PATH` for repository-backed tasks.
- Claude Code CLI on `PATH` for implementation.
- An API key for each chat provider you want to use: `GEMINI_API_KEY` (Google AI
  Studio), `ANTHROPIC_API_KEY` (Anthropic), and/or `OPENAI_API_KEY` (OpenAI). Neither is required to start
  the application; each one enables its own provider.
- A Claude Code stored login for worker execution when `ANTHROPIC_API_KEY` is
  not configured. The worker does not require that key solely to run.

## Setup

```bash
git clone https://github.com/Vhovsepyan/multiagent-chat
cd multiagent-chat
cp .env.example .env
```

Set the key for each chat provider you intend to use in `.env`; providers
without keys are not offered. Do not commit this file or expose its values.
Both keys are needed only when using the default proposer/critic wiring; a
single-provider installation can explicitly select that provider for both chat
roles. Claude Code may instead use its own stored login for worker execution.
See [Agent selection](#agent-selection) for details.

`WORKSPACE_ROOT` is no longer required by the web application. It remains an optional compatibility setting for the original CLI workflow.

`PERSISTENT_OUTPUT_ROOT` names the one existing folder a persistent New Project may be written into. It is optional: when unset, `WORKSPACE_ROOT` is used, and when neither is set, persistent output is refused with a message naming the variable to set.

## Usage

```bash
cargo run                  # web UI at http://127.0.0.1:3000
cargo run -- --cli         # legacy local terminal workflow
cargo run -- --help
```

In the web UI:

1. Register a public GitHub repository using `owner/repository` or its HTTPS URL when working on existing code.
2. Create a New Project, Take-home Assignment, Feature, or Bug Fix task.
3. Watch repository inspection and the proposer/critic debate through SSE.
4. Review or edit the generated specification.
5. Approve implementation.
6. Review implementation output, technology-aware verification, and the resulting working-tree diff/status.
7. Select **Export Evidence** at any point to download the run's redacted evidence package.
8. For a completed persistent project, select **Prepare GitHub publication**, review the exact repository/branch/HEAD/working-tree/verification snapshot, then explicitly confirm **Publish to GitHub**.

Feature and Bug Fix tasks require a registered Project. New Project and Take-home Assignment tasks instead require a selected technology and an output mode.

### New Project output

A New Project chooses between two outputs; the default is unchanged from before this option existed.

- **Temporary review result** — the project is built in the isolated task workspace and reviewed from the task result. The workspace is then removed.
- **Persistent local project** — after implementation and verification succeed, the finished project is copied into `PERSISTENT_OUTPUT_ROOT/<destination>` and survives workspace cleanup.

The destination is a plain folder name (letters, digits, dot, dash, underscore), never a path: the server joins it to the configured output root, so separators, `..` and absolute paths are rejected. An existing non-empty destination is never overwritten, and links are refused rather than followed. The project is staged and then moved into place, so a failed persistence leaves the destination exactly as it was, fails the task, and is recorded as `project_persistence_failed`. When milestone commits are enabled, the repository is copied verbatim, so commit history and SHAs are preserved. No remote is configured or pushed during task execution. Once the project has been moved into place it counts as persisted: if its repository metadata cannot then be read, the run stays successful and reports the metadata as unavailable with a warning, rather than claiming the destination was left unchanged.

This option does not apply to Feature and Bug Fix tasks, which inherit the registered project's output.

### Take-home Assignment

A **Take-home Assignment** reuses the New Project pipeline: task-level agent
selection, specification approval, milestone verification, post-implementation
critic/fix review, acceptance tracking, evidence export, and submission
documentation all remain active. It defaults to **Persistent local project**
output and **Commit after each successful milestone**. A valid safe destination
folder name is required; temporary-only output is refused. Nothing is pushed
automatically; publication is a separate, explicitly confirmed action.

Before a take-home project is persisted, completed, or published, the
correctness gate requires every acceptance criterion to be `PASSED`, every
milestone to have passed verification and critic review, and no failed
verification. A blocked gate records its reason in the task audit and evidence
instead of presenting a partial delivery as complete.

Its task page includes a completion checklist derived from recorded task state:
implementation, verification, acceptance review, final critic review,
documentation, evidence-export availability, and retained Git history are each
shown independently rather than assumed successful.

Approval is accepted only once, while the task is waiting for review. Early,
duplicate, and terminal-task approval requests are rejected. An approved
specification must contain non-empty text.

The local web server accepts browser requests only from
`http://127.0.0.1:PORT` or `http://localhost:PORT`, using its configured port.
Cross-origin browser requests and unrecognized Host headers are rejected;
native clients may omit Origin. This origin boundary does not replace future
user authentication or an execution sandbox.

If implementation or verification fails, available changes are captured in the
task result before workspace cleanup. If result capture itself fails, cleanup
is delayed and the server retains the UUID-named task workspace for manual
recovery until the configured recovery deadline (24 hours by default). Cleanup
failures also schedule a retry. Recover needed files before this deadline.
Results otherwise remain in memory until persistence is implemented.

Inspection skips linked repository files, including instructions and metadata.
Each task workspace has separate `repo/` and `artifacts/` directories. The exact
approved text, including user edits, is saved as `artifacts/approved-spec.md`
and its absolute path is supplied to the worker. Codex receives the task-owned
artifact directory through its `--add-dir` sandbox allowance; it is never
written into the repository, never replaces a project-owned `SPEC.md`, and
does not appear in the project's diff. Cleanup covers both directories.

Git, verification tools, and Claude Code start with cleared environments and
an explicit runtime-variable allowlist (OS paths, home/temp locations, locale,
and supported toolchain locations). Claude Code additionally receives only the
configured `ANTHROPIC_API_KEY`, and none at all when that key is not configured
— it then uses its own stored login; other provider keys, database/cloud credentials,
alternate provider endpoints/tokens, and arbitrary tool options are not inherited.
Custom setups relying on other environment variables may need a reviewed policy
change. Cloned task workspaces also have every inherited Git remote removed
before a worker can run, so a source repository's `origin` cannot be used for
an unattended push. The server retains only the credential-free
`owner/repository` source identity for a later, explicit GitHub publication
from persistent output; it never restores that identity as a worker remote.
This reduces environment exposure, but is **not a sandbox**: child
processes still have the server user's filesystem/network access, can read
on-disk credentials or tool configuration, and Claude Code's own subprocesses
may inherit its required Anthropic credential. Do not execute untrusted
repositories on this basis alone.

The legacy CLI also stores generated/imported specification snapshots outside
the project in a UUID-named temporary artifact directory. It prints that path
and retains it for manual review/recovery; `--implement-only` continues to read
a project-owned `SPEC.md` without overwriting it. These CLI artifacts require
manual cleanup and are not durable storage.

## Execution limits

`MAX_FIX_ITERATIONS` bounds the post-implementation fix loop per milestone
(default 2, maximum 5). Zero still runs the review, but findings then fail the
milestone instead of being corrected.

Child execution has configurable time and diagnostic-output limits. Set these
environment variables when starting the server (all must be positive integers):

| Variable | Default | Purpose |
| --- | ---: | --- |
| `IMPLEMENTER_TIMEOUT_SECS` | 1800 | Claude Code deadline |
| `VERIFICATION_TIMEOUT_SECS` | 600 | Deadline per verification command |
| `GIT_TIMEOUT_SECS` | 300 | Deadline per Git command |
| `PROCESS_STDOUT_BYTES` | 65536 | Retained stdout bytes per command |
| `PROCESS_STDERR_BYTES` | 65536 | Retained stderr bytes per command |
| `LOG_EVENT_BYTES` | 4096 | Maximum repetitive log-event payload, including its marker |
| `TASK_LOG_EVENTS` | 256 | Retained repetitive log-event count per task |
| `TASK_LOG_BYTES` | 262144 | Retained repetitive log text bytes per task |
| `WORKSPACE_RECOVERY_SECS` | 86400 | Recovery window before delayed cleanup retry |

The runner drains both pipes concurrently, discarding excess bytes rather than
buffering the entire output. Truncated streams include
`[output truncated: limit exceeded]`; markers and UTF-8 decoding add a small
bounded overhead to raw stream capture. Oversized lines are split into bounded
events. `LOG_EVENT_BYTES` must be large enough to hold the marker.

The timeout covers process exit and pipe draining. Partial diagnostics and
available changes survive failure/timeout. Windows Job Objects manage child
tree lifetimes; Unix timeouts kill the process group, with a bounded direct-child
termination fallback. This does not prevent intentionally escaping processes or
provide filesystem, network, CPU, or memory isolation.

The UI log tail retains the newest Build/Notice/Warning events, with a
discarded-log counter and an explanation on page reload. Proposals, critiques,
specifications, approval, state transitions, verification, failure/completion,
and final results are kept in the separate audit history and are not evicted by
that log limit. This is not a global memory limit: task count and lifecycle
documents still need durable persistence and retention policies.

## Audit history

Every significant task event is stored in an immutable envelope containing a
per-task sequence number, a backend-authored RFC3339 UTC timestamp, and the
tagged event payload. Sequence numbers start at 1 and are assigned under the
same task-manager lock that stores and broadcasts the event, so snapshot and SSE
ordering agree even when asynchronous producers report concurrently.

```json
{
  "sequence": 12,
  "timestamp": "2026-09-09T10:24:51.412Z",
  "event": {
    "type": "critic_completed",
    "stage": "debate",
    "round": 1,
    "provider": "anthropic",
    "model": "claude-sonnet-4-6"
  }
}
```

`history` is the append-only significant-event audit trail. Repetitive
`build`, `notice`, and `warning` output uses the separately bounded `log_tail`;
the task page merges both by backend sequence without changing either stored
order. Lifecycle events cover task start/completion/failure, proposer and critic
calls, specification generation/editing/approval/rejection, worker execution,
and verification. Provider/tool and model metadata comes from the selection
frozen on the task.

Configured provider credential values and common secret-bearing assignment,
authorization, token, password, and database-credential forms are redacted
before events or detailed evidence enter task state. Prompts, responses, worker
summaries, errors, and export metadata use the same redaction layer. Event
metadata never includes API keys, environment snapshots, or authorization
headers. This is defense in depth; task and evidence state are still lost when
the process restarts only if the local `.runtime` task store is removed or its
individual task record is corrupt. The store never contains credentials.

Task state is durable under `.runtime/tasks/<task-id>/` (or under
`WORKSPACE_ROOT/.runtime` when that compatibility root is configured). Each
task has an atomic `task.json` state snapshot and append-only `events.jsonl` and
`evidence.jsonl` audit files. Startup validates each task independently: a
waiting approval gate remains waiting, while an interrupted active run is
recorded as failed with a manual-recovery message and is never auto-resumed.
Generated repositories remain in their existing persistent-output destination;
the runtime store retains only task metadata and evidence.

Each snapshot carries the highest audit sequence it commits. Recovery accepts
only the contiguous event/evidence prefix through that checkpoint, discards an
uncommitted or torn final journal tail, and may use a newer flushed replacement
snapshot when its checkpoint is complete. A malformed record before the final
journal line causes that task alone to be skipped.

## Evidence export

Each task retains a detailed evidence stream separately from `history` and the
bounded `log_tail`. Proposer and critic records contain the actual role-level
prompt, response or error, provider, model, stage, round, status, duration, UTC
timestamp, and task audit sequence. Worker records contain the safe instruction
handed to the coding-agent abstraction, tool and model as separate fields,
status, bounded/redacted summary, duration, and truncation state. These records
use the same sequence allocator and clock as `RecordedEvent`; they do not create
a second ordering system and are not exposed through ordinary task snapshots or
SSE.

`GET /api/tasks/{id}/evidence` returns an in-memory ZIP download containing only:

```text
agent-session.jsonl
DEVELOPMENT_LOG.md
DECISIONS.md
AGENT_USAGE.md
FINAL_REPORT.md
```

`agent-session.jsonl` is UTF-8 JSON Lines, with one standalone object per line,
ordered by the shared task sequence. Its stable record kinds are:

- `task_event`: `sequence`, `timestamp`, `kind`, and the tagged `event` payload.
- `agent_interaction`: `sequence`, `timestamp`, `kind`, `stage`, `role`, optional
  `round`, `provider`, `model`, `prompt`, optional `response`, `status`, optional
  `error`, `duration_ms`, and `truncated`.
- `worker_execution`: `sequence`, `timestamp`, `kind`, `role`, `stage`, `tool`,
  optional `milestone_id`/`milestone_title`, `model`, `instruction`, `summary`,
  `status`, `duration_ms`, and `truncated`.
- `verification`: `sequence`, `timestamp`, `kind`, `command`, `success`, `output`,
  and `truncated`.

The Markdown files are generated deterministically from stored task, event, and
interaction data. `DECISIONS.md` conservatively uses only recorded
`Agreed solution`/`Architecture` specification sections and critic reasons; it
marks unrecorded rationale instead of inventing it. Export performs no LLM call.
Prompts, responses, summaries, and errors are redacted before retention and each
evidence text field is capped at 256 KiB with `truncated: true` and the standard
truncation marker. Verification/process output retains its existing limits.
Low-level Build/Notice/Warning lines remain only in the bounded UI `log_tail` and
are not promoted to unlimited evidence storage; the worker evidence contains a
safe outcome summary instead of raw stdout/stderr.

The archive is built in memory with constant entry names and a UUID-derived
download name, so task titles and request data cannot control a server path.
The first accepted export records one immutable `evidence_exported` event before
the snapshot; later exports reuse that event, making stable repeated exports
deterministic and avoiding recursive export history.

Git capture uses a separate 8 MiB stdout/result-content budget. Over-budget Git
output is rejected rather than parsed as a complete diff; large added files also
fail capture safely, retaining the workspace for recovery. Workspace preparation,
inspection, diff capture, and cleanup run off the HTTP runtime workers.

Delayed cleanup is in-process only. A server restart loses its timers; failed
cleanup retries and workspaces left by a restart require manual cleanup. These
limits do not make untrusted repositories safe to execute.

## Milestone execution

After approval, the server derives a non-empty ordered milestone plan from the
specification's `## Steps` section. It executes one milestone at a time: the
frozen worker receives the approved specification plus only the current
milestone and repository context, then the configured verification commands
run before the milestone can pass, followed by the critic's implementation
review described below. A failed milestone stops later work. The task snapshot and audit/evidence stream
record milestone planning, start, pass, failure, and cancellation events with
bounded, redacted details. The task page shows each milestone's status while
the existing UI log remains separately bounded.

Cancellation preserves completed milestones and prevents future milestones
from starting. Durable task cancellation controls and acceptance tracking are
outside this milestone-execution task.

### Submission documentation

After implementation, verification and review, and before the result is
captured, the run writes documentation into the finished project:

```text
README.md
docs/ARCHITECTURE.md
docs/DEVELOPMENT_LOG.md
docs/DECISIONS.md
docs/AI_USAGE.md
docs/NEXT_STEPS.md
```

Every document is rendered deterministically from recorded state — the audit
history, the acceptance criteria, the verification results, the approved
specification and the frozen agent selection — so nothing can be invented. A
**Verified commands** are only commands this run executed, with their real
outcomes. A separate **Detected startup commands (not executed)** section may
list deterministic local, backend, frontend, or Compose commands derived from
project files; it explicitly says that those commands were not verified. A
decision appears only if the debate recorded it, a timestamp only if it came
from the audit log, and the AI usage page names the models the task was created
with. Configuration, database and container sections appear only when the
finished project actually contains those files.

Status is stated plainly: a run with a failed verification or outstanding
acceptance criteria says so in the README and lists the gap in
`docs/NEXT_STEPS.md`, which separates **required but incomplete** work
(outstanding criteria, unresolved findings, failed commands) and **deferred**
requirements from optional improvements the specification recorded as out of
scope.

Existing documentation is never destroyed. A target path that already holds a
file this tool did not generate is left untouched and the generated document is
written beside it as `<name>.generated.md`; a document carrying the
`<!-- generated by multiagent-chat` marker from an earlier run is replaced. The
complete six-file set is preflighted and staged before finalization; a failure
leaves no partial generated set and fails the task rather than recording a
successful completion. The generated set is redacted through the same task
snapshot the evidence export uses, so no credential can reach it, and the files
it wrote (and preserved) are recorded as a `submission_documentation_generated`
audit event only after the complete set finalizes. Because the documents are
written before the result is captured, they appear in the reviewable diff and in
a persisted project; when milestone commits are enabled they are left
uncommitted, since a milestone commit only ever records verified milestone work.

### Acceptance criteria

Before any milestone runs, the approved specification is turned into stable,
numbered criteria (`AC-001`, `AC-002`, …) stored on the task for the life of the
run. An explicit `## Acceptance criteria` section is used when the specification
states one; otherwise they are derived from `## Steps`, so every step stays
traceable. Criteria derived from the steps map one-to-one onto the milestones;
stated criteria are matched to the milestone that covers them, and a criterion
no milestone covers is recorded as `deferred` rather than dropped. Each milestone
lists the criteria it is responsible for.

| Status | Meaning |
| --- | --- |
| `pending` | Generated; its milestone has not run yet. |
| `implemented` | The worker reported the milestone done — a claim, not proof. |
| `passed` | Supported by successful automatic verification, or by an explicit implementation-critic PASS when no automatic command is available, with no finding open against it. |
| `failed` | Verification failed, or a critic finding is open against it. |
| `deferred` | No milestone in this run is responsible for it. |

A criterion reaches `passed` only after its milestone passes implementation
review. Successful automatic commands are retained as **Automatic verification**
evidence. If the project has no automatic command, the explicit critic PASS is
retained as **Implementation review** evidence instead; “no automatic
verification commands were available” is never sufficient evidence by itself.
For New Projects, verification planning is refreshed after worker output exists,
so generated Node/Python project metadata can supply repository-defined commands.
A worker report alone never passes a criterion. Critic findings
that name a criterion (`AC-004: …`) are recorded against it and keep it from
passing until a later review resolves them — the critic is shown the criteria it
may reference. Failed verification marks the affected criteria failed.

Criterion generation and every state change are audit events
(`acceptance_criteria_generated`, `acceptance_criterion_updated`) carrying
concise evidence — the command that verified it, never a copy of its log, which
stays in the task result. The task page shows the criteria with their status,
milestones, evidence and unresolved findings, and `FINAL_REPORT.md` ends with an
acceptance-criteria summary answering what was required, which milestone
implemented it, how it was verified, whether it passed, and what is outstanding.

### Implementation review and fix loop

After a milestone verifies, the task's own critic reviews what was actually
built — the approved specification and the milestone scope against the real
diff, the verification output and the worker's reported limitations. The critic
must answer with one JSON object:

```json
{"status": "PASS" | "FIX_REQUIRED",
 "findings": [{"requirement": "...", "severity": "blocker|major|minor",
               "evidence": "...", "correction": "..."}]}
```

Prose is not a result: an unparseable answer, an unknown status, `PASS` with
any findings, or `FIX_REQUIRED` without findings fails the review. Every finding
must have a non-empty requirement, evidence, and correction; incomplete entries
and more than 20 findings are rejected, never dropped or filled with placeholders.
A rejected reply is quoted in the audit only as a bounded excerpt; the reply
itself is retained under the evidence cap, so model output can never enter task
history unbounded.
`PASS` finishes the milestone normally. `FIX_REQUIRED` sends the
findings — and only those findings — back to the same worker, then verification
reruns and the critic reviews again:

```text
critic findings → worker fixes only those findings → verification → review again
```

The loop is bounded by `MAX_FIX_ITERATIONS` (default 2, maximum 5). Findings
still outstanding after the last iteration fail the milestone and ask for human
review; so do a failed critic call, a failed worker fix, and verification that
fails after a fix. **None of these are reported as success**, and each failure
still publishes the work produced so far as a task result. The milestone is
committed (when `git_mode` asks for commits) only after the review passes, so a
milestone commit contains the reviewed and corrected work.

The review diff covers only the current milestone, including its fix iterations.
Its baseline is captured before worker execution: the previous committed state
in commit-per-milestone mode, or an external working-tree snapshot in no-commit
mode. Snapshots include tracked and non-ignored untracked files without changing
the repository or Git index/history. They are limited to 10,000 files and 64 MiB
per snapshot; unsupported links or exceeded limits fail review preparation
explicitly. Scratch snapshots are disposable and released after use. The critic
diff is capped at 32 KiB with an explicit truncation marker, and a truncated
change is announced in the prompt so the critic never reports work as missing
because it was cut out. Earlier milestones do not consume that budget; the final
task result still includes all milestones.

Review rounds, findings, fix iterations, the verification after each fix, and
the final disposition are recorded as `implementation_review_started` /
`implementation_review_completed` / `implementation_review_failed` and
`fix_started` / `fix_completed` / `fix_failed` audit events, with the critic
prompt/response retained as evidence at the `implementation review` stage and
each correction run staged as `fix`. The task page shows the disposition on the
milestone, for example `Implementation review: PASS · fix iteration 1/2`.

### Milestone commits

A task chooses its Git behavior at creation (`git_mode`, shown as **Git** in
the form and on the task page):

| Mode | Wire value | Behavior |
| --- | --- | --- |
| No commits (default) | `none` | Nothing is committed; the result stays a working-tree diff, exactly as before. |
| Commit after each successful milestone | `commit_per_milestone` | One commit per milestone that both implemented and verified cleanly. |

Commits are created inside the disposable task workspace only. **Nothing is
pushed, no remote is configured, and no history is rewritten.** A commit is
made only after the milestone's verification passes, with a deterministic
subject such as `feat(milestone-03): Capacity-safe registration`, and the SHA,
short SHA and message are stored on the milestone, shown in task details, and
published as a `milestone_commit_created` audit/evidence event.

Before the first milestone runs, an enabled run checks that committing can be
isolated and stops with an explanation if it cannot: the workspace must be its
own repository (never one nested in another), on a branch rather than a
detached HEAD, without a merge or rebase in progress, and with no uncommitted
changes the run did not create. A New Project workspace that is not yet a
repository is initialized explicitly, with no remote. Nothing is reset,
cleaned, or discarded in any of these cases. If a requested commit fails, the
milestone is recorded as failed with that error, later milestones do not run,
and the changes are captured in the task result.

Generated commits use a fixed `multiagent-chat <multiagent-chat@localhost>`
identity passed per command, so global Git configuration is never modified, and
signing is disabled for that command because headless execution cannot answer a
passphrase prompt.

The captured task result is measured from where the task started, not from
`HEAD`, so committing a milestone never removes it from the result. An existing
project is compared with the source revision the workspace was prepared at; a
New Project is compared with the empty project it began as. Either way the
result contains every committed milestone plus anything still staged, unstaged
or untracked, under the same Git output budget as before. Capture remains
read-only: it never resets, cleans, checks out, or rewrites anything.

## Agent selection

Three roles are configured independently per task:

| Role | Kind | Currently supported |
| --- | --- | --- |
| Proposer | chat provider + model | Gemini, Anthropic, OpenAI |
| Critic | chat provider + model | Gemini, Anthropic, OpenAI |
| Worker | coding tool + model | Claude Code |

### Provider availability

A chat provider is offered only when its own credential is configured:

- Gemini is available when `GEMINI_API_KEY` is set.
- Anthropic is available when `ANTHROPIC_API_KEY` is set.
- OpenAI is available when `OPENAI_API_KEY` is set. It uses the Responses API at
  `OPENAI_BASE_URL` (default `https://api.openai.com/v1`).

The application starts with any combination of these keys, including none. `GET /api/agents` and the
task form list only the available providers, and a task that asks for an
unavailable one is refused with a message naming the variable to set. An invalid
selection is never silently replaced by a working one.

The default proposer/critic wiring is Gemini/Anthropic, so using those chat
defaults requires both corresponding providers to be configured. When a
default role has no available provider, `GET /api/agents` returns
`"defaults": null` with an `unavailable` explanation, and a task created
without an explicit choice for that role fails validation instead of running
on a substitute. A single-provider installation can still run tasks by naming
the configured provider for both chat roles. The default Claude Code worker
does not add an `ANTHROPIC_API_KEY` requirement.

The Claude Code worker authenticates itself using its own stored login, so it
does not need `ANTHROPIC_API_KEY` for worker execution: with no key configured,
Claude Code is launched without one and uses its own credentials. When the key
is configured, it is passed to Claude Code as before. This is why the worker
stays available — and `cargo run -- --cli --implement-only` keeps working — on
an installation with no chat provider at all.

Codex is also available as an alternative worker tool. It uses its own CLI
authentication and the configured `CODEX_MODEL` default (plus models listed in
`CODEX_MODELS`); Claude Code remains the default worker for compatibility.

### Models

Chat providers offer their built-in general-purpose catalog, their configured
default, and any extra models listed in `GEMINI_MODELS`, `ANTHROPIC_MODELS`, or
`OPENAI_MODELS` (comma-separated). Coding tools offer their configured default
plus entries in `CLAUDE_CODE_MODELS` or `CODEX_MODELS`. The role defaults remain
`GEMINI_MODEL`, `CRITIC_MODEL` and `IMPLEMENTER_MODEL`/`CODEX_MODEL`, and a
default is always offered by its provider/tool. Model names come from
configuration; the application does not ask providers which models an account
may use.

### Choosing agents

The web form shows a provider/tool and a model selector per role, populated from
`GET /api/agents`; changing a provider reloads its models and drops a selection
that provider does not offer. The task page then shows the agents the run
actually uses. Omitting the `agents` block, or leaving the selectors alone, uses
the configured defaults. The legacy CLI has no selectors and always runs the
defaults.

```json
{
  "kind": "new_project",
  "title": "Create an event processor",
  "description": "Process events idempotently and expose health checks.",
  "technology": "rust",
  "output": "reviewable_result",
  "agents": {
    "proposer": { "provider": "gemini", "model": "gemini-3.6-flash" },
    "critic": { "provider": "anthropic", "model": "claude-sonnet-4-6" },
    "worker": { "tool": "claude_code", "model": "claude-opus-4-8" }
  }
}
```

Every field is optional: an omitted provider/tool or model falls back to the
configured default, while an unsupported provider, tool or model — or an
explicitly empty model — is rejected with HTTP 400 and an `error` message. No
credential is ever exposed through the configuration API or an error message.

## Supported technology profiles

The application currently detects or accepts:

- Rust/Cargo.
- Java/Spring Boot with Maven or Gradle.
- Python.
- TypeScript/JavaScript with Node.js.
- Custom/other repositories.

Detection uses repository evidence such as `Cargo.toml`, `pom.xml`, Gradle build files, `package.json`, `pyproject.toml`, requirements files, Dockerfiles, and Compose files. Verification prefers repository wrappers and scripts and is selected from the detected profile rather than always using Cargo.

## API overview

- `GET /api/health` — service health.
- `GET /api/agents` — available chat providers, coding tools, their configured
  models, and the default selection. Names only; never credentials.
- `GET /api/projects` — registered Projects.
- `POST /api/projects` — register a GitHub Project.
- `POST /api/tasks` — create a typed task, optionally with an `agents` selection,
  a `git_mode` (`none` or `commit_per_milestone`), and, for New Project, an
  `output` (`reviewable_result` or `persistent_local_project`) with a
  `destination` folder name for the persistent mode.
- `GET /api/tasks/{id}` — task snapshot, append-only audit `history`, and the
  bounded `log_tail`; every entry has sequence/timestamp/event fields.
- `GET /api/tasks/{id}/events` — live JSON SSE recorded-event envelopes.
- `GET /api/tasks/{id}/evidence` — download the redacted five-file evidence ZIP.
- `POST /api/tasks/{id}/approve` — approve/reject the specification, optionally with edits.
- `POST /api/tasks/{id}/publish/prepare` — finalize unchanged generated documentation and return the exact clean publication snapshot and fingerprint.
- `GET /api/tasks/{id}/publish` — read an already prepared clean publication snapshot.
- `POST /api/tasks/{id}/publish` — explicitly publish the exact confirmed fingerprint to the retained GitHub source identity, or to an existing GitHub `origin` for older persistent projects.

Example Project registration:

```json
{
  "name": "Example service",
  "repository": "owner/example-service",
  "default_branch": "main"
}
```

Example New Project task:

```json
{
  "kind": "new_project",
  "title": "Create an event processor",
  "description": "Process events idempotently and expose health checks.",
  "technology": "rust",
  "output": "reviewable_result"
}
```

Example New Project task kept on disk:

```json
{
  "kind": "new_project",
  "title": "Create an event processor",
  "description": "Process events idempotently and expose health checks.",
  "technology": "rust",
  "output": "persistent_local_project",
  "destination": "event-processor"
}
```

A finished task then reports its `persistence` (`mode`, `status`, `destination`, and repository status where applicable).

Example Feature task:

```json
{
  "kind": "feature",
  "project_id": "PROJECT_UUID",
  "title": "Add idempotency",
  "description": "Reject duplicate request keys without changing existing responses."
}
```

## Architecture

```text
Project / ProjectSource
        ↓
WorkspaceProvider
        ↓
Temporary TaskWorkspace
        ↓
Repository inspection + technology profile
        ↓
TaskKind-specific workflow and agent prompts
        ↓
Specification + user approval
        ↓
Implementation + profile-aware verification
        ↓
Critic implementation review + bounded fix loop
        ↓
Task result/diff + workspace cleanup
```

Core modules:

```text
src/
  agent/           role abstractions, per-task selection, catalogue, resolver
  project.rs       repository-backed Project domain and store boundary
  workspace.rs     isolated task workspace provider and result diff
  inspection.rs    bounded metadata and instruction discovery
  technology.rs    evidence-based technology profiles
  workflow.rs      task-kind-specific agent instructions
  verification.rs profile-aware command planning and execution
  task.rs          task state, recorded audit events, redaction, and ordering
  task_store.rs    durable task snapshots and append-only audit/evidence files
  evidence.rs      detailed interaction records and deterministic evidence ZIP
  debate.rs        proposer/critic collaboration
  spec.rs          specification drafting and checking
  implementer.rs   Claude Code coding-agent adapter, process and streamed output
  persistence.rs   persistent New Project output: safe destination and finalization
  acceptance.rs    acceptance criteria generated from the approved specification
  submission.rs    project documentation generated from recorded run evidence
  review.rs        structured post-implementation critic review and its findings
  review_baseline.rs per-milestone review baseline, separate from task results
  process_environment.rs explicit child-process environment policy
  execution_limits.rs centralized timeout, output, history, recovery settings
  process_runner.rs bounded process execution and output streaming
  process_job.rs    Windows child-process lifetime management
  web/             axum API, pipeline, SSE, and production UI
```

Project source registration remains in memory. Task state and evidence use the
local durable runtime store described above; this is single-process local
storage, not a shared or cloud-backed task queue.

## Current limitations

- Only public GitHub repositories are supported; no OAuth or GitHub App authentication exists yet.
- Raw worker stdout/stderr is intentionally not retained in the evidence
  transcript; the existing bounded UI log and process capture remain separate.
- A persistent New Project can be explicitly published to an existing GitHub remote after completion, using the local Git credential helper/SSH agent. OAuth/App authentication, automatic repository creation, and unattended pushes are not implemented.
- Persistent output requires a configured `PERSISTENT_OUTPUT_ROOT` (or `WORKSPACE_ROOT`); it cannot write anywhere else, and the generated project must contain no symlinks or junctions.
- Workspaces use the server's temporary directory and are cleaned after execution unless failed result capture requires manual recovery.
- Pull requests, user authentication, and Google Cloud deployment are not implemented. GitHub pushes are limited to the explicit, fingerprint-confirmed persistent-project action described above.
- The legacy CLI still uses `WORKSPACE_ROOT` and its original local-folder behavior.
- There is no user-facing task-cancellation endpoint yet. Cancellation event
  types exist for supported execution paths, while current worker/process
  timeouts retain their established failed-task behavior.
- A single-provider installation must name the configured provider explicitly
  for both chat roles on every task; there is no per-installation override of
  the default proposer/critic wiring. See [Agent selection](#agent-selection).
- Provider availability is decided by the presence of a key, not its validity,
  and agent model options come from environment configuration: the application
  does not query providers for the models an account can actually use, so a
  wrong key or model name fails when the task runs rather than when it is
  offered. The CLI always runs the configured defaults.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Normal tests do not call live AI or GitHub services. Live API checks remain ignored and cost tokens when explicitly enabled.
