# Development Workflow

This file defines reusable execution flows for different software-development task types. Select the workflow that matches the task rather than applying one process to every request.

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
Critique / review
    ↓
Specification
    ↓
User approval
    ↓
Implementation
    ↓
Build / tests
    ↓
Verification
```

Rules:

- The user may choose the technology stack; do not assume Rust.
- The destination directory must be explicitly known.
- The implementation may initialize a new application structure.
- Build and verification commands must depend on the selected stack.
- Do not create unrelated infrastructure unless required by the specification.

---

# 2. Feature

Use this workflow when adding functionality to an existing project.

```text
Existing repository
    ↓
Inspect architecture
    ↓
Understand relevant code
    ↓
Identify affected components
    ↓
Propose the smallest reasonable change
    ↓
Critique / review
    ↓
Implementation plan
    ↓
User approval
    ↓
Implementation
    ↓
Regression tests
    ↓
Verification
```

Rules:

- Do not recreate the application or redesign unrelated parts of the system.
- Follow existing architecture and coding conventions.
- Reuse existing abstractions where practical.
- Modify only components relevant to the requested feature.
- Preserve existing public behavior unless the feature explicitly changes it.
- Add or update tests for the new behavior.

---

# 3. Bug Fix

Use this workflow when repairing incorrect behavior in an existing project.

```text
Bug description
    ↓
Inspect relevant code
    ↓
Understand or reproduce the failure
    ↓
Identify root cause
    ↓
Evaluate blast radius
    ↓
Propose the smallest correct fix
    ↓
User approval
    ↓
Implementation
    ↓
Regression test
    ↓
Verification
```

Rules:

- Fix the root cause, not only the visible symptom.
- Avoid unrelated refactoring.
- Preserve existing behavior that is not part of the bug.
- Add a regression test when practical.
- Check whether the fix can affect nearby code paths.
- Do not redesign the whole component unless the root cause requires an architectural change.

---

# 4. Refactor

Use this supported workflow for refactoring tasks, even if the application does not implement this task type yet.

```text
Refactor goal
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
```

Rules:

- Behavior should remain unchanged unless explicitly requested.
- Avoid mixing refactoring with unrelated feature work.
- Prefer measurable reasons for refactoring, such as duplication, coupling, testability, maintainability, or performance.

---

# 5. Investigation

Use this workflow for diagnostic and read-only tasks.

```text
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

---

# Existing Project Inspection

Before proposing changes to an existing repository, inspect enough context to understand how the project works.

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

Inspect relevant source code and tests. Do not dump the entire repository into an agent prompt; prefer task-relevant context. Do not modify files during the inspection stage.

---

# Technology Detection

For existing projects, technology should normally be detected automatically.

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

Also detect framework and infrastructure when practical. For example:

```text
Language: Java 21
Framework: Spring Boot
Build: Gradle
Database: PostgreSQL
Messaging: Kafka
Containers: Docker
Tests: JUnit / Testcontainers
```

Detection must be evidence-based. Do not claim a technology is used only because it appears in documentation when the repository structure contradicts it. Allow manual override when automatic detection is incomplete.

---

# Context Selection

For feature and bug-fix tasks, gather only context relevant to the requested change. Useful context may include:

- Project tree.
- Build files.
- Relevant modules.
- Configuration.
- Interfaces.
- Tests.
- Git status.
- Relevant documentation.

Avoid unnecessary token usage by loading the whole repository. Prefer targeted inspection.

---

# Verification

Verification must depend on the project stack. Use repository-defined commands when available.

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

On Windows, use the repository's appropriate wrapper if needed.

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

# Scope Discipline

Every task should follow its task-specific specification under `docs/tasks/`. Do not add unrelated features or broaden the implementation because another improvement seems useful. If something important is discovered outside the current task scope, report it as a recommendation instead of implementing it.

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

Keep this file reusable. Do not include details that belong only to one specific task.
