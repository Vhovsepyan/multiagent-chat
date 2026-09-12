# 0022 — Specification Format Contract

## Goal

Make generated specifications follow a deterministic Markdown structure that Multiagent Chat can parse reliably.

Do not wait until worker execution to discover that the approved specification has an invalid `## Steps` section.

Preserve all guarantees from tasks `0001–0021`.

## Specification Template

The specification-generation prompt must include this exact structural example:

```markdown
# Specification

## Goal

Short description of what must be implemented.

## Requirements

- Requirement one.
- Requirement two.
- Requirement three.

## Acceptance Criteria

- AC-1: First observable result.
- AC-2: Second observable result.
- AC-3: Third observable result.

## Steps

1. Implement first milestone
2. Implement second milestone
3. Add or update tests
4. Run verification

## Verification

- Run relevant unit tests.
- Run relevant integration tests.
- Run the project's required final verification.
```

The actual content may differ, but the section structure and `## Steps` syntax must follow this contract.

## `## Steps` Contract

Each milestone must be exactly one top-level numbered list entry:

```markdown
## Steps

1. First milestone
2. Second milestone
3. Third milestone
```

Do NOT generate milestones using:

```markdown
### 1. First milestone
```

```markdown
### Step 1
```

```markdown
- First milestone
```

or arbitrary prose.

Do not use Markdown headings as milestone entries.

Each top-level numbered entry represents exactly one executable milestone.

Additional explanation may be placed under the numbered entry as indented content if needed, but the milestone title must remain on the numbered line.

Example:

```markdown
## Steps

1. Update KafkaBetPublisher
   - Add the required producer configuration.
   - Preserve existing publishing behavior.

2. Add publisher tests
   - Cover successful publishing.
   - Cover failure handling.
```

Nested bullets are descriptive content and must not become separate milestones.

## Prompt Contract

The specification-generating agent must be explicitly told:

* return Markdown only;
* follow the supplied template;
* include exactly one `## Steps` section;
* represent milestones only as top-level numbered list items;
* do not use `###` headings for individual steps;
* do not rename the required sections;
* do not wrap the complete specification in a Markdown code fence.

Include the structural example directly in the generation prompt.

## Pre-Approval Validation

Validate the generated specification immediately after generation and before presenting it for human approval.

Validation must confirm at least:

* `## Goal` exists;
* `## Requirements` exists;
* `## Acceptance Criteria` exists;
* exactly one `## Steps` section exists;
* `## Steps` contains at least one valid top-level numbered milestone;
* no top-level malformed milestone syntax exists inside `## Steps`;
* `## Verification` exists.

The same milestone parser/contract used during execution should be used for validation where practical.

Do not allow a specification that would later fail milestone planning to reach the approved state.

## Format Repair

If specification structure is invalid, perform a bounded format-repair attempt.

The repair prompt must:

* provide the required template again;
* identify the structural validation error;
* ask the agent to preserve the technical meaning;
* change formatting/structure only where possible.

Do not silently rewrite the specification with local string manipulation.

Limit repair attempts to a small fixed number.

If repair still fails, fail specification generation with a clear validation error before human approval.

## Approval

Only a specification that passes structural validation may enter:

```text
WaitingForApproval
```

The UI should therefore never offer approval for a specification that the execution pipeline cannot parse.

## Audit / Evidence

Record:

* initial specification generation;
* format validation result;
* format-repair attempt, if any;
* final validated specification.

Do not remove the original generation evidence.

## Acceptance Criteria

* specification prompt contains the required structural template;
* generated specifications use deterministic required sections;
* milestones use top-level numbered entries;
* `### 1. ...` milestone format is rejected before approval;
* malformed `## Steps` cannot reach execution;
* valid nested descriptions/code remain supported;
* bounded format repair can correct structural-only failures;
* technical content is not silently changed by local normalization;
* validated specification is the version shown for approval;
* evidence retains generation/repair history;
* existing approved-spec execution remains compatible.

## Required Tests

Cover at least:

1. valid specification template passes;
2. `### 1. Update KafkaBetPublisher` inside `## Steps` fails pre-approval validation;
3. normal `1. Update KafkaBetPublisher` passes;
4. nested descriptive bullets remain valid;
5. fenced code inside a milestone remains valid;
6. missing `## Steps` fails;
7. duplicate `## Steps` fails;
8. empty `## Steps` fails;
9. missing required section fails;
10. successful format repair becomes approvable;
11. failed bounded repair never reaches approval;
12. validated spec can be parsed by milestone planning without error.

## Out of Scope

Do not implement:

* retry/resume;
* arbitrary Markdown-to-spec normalization;
* automatic changes to technical requirements;
* new orchestration roles.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
