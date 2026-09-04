# Progress

## Current status
The repository-backed task-platform phase is implemented. The production web
model now has registered GitHub Projects, New Project / Feature / Bug Fix task
kinds, isolated disposable task workspaces, bounded repository inspection,
evidence-based technology profiles, task-specific prompts, stack-aware
verification, and reviewable task results. The original v2 state machine, SSE
streaming, editable approval gate, and Claude Code implementation stage remain.

The legacy CLI remains available behind `--cli`; only that compatibility path
uses optional `WORKSPACE_ROOT`. Production web forms and APIs do not accept
arbitrary server filesystem paths.

## Next steps
- Add durable Project/task/artifact persistence behind the new store boundaries.
- Add secure GitHub authentication and a durable publication/download flow as
  separate production tasks.
- Split task execution from the web/API process before cloud deployment.
- Re-package the distributable with its static frontend assets.

## Decisions made
- DP-15 (2026-09-04): A Project is repository identity and metadata, never a
  persistent workspace path. GitHub `owner/repository` is normalized at the
  domain boundary; provider-specific acquisition stays behind ProjectSource.
- DP-16 (2026-09-04): Every web task gets a UUID-named, provider-owned temporary
  workspace. Existing repositories are shallow-cloned at their configured
  branch; new projects are initialized only after approval. Cleanup is explicit
  and restricted to the managed root.
- DP-17 (2026-09-04): TaskKind and its validation matrix are domain data.
  Existing-project tasks require a registered Project; New Project requires a
  technology and output configuration. Invalid combinations fail before agent
  calls or workspace preparation.
- DP-18 (2026-09-04): Technology detection, workflow prompts, repository
  inspection, and verification are separate modules. Verification commands are
  selected from structured profiles and repository wrappers/scripts rather than
  generated freely by an agent.
- DP-19 (2026-09-04): Project/task storage stays in memory for this phase, but
  ProjectStore and WorkspaceProvider are explicit replacement boundaries for
  durable persistence and separate cloud task execution.
- DP-1..DP-6: (Retained from v1 CLI milestone).
- DP-7 (2026-08-21): Adopted a unified `Task` state machine (`Created` -> `Debating` -> `SpecReady` -> `WaitingForApproval` -> `Implementing` -> `Completed` / `Failed`) driven by background Tokio tasks communicating via `tokio::sync::broadcast`.
- DP-8 (2026-08-21): UI separation: `Title` (concise identifier) and `Description` (detailed context) split at input, concatenated cleanly for agent prompt ingestion. Implemented as `Task::topic()`, which falls back to the title alone when the description is blank.
- DP-7 AMENDED (2026-08-21, approved by the user): `SpecReady` renamed to
  `GeneratingSpec`, because drafting the spec is two API calls and without it the
  timeline would still read "Debating" while the spec is being written; and
  `SpecReady` would have fired microseconds before `WaitingForApproval` anyway.
  Added `Rejected` as a terminal state: declining at the gate is not a failure,
  and SPEC.md stays on disk to re-run later.
- DP-9 (2026-08-21): pipeline stages take an explicit `&Emitter` rather than
  reaching for a global channel. Chosen over a Sink trait and over a
  channel-only design because it is the most explicit and easiest to test — a
  stage can be handed `Emitter::detached()`. Cost: every stage signature changes
  in Phase 9. `Emitter::emit` records the event into the stored `Task` BEFORE
  broadcasting, so a browser that fetches a snapshot right after seeing an event
  can never find state that is behind.
- DP-10 (2026-08-21): the approve endpoint carries the edited spec —
  `POST /api/tasks/:id/approve { approve: bool, spec: Option<String> }`. Chosen
  over a separate PUT so editing and approving are one atomic action. Note this
  resolves a gap in plan_v2: section 1.3 offers an Edit option that section 4's
  `approve: bool` endpoint had nowhere to put.

- DP-11 (2026-08-21): the background pipeline parks at Gate 2 on a
  `tokio::sync::Notify` and re-checks the stored decision. Chosen over a oneshot
  channel because it tolerates a repeated approve. The missed-wakeup hazard is
  closed deliberately: the state is checked BEFORE parking, and `notify_one` is
  used rather than `notify_waiters` because only `notify_one` stores a permit —
  so an answer arriving before the pipeline parks is still delivered. Both
  directions have tests.
- DP-12 (2026-08-21): `cargo run` stays the v1 terminal pipeline; `--web` opts
  into the server. The default flips in Phase 10 when there is a page to serve,
  and CLAUDE.md gets updated at the same time. `--web` refuses to be combined
  with `--topic` or `--implement-only` rather than ignoring them silently.
- `web::router()` is separate from `web::serve()` so integration tests drive the
  full extractor/handler/serialisation path via `tower::ServiceExt::oneshot`,
  with no port to bind and no chance of two test runs colliding.
