# Development Workflow

This file defines reusable execution flows for different software-development task types in the production web application. Select the workflow that matches the task.

The production model is:

```text
User
  ↓
Register/connect project source
  ↓
Create task
  ↓
Task type
  New Project / Feature / Bug Fix
  ↓
Prepare isolated task workspace
  ↓
Inspect repository/project
  ↓
Agents collaborate
  ↓
Specification
  ↓
User approval
  ↓
Implementation
  ↓
Build/tests
  ↓
Diff/result
  ↓
User review
```

---

# Project Source

An existing project normally comes from a source repository. GitHub is the primary source for the initial production design; GitLab and uploaded archives may be supported later.

A Project represents repository source metadata, not a persistent filesystem directory:

```text
Project
- id
- name
- source/provider
- repository identifier or URL
- default branch
- detected technology profile
```

Users register or connect a source. They do not select local IDE workspaces, arbitrary server directories, or operating-system paths.

---

# Task Workspace

Each task operates in its own isolated, temporary server-side workspace. For an existing project:

```text
repository
  ↓
prepare isolated workspace
  ↓
checkout requested branch/commit
  ↓
inspect
  ↓
implement
  ↓
verify
  ↓
produce diff/result
```

The workspace is disposable execution state, not permanent application state. Persistent project and task state must eventually live in durable services such as a database or object storage.

---

# 1. New Project

Use this workflow when the user wants to create a new application from scratch.

```text
Requirements
    ↓
Technology / stack selection
    ↓
Architecture proposal
    ↓
Critique
    ↓
Specification
    ↓
User approval
    ↓
Create isolated workspace
    ↓
Implementation
    ↓
Build/tests
    ↓
Verification
    ↓
Publish result
```

Rules:

- The user may choose the technology stack; do not assume Rust.
- The user selects an output target, not a local operating-system directory.
- Future output targets may include creating a GitHub repository, pushing to an existing empty repository, or providing a downloadable project archive.
- The implementation may initialize a new application structure inside the isolated workspace.
- Build and verification commands must depend on the selected stack.
- Do not create unrelated infrastructure unless required by the specification.

---

# 2. Feature

Use this workflow when adding functionality to an existing project.

```text
Select Project
    ↓
Prepare repository workspace
    ↓
Inspect architecture
    ↓
Inspect task-relevant code
    ↓
Detect project technology
    ↓
Propose minimal change
    ↓
Critique
    ↓
Specification
    ↓
User approval
    ↓
Implementation
    ↓
Regression tests
    ↓
Verification
    ↓
Show diff/result
```

Rules:

- Do not recreate the application or redesign unrelated parts of the system.
- Respect the existing architecture and follow established coding conventions.
- Minimize the blast radius and modify only relevant components.
- Reuse existing abstractions where practical.
- Preserve unrelated behavior and public behavior unless the feature explicitly changes it.
- Follow repository instructions such as `AGENTS.md` and `CLAUDE.md` when present.
- Add or update tests for the new behavior.

---

# 3. Bug Fix

Use this workflow when repairing incorrect behavior in an existing project.

```text
Select Project
    ↓
Prepare repository workspace
    ↓
Inspect relevant code/tests
    ↓
Understand or reproduce failure
    ↓
Identify root cause
    ↓
Evaluate blast radius
    ↓
Propose smallest correct fix
    ↓
User approval
    ↓
Implementation
    ↓
Regression test
    ↓
Verification
    ↓
Show diff/result
```

Rules:

- Fix the root cause rather than hiding the visible symptom.
- Avoid unrelated refactoring.
- Preserve behavior that is not part of the bug.
- Add a regression test when practical.
- Check whether the fix affects nearby code paths.
- Follow repository instructions when present.
- Do not redesign the whole component unless the root cause requires an architectural change.

---

# 4. Refactor

Use this reusable workflow for refactoring tasks, even if the application does not implement this task type yet.

```text
Select Project
    ↓
Prepare repository workspace
    ↓
Define refactor goal
    ↓
Inspect current design
    ↓
Identify technical problem
    ↓
Define behavior that must remain unchanged
    ↓
Propose limited structural change
    ↓
User approval
    ↓
Implementation
    ↓
Regression tests
    ↓
Verification
    ↓
Show diff/result
```

Rules:

- Behavior should remain unchanged unless explicitly requested.
- Avoid mixing refactoring with unrelated feature work.
- Follow repository instructions and conventions.
- Prefer measurable reasons for refactoring, such as duplication, coupling, testability, maintainability, or performance.

