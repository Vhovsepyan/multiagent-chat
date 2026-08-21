# Plan: Multiagent Chat Orchestrator (v2 — Web UI)

This file is the master plan for Claude Code. Read it fully before starting any work.

---

## 1. What We Are Building in v2

A web-based interface for `multiagent-chat` powered by `axum` and Server-Sent Events (SSE).

**The Workflow:**
1. **Create Screen (`/`):** User inputs `Title`, `Description`, and selects/creates a `Project` under `WORKSPACE_ROOT`.
2. **Mission Control (`/task/:id`):**
    - Displays live pipeline timeline: `Debate` -> `Spec` -> `Approval` -> `Implementation`.
    - Streams the Gemini (Proposer) and Claude (Critic) debate live in real-time.
    - Highlights `REASON` and `VERDICT` prominently.
3. **Spec Gate Screen (`/task/:id` state transition):**
    - Displays generated `SPEC.md` with options: `Approve & Build`, `Edit`, or `Reject`.
4. **Implementation Terminal:**
    - Streams Claude Code headless execution (`stdout`/`stderr`) live in a terminal card.
5. **Completion Summary:**
    - Final status report with links to the workspace project.

---

## 2. Working Rules & Rust Mentorship
- **Vahe is learning Rust:** Boilerplate is written ready-to-use. For core architectural decisions, STOP and provide 2–3 concrete options.
- **Explain concepts simply:** 2–3 sentence explanations when introducing web/async patterns (e.g., `axum::extract::State`, `broadcast::channel`, SSE streams).
- **Single-line commit messages:** Always plain text.

---

## 3. Tech Stack & Architecture (v2)

- **Backend:** `axum`, `tokio` (broadcast channels), `tower-http` (cors, trace, static files).
- **Event Streaming:** Server-Sent Events (`axum::response::sse`).
- **State Store:** Thread-safe in-memory task registry (`Arc<RwLock<HashMap<Uuid, TaskState>>>`).
- **Frontend:** Lightweight static frontend (HTML5, Tailwind/modern CSS, Vanilla JS or HTMX for SSE consumption) served directly by `axum` via embedded assets or a static folder.
- **Port:** Default `http://127.0.0.1:3000` (configurable via `PORT` in `.env`).

---

## 4. Phase Breakdown for v2

**Phase 7 — Domain Model & Task State Machine**
- Define `Task`, `TaskId` (`Uuid`), `TaskStatus`, and `TaskEvent` enums.
- Create `TaskManager` wrapped in `Arc<RwLock<...>>` with a broadcast channel for SSE.
- ✅ Done when: Unit tests prove state transitions from `Created` to `Completed`.

**Phase 8 — Axum Server & REST Endpoints**
- Set up `axum` router in `src/web/`.
- Endpoints:
    - `GET /api/projects`: List directories inside `WORKSPACE_ROOT`.
    - `POST /api/tasks`: Create task & launch background debate task.
    - `GET /api/tasks/:id`: Fetch current snapshot.
    - `POST /api/tasks/:id/approve`: Human Gate 2 trigger (`approve: bool`).
- ✅ Done when: Endpoints tested with integration tests (`axum::test` / `reqwest`).

**Phase 9 — Real-Time SSE Stream**
- Implement `GET /api/tasks/:id/events` yielding SSE messages for all `TaskEvent` updates.
- Wire existing `debate.rs`, `spec.rs`, and `implementer.rs` to publish events into the channel instead of only printing to terminal stdout.
- ✅ Done when: `curl -N http://localhost:3000/api/tasks/:id/events` streams real debate turns and implementation output chunks.

**Phase 10 — Frontend UI Integration**
- Serve static SPA assets from `src/web/static/`.
- Screen 1: Task creation form with dynamic project selector.
- Screen 2: Real-time debate transcript with status sidecar.
- Screen 3: Spec Markdown viewer with "Approve & Build" action.
- Screen 4: Live terminal stream for Claude Code execution.
- ✅ Done when: A full end-to-end task can be created, approved, and built entirely from the browser.