# 0016 — Explicit GitHub Publishing

## Goal

Allow a completed project to be published to GitHub only after an explicit human action.

Publishing must never happen automatically as part of worker execution.

Preserve all guarantees from tasks `0001–0015`.

## Requirements

### Publish action

Add an explicit action such as:

```text
Publish to GitHub
```

It should be available only when a persistent project exists.

The worker itself must never invoke publish automatically.

### Preconditions

Before allowing publish, validate:

* persistent project exists;
* project is a valid Git repository;
* branch is valid and not detached;
* no merge/rebase/conflict is in progress;
* verification/final task state is visible;
* target remote/repository is known;
* authentication is available through existing local Git/GitHub tooling.

Do not collect GitHub passwords or tokens in task prompts.

### Final uncommitted files

Milestone commits may finish before final generated documentation.

Before publishing, inspect the working tree.

If the only uncommitted changes are known/generated final submission artifacts, safely create one final commit, for example:

```text
docs: finalize submission artifacts
```

If unrelated or ambiguous user changes exist, block publishing and require review.

Do not silently commit arbitrary files.

### Existing remote

Support publishing to an existing configured GitHub remote.

Before push, show:

* remote URL/repository;
* branch;
* HEAD commit;
* working-tree state;
* verification summary.

Require explicit confirmation.

### Repository creation

If no remote exists, optionally support creating a GitHub repository using an already authenticated supported tool such as `gh`.

Repository creation must also require explicit confirmation.

Do not implement OAuth or token collection.

### Push safety

Never automatically:

* force-push;
* rewrite history;
* delete remote branches;
* reset/clean;
* pull/rebase;
* resolve conflicts;
* overwrite a mismatched remote.

If the remote cannot be fast-forwarded safely, stop and report the problem.

### Result

After successful publish, store/display:

* repository URL;
* branch;
* published commit SHA;
* publish timestamp.

### Audit / evidence

Record:

```text
github_publish_started
github_publish_completed
github_publish_failed
```

Include safe metadata such as:

* repository;
* branch;
* commit SHA.

Never record credentials.

Evidence/final report should include successful publication information where applicable.

### UI

Provide a clear human confirmation step.

Example:

```text
Repository: github.com/example/project
Branch: main
Commit: abc123
Verification: PASS

[Cancel] [Publish to GitHub]
```

Show publishing errors without exposing credentials.

## Acceptance Criteria

* Nothing is pushed automatically.
* Publishing requires explicit human confirmation.
* Existing GitHub remote can be published safely.
* Optional repository creation uses existing authenticated tooling.
* Final generated submission docs are not accidentally omitted.
* Unrelated dirty changes block publishing.
* Force push/history rewriting is never automatic.
* Failed publish leaves the local repository intact.
* Successful publish stores repository/branch/SHA metadata.
* Audit/evidence records publication safely.
* Existing task workflows remain unchanged.
* Relevant tests are added.
* Required project checks pass.

## Required Tests

Cover at least:

1. publish requires explicit action;
2. successful existing-remote publish;
3. dirty generated-doc-only state can be finalized safely;
4. unrelated dirty files block publishing;
5. detached/conflicted repository blocks publishing;
6. non-fast-forward/unsafe remote state fails safely;
7. failed publish leaves local repo unchanged;
8. successful audit/evidence metadata;
9. credentials never appear in logs/evidence.

## Out of Scope

Do not implement:

* automatic employer submission;
* GitHub OAuth flow;
* GitHub App installation;
* force push;
* CI/CD deployment;
* cloud deployment.

## Completion

Run targeted tests during development and final required verification before completion.

Commit and push the Multiagent Chat implementation to `main`.
