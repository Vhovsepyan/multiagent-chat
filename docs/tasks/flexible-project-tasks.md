# Flexible Projects and Task Types

## Status and Purpose

This specification defines the evolution of Multiagent Chat from a Rust-only greenfield generator into a repository-backed, multi-agent software-engineering platform. It targets the production web application and must be implemented incrementally within the existing Rust/axum architecture.

The implementation must support:

1. Multiple projects.
2. Repository-backed existing projects.
3. Multiple technology stacks.
4. New-project creation.
5. Feature implementation.
6. Bug fixing.
7. Isolated task workspaces.
8. Automatic project inspection.
9. Technology-aware verification.

The core domain and product UI must not be designed around user-provided filesystem paths.

## Product Flow

```text
User
  ↓
Register/connect project source
  ↓
Create task
  ↓
Choose New Project / Feature / Bug Fix
  ↓
Prepare isolated task workspace
  ↓
Inspect repository/project
  ↓
Run task-specific agent collaboration
  ↓
Generate specification
  ↓
User approval
  ↓
Implement
  ↓
Build and test
  ↓
Produce diff/result
  ↓
User review
```

## Project Domain

Introduce a first-class `Project` domain model. Conceptually:

```rust
struct Project {
    id: ProjectId,
    name: String,
    source: ProjectSource,
    default_branch: String,
    detected_stack: Option<ProjectProfile>,
}
```

The exact Rust types, ownership, serialization, and module placement must follow existing project conventions.

Initially support GitHub repositories:

```rust
enum ProjectSource {
    GitHub {
        repository: String,
    },
}
```

Requirements:

- A Project represents a source repository, not a persistent filesystem directory.
- `ProjectId` must be a stable application identifier independent of repository names.
- GitHub repository identifiers must be validated and normalized at the domain boundary.
- The representation should accept the public-repository form chosen during implementation, such as an HTTPS clone URL or canonical `owner/repository` identifier, without embedding credentials.
- `default_branch` identifies the normal checkout target; a task may eventually select a specific branch or commit without changing the Project identity.
- Design `ProjectSource` so GitLab, uploaded archives, and other Git providers can be added without changing task orchestration throughout the application.
- Do not add local filesystem paths to the core production Project model.

Project persistence may remain an in-memory implementation during this phase if necessary, but access must sit behind a focused store/service boundary so external persistence can replace it later.

## Task Domain and Validation

Introduce distinct task kinds:

```rust
enum TaskKind {
    NewProject,
    Feature,
    BugFix,
}
```

Design the enum and workflow dispatch so `Refactor` and `Investigation` can be added later without being implemented in this task.

Task input should express kind-specific data instead of relying on one greenfield-oriented shape. The exact representation may use validated request types, an enum with payloads, or another idiomatic Rust design.

Validation rules:

| Task kind | Registered Project | Technology selection | Output configuration |
| --- | --- | --- | --- |
| `NewProject` | Not required | Required | Required |
| `Feature` | Required | Auto-detected by default | Not applicable |
| `BugFix` | Required | Auto-detected by default | Not applicable |

All task kinds require a non-empty title and description according to the application's existing validation conventions.

Invalid combinations must fail before agent calls or workspace preparation. Examples include a Feature without a Project, a BugFix with an unknown Project, or a NewProject without a requested technology or output configuration.

Existing task status, event history, approval, rejection, and SSE behavior should remain compatible where practical. `TaskKind`, Project identity, inspection results, verification activity, and final diff/result should be represented in snapshots/events where needed by the UI.

## New-Project Inputs and Output

A `NewProject` task requires:

- Title.
- Description.
- Selected or requested technology stack.
- Output configuration.

The output configuration describes a publication target, not a local Windows or Linux destination path. Design it as an extensible domain abstraction. Future output variants may include:

- Create a GitHub repository.
- Push to an existing empty repository.
- Produce a downloadable project archive.

This phase may use a simple non-production publisher or retain the completed workspace long enough to expose a result for development and tests. That temporary mechanism must remain behind an output/publishing boundary and must not make a local user directory part of the production domain.

New-project execution follows the `New Project` workflow in `docs/WORKFLOW.md` and must not assume Rust.

## Technology Model

Introduce an extensible `ProjectProfile`/`TechStack` abstraction rather than distributing language checks across handlers, prompts, and pipeline stages.

Initially represent:

- Rust.
- Java with Spring Boot, including Maven or Gradle build metadata.
- Python.
- TypeScript/JavaScript with Node.js.
- Custom/other.

The profile should carry evidence-based information needed by orchestration and verification, such as:

- Primary language or runtime.
- Framework when detected.
- Build/package tool.
- Relevant metadata files.
- Test tooling when detected.
- Container and infrastructure signals when relevant.
- Detection confidence or an explicit custom/manual selection where useful.

New projects use the user's selected technology. Existing projects default to automatic detection after checkout, with a manual override path for incomplete or ambiguous detection.

### Detection Evidence

Inspect repository files including, when present:

- `Cargo.toml` and `Cargo.lock`.
- `pom.xml`.
- `build.gradle`, `build.gradle.kts`, `settings.gradle`, and `settings.gradle.kts`.
- `package.json` and relevant lock files.
- `pyproject.toml` and `requirements.txt`.
- `Dockerfile`.
- `docker-compose.yml` and `docker-compose.yaml`.

Minimum detection behavior:

- `Cargo.toml` identifies Rust.
- `pom.xml` identifies Java/Maven and should identify Spring Boot when supported by file evidence.
- Gradle build files identify Java/Kotlin with Gradle and should identify Spring Boot when supported by plugin/dependency evidence.
- `package.json` identifies Node.js and should distinguish TypeScript when repository evidence supports it.
- `pyproject.toml` or relevant Python project metadata identifies Python.
- Unrecognized or mixed repositories produce a Custom/other or composite-capable profile rather than an unsupported-language failure.

Detection must return structured evidence and remain independently testable. Documentation claims alone must not override contradictory repository evidence.

## Repository Inspection and Context Selection

After preparing an existing project's workspace, inspect the repository before proposal or implementation.

Inspection should gather targeted context:

- Repository tree at an appropriate depth.
- Build and package metadata.
- Relevant modules and interfaces.
- Configuration relevant to the task.
- Existing tests.
- Git status and checked-out revision.
- Relevant repository instructions and documentation.

Detect and apply repository guidance such as:

- `AGENTS.md`.
- `CLAUDE.md`.
- `README.md`.

Instruction discovery should respect scope where nested instruction files exist. Do not load the entire repository into agent prompts. Select the smallest context needed for the task and avoid secrets, generated outputs, dependency caches, and unrelated files.

Inspection is read-only. It must occur before agents propose changes to an existing project.

## Workspace Abstraction

Separate repository source, workspace preparation, and orchestration:

```text
Project Source
      ↓
Workspace Provider
      ↓
Temporary Task Workspace
      ↓
Agents / Implementation
```

Introduce a conceptual workspace abstraction, for example:

```rust
trait WorkspaceProvider {
    async fn prepare(...);
    async fn cleanup(...);
}
```

The final trait signatures and async strategy must be chosen after inspecting the existing code and dependency patterns.

Workspace requirements:

- Every task receives its own isolated server-side workspace identified by the task, not by a user-selected path.
- Existing-project tasks fetch or clone the registered source and check out the requested default branch/revision into that workspace.
- New-project tasks begin in an empty isolated workspace.
- The workspace exposes only the internal path/capabilities needed by inspection, implementation, verification, diff generation, and publishing.
- Orchestration must not depend directly on one configured root directory.
- Concurrent tasks for the same Project must not share a mutable checkout.
- Cleanup must be explicit, safe, idempotent where practical, and constrained to provider-owned temporary locations.
- Workspace preparation failures must become clear task failures without starting implementation.
- The filesystem is temporary execution state. Project identity, task history, specifications, and durable results must not depend on workspace survival.

A local temporary-directory provider is acceptable for development and tests in this phase. It represents a server-side execution implementation and must never expose arbitrary filesystem selection to product users.

## Repository Provider Boundary

GitHub is the first repository provider. Define the source/provider boundary needed to obtain a repository into a prepared workspace without coupling the complete pipeline to GitHub-specific details.

For this phase:

- Public GitHub repository source information may be used in development and tests.
- Repository acquisition should report the resolved revision used for the task.
- Provider errors must be surfaced without leaking credentials or command details containing secrets.
- Tests should avoid depending on live GitHub availability; use fixtures, local test repositories, fakes, or injected command boundaries.

Do not implement GitHub OAuth, GitHub App authentication, pushing commits, or pull-request creation in this task.

## Task-Specific Agent Behavior

