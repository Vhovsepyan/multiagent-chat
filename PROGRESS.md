# Progress

## Current status
v1 is DONE and proven live. plan.md's definition of done was met on 2026-08-20:
a typed topic went through a 3-round Gemini/Claude debate to VERDICT: APPROVED,
produced SPEC.md, took `y` at Gate 2, and Claude Code built a working Rust CLI
in the target repo with no source edited by hand. 56 tests, clippy clean.

## Next steps
- Decide whether v2 (web UI, axum + SSE) is worth starting. Nothing in v1 is
  outstanding.
- If v1 gets more use, the thing most worth improving is spec ambiguity: see the
  finding under "Open questions".

## Decisions made
- DP-1 (2026-08-20): ONE shared `Transcript` of `Turn { speaker, text }` is the
  source of truth. Each model's view is rebuilt from it per call
  (`for_proposer` / `for_critic`), flipping which side counts as the assistant.
  Chosen over per-model histories because the same text is never stored twice,
  so the two views cannot drift, and SPEC.md later reads the same one object.
  A test asserts both views start with a user message and strictly alternate,
  since a drift there would only show up as a 400 at runtime.
- DP-2 (2026-08-20): scan the critique's lines from the BOTTOM for
  `VERDICT: APPROVED` / `VERDICT: NEEDS_WORK`. Chosen over "must be the last
  line" so a trailing "Hope this helps!" cannot break a run, and over asking for
  JSON so the critique stays plain prose we can print live in color. Each line is
  stripped of `*`, `` ` ``, `#`, `_` first, so `**VERDICT: APPROVED**` matches.
  A missing verdict counts as NEEDS_WORK, never approval — ending a debate on a
  formatting slip would be the worse failure.
- DP-3 (2026-08-20): the Proposer drafts SPEC.md, then the Critic checks it
  against the debate and returns a corrected full document. Two extra calls,
  chosen over a single call because it catches a Proposer quietly dropping a
  concession it made under review. The check reuses CRITIC_MODEL, so no new var.
- DP-4 (2026-08-20): retry 429, 5xx and transport failures, fail immediately on
  every other status because a 400 or 401 fails identically forever. Started at
  3 attempts / 1s / 2s, raised after live runs to **5 attempts with 1s/2s/4s/8s**
  — Gemini's own 429 body asks for a ~9s wait, so the first budget gave up
  before the provider expected. Policy (`MAX_ATTEMPTS`, `backoff`,
  `is_retryable`, `Failure`) lives in `api/mod.rs`; each client keeps its own
  small loop around a private `send_once`. A generic async retry helper was
  tried and rejected — a closure returning a future that borrows `self` needs
  higher-ranked lifetimes, too much machinery for a 15-line loop.
- DP-5 (2026-08-20): launch the `claude` CLI headless with `-p`, cwd set to the
  target repo, `--model` from IMPLEMENTER_MODEL (default claude-opus-4-8, Vahe's
  choice) and `--permission-mode` from CLAUDE_PERMISSION_MODE (default
  bypassPermissions). Flags verified against Claude Code 2.1.237. The permissive
  default is forced by headless mode: `-p` has nobody to answer a prompt, so
  acceptEdits would let it write code but never run tests or install anything.
  Output is plain stdout/stderr inheritance rather than parsing
  `--output-format stream-json`, so it streams live and cannot break when the
  CLI's event shape changes.
- DP-6 (2026-08-20): the target repo is *per-run input*, not configuration.
  plan.md had `TARGET_REPO_PATH` as a static env var, but the topic changes every
  run and the repo may not exist yet. `.env` holds `WORKSPACE_ROOT`; `target.rs`
  asks for a project name after the topic, suggesting a slug derived from it, and
  offers `mkdir` + `git init` when it is missing. Names containing a slash or
  `..` are rejected so a typo cannot escape the workspace root.
