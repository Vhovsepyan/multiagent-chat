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