Use distinct orchestration instructions from `docs/WORKFLOW.md`.

### New Project

- Start from requirements and selected technology.
- Propose architecture suitable for a new application.
- Critique the proposal, produce a specification, and wait for user approval.
- Implement only after approval in a new isolated workspace.
- Verify using the selected stack and publish the configured result.

### Feature

- Require a registered Project and prepared repository workspace.
- Inspect architecture, repository guidance, technology, and task-relevant code before proposing changes.
- Prefer the smallest compatible change and preserve unrelated behavior.
- Produce a specification and wait for approval before implementation.
- Add regression coverage, verify, and show the resulting diff.

### Bug Fix

- Require a registered Project and prepared repository workspace.
- Inspect relevant code/tests and reproduce or otherwise understand the failure.
- Identify root cause and blast radius before proposing the smallest correct fix.
- Wait for approval before implementation.
- Add a regression test when practical, verify, and show the resulting diff.

Prompt construction should use typed task kind, project profile, repository instructions, and selected context. Do not concatenate one generic greenfield prompt for all task kinds. Tests must demonstrate meaningful prompt/workflow differences without asserting fragile full prompt strings.

## Verification Abstraction

Verification must be selected from the Project profile and repository configuration, not hard-coded to Cargo.

Introduce a focused verification planner/runner boundary that returns structured commands and results. Prefer repository-defined wrappers and scripts over global tools or invented commands.

Initial behavior:

- Rust: repository-appropriate Cargo formatting, linting, and test commands.
- Gradle: use the repository Gradle wrapper and relevant project tasks when present.
- Maven: use the Maven wrapper when present, otherwise an available repository-appropriate Maven command.
- Node.js: inspect `package.json` and run only relevant scripts that exist, such as test, lint, type-check, or build.
- Python: use configured repository tooling, such as pytest, only when supported by project configuration.
- Custom/other: use explicitly configured or confidently detected commands; otherwise report that automatic verification is unavailable.

Requirements:

- Commands execute inside the isolated task workspace.
- Do not execute arbitrary verification text generated by an agent without validation or policy controls.
- Capture command, exit status, and bounded output for task events and the final report.
- Distinguish failures introduced by the task from known or discovered pre-existing failures where evidence permits.
- Never claim verification succeeded unless every required command actually succeeded.

## API and Service Behavior

The exact routes and payloads should follow the existing axum API conventions, but the application needs boundaries for:

- Registering and listing Projects.
- Retrieving Project metadata and detected profile.
- Creating kind-specific tasks.
- Validating Project/task-kind combinations.
- Selecting or recording a source revision.
- Exposing inspection, verification, diff, and publication results.

Handlers should validate and delegate. Project lookup, workspace lifecycle, detection, workflow selection, and verification should remain testable outside HTTP handlers.

Do not treat an in-memory store as the permanent production design. Define service/store interfaces that permit later external persistence without implementing a cloud database now.

## Production UI

Task creation must change conceptually according to task kind.

Existing-project task:

```text
Task Type:
Feature / Bug Fix

Project:
[registered repository]

Title:
...

Description:
...
```

New-project task:

```text
Task Type:
New Project

Technology:
Rust / Java Spring Boot / Python / TypeScript / Other

Title:
...

Description:
...
```

The New Project form must also collect output configuration appropriate to the initially supported publishing behavior.

UI requirements:

- Project registration uses repository source information, initially GitHub.
- Feature and Bug Fix forms require a registered Project.
- New Project does not require an existing Project.
- Browser users never see or submit arbitrary server filesystem paths.
- Task pages expose detected technology, selected workflow, source revision when applicable, verification results, and the final diff/result.
- Preserve the existing live task timeline, approval gate, and streamed events where compatible.

## Cloud-Ready Boundaries

The design must remain compatible with eventual deployment where:

- The web/API service is stateless.
- Task execution occurs separately from request handling.
- Task workspaces are isolated and temporary.
- Persistent Project and task state uses external persistence.
- Durable artifacts/results use external storage when needed.
- Credentials use external secret storage.

Do not rely on local container disk for persistent state. Do not place repository credentials in Project records, task specifications, prompts, events, or logs.

This task defines application boundaries only. It must not add Google Cloud deployment or product-specific infrastructure.

## Backward Compatibility and Migration

Implement incrementally and preserve existing behavior where compatible:

