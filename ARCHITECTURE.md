# Architecture

agent-witness records what an AI coding agent session *actually did* — as reported
by Claude Code hooks — and turns it into a replayable, shareable audit trail.
This document describes how the pieces fit together. Design rationale lives in
[docs/adr/](docs/adr/); deliberate scope cuts in [NON-GOALS.md](NON-GOALS.md).

## Event flow

```
Claude Code session
    │  SessionStart / UserPromptSubmit / PreToolUse / PostToolUse / Stop
    │  hooks (JSON on stdin)
    ▼
agent-witness emit          ── bridge invoked by each hook
    │  unix socket ($XDG_RUNTIME_DIR or ~/.agent-witness/witness.sock)
    │  └─ fallback: writes to the store directly when no daemon is running
    ▼
agent-witness watch         ── optional daemon (tokio unix-socket server)
    │  normalize (pure) + persist raw payload verbatim
    ▼
JSONL session store         ── ~/.agent-witness/sessions/<session-id>/
    │     events.jsonl  (one AgentEvent per line, schema v1)
    │     raw.jsonl     (hook stdin preserved verbatim — the canonical record)
    │     meta.json
    ▼
TUI (show / top / --pick) · markdown & JSON report · ls
```

Two properties are load-bearing:

- **Hooks are the canonical source** ([ADR-0001](docs/adr/0001-hooks-as-canonical-source.md)).
  The raw hook stdin JSON is persisted verbatim; every normalized event links back
  to it via `raw_event_ref`. The transcript adapter is a best-effort *secondary*
  source and can never break hooks-based recording.
- **No daemon required.** `emit` falls back to writing the store directly when the
  socket is absent. `watch` adds a single-writer ingest path and a one-byte
  persistence ack (at-least-once delivery), but recording works without it.

## Workspace layout

```
crates/
  witness-core/       # library: event model, store, adapters — no I/O surprises
    src/event.rs        # AgentEvent (schema v1) + Attribution + EventKind
    src/store.rs        # append-only JSONL store; corrupt lines skipped AND counted
    src/clock.rs        # injectable Clock (SystemClock / FixedClock) — no wall-clock in pure paths
    src/hooks.rs        # pure normalizer: hook payload -> AgentEvent
    src/receiver.rs     # ingest orchestration: raw + normalized together
    src/selector.rs     # @last / @N / @project: / @live: / id-prefix resolution
    src/liveness.rs     # pure "is this session running?" rule (store scan only)
    src/transcript/     # versioned best-effort transcript adapter (transcript_v1)
  agent-witness/      # binary: CLI (clap derive) + ratatui TUI
    src/init.rs         # settings.json hooks registration (idempotent, merge-safe, backup)
    src/emit.rs         # hook bridge: stdin -> socket, store fallback
    src/watch.rs        # unix socket server daemon
    src/ls.rs           # session table (+ --live filter, STATE column)
    src/tui.rs          # timeline view (show), detail pane, follow mode
    src/top.rs          # htop-like resident view of live sessions
    src/pick.rs         # interactive session picker (show --pick)
    src/report.rs       # SessionExporter trait + Markdown/JSON exporters
tests/fixtures/       # sanitized real-session hook/transcript JSONL (golden inputs)
tools/fixtures/       # capture.sh + sanitize.py + checklist (fixture pipeline)
docs/adr/             # architecture decision records
```

## The honesty invariants

The one unforgivable bug in this project is overclaiming
([ADR-0002](docs/adr/0002-attribution-honesty.md)). Concretely:

- Every `AgentEvent` carries `attribution: direct | observed | inferred` and a
  `confidence`. Hook-derived events are `direct`; transcript-derived events are
  `observed` (0.9); liveness is *inferred* from recency, never asserted.
- Corrupt/unreadable store lines are skipped **and counted** — the count is shown
  in the TUI header, `ls`, and every report. Silence would imply full coverage.
- Every report ends with an observation-scope disclaimer: hooks don't see inside
  `bash script.sh` side effects, and a failed Bash call fires no completion hook
  (it appears as a call with no result — never as a success).
- Fixtures are real captures, sanitized by machine rules + a manual checklist,
  with provenance recorded (`tests/fixtures/*/provenance.json`). Synthetic data
  must be labeled synthetic.

## Determinism & testing

Core logic takes an injected `Clock`; no wall-clock or RNG inside pure paths.
This makes the TUI a pure function of app state, rendered against a fixed-size
ratatui `TestBackend` for golden-screen tests. The fixture pipeline
(capture → sanitize → lint) doubles as a canary: if a Claude Code update changes
hook payload shapes, fixture-shape tests surface the diff.
Primary gate: `just verify` (fmt + clippy `-D warnings` + build + nextest) — CI
runs exactly the same gate.

## Decision records

| ADR | Decision |
|---|---|
| [0001](docs/adr/0001-hooks-as-canonical-source.md) | Claude Code hooks are the canonical event source; PTY/fs/network observation deferred |
| [0002](docs/adr/0002-attribution-honesty.md) | Every event carries attribution + confidence; never overclaim |
| [0003](docs/adr/0003-dev-harness-and-review-accumulation.md) | Issue-driven dev harness; recurring review findings promoted to rules |
| [0004](docs/adr/0004-distribution-cargo-dist.md) | Distribution via dist (cargo-dist): shell / Homebrew / binstall from one config |