- The Critic must end every review with a REASON line above the VERDICT line
  (Vahe's request). A bare NEEDS_WORK forced you to re-read the whole review to
  learn what was blocking it. The reason is shown each round, repeated at Gate 2,
  and when the debate ends unapproved the spec-check call is told to carry every
  unresolved objection into "Open risks".
- `--implement-only` skips the debate and builds from the SPEC.md already in the
  chosen project. Makes NO API calls, so re-running an implementation or building
  from a hand-edited spec costs nothing. Still goes through Gate 2, and refuses
  to create a missing project — the point is that the spec is already there.
- Gate 2 defaults to NO: anything that is not an explicit `y` stops the run. The
  spec is written to disk BEFORE the prompt (plan.md's order), so declining still
  leaves SPEC.md there to edit and re-run.
- SPEC.md lives at the root of whichever repo the run resolves to,
  `<workspace>/<project>/SPEC.md`, overwritten each run.
- `Message`/`Role` live in `src/api/mod.rs`, shared by both clients, because the
  two APIs disagree on names: Anthropic sends `{role, content}` with the model
  turn called "assistant", Google sends `{role, parts:[{text}]}` with it called
  "model". Each client converts to its own wire structs, so the debate loop never
  has to know which model it is talking to.
- `push_user` in `api/mod.rs` merges into a trailing user message instead of
  appending, because a transcript ends on the Critic's review — a user message
  from the Proposer's side — and two user messages in a row are a 400.
- `Config` has a hand-written `Debug` that redacts both API keys, so no `{:?}`
  print or log can leak one. Config also validates eagerly at startup: missing or
  empty required vars, a `WORKSPACE_ROOT` that is not a real directory, and a
  non-numeric or zero `MAX_ROUNDS` all fail before any API call.
- Live API tests are `#[ignore]`d on purpose (Vahe's call): they cost tokens, so
  they only run with `cargo test -- --ignored`. Everything else is tested against
  captured JSON, including a real 401 body from the Anthropic API.
- Terminal colors: `owo-colors` over `colored` — no allocation, and `.blue()`
  works on anything printable.
- CLI args are hand-rolled in `cli.rs` rather than pulling in `clap`: two real
  flags. Swap to `clap` derive if it ever passes three.
- Both clients cap output at 16k tokens and time out after 120s.
- Rust edition 2024, toolchain 1.97.1.

## Open questions / problems
- Spec ambiguity survives both gates. The implementer reported that SPEC.md's
  wording on the default execution mode was ambiguous ("defaults to dry-run style
  OR requires explicit execution confirmation"). That got through three debate
  rounds AND the Critic's spec check, and only surfaced at implementation time.
  A real limit of the two-gate design, not a bug. If v1 gets more use, having the
  Critic explicitly hunt for ambiguous wording during the spec check is the
  obvious fix.
- Gemini free tier is flaky: `gemini-3.7-flash` returns 503 "high demand" in
  bursts (it killed two whole runs), `gemini-3.1-pro-preview` is 429 with quota
  limit 0 (paid only), `gemini-2.5-pro` is 404 for new users. `gemini-3.6-flash`
  is reliable and is the default. If a run dies on 503, just run it again.
- Cheap testing pair (Vahe's local .env): `GEMINI_MODEL=gemini-3.5-flash-lite` +
  `CRITIC_MODEL=claude-haiku-4-5`. Haiku emits the VERDICT line correctly, so
  DP-2 detection is fine, but it is a stricter reviewer — it did not approve in
  2 rounds where Sonnet did, and approved on round 3 at MAX_ROUNDS=5. Strict, not
  broken. The committed defaults in .env.example stay on the better models.
- Windows toolchain workaround (personal laptop): security software deletes
  `wasm-component-ld.exe` the instant it is written, so every
  `rustup toolchain install` rolls back and leaves no toolchain. Confirmed by
  hand: tar exits 0, the neighbouring `.pdb` survives, only that one `.exe`
  vanishes; rustc, cargo, rustdoc and rust-lld are all fine. Worked around by
  extracting the cached tarballs from `~/.rustup/downloads` into
  `C:\Users\vaheh\rust-1.97.1` and registering it with
  `rustup toolchain link stable-manual <dir>` + `rustup default stable-manual`.
  The missing binary is only the wasm32 linker, which this project never uses.
  Proper fix: an antivirus allowlist for `C:\Users\vaheh\.rustup` (needs admin).
  Try a plain `rustup default stable` on the office laptop first.

## Distribution
- Release build for someone else MUST use a static CRT, or the exe needs
  `vcruntime140.dll` on their machine:
      RUSTFLAGS="-C target-feature=+crt-static" cargo build --release
  Verified the result references only Windows system DLLs.
- Never ship `.env` — it holds keys that bill Vahe's accounts. Ship
  `.env.example` only. The recipient needs their own Gemini and Anthropic keys
  AND the Claude Code CLI installed and signed in.
- `.env` is read from the WORKING DIRECTORY, not from beside the exe, so the
  recipient must run it from the folder holding their `.env`.
- Built 2026-08-20 at `C:\Users\vaheh\RustroverProjects\multiagent-chat-v0.1.0-windows-x64.zip`
  (exe + .env.example + README.txt, 1.36 MB). Unsigned, so SmartScreen warns.

## Session log
- 2026-08-20 (one long session, phases 0-6 plus live proving and packaging):
  Built the whole app from plan.md — config, both API clients, the debate loop,
  spec generation, both gates, the implementer, retries, `--topic`, README.
  Decided DP-1..DP-6. Then proved it live: a real debate reached APPROVED, the
  spec was written, `y` at Gate 2 let Claude Code build a working Rust CLI
  (`rnm`) in spec-scratch — verified independently, 16/16 of its tests pass and
  a manual smoke test renamed files correctly. Added `--implement-only`
  afterwards and proved it makes zero debate calls and left `git diff` empty on
  an already-implemented repo. Finished by packaging a distributable zip.
  What broke along the way, and was fixed: the rustup/antivirus blocker; dotenvy
  rejecting Windows backslash paths; real keys pasted into the tracked
  `.env.example` twice (nothing leaked, commits only ever had placeholders); a
  slug bug that produced "small-cli-tool-that"; a retry budget too short for
  Gemini's own advertised 9s wait; Gate 2 claiming "written to" when the spec had
  been read.