- axum 0.8 spells path params `{id}`, not `:id` as older versions did.

- Phase 9 changed `implementer.rs` from INHERITING stdout to PIPING it, so each
  line can be printed and published as `TaskEvent::Build`. Real tradeoff: Claude
  Code no longer sees a TTY, so it may drop colour and progress animations that
  it showed when run directly. Line content is otherwise identical. Both streams
  are read on their own tasks — reading them in sequence would deadlock as soon
  as the unread pipe filled.
- The stages both PRINT and EMIT. v1's terminal behaviour is untouched, so the
  CLI is unchanged and a `--web` run also shows the debate in the server console.
  The CLI passes `Emitter::detached()`, so nothing is published.

- DP-13 (2026-08-21): frontend assets are served from disk with
  `tower-http::ServeDir`, not embedded, so editing style.css needs only a browser
  refresh. Cost, accepted knowingly: the .exe is no longer self-contained, and
  the path is relative to the working directory — `STATIC_DIR` overrides it.
- DP-14 (2026-08-21): HTMX rather than vanilla JS. htmx and its SSE extension are
  VENDORED into `src/web/static/vendor/` rather than loaded from a CDN, so the
  tool still works offline. The predicted cost was real: HTMX swaps HTML but our
  SSE emits JSON, so `src/web/ui.rs` is a second rendering path serving `/ui/*`
  and `/task/{id}`. The `/api/*` JSON endpoints are untouched — the API contract
  was never bent to suit a widget.
- One SSE stream feeds four page regions by NAMING each event (`status`,
  `debate`, `spec`, `build`, `done`) and giving each div its own `sse-swap`.
- The task page renders `history` server-side before attaching the stream, which
  closes the snapshot-then-subscribe race noted in Phase 9. A page opened
  mid-debate shows everything that already happened.
- Everything the models write is HTML-escaped before rendering (`ui::esc`), with
  a test asserting a `<script>` in a proposal cannot execute.

## Open questions / problems
- Spec ambiguity surviving both gates (from v1) — keep Critic spec-checking prompt strict.

## Session log
- 2026-08-21 (cont. 3): Phase 10 done, v2 complete. `src/web/ui.rs` renders the
  UI; `src/web/static/` holds index.html, style.css and vendored htmx. Flipped
  `cargo run` to the web UI with `--cli` opting back (DP-12 as promised), and
  updated CLAUDE.md in the same commit. 106 tests. Proven by driving the actual
  browser endpoints: form POST returned HX-Redirect, the HTML stream delivered
  status/debate/spec/build/done events, approve came back 200, and the built
  `greet.py` greets by name correctly. The two throwaway projects used for those
  runs (sse-probe, ui-probe) were deleted afterwards; spec-scratch stays, since
  it holds the working `rnm` tool from v1.
- 2026-08-21 (cont. 2): Phase 9 done. SSE at GET /api/tasks/{id}/events via
  tokio-stream's BroadcastStream, filtered to one task, with a `lagged` event so
  a slow client learns it missed data instead of silently showing a debate with
  holes. Threaded the Emitter (DP-9) through debate/spec/implementer and switched
  the implementer to piped stdout. Proven end to end with curl -N on port 3111:
  real critique with verdict+reason extracted, spec, HTTP approve, 19 live build
  chunks, finished=completed, and a working print_date.py in the sse-probe
  project. 91 tests.
- 2026-08-21 (cont.): Phase 8 done. axum 0.8.9 + tower-http 0.7. `src/web/`
  with mod/handlers/pipeline/tests: GET /api/health, GET /api/projects,
  POST /api/tasks (creates the project, spawns the pipeline, 201), GET
  /api/tasks/{id}, POST /api/tasks/{id}/approve (DP-10 edited spec, 409 if not
  at the gate). ApiError renders JSON, not bare status codes. 15 integration
  tests including path-traversal rejection and both gate-wakeup directions;
  88 total. Also added PORT to config and .env.example. Verified live on
  port 3111 with curl: health, projects, a 404 and a 400 all correct.
- 2026-08-21: Phase 7 done. `src/task.rs` with the state machine, tagged
  `TaskEvent` enum (serialises with a `type` discriminator for the browser),
  `Task` carrying full `history` so a late-joining tab replays the debate,
  `Emitter` (DP-9) and `TaskManager` (Arc + RwLock + broadcast, cheaply cloned
  for axum handlers). 17 tests, 76 total. Also fixed CLAUDE.md, which still
  pointed at plan.md / PROGRESS.md and documented `--cli` and the web server as
  if they already worked.
- 2026-08-20: Completed v1 CLI pipeline, validated live implementation of `rnm` CLI tool, 56 unit tests passing.
- 2026-08-21: Finalized v2 Web UI specification and architecture plan.
