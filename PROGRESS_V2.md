# Progress

## Current status
Phases 7, 8 and 9 are DONE and proven live end to end over HTTP. A task created
with curl streamed its debate, spec, HTTP approval and 19 live build chunks over
SSE, and Claude Code produced a working script in the target project. 91 tests.

## Next steps
- Phase 10: the browser UI. Serve static assets from `src/web/static/`, build the
  four screens, and flip `cargo run` to default to the web server (DP-12) with
  CLAUDE.md updated in the same commit.
- Known limitation to solve in the UI: SSE streams from the moment you connect,
  so the client MUST fetch `GET /api/tasks/{id}` first and render `history`,
  then subscribe. Proven in the live run — attaching curl a second after
  creating the task missed `round_started` and `proposal`, both of which were
  present in the snapshot. There is still a tiny race between the two calls;
  closing it properly would need sequence numbers on events.

## Decisions made
- DP-1..DP-6: (Retained from v1 CLI milestone).
- DP-7 (2026-08-21): Adopted a unified `Task` state machine (`Created` -> `Debating` -> `SpecReady` -> `WaitingForApproval` -> `Implementing` -> `Completed` / `Failed`) driven by background Tokio tasks communicating via `tokio::sync::broadcast`.
- DP-8 (2026-08-21): UI separation: `Title` (concise identifier) and `Description` (detailed context) split at input, concatenated cleanly for agent prompt ingestion. Implemented as `Task::topic()`, which falls back to the title alone when the description is blank.
- DP-7 AMENDED (2026-08-21, approved by Vahe): `SpecReady` renamed to
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

## Open questions / problems
- Spec ambiguity surviving both gates (from v1) — keep Critic spec-checking prompt strict.

## Session log
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