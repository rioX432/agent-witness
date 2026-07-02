# agent-witness

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
agent-witness ls                  # list every recorded session
```

- Observation only — this is not a sandbox and not a security boundary (see SECURITY.md)
- Every event is labeled `direct | observed | inferred` — we record what we saw, not what we guess
- Planned interop: export to Cursor's agent-trace format

## Install

> Pre-v0.1: no release is published yet. These paths become available once the first tag is pushed.

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