---

# 5. Investigation

Use this workflow for diagnostic and read-only tasks against an isolated repository workspace.

```text
Select Project
    ↓
Prepare repository workspace
    ↓
Question / problem
    ↓
Inspect repository and runtime evidence
    ↓
Form hypotheses
    ↓
Validate hypotheses
    ↓
Report findings
    ↓
Recommend next action
```

Rules:

- Do not modify code unless the user explicitly converts the investigation into an implementation task.
- Clearly separate confirmed findings from hypotheses.
- Reference relevant files, modules, logs, or tests.
- Treat the prepared workspace as disposable after findings are reported.

---

# Existing Project Inspection

Before proposing changes, prepare the isolated repository workspace and inspect enough context to understand the project.

Potential project metadata:

```text
Cargo.toml
Cargo.lock
pom.xml
build.gradle
build.gradle.kts
settings.gradle
settings.gradle.kts
package.json
pyproject.toml
requirements.txt
Dockerfile
docker-compose.yml
docker-compose.yaml
```

Potential instruction and documentation files:

```text
AGENTS.md
CLAUDE.md
README.md
plan*.md
PROGRESS*.md
```

Inspect relevant source code and tests. Do not dump the entire repository into an agent prompt; prefer task-relevant context. Do not modify files during inspection.

---

# Technology Detection

For existing repositories, detect technology automatically from repository evidence.

```text
Cargo.toml
→ Rust

pom.xml
→ Java / Maven

build.gradle or build.gradle.kts
→ Java/Kotlin / Gradle

package.json
→ JavaScript/TypeScript / Node.js

pyproject.toml
→ Python
```

Detect frameworks and infrastructure when evidence exists. For example:

```text
Language: Java 21
Framework: Spring Boot
Build: Gradle
Database: PostgreSQL
Messaging: Kafka
Containers: Docker
Tests: JUnit / Testcontainers
```

Detection must be evidence-based. Do not claim a technology is used only because documentation mentions it when repository structure contradicts that claim. Allow manual override when automatic detection is incomplete.

---

# Context Selection

For existing-project tasks, gather only context relevant to the requested change. Useful context may include:

- Repository tree.
- Build files.
- Relevant modules.
- Configuration.
- Interfaces.
- Tests.
- Git status within the task workspace.
- Relevant documentation and repository instructions.

Avoid unnecessary token usage by loading the whole repository. Prefer targeted inspection.

---

# Verification

Verification must depend on the detected or selected technology stack. Prefer repository-defined commands.

## Rust

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Gradle

```bash
./gradlew test
```

Use the repository's appropriate wrapper on the task execution platform.

## Maven

```bash
./mvnw test
```

or:

```bash
mvn test
```

Use the command appropriate to the repository.

## Node.js

Inspect `package.json` and use existing relevant scripts, such as:

```bash
npm test
npm run lint
npm run build
```

Run these only when the scripts exist and are relevant.

## Python

Use the repository's existing test configuration, for example:

```bash
pytest
```

Run `pytest` only when it is configured. Do not invent verification commands when the repository already defines them.

---

# Failure Handling

If build, lint, or tests fail:

1. Determine whether the failure was introduced by the current change.
2. Fix failures caused by the change.
3. Do not hide pre-existing failures.
4. Report unresolved pre-existing failures clearly.
5. Do not claim verification succeeded unless it actually succeeded.

---

# Production Considerations

- Browser users never provide arbitrary server filesystem paths.
- Project source and task workspace are separate concepts.
- Each task gets an isolated workspace.
- Workspace data is disposable.
- Persistent state must not depend on local container disk.
- Credentials must never be stored in task specifications.
- Repository credentials should eventually use secure secret storage.
- Application behavior should remain compatible with stateless, cloud-based execution.

These principles are application-level requirements. Detailed Google Cloud infrastructure is intentionally out of scope for this workflow.

---

# Scope Discipline

Every task should follow its task-specific specification under `docs/tasks/`. Do not add unrelated features or broaden the implementation because another improvement seems useful. Report important out-of-scope discoveries as recommendations instead of implementing them.

---

# Completion

At the end of an implementation task, provide:

1. What was implemented.
2. Important design decisions.
3. Files changed.
4. Tests added or changed.
5. Verification commands and results.
6. Known limitations.
7. Recommended next step.

Keep this file application-level and reusable. Do not include details that belong only to one specific task.
