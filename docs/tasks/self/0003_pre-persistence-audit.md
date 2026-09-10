# Pre-Persistence Milestone Audit

## Goal

Audit the current codebase before beginning PostgreSQL persistence.

Do not add new product features.

Read:

* `AGENTS.md`
* `CLAUDE.md`
* `docs/WORKFLOW.md`
* relevant files under `docs/tasks/`
* current implementation

## Verify

Check each requirement and mark PASS or FAIL.

### Repository / Project Model

1. Project identity is repository-backed, not based on a user's local filesystem path.
2. Temporary workspace location is separate from Project identity.
3. Feature and BugFix operate against existing repository-backed Projects.
4. NewProject supports multiple technologies.

### Inspection

5. Feature tasks inspect task-relevant source code.
6. BugFix tasks inspect relevant source and tests where available.
7. Inspection is bounded and skips generated/vendor directories.

### Specification

8. Approved specification is authoritative.
9. Edited approved specification is used by the implementer.
10. Orchestration specification files are outside the user repository.
11. Existing repository `SPEC.md` cannot be overwritten by orchestration.

### Verification / Results

12. Verification is technology-aware and not Cargo-only.
13. Newly created files appear completely in ChangeSet/result.
14. Verification failures still preserve useful ChangeSet information where possible.

### Process Security

15. Repository commands do not inherit the complete application environment.
16. AI implementer receives only required credentials where practical.
17. Documentation clearly states environment filtering is not a sandbox.

### Runtime Limits

18. Implementer execution has a timeout.
19. Verification commands have timeouts.
20. stdout/stderr are bounded.
21. timeout output is preserved where possible.
22. repetitive in-memory logs/history are bounded.
23. important lifecycle events remain available.

### Task State

24. Duplicate/invalid approval transitions are rejected by the domain layer.
25. Finished events update live status correctly.

## Known Deferred Issue

Do NOT implement a workaround for the snapshot/SSE subscription race.

Confirm that this remains documented for the PostgreSQL event-store milestone.

The future persistence implementation should support:

```text
sequenced TaskEvents
event replay
restart recovery
Last-Event-ID
SSE reconnect
snapshot/subscription consistency
```

## Security Review

Explicitly state whether repository code still executes in the same host/container as Multiagent Chat.

If yes, clearly mark:

```text
NOT SAFE YET FOR ARBITRARY UNTRUSTED REPOSITORY EXECUTION
```

This is expected until the isolated-worker milestone.

## Verification

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Documentation

Check:

* `README.md`
* `PROGRESS_V2.md`

for contradictions with the implemented behavior.

Fix only clearly outdated statements.

## Final Report

Provide:

1. PASS/FAIL for all 25 requirements
2. unresolved issues
3. total tests and result
4. fmt result
5. clippy result
6. current security limitations
7. current production limitations
8. whether the project is ready for PostgreSQL persistence

Do not implement PostgreSQL.

Read AGENTS.md, CLAUDE.md, docs/WORKFLOW.md and docs/tasks/self/0003_pre-persistence-audit.md

Perform the audit described in docs/tasks/self/0003_pre-persistence-audit.md
Fix only defects required to make the existing milestone internally consistent.
Do not add new features.
Do not commit or push.