- Reuse the existing task state machine, event emitter, approval gate, debate/spec stages, and web patterns rather than recreating the application.
- Replace fixed-directory assumptions behind the new Project and Workspace abstractions.
- Keep existing terminal behavior only where it does not force local-path concepts into the production domain; clearly isolate any temporary compatibility adapter.
- Avoid silent API behavior changes. Update contracts and tests deliberately where new required fields make old requests invalid.
- Do not migrate `.env` credentials or print their values.

Before implementation, inspect current modules and propose the smallest staged architecture. Likely responsibilities include Project domain/storage, task input validation, workspace preparation, repository inspection, technology detection, workflow/prompt selection, verification, and UI/API adaptation; exact file placement is an implementation decision.

## Implementation Sequence

Use small, testable increments:

1. Add Project, ProjectSource, TaskKind, technology profile, and kind-specific validation.
2. Add Project storage/service boundaries and GitHub source validation.
3. Add WorkspaceProvider and a safe temporary-directory implementation.
4. Add repository inspection, instruction discovery, and technology detection.
5. Add task-specific workflow and prompt selection.
6. Add technology-aware verification planning and execution.
7. Integrate workspace lifecycle and result/diff production with the existing pipeline.
8. Update API and UI for Project registration and kind-specific task creation.
9. Preserve and extend SSE status/history presentation.
10. Run full regression and acceptance verification.

If implementation reveals a major architectural choice, present concrete options to the user as required by `CLAUDE.md` before proceeding.

## Testing Requirements

Add focused unit and integration coverage for at least:

- `TaskKind` serialization and validation.
- Project creation and registration.
- GitHub source validation and normalization.
- NewProject validation.
- Feature validation.
- BugFix validation.
- Invalid task/Project combinations.
- Technology detection and structured evidence.
- Rust detection.
- Maven Java/Spring detection.
- Gradle Java/Spring detection.
- Node.js and TypeScript detection.
- Python detection.
- Custom/other fallback.
- Workspace abstraction and isolated workspace creation.
- Independent workspaces for concurrent tasks.
- Safe cleanup limited to provider-owned paths.
- Workspace preparation failure handling.
- Repository instruction discovery and task-relevant context selection.
- Correct workflow selection.
- Meaningful prompt differences between task kinds.
- Verification command selection for each supported stack.
- Preference for repository wrappers/scripts.
- Invalid or unavailable verification handling.
- API validation and Project lookup.
- UI fields and absence of arbitrary filesystem-path inputs.
- Existing task state, approval, event, and regression behavior where compatible.

Tests must not require real credentials or live external services. Use deterministic fixtures, fakes, and temporary repositories/workspaces.

Before completing the later implementation task, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Out of Scope

Do not implement in this task:

- Google Cloud deployment or detailed Google Cloud infrastructure.
- Cloud Run.
- Cloud SQL.
- Pub/Sub.
- Cloud Storage.
- Secret Manager.
- GitHub OAuth.
- GitHub App authentication.
- GitLab.
- Uploaded archive support.
- Jira.
- MCP.
- Pull-request creation.
- Pushing commits.
- Additional AI providers.
- Git worktrees.
- Git branches managed by Multiagent Chat.
- User authentication.

These are separate future tasks. Interfaces may permit future extensions but must not add speculative implementations.

## Acceptance Criteria

The feature is complete when:

1. Project is a first-class repository-backed concept.
2. Existing tasks operate against Projects rather than a fixed RustRover or user directory.
3. NewProject, Feature, and BugFix are distinct task kinds.
4. Feature and BugFix operate on existing Project sources.
5. NewProject supports multiple technology choices.
6. Existing-project technology can be detected from repository evidence.
7. Orchestration is not hard-coded to Rust.
8. Verification is not hard-coded to Cargo.
9. Task execution uses an isolated workspace abstraction.
10. Production UI does not expose arbitrary server filesystem selection.
11. Existing behavior and tests remain working where compatible.
12. Repository instructions influence existing-project task context.
13. The final task result includes verification details and a diff or publishable result.
14. No credentials are persisted in specifications, prompts, events, or logs.
15. The architecture remains extensible toward stateless Google Cloud execution without implementing cloud infrastructure.

## Completion Report for the Implementation Task

At implementation completion, report:

1. What was implemented.
2. Important design decisions and approved deviations from this specification.
3. Files changed.
4. Tests added or updated.
5. Verification commands and results.
6. Known limitations, including temporary development-only adapters.
7. Recommended next production step.
