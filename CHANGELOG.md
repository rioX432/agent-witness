# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6] - 2026-07-12

The claim-vs-reality release: put the agent's final "done — tests pass" next to
what the record shows actually ran, so you can check the claim instead of taking
it on trust. Claim-aware presentation, never a verdict — the record shows what
ran; the judgment stays yours (ADR-0005).

### Added
- `report` gains a **"Final message vs recorded evidence"** section: the agent's verbatim final message (the last `Stop` hook's `last_assistant_message`) beside the recorded execution facts — test/build/lint commands with their observed status (`ok` / `failed` / `no-result`), a count of unpaired calls, and test files written/edited — plus neutral cues that point at a tension (e.g. "the final message mentions tests, but no test-like command was recorded") without ever judging it. It never says the claim is false or that the agent lied, and a `no-result` is an unpaired call whose outcome was not observed, never a failure. No new capture — it reads events already recorded (ADR-0005, #63)
- `digest` aggregates the same **test/build/lint command facts** across sessions, per project and overall: counts by kind with their observed-status roll-up (`ok` / `failed` / `no-result`). Facts only, on the existing digest footing — no pass-rate or efficiency verdict, and a `no-result` is never upgraded into a failure (#64)

### Changed
- README lead repositioned onto claim-vs-reality verification and the delegation ledger (keeping the honest-observation identity front and center); crate and repository descriptions realigned to match (#51)

## [0.0.5] - 2026-07-12

The retrospective-usage release: turn the record into an answer to "what did my
agents actually do — and on whose tokens?" A cross-session delegation ledger, a
configured-vs-used capability inventory, per-session model/token usage, and
destructive-command flags — all facts-only, with the judgment left to you (or a
scheduled agent).

### Added
- `digest` command: a cross-session **delegation ledger** that aggregates every recorded session in a time window (`--today` / `--week` / `--since <dur>`, or all history), grouped by project, into a factual markdown (or `--json`) summary — sessions, durations, user prompts, tool calls, distinct files, Bash commands, destructive-class command-flag counts by severity, and per-model token totals. Windows are UTC calendar days (stated explicitly in the header); a session is included by its start time and contributes its whole totals. Facts only: no efficiency/waste/model-choice judgments (those belong to the agent layer), a missing usage sidecar is surfaced as "usage unavailable" and never counted as zero tokens, prompts count only hook-sourced turns (not transcript prose), and multi-cwd / unknown-project / corrupt-line states are disclosed (#45)
- `inventory` command: reports which MCP servers and skills are **configured** — unioned across `~/.claude.json` (user-global and per-project `mcpServers`) and a project `.mcp.json` — versus **actually used** (aggregated from recorded `mcp__*` / `Skill` tool calls, with call counts and last-used), plus the diff (configured-not-used / used-not-configured). Configured and used are separately labeled and never conflated; each config source carries a read status (read / missing / unreadable / parse-failed) and the report states whether the diff is complete or partial, so an unreadable source is never silently shown as empty. `~/.claude.json` is parsed keys-only — no MCP server secret (env/token) is ever read. `--since <dur>` windows the used side (#50)
- Per-message **model + token usage** extraction: the transcript adapter aggregates each session's Claude Code usage — per-model input/output/cache tokens, deduped by message id so a response split across content-block lines is counted once — into a per-session `usage.json` sidecar (`observed` attribution), read cheaply by `digest`. Honest coverage: an absent sidecar means "usage unavailable", explicitly distinct from zero tokens (#46)
- Destructive-command **flagging** in `report`: recorded Bash commands whose class is destructive — `rm -rf` of a home/root path, `git push --force`, `git clean -fd(x)`, `chmod -R 777`, `curl … | sh`, `dd`/`mkfs` to a device — are flagged with a severity. A flag names the command *class* only, never intent or outcome, and cannot see side effects inside `Bash("script.sh")`; the matcher is token-based and deliberately narrow (only catastrophic `rm -rf` targets, anchored on the leading command) to keep false positives near zero (#48)
- Weekly usage-audit recipe (`docs/recipes/scheduled-audit.md`): a Claude Code scheduled routine (created with `/schedule`) runs `agent-witness digest --week --json` and narrates the interpretation — wasteful interaction loops, oversized-model usage for mechanical work, top projects by delegated work, notable destructive-command flags. The facts/judgment split is load-bearing: the CLI reports FACTS ONLY, the agent supplies the judgment, and the audit is a convenience view — the record (`show`/`report`), not an agent's retelling, stays the trusted read path (doubly so since the audit agent is the same kind of agent the record observes). The `/witness` skill gains a matching "Usage audit" section and pre-authorizes `digest`/`inventory` (re-run `agent-witness init` to update the installed skill) (#49)

### Fixed
- `top` / `show` no longer panic (with a leaked alternate-screen escape) when stdout is not a TTY — pipes, CI, and editor-embedded shells now get a one-line error pointing at the non-interactive equivalent (`ls --live` / `report`) (#40)
- TUI startup is hardened against terminal init failing even when stdout *reports* as a TTY (e.g. Claude Code's `!` command runner, some embedded/remote shells): the entry points use ratatui's non-panicking `try_init` and, on failure, restore the terminal and print the same actionable one-line error — instead of panicking and leaking an alternate-screen escape. Defense-in-depth alongside the `require_tty` pre-check (#54)

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

[Unreleased]: https://github.com/rioX432/agent-witness/compare/v0.0.5...HEAD
[0.0.5]: https://github.com/rioX432/agent-witness/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/rioX432/agent-witness/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/rioX432/agent-witness/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/rioX432/agent-witness/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/rioX432/agent-witness/releases/tag/v0.0.1
