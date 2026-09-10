# 0010 — Persistent New Project Output

## Goal

Allow a successful **New Project** task to keep the generated project in a persistent destination instead of losing it when the temporary workspace is cleaned up.

Preserve all guarantees from tasks `0001–0009`.

---

## Requirements

### Output mode

Add a per-task New Project output option with at least:

```text
Temporary review result
Persistent local project
```

Default should preserve the current safe behavior.

---

### Persistent destination

For `Persistent local project`:

* require a valid allowed destination path;
* validate it using the existing workspace/path-security rules;
* do not allow path traversal;
* do not follow unsafe symlinks;
* do not silently overwrite an existing non-empty directory.

If the destination already exists and cannot be used safely, fail clearly.

---

### Finalization flow

The generated project may still be built in the isolated task workspace.

After successful implementation and verification:

```text
temporary task workspace
→ final verification
→ persist project safely
→ keep final project
→ normal temporary cleanup
```

The persistent project must survive task cleanup.

---

### Git history

If milestone Git commits are enabled:

* preserve the repository and commit history;
* preserve commit SHAs;
* do not squash or recreate commits;
* do not configure or push a remote.

The persistent result should remain a valid Git repository.

---

### Safe persistence

Avoid partially replacing a destination.

Prefer a safe finalization strategy such as:

```text
prepare destination/staging
→ copy/materialize complete project
→ verify success
→ finalize
```

Do not delete unrelated user files if persistence fails.

---

### Failure behavior

If implementation or verification fails:

* do not present the persistent destination as successfully completed;
* preserve task evidence;
* keep existing destination content unchanged;
* do not destructively replace an existing project.

If final persistence itself fails, mark the task accordingly and preserve evidence for debugging.

---

### Result metadata

Store safe persistent-output metadata on the task/result:

```text
output mode
final destination
persistence status
git repository status where applicable
```

Do not expose unsafe internal temporary paths unnecessarily.

---

### Audit / evidence

Record persistent-output lifecycle events, for example:

```text
project_persistence_started
project_persisted
project_persistence_failed
```

Include only safe metadata.

Evidence export should reflect the final persistent-output result.

---

### UI

For New Project tasks:

* allow choosing output mode;
* allow choosing/entering the persistent destination using the existing safe path UX;
* show final persistent path after success;
* show persistence failure clearly.

Do not expose this option for task types where it does not apply.

---

## Acceptance Criteria

* New Project can still use temporary review mode.
* New Project can produce a persistent local project.
* Persistent output survives temporary workspace cleanup.
* Existing non-empty destinations are not silently overwritten.
* Path traversal and unsafe paths are rejected.
* Git history survives when milestone commits are enabled.
* Failed runs do not masquerade as successful persistence.
* Persistence failures are audited.
* Existing task types remain unaffected.
* Relevant tests are added.
* `cargo fmt`, `cargo clippy`, frontend checks, and full tests pass.

---

## Required Tests

Cover at least:

1. successful persistent New Project;
2. temporary mode remains unchanged;
3. persistent output survives cleanup;
4. existing non-empty destination is rejected;
5. unsafe destination/path traversal is rejected;
6. Git history survives persistence;
7. persistence failure does not destroy an existing destination;
8. audit/evidence records persistence result.

---

## Out of Scope

Do not implement:

* GitHub push;
* GitHub repository creation;
* automatic remote configuration;
* cloud deployment;
* critic fix loop;
* Codex worker;
* take-home assignment mode.

---

## Completion Report

Report:

* files changed;
* output-mode design;
* persistence/finalization approach;
* path-safety behavior;
* Git-history preservation;
* audit/evidence changes;
* tests and verification;
* remaining limitations.

Commit and push the completed implementation to `main`.
