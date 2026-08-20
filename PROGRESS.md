# Progress

## Current status
Phase 0 done. `cargo run` prints the colored hello line and loads config from
the environment / `.env`; `cargo fmt` and `cargo clippy -- -D warnings` are
clean. Next up is Phase 1 (Anthropic client).

## Next steps
- Phase 1: `src/api/claude.rs` — POST to `https://api.anthropic.com/v1/messages`
  with headers `x-api-key` and `anthropic-version: 2023-06-01`; request/response
  serde types; test with a real "pong" call.
- Vahe still needs to create his own `.env` from `.env.example` (real keys).

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
- 2026-08-20: Phase 0 complete. Wrote Cargo.toml, .gitignore, .env.example,
  CLAUDE.md, src/main.rs, src/config.rs, src/ui.rs. Verified API details against
  live docs (Gemini `v1beta/models/{model}:generateContent` + `x-goog-api-key`;
  Anthropic `/v1/messages`). Lost time to the antivirus/rustup blocker above
  before working around it. Next: Phase 1, and DP-2/DP-4 will come up soon.
