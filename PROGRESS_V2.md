# Progress

## Current status
v1 is fully completed and proven live. Phase 7 is DONE: `src/task.rs` holds the
domain model (`Task`, `TaskStatus`, `TaskEvent`, `Emitter`, `TaskManager`) with
17 tests, including a full Created -> Completed walk. Nothing is wired to the
pipeline yet — `debate.rs` / `spec.rs` / `implementer.rs` still print directly.

## Next steps
- Phase 8: axum router in `src/web/`, the four REST endpoints, `--cli` flag so
  the terminal pipeline stays reachable once `cargo run` serves the web UI.
- Phase 9 will need `implementer.rs` changed from inheriting stdout to piping it,
  so build output can become `TaskEvent::Build` chunks. v1 inherits the terminal
  deliberately (DP-5), so that is a real change, not a tweak.

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

## Open questions / problems
- Spec ambiguity surviving both gates (from v1) — keep Critic spec-checking prompt strict.

## Session log
- 2026-08-21: Phase 7 done. `src/task.rs` with the state machine, tagged
  `TaskEvent` enum (serialises with a `type` discriminator for the browser),
  `Task` carrying full `history` so a late-joining tab replays the debate,
  `Emitter` (DP-9) and `TaskManager` (Arc + RwLock + broadcast, cheaply cloned
  for axum handlers). 17 tests, 76 total. Also fixed CLAUDE.md, which still
  pointed at plan.md / PROGRESS.md and documented `--cli` and the web server as
  if they already worked.
- 2026-08-20: Completed v1 CLI pipeline, validated live implementation of `rnm` CLI tool, 56 unit tests passing.
- 2026-08-21: Finalized v2 Web UI specification and architecture plan.