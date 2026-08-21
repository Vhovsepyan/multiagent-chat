# Progress

## Current status
Phases 7 and 8 are DONE. `src/task.rs` holds the domain model; `src/web/` holds
an axum 0.8 server with the four REST endpoints plus a health check, and a
background pipeline that runs the real v1 stages and parks at Gate 2 for an HTTP
approval. 88 tests. Verified live with curl on a real port, not just in-process.

## Next steps
- Phase 9: SSE endpoint `GET /api/tasks/{id}/events`, and thread the `Emitter`
  (DP-9) through `debate.rs` / `spec.rs` / `implementer.rs` so the turn-by-turn
  events actually flow. Phase 8 only emits coarse status transitions.
- Phase 9 also needs `implementer.rs` changed from INHERITING stdout to piping
  it, so build output can become `TaskEvent::Build` chunks. v1 inherits the
  terminal deliberately (DP-5), so that is a real change to a proven module.

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

## Open questions / problems
- Spec ambiguity surviving both gates (from v1) — keep Critic spec-checking prompt strict.

## Session log
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