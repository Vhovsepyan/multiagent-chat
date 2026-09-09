# 0009 — Git Commit After Successful Milestone

## Goal

Optionally create one Git commit after each successfully implemented and verified milestone.

The commit history should prove that development happened incrementally.

Preserve all guarantees from tasks `0001–0008`.

---

## Requirements

### Git mode

Add a per-task/run Git behavior setting with at least:

```text
No commits
Commit after successful milestone
```

Default should preserve current safe behavior.

Do not add automatic push in this task.

---

### Commit timing

Create a commit only after:

```text
milestone worker completes
→ milestone verification passes
→ commit
```

Do not commit a failed milestone as successful.

---

### Commit scope

Each milestone commit should contain only changes belonging to that task workspace/repository state.

Do not:

* reset unrelated user changes;
* delete existing modifications;
* rewrite history;
* force-clean the repository.

If pre-existing uncommitted changes make safe commit isolation impossible, stop and surface the problem.

---

### Commit message

Use a clear deterministic message based on milestone id/title.

Example:

```text
feat(milestone-03): implement capacity-safe registration
```

Exact convention may follow existing repository standards.

---

### Store commit metadata

Record at least:

```text
milestone id
commit SHA
commit message
timestamp
```

Associate it with the milestone/task state.

---

### Audit / evidence

Add audit/evidence events for successful commit creation.

For example:

```text
milestone_commit_created
```

Include safe metadata:

```text
milestone id
commit SHA
commit message
```

Do not expose credentials or unsafe filesystem information.

---

### Failure behavior

If Git commit creation fails:

* record the failure;
* do not mark the milestone as fully finalized;
* do not silently continue as if a commit exists;
* preserve the working tree for inspection.

Do not automatically retry indefinitely.

---

### New Project repositories

If milestone commits are enabled for a New Project and the workspace is not yet a Git repository:

* initialize Git explicitly and safely;
* preserve the repository for later tasks;
* do not configure or push a remote.

Do not initialize Git inside unrelated existing repositories.

---

### Existing repositories

Before committing, inspect repository state.

Protect against:

* detached/unsafe repository state;
* unresolved merge conflicts;
* unrelated dirty changes;
* repository path mismatch.

Do not perform destructive recovery automatically.

---

### UI

Expose the Git mode in task creation/configuration.

Show commit information in milestone/task details when available.

Example:

```text
Milestone 3 — PASS
Commit: a1b2c3d
```

---

## Acceptance Criteria

* User can choose whether milestone commits are enabled.
* Successful verified milestones create one commit when enabled.
* Failed milestones create no success commit.
* Commit SHA/message are stored.
* Audit/evidence includes milestone commit creation.
* Existing dirty/unrelated changes are not destroyed.
* No automatic push happens.
* Existing task types continue to work.
* Existing milestone behavior remains correct.
* Relevant tests are added.
* `cargo fmt`, `cargo clippy`, frontend checks, and full tests pass.

---

## Required Tests

Cover at least:

1. commit created after successful verified milestone;
2. no commit when Git mode is disabled;
3. no commit for failed milestone;
4. commit metadata stored correctly;
5. New Project Git initialization;
6. existing repository dirty-state protection;
7. commit failure handling;
8. audit/evidence integration.

---

## Out of Scope

Do not implement:

* GitHub push;
* GitHub repository creation;
* force push;
* automatic pull/rebase;
* persistent New Project finalization;
* critic fix loop;
* Codex worker.

---

## Completion Report

Report:

* files changed;
* Git mode/configuration;
* commit lifecycle;
* safety checks;
* audit/evidence integration;
* tests and verification;
* remaining limitations.

Commit and push the completed implementation to `main`.
