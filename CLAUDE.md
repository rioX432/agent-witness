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

## Build & Run

```bash
rustup default stable
cargo install cargo-nextest just

just verify    # primary local gate: check (fmt+clippy+build) + nextest. Run before merge.
just check     # fmt + clippy + build
just test      # nextest
just build
```

## Verification

The primary merge gate is **`just verify` locally**; GitHub Actions CI mirrors the exact same gate (public-OSS trust signal — see ADR-0003 for why this diverges from avatar-core's PoC no-CI stance). If `just verify` is green, the change is mergeable.

## Architecture

```
crates/
  witness-core/     # lib: AgentEvent model, JSONL session store, adapters (hooks receiver, transcript)
  agent-witness/    # bin: CLI (init/watch/ls/show/report/emit/top) + ratatui TUI
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
- `agent-witness init` edits `~/.claude/settings.json` AND installs `~/.claude/skills/witness/SKILL.md` — both must be idempotent, merge-safe, and keep a backup; the skill file is marker-owned and a foreign file at its path is never touched
- Fixtures must be sanitized (no real paths/secrets from recorded sessions)
- Naming: crates.io `agent-witness` / `agent-witness-core`; note cursor/agent-trace is an export-format interop target, not a competitor

## Rust Conventions

- `just check` must pass (fmt, clippy `-D warnings`, build); tests via nextest.
- No `unsafe`. Determinism in core logic: no wall-clock or RNG inside pure paths — inject them.
- No magic numbers; errors via `thiserror` in lib crates, `anyhow` in bins; no `unwrap`/`expect` outside tests.

## Development Harness

Issue-driven development with `/dev` (single issue) and `/dev-all` (sequential). Other skills: `/audit`, `/update-docs`, `/decompose`, `/investigate`, `/review`, `/tech-debt`, `/pr`.

Review accumulation (ADR-0003): valid review findings are promoted into rules (`.claude/rules/`), lints, and skills. Promotion: a finding that recurs twice becomes a rule. Retirement: a rule unused for 3 months is removed.

Human judgment points: event/report schema changes, CLI surface, launch copy. Every PR includes a short design-decision note (launch article material). No squash merges.

## Phasing

Design is decided ahead (docs/adr/, zero-base design docs), but implementation may diverge. v0.1 issues are fully detailed; v0.2+ exist only as ADR notes and are detailed at the v0.1 gate. The v0.1 gate is a human decision after the first vertical spike (calibrate actual pace; shrink scope if estimates double).

## Language

- Code comments, variable names: English
- Commits: concise single line, English
