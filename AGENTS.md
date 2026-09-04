# Multiagent Chat — Agent Instructions

## Project

Multiagent Chat is a Rust application that orchestrates multiple AI agents during software-development tasks. Understand the existing architecture before modifying code, and preserve existing behavior unless the task explicitly requires a change.

## General Development Workflow

For every development task:

1. Read the task-specific specification under `docs/tasks/`.
2. Read `docs/WORKFLOW.md` when the task refers to a reusable workflow.
3. Read the relevant project documentation.
4. Inspect the existing implementation before changing code.
5. Identify the smallest reasonable architectural change.
6. Implement incrementally.
7. Add or update tests.
8. Run formatting, linting, and tests.
9. Update documentation if architecture or behavior changed.
10. Summarize the completed work.

Do not rewrite large sections of the application unless necessary.

## Important Repository Files

- `AGENTS.md` — permanent project-level instructions for AI coding agents.
- `CLAUDE.md` — Claude-specific project guidance.
- `docs/WORKFLOW.md` — reusable workflows for development tasks.
- `docs/tasks/` — task-specific implementation specifications.
- `README.md` — project overview, setup, usage, and current structure.
- `plan_v2.md` — architecture and planning information for the web application.
- `PROGRESS_V2.md` — implementation progress, decisions, and current status.

## Development Rules

- Keep Rust code idiomatic.
- Prefer clear domain abstractions over large conditional blocks.
- Avoid unnecessary dependencies and unrelated refactoring.
- Preserve backward compatibility where practical.
- Keep modules focused on one responsibility.
- Do not hard-code assumptions that belong in configuration or domain models.
- Reuse existing abstractions where appropriate.
- Prefer small, understandable changes.
- Do not silently change public behavior.

## Existing Codebase Tasks

For feature development and bug fixes, inspect the existing architecture first and follow established conventions. Do not regenerate or recreate the whole application. Modify only relevant components, preserve unrelated behavior, and add regression tests when practical.

## Safety

Never:

- Delete unrelated files.
- Modify `.env` files containing credentials.
- Expose API keys, tokens, or secrets.
- Run destructive Git commands.
- Reset or discard existing user changes.
- Create branches automatically.
- Create Git worktrees automatically.
- Commit or push automatically.

The developer works alone. Work directly in the currently checked-out repository unless the user explicitly asks for a branch, worktree, commit, or push.

## Git Rules

Inspect `git status` before making changes. Preserve existing user changes, and do not revert files merely because they are modified.

Do not use `git reset --hard`, `git clean -fd`, force push, or destructive checkout commands unless explicitly requested by the user.

## Testing

Before completing a Rust-code task, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

If repository-specific commands differ, use the documented commands. Do not claim tests passed unless they were actually executed successfully. Report command failures clearly.

## Documentation

If a task changes architecture, workflow, configuration, or user-visible behavior, update the relevant documentation. Do not update unrelated documentation.

## Task Completion Report

At the end of each implementation task, report:

1. What was implemented.
2. Important design decisions.
3. Files changed.
4. Tests added or updated.
5. Commands executed and their results.
6. Known limitations.
7. Recommended next step.

## Scope Discipline

Follow the task specification closely. Do not add unrelated features merely because they seem useful. If the task explicitly marks something as out of scope, do not implement it.
