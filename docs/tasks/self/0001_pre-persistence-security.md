# Pre-Persistence Security Hardening

## Goal

Harden task execution before PostgreSQL persistence is implemented.

This task covers only:

1. Moving orchestration specification artifacts outside the target repository.
2. Restricting environment variables inherited by child processes.

Do not implement PostgreSQL, authentication, Cloud Run deployment, GitHub App integration, or unrelated features.

## 1. Approved Specification Artifact

Currently the approved specification may be written as `SPEC.md` inside the cloned user's repository.

This must change.

The approved specification is an orchestration artifact, not part of the user's project.

Use a structure conceptually similar to:

```text
task-workspace/
├── repo/
│   └── cloned repository
└── artifacts/
    └── approved-spec.md
```

The exact implementation may differ if the current workspace abstractions suggest a cleaner design.

Required invariants:

* orchestration `SPEC.md` must not be created inside the target repository
* an existing project-owned `SPEC.md` must never be overwritten
* the approved specification must still be passed exactly to the implementer
* edited approved specifications must remain authoritative
* orchestration artifacts must not appear in the user's ChangeSet/diff

## 2. Child Process Environment

Review all external process execution:

* Claude Code / implementer
* Cargo
* Gradle
* Maven
* npm
* Python
* other verification commands

Do not allow repository-controlled commands to inherit the entire Multiagent Chat process environment.

Introduce a reusable process environment policy/helper.

Prefer an explicit allowlist.

Preserve OS/runtime variables where required, for example:

```text
PATH
HOME
USERPROFILE
TEMP
TMP
SystemRoot
```

depending on platform.

For AI implementer processes, expose only credentials actually required by that implementer.

Do not expose unrelated:

* database credentials
* other AI-provider API keys
* GitHub administration credentials
* cloud credentials
* internal service secrets

Important:

Environment filtering reduces secret exposure but is NOT a security sandbox.

Do not claim arbitrary untrusted repositories are safe to execute yet.

## Tests

Add tests covering:

* approved spec stored outside repository
* existing repository `SPEC.md` preserved
* edited approved spec reaches implementer
* orchestration spec excluded from ChangeSet
* required environment variables preserved
* unrelated secret-like variables excluded
* implementer receives only required provider configuration

## Verification

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Scope

Do not:

* implement persistence
* fix SSE replay
* implement Cloud Run
* add GitHub authentication
* create branches/worktrees
* commit or push

## Completion Report

Report:

1. design changes
2. files changed
3. tests added
4. fmt result
5. clippy result
6. test result
7. remaining security limitations
