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
  web/             axum API, pipeline, SSE, and production UI
```

Project/task stores remain in memory in this phase. The boundaries are designed for later external persistence and separate task execution; local container disk is not treated as durable application state.

## Current limitations

- Only public GitHub repositories are supported; no OAuth or GitHub App authentication exists yet.
- Projects and task history are lost when the process restarts.
- The initial New Project output is a reviewable result, not a downloadable archive or pushed repository.
- Workspaces use the server's temporary directory and are cleaned after execution.
- Pull requests, pushes, user authentication, and Google Cloud deployment are not implemented.
- The legacy CLI still uses `WORKSPACE_ROOT` and its original local-folder behavior.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Normal tests do not call live AI or GitHub services. Live API checks remain ignored and cost tokens when explicitly enabled.
