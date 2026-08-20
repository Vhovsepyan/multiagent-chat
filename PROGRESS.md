# Progress

## Current status
Phase 0 done and committed (58bb438). On top of it, the target repo is now
resolved per run (DP-6) in `src/target.rs`: type a topic, get a suggested
project name, and the app offers to create the repo + `git init` if it is new.
`cargo test` (3 tests), `cargo fmt` and `cargo clippy -- -D warnings` are clean.

## Next steps
- Vahe must update his `.env`: replace `TARGET_REPO_PATH` with
  `WORKSPACE_ROOT=C:/Users/vaheh/RustroverProjects` (forward slashes).
- Commit the DP-6 work.
- Phase 1: `src/api/claude.rs` — POST to `https://api.anthropic.com/v1/messages`
  with headers `x-api-key` and `anthropic-version: 2023-06-01`; request/response
  serde types; test with a real "pong" call.

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
