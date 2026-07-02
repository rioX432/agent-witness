# agent-witness

[![CI](https://github.com/rioX432/agent-witness/actions/workflows/ci.yml/badge.svg)](https://github.com/rioX432/agent-witness/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/rioX432/agent-witness)](https://github.com/rioX432/agent-witness/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

> A session recorder and audit log for AI coding agents.

`agent-witness init` → use Claude Code as usual → `agent-witness show` replays what the session actually did (tool calls, files, commands) as a TUI timeline, backed by a persistent JSONL audit trail.

**Status**: pre-v0.1, building in public. Not launched yet — interfaces and scope will change until v0.1.

## Quick Start

```bash
agent-witness init            # register the recording hooks (once)
# ... use Claude Code as usual ...

agent-witness show            # replay the latest session for this directory
agent-witness report          # print a shareable markdown audit of it
```

No session id required: with no argument, `show` and `report` open the latest
session recorded from the current directory (falling back to the globally latest
session, with a note, when this directory has none). When you do want a specific
one, pass a selector:

```bash
agent-witness show @last          # most recent session (any directory)
agent-witness show @2             # the one before that
agent-witness show @project:avvy  # latest session whose cwd matches "avvy"
agent-witness show @live:1        # the most recent still-running session
agent-witness show 9f8c           # unique session-id prefix
agent-witness show --pick         # choose from an interactive list, then drill in
agent-witness show --follow       # live-tail the timeline as new events land
agent-witness ls                  # list every recorded session (STATE = live/idle)
agent-witness ls --live           # only the sessions running right now
agent-witness top                 # htop-like live view of every running session
```

- Observation only — this is not a sandbox and not a security boundary (see SECURITY.md)
- Every event is labeled `direct | observed | inferred` — we record what we saw, not what we guess
- Planned interop: export to Cursor's agent-trace format

## Live sessions: the live end of the audit trail

`top` and `show --follow` are the *now* end of the same audit record `show`/`report`
replay after the fact — not a separate telemetry product.

```bash
agent-witness top             # resident view of live sessions; Enter drills into one, q quits
agent-witness show --follow   # tail one session's timeline (unifies with the in-TUI `f` toggle)
```

A session is **live** when it has started, has not stopped, and its last event is
within a recency window (default 5 min, `--window <seconds>`). Liveness is
*inferred* from a store scan — no daemon required — so a crashed agent reads as
live until its window lapses, then flips to idle. We label that honestly rather
than claim certainty (ADR-0002).

**Positioning vs. abtop and usage dashboards:** `top`
answers "what are my agents *doing* right now?" — project, the tool currently
running, last activity, elapsed, event count. It is **not** a usage/cost meter.
Token and spend aggregation is a deliberate Won't Do (see NON-GOALS.md): it would
pull the tool toward a metrics dashboard and away from honest observation.

**Watching several sessions at once?** agent-witness has no in-app split pane (a
deliberate cut). Use an external multiplexer — tile `show --follow` panes in Cmux
or tmux. Recipe: [`docs/recipes/cmux.md`](docs/recipes/cmux.md).

## How it works

Claude Code hooks (`PreToolUse` / `PostToolUse` / `Stop`) pipe each event into
`agent-witness emit`, which forwards it to a unix socket — or, when no daemon is
running, writes straight to the JSONL store. **No daemon required**; `agent-witness
watch` is optional. The raw hook payload is preserved verbatim next to every
normalized event, so the record is always traceable to its evidence.

A best-effort transcript adapter fills in what hooks never surface (assistant
prose between tool calls) as `observed`-attribution events — disable with
`--no-transcript` for a hooks-only canonical record. Details: [ARCHITECTURE.md](ARCHITECTURE.md).

## Install

```bash
# Homebrew (macOS / Linuxbrew)
brew install rioX432/tap/agent-witness

# cargo-binstall (prebuilt binary, no compile)
cargo binstall agent-witness

# Shell installer (downloads the right prebuilt archive)
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/rioX432/agent-witness/releases/latest/download/agent-witness-installer.sh | sh

# cargo (build from source)
cargo install agent-witness

# Manual download
# Grab the archive for your platform from the GitHub Releases page and extract
# `agent-witness` onto your PATH. Each archive ships a .sha256 checksum:
#   https://github.com/rioX432/agent-witness/releases
```

Prebuilt binaries are published for macOS (arm64, x86_64) and Linux (x86_64, arm64).

## v0.1 scope

hooks receiver + JSONL session store + TUI timeline/replay + markdown report. See NON-GOALS.md for deliberate cuts.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — event flow, workspace layout, honesty invariants
- [CHANGELOG.md](CHANGELOG.md) — release history
- [CONTRIBUTING.md](CONTRIBUTING.md) — dev setup, conventions, fixture rules
- [SECURITY.md](SECURITY.md) — what this tool is *not* (a security boundary)
- [NON-GOALS.md](NON-GOALS.md) — deliberate scope cuts
- [docs/adr/](docs/adr/) — architecture decision records
