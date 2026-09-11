# 0014 — Take-Home Assignment Mode

## Goal

Add a dedicated `Take-home Assignment` task type for evaluation projects such as the MadDevs assignment.

Reuse the existing New Project pipeline and features from tasks `0001–0013`.

Do not create a separate orchestration engine.

## Requirements

### Task type

Add:

```text
Take-home Assignment
```

alongside existing task types.

It should reuse New Project behavior where appropriate.

### Default workflow

Take-home mode should enable/default to:

* task-level proposer/critic/worker selection;
* specification generation and human approval;
* acceptance-criteria tracking;
* timestamped audit;
* evidence capture/export;
* milestone-based implementation;
* milestone verification;
* post-implementation critic/fix loop;
* milestone Git commits;
* persistent project output;
* automatic submission documentation.

Do not hide these settings. The user should still be able to see the selected configuration.

### Assignment input

Reuse the existing task title/description input.

The assignment description must become the source input for proposal/spec generation.

Do not build PDF ingestion or employer-specific parsing in this task.

### Persistent output

Take-home mode should default to persistent output rather than temporary-only output.

Require a valid safe destination using the existing persistence rules.

### Git mode

Default to:

```text
Commit after successful milestone
```

so the final repository demonstrates gradual development history.

Do not push automatically.

### Completion state

Expose a final completion checklist based on actual task state:

```text
Implementation complete
Verification complete
Acceptance criteria reviewed
Final critic review complete
Documentation generated
Evidence export available
Git history available
```

Checklist items must reflect real evidence/state and must not be hard-coded as successful.

### UI

Add Take-home Assignment to task creation.

When selected:

* show agent/model selectors;
* show persistent output configuration;
* show Git mode;
* show existing specification/approval workflow;
* show completion checklist in task details.

Keep the existing New Project, Feature, and Bug Fix flows unchanged.

### Audit / evidence

Record the selected task type and relevant take-home configuration in the existing audit/evidence system.

The final report should clearly identify the run as a Take-home Assignment.

### Safety

Do not weaken:

* path/workspace restrictions;
* execution limits;
* secret redaction;
* human approval gates;
* Git safety;
* persistence safety.

## Acceptance Criteria

* Take-home Assignment is selectable as a task type.
* It reuses the existing pipeline instead of duplicating orchestration.
* Evidence-first workflow features are enabled by default.
* Persistent output is the default.
* Milestone commits are the default.
* Agent selections remain configurable.
* Completion checklist reflects actual run state.
* Final evidence identifies the task type.
* Existing task types remain unchanged.
* Relevant tests are added.
* Required project checks pass.

## Required Tests

Cover at least:

1. Take-home task creation;
2. expected defaults;
3. persistent output requirement;
4. milestone Git mode default;
5. existing task types retain previous defaults;
6. completion checklist reflects real state;
7. evidence/final report identifies Take-home Assignment.

## Out of Scope

Do not implement:

* MadDevs-specific business logic;
* PDF assignment ingestion;
* automatic employer submission;
* Codex worker;
* GitHub publishing.

## Completion

Run targeted tests during development and final required verification before completion.

Commit and push to `main`.
