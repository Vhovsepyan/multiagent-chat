# multiagent-chat

A Rust web application for repository-backed, multi-agent software engineering. A proposer agent designs a solution, a critic agent reviews it, the application produces an editable specification, and a worker agent implements the user-approved result in an isolated task workspace.

Each task chooses its own agents: the proposer and critic can run on any configured chat provider (currently Gemini or Anthropic) with any configured model, and the worker runs a coding tool (currently Claude Code) with its own model. The choice is stored on the task, so a run is not affected by later configuration changes. With no explicit choice, a task uses the configured defaults — proposer Gemini, critic Anthropic, worker Claude Code — which is how the application behaved before selection existed. See [Agent selection](#agent-selection).

Multiagent Chat supports three task kinds:

- New Project for a selected technology stack.
- Feature for a registered repository.
- Bug Fix for a registered repository.

GitHub public repositories are the initial existing-project source. Each task uses a separate disposable server-side workspace; a Project never represents a persistent local directory.

## Requirements

- Rust stable with `cargo`.
- Git on `PATH` for repository-backed tasks.
- Claude Code CLI on `PATH` for implementation.
- An API key for each chat provider you want to use: `GEMINI_API_KEY` (Google AI
  Studio) and/or `ANTHROPIC_API_KEY` (Anthropic). Neither is required to start
  the application; each one enables its own provider.

## Setup

```bash
git clone https://github.com/Vhovsepyan/multiagent-chat
cd multiagent-chat
cp .env.example .env
```

Set the keys for the providers you intend to use in `.env`. Do not commit this
file or expose its values. Both keys are needed only for the default
proposer/critic wiring; see [Agent selection](#agent-selection) for what a
single-provider installation can do.

`WORKSPACE_ROOT` is no longer required by the web application. It remains an optional compatibility setting for the original CLI workflow.

## Usage

```bash
cargo run                  # web UI at http://127.0.0.1:3000
cargo run -- --cli         # legacy local terminal workflow
cargo run -- --help
```

In the web UI:

1. Register a public GitHub repository using `owner/repository` or its HTTPS URL when working on existing code.
2. Create a New Project, Feature, or Bug Fix task.
3. Watch repository inspection and the proposer/critic debate through SSE.
4. Review or edit the generated specification.
5. Approve implementation.
6. Review implementation output, technology-aware verification, and the resulting working-tree diff/status.

Feature and Bug Fix tasks require a registered Project. New Project tasks instead require a selected technology and an output configuration. The initial output is a reviewable task result; repository publishing is intentionally deferred.

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
and its absolute path is supplied to Claude Code. It is never written into the
repository, never replaces a project-owned `SPEC.md`, and does not appear in
the project's diff. Cleanup covers both directories.

Git, verification tools, and Claude Code start with cleared environments and
an explicit runtime-variable allowlist (OS paths, home/temp locations, locale,
and supported toolchain locations). Claude Code additionally receives only the
configured `ANTHROPIC_API_KEY`, and none at all when that key is not configured
— it then uses its own stored login; other provider keys, database/cloud credentials,
alternate provider endpoints/tokens, and arbitrary tool options are not inherited.
Custom setups relying on other environment variables may need a reviewed policy
change. This reduces environment exposure, but is **not a sandbox**: child
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

Task history retains the newest Build/Notice/Warning logs, with a discarded-log
counter and an explanation on page reload. Proposals, critiques, specifications,
approval, state transitions, verification, failure/completion, and final results
are not evicted by that log limit. This is not a global memory limit: task count
and lifecycle documents still need durable persistence and retention policies.

Git capture uses a separate 8 MiB stdout/result-content budget. Over-budget Git
output is rejected rather than parsed as a complete diff; large added files also
fail capture safely, retaining the workspace for recovery. Workspace preparation,
inspection, diff capture, and cleanup run off the HTTP runtime workers.

Delayed cleanup is in-process only. A server restart loses its timers; failed
cleanup retries and workspaces left by a restart require manual cleanup. These
limits do not make untrusted repositories safe to execute.

## Agent selection

Three roles are configured independently per task:

| Role | Kind | Currently supported |
| --- | --- | --- |
| Proposer | chat provider + model | Gemini, Anthropic |
| Critic | chat provider + model | Gemini, Anthropic |
| Worker | coding tool + model | Claude Code |

### Provider availability

A chat provider is offered only when its own credential is configured:

- Gemini is available when `GEMINI_API_KEY` is set.
- Anthropic is available when `ANTHROPIC_API_KEY` is set.

The application starts with one key, both, or neither. `GET /api/agents` and the
task form list only the available providers, and a task that asks for an
unavailable one is refused with a message naming the variable to set. An invalid
selection is never silently replaced by a working one.

The default wiring is proposer Gemini, critic Anthropic, worker Claude Code, so
those defaults need their corresponding providers configured. When a default
role has no available provider, `GET /api/agents` returns `"defaults": null`
with an `unavailable` explanation, and a task created without an explicit choice
for that role fails validation instead of running on a substitute. A
single-provider installation can still run tasks by naming the configured
provider for both chat roles.

The Claude Code worker authenticates itself using its own stored login, so it
does not need `ANTHROPIC_API_KEY` for worker execution: with no key configured,
Claude Code is launched without one and uses its own credentials. When the key
is configured, it is passed to Claude Code as before. This is why the worker
stays available — and `cargo run -- --cli --implement-only` keeps working — on
an installation with no chat provider at all.

### Models

Each provider or tool offers its configured default model plus any extra models
listed in `GEMINI_MODELS`, `ANTHROPIC_MODELS` and `CLAUDE_CODE_MODELS`
(comma-separated). The role defaults remain `GEMINI_MODEL`, `CRITIC_MODEL` and
`IMPLEMENTER_MODEL`, and a default is always offered by its provider. Model
names come from configuration; the application does not ask providers which
models an account may use.

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
- `POST /api/tasks` — create a typed task, optionally with an `agents` selection.
- `GET /api/tasks/{id}` — task snapshot and history.
- `GET /api/tasks/{id}/events` — live JSON SSE events.
- `POST /api/tasks/{id}/approve` — approve/reject the specification, optionally with edits.

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
  task.rs          task state, validation, history, and result model
  debate.rs        proposer/critic collaboration
  spec.rs          specification drafting and checking
  implementer.rs   Claude Code coding-agent adapter, process and streamed output
  process_environment.rs explicit child-process environment policy
  execution_limits.rs centralized timeout, output, history, recovery settings
  process_runner.rs bounded process execution and output streaming
  process_job.rs    Windows child-process lifetime management
  web/             axum API, pipeline, SSE, and production UI
```

Project/task stores remain in memory in this phase. The boundaries are designed for later external persistence and separate task execution; local container disk is not treated as durable application state.

## Current limitations

- Only public GitHub repositories are supported; no OAuth or GitHub App authentication exists yet.
- Projects and task history are lost when the process restarts.
- The initial New Project output is a reviewable result, not a downloadable archive or pushed repository.
- Workspaces use the server's temporary directory and are cleaned after execution unless failed result capture requires manual recovery.
- Pull requests, pushes, user authentication, and Google Cloud deployment are not implemented.
- The legacy CLI still uses `WORKSPACE_ROOT` and its original local-folder behavior.
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
