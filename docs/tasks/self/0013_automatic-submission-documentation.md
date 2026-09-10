# 0013 — Automatic Submission Documentation

## Goal

Generate accurate project/submission documentation from the actual task result, audit history, acceptance criteria, verification results, and agent configuration.

Preserve all guarantees from tasks `0001–0012`.

## Requirements

Generate or update:

```text
README.md
docs/ARCHITECTURE.md
docs/DEVELOPMENT_LOG.md
docs/DECISIONS.md
docs/AI_USAGE.md
docs/NEXT_STEPS.md
```

Do not overwrite valuable existing documentation blindly.

### README

Include only information supported by the implemented project:

* project purpose;
* architecture summary;
* prerequisites;
* local startup;
* configuration;
* database setup/migrations where applicable;
* backend/frontend startup;
* Docker/Compose startup if implemented;
* tests;
* current status;
* known limitations.

Do not claim unverified commands work.

### ARCHITECTURE.md

Describe the actual implemented architecture:

* components;
* storage;
* major APIs/flows;
* background/realtime behavior;
* important consistency/concurrency decisions.

### DEVELOPMENT_LOG.md

Generate from the timestamped audit/evidence history.

Use actual timestamps only.

### DECISIONS.md

Generate from recorded proposer/critic/spec decisions.

Do not invent rationale or alternatives that were not recorded.

### AI_USAGE.md

Record the actual:

* proposer provider/model;
* critic provider/model;
* worker tool/model;
* purpose of each role.

Use the frozen task configuration.

### NEXT_STEPS.md

Document:

* incomplete acceptance criteria;
* critic findings;
* known limitations;
* deferred work;
* reasonable future improvements.

Clearly separate required-but-incomplete work from optional future improvements.

### Status accuracy

Documentation must reflect actual task state.

If verification failed or criteria remain incomplete, say so.

Do not present the project as fully complete when evidence says otherwise.

### Security

Reuse existing redaction.

Never include credentials, API keys, tokens, secret environment values, or unsafe internal paths.

### Evidence integration

Generated documentation should be available in the final project and represented in evidence/final-report output where appropriate.

## Acceptance Criteria

* All required documentation files are generated or safely updated.
* Documentation is based on actual run/project evidence.
* README startup/test instructions are accurate.
* Development log uses real timestamps.
* AI usage reports actual models/tools.
* Acceptance-criteria failures/deferred items remain visible.
* Known limitations are honest.
* Existing documentation is not destroyed blindly.
* Secrets are not written into generated docs.
* Relevant tests are added.
* Required project checks pass.

## Required Tests

Cover at least:

1. documentation generation for successful task;
2. failed/incomplete task documented honestly;
3. correct frozen agent/model values;
4. acceptance-criteria failures appear in next steps/status;
5. existing documentation is preserved safely;
6. secret values are redacted;
7. development log uses recorded timestamps.

## Out of Scope

Do not implement:

* take-home assignment mode;
* Codex worker;
* GitHub publishing;
* cloud deployment.

## Completion

Run targeted tests while developing and final required verification once before completion.

Commit and push to `main`.
