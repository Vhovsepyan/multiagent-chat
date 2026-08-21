# multiagent-chat

Multiagent chat & coding orchestrator in Rust: Gemini proposes, Claude Sonnet critiques,
they iterate until APPROVED, generate SPEC.md, Vahe approves via Web UI, and Claude Code
implements the spec in the target repository.

Master plan: see plan_v2.md (v1, shipped: plan.md).
Session state: see PROGRESS_V2.md (v1 history: PROGRESS.md).

## Rules
- Read PROGRESS_V2.md at the start of every session, before anything else.
- When Vahe says "wrap up", update PROGRESS_V2.md, then suggest a single-line
  commit message as plain text (never multi-line).
- Vahe is learning Rust. Boilerplate: write it ready to use. Logical/important
  parts: STOP and ask Vahe first with 2-3 concrete options, then discuss.
- Explain new Rust concepts briefly, in simple English.
- Never touch .env. Never print API keys.
- Rust stable only. Run `cargo fmt` and `cargo clippy` before finishing a phase.

## Commands
- Run Web UI: `cargo run` (http://127.0.0.1:3000, PORT in .env)
- Run CLI mode: `cargo run -- --cli` (also implied by --topic / --implement-only)
- Test: `cargo test`
- Lint: `cargo clippy -- -D warnings`

Frontend assets are served from `src/web/static/` at runtime (DP-13), so the
server must be started from the repo root, or STATIC_DIR must point at them.
