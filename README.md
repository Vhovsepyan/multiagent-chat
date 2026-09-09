# multiagent-chat

A Rust web application for repository-backed, multi-agent software engineering. Gemini proposes a solution, Claude critiques it, the application produces an editable specification, and Claude Code implements the user-approved result in an isolated task workspace.

Multiagent Chat supports three task kinds:

- New Project for a selected technology stack.
- Feature for a registered repository.
- Bug Fix for a registered repository.

GitHub public repositories are the initial existing-project source. Each task uses a separate disposable server-side workspace; a Project never represents a persistent local directory.

## Requirements

- Rust stable with `cargo`.
- Git on `PATH` for repository-backed tasks.
- Claude Code CLI on `PATH` for implementation.
- Google AI Studio and Anthropic API keys.

## Setup

```bash
git clone https://github.com/Vhovsepyan/multiagent-chat
cd multiagent-chat
cp .env.example .env
```

Set `GEMINI_API_KEY` and `ANTHROPIC_API_KEY` in `.env`. Do not commit this file or expose its values.

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
configured `ANTHROPIC_API_KEY`; other provider keys, database/cloud credentials,
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
- `GET /api/projects` — registered Projects.
- `POST /api/projects` — register a GitHub Project.
- `POST /api/tasks` — create a typed task.
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
  project.rs       repository-backed Project domain and store boundary
  workspace.rs     isolated task workspace provider and result diff
  inspection.rs    bounded metadata and instruction discovery
  technology.rs    evidence-based technology profiles
  workflow.rs      task-kind-specific agent instructions
  verification.rs profile-aware command planning and execution
  task.rs          task state, validation, history, and result model
  debate.rs        proposer/critic collaboration
  spec.rs          specification drafting and checking
  implementer.rs   Claude Code process and streamed output
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

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Normal tests do not call live AI or GitHub services. Live API checks remain ignored and cost tokens when explicitly enabled.
