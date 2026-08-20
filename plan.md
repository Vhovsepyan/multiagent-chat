# Plan: Multiagent Chat Orchestrator (Rust, terminal v1)

This file is the master plan for Claude Code. Read it fully before starting any work.

---

## 1. What we are building

A terminal application in Rust called `multiagent-chat`. It runs a debate between two AI models and then hands the result to Claude Code for implementation.

**The pipeline:**

1. Vahe types a topic (e.g. "I need credit applications").
2. **Proposer** (Gemini, via Google AI API) writes a solution proposal.
3. **Critic** (Claude Sonnet 4.6, via Anthropic API) reviews it and ends every review with a verdict line: `VERDICT: APPROVED` or `VERDICT: NEEDS_WORK`.
4. If `NEEDS_WORK` → the critique goes back to the Proposer, which revises. Loop continues.
5. **Gate 1 (automatic):** loop ends on `VERDICT: APPROVED`, or after `MAX_ROUNDS` (default 5). If max rounds is reached without approval, show the latest state with a warning.
6. A clean **`SPEC.md`** is generated from the aligned discussion and written into the target repo.
7. **Gate 2 (human):** the spec is shown in the terminal. Vahe types `y` to continue or `n` to abort.
8. **Implementer:** the app launches Claude Code (Opus) in headless mode inside the target repo to implement `SPEC.md`.

**v1 constraints:**
- Terminal only. No web UI, no database. (Web UI with axum + SSE is v2 — do not build it now.)
- The whole debate is printed live with colors: Proposer in blue, Critic in yellow/orange, system messages in gray.

---

## 2. Very important: how to work with Vahe

Vahe's main goal is to **learn Rust** with this project. Follow these rules strictly:

- **Boilerplate:** write it fully, ready to paste/apply (Cargo.toml, module skeletons, structs, API request/response types, terminal colors, error plumbing).
- **Logical / important parts:** do NOT write them immediately. Stop and ask Vahe first, always offering **2–3 concrete options** to choose from (never an open question, never just a hint). Discuss his choice, then implement together.
- Explain Rust concepts in **simple English** (intermediate level). When a new concept appears (ownership, `Result`, traits, async), give a 2–3 sentence explanation the first time.
- Commit messages: **single line, plain text** (multi-line blocks break his paste).

**Pre-marked decision points** (ask Vahe when you reach them; you may find more):
- DP-1: How to store the debate conversation state (one shared transcript vs per-model histories).
- DP-2: How to detect the verdict robustly (exact line match vs parse last line vs ask model for JSON).
- DP-3: Who writes the final spec (Critic alone / Proposer writes + Critic checks / one extra "summarizer" call).
- DP-4: Retry & error strategy for API calls (fail fast vs N retries with backoff vs ask user).
- DP-5: How to launch Claude Code (exact flags, permission mode, working directory handling).

---

## 3. Tech decisions (already made — do not reopen)

