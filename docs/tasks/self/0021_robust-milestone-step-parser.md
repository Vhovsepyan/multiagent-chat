# 0021 — Robust Milestone Step Parser

## Goal

Fix milestone planning so valid Markdown content inside `## Steps` does not get interpreted as additional milestone entries.

Preserve all guarantees from tasks `0001–0020`.

## Problem

The current parser trims every non-empty line inside `## Steps` and expects each line to be a bullet or numbered milestone.

This incorrectly rejects valid nested content such as:

````markdown
## Steps

1. Add CI workflow

   ```yaml
   jobs:
     build:
       steps:
         - uses: actions/checkout@v4
           with:
             fetch-depth: 0
````

2. Add tests

````

`with:` must not be parsed as a milestone.

## Requirements

Parse milestones only from top-level list entries in the `## Steps` section.

Support existing forms:

```text
1. First milestone
2. Second milestone
````

and:

```text
- First milestone
- Second milestone
```

Ignore content belonging to a milestone, including:

* fenced code blocks;
* indented continuation text;
* nested lists;
* nested YAML/JSON/code;
* explanatory paragraphs associated with a step.

Do not create milestones from nested list items.

Stop parsing when the next level-2 Markdown section begins.

Keep deterministic milestone ordering.

A `## Steps` section with no valid top-level milestone entries must still fail.

Do not silently convert arbitrary prose into a milestone.

## Required Tests

Cover at least:

1. existing numbered-step parsing still works;
2. existing bullet-step parsing works;
3. fenced YAML containing `with:` is ignored;
4. fenced JSON/code is ignored;
5. indented continuation text is ignored;
6. nested bullet/list items are not separate milestones;
7. parsing continues correctly after a code block;
8. next `##` section ends milestone parsing;
9. empty/malformed `## Steps` still fails;
10. milestone order/titles remain unchanged.

## Out of Scope

Do not implement:

* retry/resume;
* new milestone semantics;
* specification regeneration;
* automatic modification of approved specifications;
* Markdown rendering changes.

## Completion

Run targeted tests and final required verification.

Commit and push to `main`.
