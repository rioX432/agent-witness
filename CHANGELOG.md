# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `digest` command: a cross-session **delegation ledger** that aggregates every recorded session in a time window (`--today` / `--week` / `--since <dur>`, or all history), grouped by project, into a factual markdown (or `--json`) summary — sessions, durations, user prompts, tool calls, distinct files, Bash commands, destructive-class command-flag counts by severity, and per-model token totals. Windows are UTC calendar days (stated explicitly in the header); a session is included by its start time and contributes its whole totals. Facts only: no efficiency/waste/model-choice judgments (those belong to the agent layer), a missing usage sidecar is surfaced as "usage unavailable" and never counted as zero tokens, prompts count only hook-sourced turns (not transcript prose), and multi-cwd / unknown-project / corrupt-line states are disclosed (#45)

### Fixed
- `top` / `show` no longer panic (with a leaked alternate-screen escape) when stdout is not a TTY — pipes, CI, and editor-embedded shells now get a one-line error pointing at the non-interactive equivalent (`ls --live` / `report`) (#40)

## [0.0.4] - 2026-07-11

Consumption-surface release: the record now reaches you where you already are
(the status line, the conversation, the PR), instead of waiting for a CLI habit
that dogfooding proved nobody forms.

### Added
- Recording statusline (opt-in: `init --statusline`): Claude Code's status line shows `● witness <n>ev` for the current session — and `○ witness not recording` when hooks are silently broken. An existing statusLine command is wrapped, not replaced (it renders ahead of the witness segment; the original is preserved verbatim inside the wrapper and `init --remove` restores it exactly) (#32)
- `init` now registers the `SessionEnd` hook and the pipeline records it (with its `reason`) as a first-class event: session termination is now *directly observed*, so cleanly ended sessions read idle in `ls`/`top` immediately instead of appearing live until the recency window lapses. Crashed sessions still fall back to honest window inference. **Re-run `agent-witness init` after upgrading** to register the new event (#31)
- `init` now also installs a `/witness` skill (`~/.claude/skills/witness/SKILL.md`) so a Claude Code session can query the audit record conversationally; `init --remove` uninstalls it. The file is marker-owned: a foreign file at that path is never touched (a warning is printed to stderr), and a locally edited managed file is backed up before being rewritten or removed. **Re-run `agent-witness init` after upgrading** to install the skill (#30)

## [0.0.3] - 2026-07-02

Two root causes found by dogfooding on real sessions — both made the live-session
features (`top`, `ls --live`, `@live:`) silently useless outside fixtures.

### Fixed
- Liveness misread every interactive session as idle after its first turn: Claude Code's `Stop` hook fires per-turn, not per-session, but the rule treated any observed `Stop` as terminal. Liveness now keys on whether the *last* event is a `Stop`; a new real-captured multi-turn fixture pins the behavior as a canary (#23)
- `init` now registers all five supported hook events — `SessionStart` and `UserPromptSubmit` were missing, so real sessions recorded no prompts and liveness (`ls`/`top`/`@live:`) never fired. A hook-set parity test pins init's event set to `capture.sh` so fixtures and real usage can't drift again. **Re-run `agent-witness init` after upgrading** to register the new events (idempotent; adds only the missing entries) (#26)
- Liveness no longer requires an observed `SessionStart`: any recent, unstopped event now reads as live, so sessions recorded by pre-fix configs are detected correctly (#26)

## [0.0.2] - 2026-07-02

### Added
- Session selectors and no-arg defaults for `show`/`report`: latest-for-cwd default with honest fallback note, `@last`, `@N`, `@project:<substring>`, `@live:<n>`, unique id-prefix matching, and an interactive `show --pick` session picker (9c4cc6d)
- Live sessions: store-scan liveness rule (no daemon), `ls` STATE column + `--live` filter, `show --follow` live tail unified with the in-TUI `f` toggle, and an htop-like `top` view with Enter-to-drill-down (4e508b1)
- Cmux/tmux multi-session monitoring recipe (`docs/recipes/cmux.md`) (4e508b1)

## [0.0.1] - 2026-07-02

First test release — validates the full distribution pipeline (GitHub Release,
Homebrew tap, cargo-binstall, shell installer). Functionally this is the complete
v0.1 feature set.

### Added
- `AgentEvent` model (schema v1, attribution `direct | observed | inferred` + confidence) and append-only JSONL session store with corrupt-line skip-and-count (a0cafce)
- Golden fixture pipeline: real-session capture script, sanitizer with mechanical + manual checks, and fixture lint tests that double as a hook-payload canary (b0c5004)
- Hooks receiver: `emit` bridge (stdin → unix socket with persistence ack, direct-store fallback so no daemon is required) and `watch` server; raw hook payloads preserved verbatim alongside normalized events (47006f6)
- `init`: idempotent, merge-safe registration of recording hooks in `~/.claude/settings.json` (or `./.claude/settings.json` with `--project`), with timestamped backup and `--remove` (9f94a1b)
- Transcript adapter (best-effort, versioned, feature-flagged): extracts assistant prose hooks never surface, as `observed`-attribution events with honest skip accounting; `--no-transcript` opt-out (2d28b5c)
- TUI timeline viewer (ratatui): session `ls`, replay with tool-call pairing, detail pane, live follow; golden-tested via `TestBackend` (b1ff7b1)
- `report`: shareable markdown/JSON session audit with a mandatory observation-scope disclaimer (9f79d6a)

### Infrastructure
- Workspace scaffold, `just verify` gate, CI mirroring the local gate, ADRs (5e26c33, 4e056ad)
- Distribution via dist (cargo-dist): release CI on version tags, Homebrew tap, cargo-binstall metadata, shell installer (df46a90)

[Unreleased]: https://github.com/rioX432/agent-witness/compare/v0.0.3...HEAD
[0.0.3]: https://github.com/rioX432/agent-witness/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/rioX432/agent-witness/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/rioX432/agent-witness/releases/tag/v0.0.1
