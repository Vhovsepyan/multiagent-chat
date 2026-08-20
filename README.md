# multiagent-chat

A terminal app that runs a design debate between two AI models, then hands the
agreed design to Claude Code to build.

You type a topic. Gemini proposes a solution, Claude Sonnet reviews it, and they
iterate until the reviewer approves. The agreed design becomes a `SPEC.md` in
your target repo. You approve it with `y`, and Claude Code implements it there.

```
Topic  ──►  Proposer (Gemini)  ──►  Critic (Claude Sonnet)  ──┐
              ▲                                               │
              └────────── NEEDS_WORK ◄──────────────────────  │
                                                         APPROVED
                                                              │
                          SPEC.md  ◄──────────────────────────┘
                             │
                        your y/n
                             │
                          Claude Code (Opus)  ──►  code in the target repo
```

## Requirements

- Rust (stable) with `cargo`
- The [Claude Code CLI](https://claude.com/claude-code) on your `PATH`
- A Google AI Studio API key and an Anthropic API key

## Setup

```bash
git clone https://github.com/Vhovsepyan/multiagent-chat
cd multiagent-chat
cp .env.example .env
```

Then open `.env` and fill in the three blanks: `GEMINI_API_KEY`,
`ANTHROPIC_API_KEY`, and `WORKSPACE_ROOT`.

> **Windows:** write paths with **forward slashes** — `C:/Users/you/projects`.
> A backslash starts an escape sequence and the `.env` parser will reject the
> whole file.

`.env` is gitignored. Do not put real keys in `.env.example`, which is tracked.

## Usage

```bash
cargo run                                  # asks for the topic
cargo run -- --topic "I need credit applications"
cargo run -- --help
```

The target repo is chosen per run, not fixed in config. After the topic, you are
asked for a project name inside `WORKSPACE_ROOT`, with a suggestion derived from
the topic:

```
Topic: I need credit applications
Project [credit-applications]:
  -> C:/Users/you/projects/credit-applications
     that project does not exist yet.
Create it and run git init? [y/n]:
```

There are two gates. **Gate 1** is automatic: the debate ends when the Critic
writes `VERDICT: APPROVED`, or after `MAX_ROUNDS` with a warning. **Gate 2** is
you: the spec is printed and nothing touches the repo until you type `y`.

## Configuration

All settings live in `.env`. See `.env.example` for the annotated version.

| Variable | Default | Meaning |
| --- | --- | --- |
| `GEMINI_API_KEY` | — | required |
| `ANTHROPIC_API_KEY` | — | required |
| `WORKSPACE_ROOT` | — | required; folder holding your projects |
| `MAX_ROUNDS` | `5` | debate rounds before giving up on approval |
| `GEMINI_MODEL` | `gemini-3.6-flash` | the Proposer |
| `CRITIC_MODEL` | `claude-sonnet-4-6` | the Critic, and the spec checker |
| `IMPLEMENTER_MODEL` | `claude-opus-4-8` | the model Claude Code builds with |
| `CLAUDE_PERMISSION_MODE` | `bypassPermissions` | see the warning below |

> **On `bypassPermissions`:** Claude Code runs headless (`-p`), so there is
> nobody to answer a permission prompt. The default lets it edit files *and run
> commands* unattended in the target repo — which is what it needs to install
> dependencies and run your tests. Point `WORKSPACE_ROOT` at projects you are
> happy to let it work in. Use `acceptEdits` to restrict it to file edits, but
> expect it to fail on anything it needs to run.

## How it is put together

```
src/
  main.rs          the pipeline, top to bottom
  cli.rs           --topic / --help / --version
  config.rs        .env into a Config (keys redacted from Debug)
  api/
    mod.rs         shared Message/Role + the retry policy
    claude.rs      Anthropic Messages API — the Critic
    gemini.rs      Gemini generateContent — the Proposer
  debate.rs        the transcript, both model views, the round loop
  spec.rs          draft the spec, check it, write SPEC.md
  approve.rs       Gate 2
  implementer.rs   spawn Claude Code in the target repo
  ui.rs            colors and prompts
```

A few decisions worth knowing, with the reasoning kept in `PROGRESS.md`:

- **One shared transcript.** Each model's view is rebuilt from it per call, with
  that model's own turns as the assistant. The same text is never stored twice,
  so the two views cannot drift.
- **The verdict is found by scanning upward** for `VERDICT: APPROVED`, ignoring
  markdown decoration, so a trailing "Hope this helps!" cannot break a run. A
  missing verdict counts as `NEEDS_WORK`, never as approval.
- **Every review carries a one-line `REASON`**, shown next to the verdict each
  round and again at the approval gate, so you never have to re-read a review to
  learn what is blocking it. If the debate ends unapproved, the unresolved
  objections are pushed into the spec's Open risks.
- **The spec is drafted by the Proposer and checked by the Critic**, which
  catches a proposal quietly dropping a concession it made under review.
- **Retries** cover 429, 5xx and network failures across 5 attempts with
  1s/2s/4s/8s backoff. A 400 or 401 fails immediately, because it will fail
  identically forever. The budget is that large because Gemini returns 503
  "high demand" in bursts and its 429 bodies ask for a ~9 second wait.
- **API keys never reach the terminal.** `Config`'s `Debug` prints `<redacted>`,
  and no error message includes request headers.

## Development

```bash
cargo test                  # unit tests, no network
cargo test -- --ignored     # the live API calls; these cost tokens
cargo fmt
cargo clippy -- -D warnings
```

The tests that hit real APIs are `#[ignore]`d on purpose, so a normal
`cargo test` costs nothing. Everything else is checked against captured JSON
response bodies.
