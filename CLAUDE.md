# agent-witness

@REVIEW.md

## Project Overview

A session recorder and audit log for AI coding agents. Records what a Claude Code / Codex session actually did (tool calls, files, commands), shows it as a TUI timeline, and persists a replayable JSONL audit trail. Observation only — not a sandbox.

## Core Values

1. **Honest observation** — never claim more than what was observed. Every event carries `attribution: direct | observed | inferred` and a confidence. Overclaiming is the one unforgivable bug.
2. **Zero-friction adoption** — `agent-witness init` and you're recording. No sudo, no entitlements, no wrapper required in v0.1.
3. **Evidence you can share** — every session becomes a replayable record and a markdown report you can paste into a PR or issue.

## Won't Do

- **Sandboxing / prevention**: Anthropic ships an official sandbox; we observe and audit, we do not block (policy layer in v0.2 is hooks-based deny, still not a security boundary)
- **Being an agent / agent framework**: saturated, out of scope
- **v0.1 scope cuts (deliberate, from design review)**: PTY wrapping, fs watching, process polling → v0.2; socket-level network observation → v0.2 proxy / v0.3 eBPF
- **Quantitative "we catch everything" claims**: hooks don't see inside `Bash("script.sh")` side effects — document the gap, never paper over it
- **Windows first-class support in v0.1**: best effort only

## Commands

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## Architecture

```
crates/
  witness-core/     # lib: AgentEvent model, JSONL session store, adapters (hooks receiver, transcript)
  agent-witness/    # bin: CLI (init/watch/ls/show/report/emit) + ratatui TUI
tests/fixtures/     # sanitized hook-event JSONL from real sessions (golden test inputs)
```

Event flow: Claude Code hooks → unix socket (`emit` bridge) → normalizer → JSONL store (`~/.agent-witness/sessions/<id>/`) + ring buffer → TUI / report.

## Tech Stack

| Layer | Technology |
|---|---|
| async runtime | tokio |
| TUI | ratatui + crossterm (TestBackend for golden tests) |
| CLI | clap (derive) |
| serialization | serde / serde_json (JSONL) |
| errors | thiserror (core) / anyhow (bin) |
| distribution | cargo-dist + Homebrew tap + cargo-binstall |

## Key Gotchas

- Claude Code hooks payloads: `PreToolUse` = tool_name/tool_input/tool_use_id; `PostToolUse` adds tool_response/duration_ms; hook input includes `transcript_path` — use it per-session, never glob `~/.claude/projects/*.jsonl`
- Transcript JSONL internal schema is NOT a stable contract — transcript adapter is best-effort, versioned, feature-flagged; hook stdin JSON is the canonical record
- `agent-witness init` edits `~/.claude/settings.json` — must be idempotent, merge-safe, and keep a backup
- Fixtures must be sanitized (no real paths/secrets from recorded sessions)
- Naming: crates.io `agent-witness` / `agent-witness-core`; note cursor/agent-trace is an export-format interop target, not a competitor

## Development Process

- AI-driven: issues are Dev Ready; use `/dev` per issue, `/dev-all` for batches. Human judgment points: event schema changes, CLI surface, launch copy
- Every PR includes a short design-decision note (these become launch article material)
- No squash merges; incremental history is part of the public ownership story

## Language

- Code comments, variable names: English
- Commits: concise single line, English