- Language: **Rust**, latest stable edition. `cargo new multiagent-chat`.
- Async runtime: `tokio`. HTTP: `reqwest` (json feature). Serialization: `serde`, `serde_json`.
- Terminal colors: `owo-colors` (or `colored` — pick one, say why in one sentence).
- Config: `.env` file loaded with `dotenvy`. **`.env` goes into `.gitignore` — never commit API keys.**
- Models:
  - Proposer: Gemini (model name in config; verify the current model string in Google's docs before hardcoding a default).
  - Critic: `claude-sonnet-4-6` via Anthropic Messages API (`https://api.anthropic.com/v1/messages`, `anthropic-version` header required).
  - Implementer: Opus via Claude Code CLI in print/headless mode (`claude -p ...`; check `claude --help` for the current flags at Phase 5).
- Env vars: `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `TARGET_REPO_PATH`, `MAX_ROUNDS` (default 5), `GEMINI_MODEL`, `CRITIC_MODEL`.

**Suggested module layout** (adjust if needed, but explain changes):

```
src/
  main.rs        // entry: read topic, run pipeline
  config.rs      // load env vars into a Config struct
  api/
    gemini.rs    // Gemini client
    claude.rs    // Anthropic client
  debate.rs      // the loop, rounds, verdict detection (Gate 1)
  spec.rs        // build SPEC.md, write it to target repo
  approve.rs     // Gate 2: show spec, read y/n from stdin
  implementer.rs // spawn Claude Code process, stream its output
  ui.rs          // colored printing helpers
```

---

## 4. Phases

Work phase by phase. Finish, test, and commit each phase before moving on. One phase ≈ one or a few sessions.

**Phase 0 — Setup**
- `cargo new`, add dependencies, create `.gitignore` (include `.env`, `target/`), `.env.example` with all variable names.
- Create `CLAUDE.md` and `PROGRESS.md` from the templates in sections 6 and 7 of this file.
- First commit.
- ✅ Done when: `cargo run` prints a hello line and config loads from `.env`.

**Phase 1 — Claude API client**
- `config.rs` + `api/claude.rs`: send a message, get text back. Include request/response serde types.
- Test with a real call: ask the model to reply "pong".
- ✅ Done when: a critic-style call works end to end.

**Phase 2 — Gemini API client**
- `api/gemini.rs`, same shape as Phase 1. (Endpoint style: `.../v1beta/models/{model}:generateContent` — verify against current Google docs.)
- ✅ Done when: a proposer-style call works end to end.

**Phase 3 — The debate loop (core of the project)**
- `debate.rs` + `ui.rs`. This phase contains DP-1 and DP-2 — ask Vahe before coding those parts.
- Live colored output of every message as it arrives.
- ✅ Done when: a full debate runs on a test topic and stops on APPROVED or max rounds.

**Phase 4 — Spec + human gate**
- `spec.rs` (contains DP-3) + `approve.rs`.
- Spec structure: Problem, Agreed solution, Architecture, Steps, Out of scope, Open risks.
- ✅ Done when: SPEC.md lands in the target repo and `y`/`n` works.

**Phase 5 — Implementer**
- `implementer.rs` (contains DP-5): spawn Claude Code in `TARGET_REPO_PATH`, stream its stdout live, report exit status.
- ✅ Done when: a small toy spec gets implemented in a scratch repo.

**Phase 6 — Polish**
- DP-4 (retries), nice error messages, `--topic` CLI argument as alternative to interactive input, README.

---

## 5. Working flow (two laptops)

Vahe works on an **office laptop** and a **personal laptop**. Continuity lives in the repo, not in local chat history.

**Every session, in order:**
1. `git pull`.
2. Claude reads `CLAUDE.md`, `PROGRESS.md`, and this `plan.md`.
3. Claude states in 2–3 sentences: where we are, what today's step is. Vahe confirms or redirects.
4. Work.
5. When Vahe says **"wrap up"** (or the session is ending): Claude updates `PROGRESS.md` (see rules in section 7), suggests a single-line commit message.
6. Vahe commits and pushes.

If `PROGRESS.md` and reality disagree (e.g. code is ahead of the notes), trust the code, then fix `PROGRESS.md`.

---

## 6. CLAUDE.md — create this file in the repo root (Phase 0)

```markdown
# multiagent-chat

Terminal Rust app: Gemini proposes a solution, Claude Sonnet critiques it,
they iterate until APPROVED (max rounds limit), a clean SPEC.md is produced,
Vahe approves it, then Claude Code (Opus) implements the spec in a target repo.

Master plan: see plan.md. Session state: see PROGRESS.md.

## Rules
- Read PROGRESS.md at the start of every session, before anything else.
- When Vahe says "wrap up", update PROGRESS.md, then suggest a single-line
  commit message as plain text (never multi-line).
- Vahe is learning Rust. Boilerplate: write it ready to use. Logical/important
  parts: STOP and ask Vahe first with 2-3 concrete options, then discuss.
- Explain new Rust concepts briefly, in simple English.
- Never touch .env. Never print API keys.
- Rust stable only. Run `cargo fmt` and `cargo clippy` before finishing a phase.

## Commands
- Run: `cargo run`
- Test: `cargo test`
- Lint: `cargo clippy -- -D warnings`
```

---

## 7. PROGRESS.md — create this file in the repo root (Phase 0)

Initial content:

```markdown
# Progress

## Current status
Phase 0 — project setup. Nothing implemented yet.

## Next steps
- Finish Phase 0 checklist from plan.md.

## Decisions made
- (empty — record every DP-x decision here with one line of reasoning)

## Open questions / problems
- (empty)

## Session log
- (newest first; one short block per session)
```

**Update rules (also enforced by CLAUDE.md):**
- Keep "Current status" to 1–3 sentences — it is the first thing the next session reads.
- Every decision point (DP-1…DP-5 and new ones) gets a permanent line under "Decisions made". These never get deleted.
- "Session log" entries: date, what was done, what broke, what is next. 3–6 lines maximum.
- Old session log entries older than ~10 sessions can be compressed into one summary line.

---

## 8. Definition of done (v1)

Vahe runs `cargo run`, types a real topic, watches a colored Gemini↔Claude debate live, gets a SPEC.md, approves it with `y`, and Claude Code implements it in the target repo — all without editing any source code manually.
