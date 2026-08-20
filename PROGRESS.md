# Progress

## Current status
Phases 0, 1 and 2 are written. Both API clients exist (`src/api/claude.rs` =
Critic, `src/api/gemini.rs` = Proposer) with full request/response serde types
and 15 passing unit tests; `cargo fmt` and `cargo clippy -- -D warnings` clean.
Next is Phase 3, the debate loop, which starts with DP-1 and DP-2.

## Next steps
- Vahe must update his `.env`: replace `TARGET_REPO_PATH` with
  `WORKSPACE_ROOT=C:/Users/vaheh/RustroverProjects` (forward slashes). Nothing
  runs end to end until that is done.
- Phase 3: `debate.rs` — ask DP-1 (conversation state) and DP-2 (verdict
  detection) before writing the loop.

## Decisions made
- Terminal color crate: `owo-colors` over `colored` — it adds no allocation and
  works as `.blue()` on anything printable, so color is free at runtime.
- `Config` has a hand-written `Debug` that redacts both API keys, so no `{:?}`
  print or log can ever leak a key.
- Config validates eagerly at startup: missing/empty required vars,
  `TARGET_REPO_PATH` that is not a real directory, and non-numeric or zero
  `MAX_ROUNDS` all fail with a readable message before any API call.
- Default `GEMINI_MODEL` is `gemini-3.7-flash` (current stable, cheap while
  iterating). `gemini-2.5-pro` is the stable pro option; flagship
  `gemini-3.1-pro-preview` is preview-only, so not a default. Revisit once we
  see how good the proposals actually are.
- Rust edition 2024, toolchain 1.97.1.
- DP-6 (new, decided 2026-08-20): the target repo is *per-run input*, not
  configuration. plan.md had `TARGET_REPO_PATH` as a static env var, but the
  topic changes every run (credit applications today, a messenger tomorrow) and
  the repo may not exist yet. So `.env` now holds `WORKSPACE_ROOT` (the folder
  all projects live in) and `target.rs` asks for a project name after the topic,
  suggesting a slug derived from it. If the folder is missing, the app shows the
  resolved path and asks y/n before `mkdir` + `git init`. Project names are
  rejected if they contain a slash or `..`, so a typo cannot escape the
  workspace root.
- SPEC.md location: the root of whichever repo that run resolves to
  (`<workspace>/<project>/SPEC.md`), overwritten each run.
- `Message`/`Role` live in `src/api/mod.rs`, shared by both clients, because the
  two APIs disagree on names: Anthropic sends `{role, content}` with the model
  turn called "assistant", Google sends `{role, parts:[{text}]}` with it called
  "model". Each client converts the shared type to its own wire structs, so the
  debate loop never has to know which model it is talking to.
- Live API tests are `#[ignore]`d on purpose (Vahe's call, 2026-08-20): they
  cost tokens, so they only run with `cargo test -- --ignored`. Everything else
  is tested against captured JSON, including a real 401 body from the Anthropic
  API.
- Both clients cap output at 16k tokens and time out after 120s.
- DP-1..DP-5 from plan.md are all still open.

## Open questions / problems
- Windows toolchain workaround (personal laptop, 2026-08-20): security software
  deletes `wasm-component-ld.exe` the instant it is written, so every
  `rustup toolchain install` rolls back and leaves no toolchain. Confirmed by
  hand: tar exits 0, the neighbouring `.pdb` survives, only that one `.exe`
  vanishes. Every other binary (rustc, cargo, rustdoc, rust-lld) is fine.
  Worked around by extracting the cached component tarballs from
  `~/.rustup/downloads` into `C:\Users\vaheh\rust-1.97.1` and registering it
  with `rustup toolchain link stable-manual <dir>` + `rustup default
  stable-manual`. The missing binary is only the wasm32 linker, which this
  project never uses. Proper fix: an antivirus allowlist entry for
  `C:\Users\vaheh\.rustup` (needs admin). The office laptop may not have this
  problem — try a plain `rustup default stable` there first.

## Session log
- 2026-08-20 (cont. 2): Phases 1 and 2 written. `api/claude.rs` (Messages API,
  x-api-key + anthropic-version) and `api/gemini.rs`
  (v1beta/models/{model}:generateContent, x-goog-api-key header rather than
  ?key= so the key stays out of logs). Verified the Anthropic error path for
  real against a deliberately invalid key: 401 parses into a clean message with
  no key leaked. The live "pong" tests are written but not run — Vahe asked to
  skip them to save tokens. Next: DP-1 and DP-2, then the debate loop.
- 2026-08-20 (cont.): DP-6 decided and implemented — `src/target.rs` with slug
  derivation + unit tests, `ui::prompt`/`ui::confirm` stdin helpers,
  `TARGET_REPO_PATH` replaced by `WORKSPACE_ROOT` in config and .env.example.
  Also improved the .env parse error: dotenvy chokes on Windows backslash paths
  (a backslash starts an escape sequence), and the old message did not say so.
  Watch out: .env and .env.example look identical in an editor — real values
  went into the tracked template twice by mistake. Nothing leaked; the commit
  only ever had placeholders.
- 2026-08-20: Phase 0 complete. Wrote Cargo.toml, .gitignore, .env.example,
  CLAUDE.md, src/main.rs, src/config.rs, src/ui.rs. Verified API details against
  live docs (Gemini `v1beta/models/{model}:generateContent` + `x-goog-api-key`;
  Anthropic `/v1/messages`). Lost time to the antivirus/rustup blocker above
  before working around it. Next: Phase 1, and DP-2/DP-4 will come up soon.
