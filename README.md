# agent-witness

[![CI](https://github.com/rioX432/agent-witness/actions/workflows/ci.yml/badge.svg)](https://github.com/rioX432/agent-witness/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/rioX432/agent-witness)](https://github.com/rioX432/agent-witness/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

> The audit trail for AI coding agents — check what the agent *actually did*, not just what it *said* it did.

Your agent reports "done — tests pass, refactor complete." `agent-witness` records
what the session actually did — every tool call, command, and its `ok` / `failed` /
`no-result` outcome — so you can check that claim against the record instead of taking
it on trust. And `digest` rolls every session into a per-project **delegation ledger**
with token totals, so you can see where your agent time and quota actually went.

`agent-witness init`, use Claude Code as usual, then `agent-witness report` for a
shareable audit of one session, or `agent-witness digest` for the ledger across all of
them. `agent-witness show` replays any session as a TUI timeline.

**We record what we saw, not what we guess** — every event is labeled
`direct | observed | inferred`. Observation only: not a sandbox, not a security
boundary (see SECURITY.md).

**Status**: pre-v0.1, building in public. Not launched yet — interfaces and scope will change until v0.1.

## Quick Start

```bash
agent-witness init            # register the recording hooks (once)
# ... use Claude Code as usual ...

agent-witness show            # replay the latest session for this directory
agent-witness report          # print a shareable markdown audit of it
```

> **Upgrading?** Re-run `agent-witness init` after updating. Earlier releases
> registered fewer hook events; re-running init adds the newer ones
> (`SessionStart`, `UserPromptSubmit`, `SessionEnd`) so prompts, live-session
> detection, and direct session-end observation work. It is idempotent and
> keeps a timestamped backup.

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

## Ask your agent: the `/witness` skill

`init` also installs a `/witness` skill (`~/.claude/skills/witness/`), so you
can query the audit record without leaving a Claude Code session:

```
/witness                 # what did this directory's latest session do?
/witness @project:avvy   # summarize another project's latest session
```

Under the hood the skill just runs `agent-witness report` and summarizes the
output. **Trust boundary:** this is a convenience view, not the trusted read
path — the agent summarizing the record is the same kind of agent the record
observes. For incident review (e.g. suspected prompt injection), don't rely on
an agent narrating its own audit trail: open the record directly with
`agent-witness show` / `agent-witness report` in your own terminal.
`init --remove` uninstalls the skill together with the hooks; a foreign file at
that path is never touched.

## Always-on visibility: the recording statusline

```bash
agent-witness init --statusline    # opt-in: show recording state in Claude Code's status line
```

Claude Code's status line then carries a live segment for the session you are
in: `● witness 42ev` while events are landing, and — the part that matters —
`○ witness not recording` when they are not. A silently broken hook setup
becomes visible instead of discovered weeks later.

Already have a statusLine command? It is **wrapped, not replaced**: your
command keeps rendering first (`your-segment | ● witness 42ev`), its original
definition is preserved verbatim inside the wrapper, and `init --remove`
restores it exactly. A statusLine shape agent-witness doesn't recognize is
left untouched with a warning — never clobbered.

## Live sessions: the live end of the audit trail

`top` and `show --follow` are the *now* end of the same audit record `show`/`report`
replay after the fact — not a separate telemetry product.

```bash
agent-witness top             # resident view of live sessions; Enter drills into one, q quits
agent-witness show --follow   # tail one session's timeline (unifies with the in-TUI `f` toggle)
```

A session is **live** when it has started, its last event is not a `Stop` or
`SessionEnd` (it is mid-turn), and that event is within a recency window
(default 5 min, `--window <seconds>`). Claude Code's `Stop` hook fires at the
end of *every* assistant turn — not at session end — so a live session between
turns reads as idle and flips back to live the moment its next turn begins. A
recorded `SessionEnd` is *direct* observation: the session reads idle
immediately, no window wait (and a resumed session flips back to live with its
next event). Sessions that crash without a `SessionEnd` still fall back to
window inference — a crashed agent reads as live until its window lapses, then
flips to idle. We label that honestly rather than claim certainty (ADR-0002).

**Positioning vs. abtop and usage dashboards:** `top`
answers "what are my agents *doing* right now?" — project, the tool currently
running, last activity, elapsed, event count. It is **not** a usage/cost meter.
Token and spend aggregation is a deliberate Won't Do (see NON-GOALS.md): it would
pull the tool toward a metrics dashboard and away from honest observation.

**Watching several sessions at once?** agent-witness has no in-app split pane (a
deliberate cut). Use an external multiplexer — tile `show --follow` panes in Cmux
or tmux. Recipe: [`docs/recipes/cmux.md`](docs/recipes/cmux.md).

## How it works

Claude Code hooks (`SessionStart` / `UserPromptSubmit` / `PreToolUse` /
`PostToolUse` / `Stop` / `SessionEnd`) pipe each event into
`agent-witness emit`, which forwards it to a unix socket — or, when no daemon is
running, writes straight to the JSONL store. **No daemon required**; `agent-witness
watch` is optional. The raw hook payload is preserved verbatim next to every
normalized event, so the record is always traceable to its evidence.

A best-effort transcript adapter fills in what hooks never surface (assistant
prose between tool calls) as `observed`-attribution events — disable with
`--no-transcript` for a hooks-only canonical record. Details: [ARCHITECTURE.md](ARCHITECTURE.md).

## Flagged commands

`report` surfaces a **Flagged commands** section for recorded shell commands
whose *class* is destructive — `rm -rf` on a home/root path, `git push --force`,
`git clean -fd`, `git reset --hard`, `chmod -R 777`, `dd of=/dev/…`, `mkfs` on a
device, and `curl … | sh`. Each flag carries a severity (`critical` / `warning`)
and a one-line description of the command class.

**What a flag is — and is not.** A flag says *this command's class is
destructive*. It makes **no claim** about intent, maliciousness, outcome, or
whether any damage occurred. Read it as "worth a second look", not "this did
harm".

**Coverage gap (documented, not papered over).** Matching is a best-effort,
token-based check over the recorded command text, so it is deliberately narrow
and evadable:

- It is **blind to side effects inside a script** — `Bash("deploy.sh")` is
  recorded by its command line only; what runs *inside* it is never observed.
- Only the **catastrophic `rm -rf` home/root form** is flagged (`~/`, `/`,
  `$HOME`, …). Scoped or relative deletes like `rm -rf ./node_modules` or
  `rm -rf ~/project/build` are intentionally **not** flagged, to keep false
  positives near zero.
- Quoting, heredocs, aliases, and obfuscation can evade it. This is not a
  security boundary and makes no "we catch everything" claim (see SECURITY.md).

## Claim vs reality: `report`'s final-message panel

When the agent ends with "done — tests pass", `report` puts that verbatim final
message beside the recorded execution facts, so you can check the claim against
the record:

- the agent's **final message**, shown verbatim (the last `Stop` hook's
  `last_assistant_message`);
- **test/build/lint commands** that were recorded, each with its observed status
  (`ok` / `failed` / `no-result`);
- a count of **unpaired calls** (`no-result`) — a failed Bash fires no completion
  hook, so these are surfaced as *outcome not observed*, never as a failure;
- **test files** written or edited (by path heuristic);
- neutral **notes** that point at a tension without judging it — e.g. "the final
  message mentions tests, but no test-like command was recorded."

**It shows the record; it does not judge the claim (deliberately).** The section
never says the message is false, contradicted, or that the agent lied — that
verdict would be exactly the overclaim the whole tool refuses (ADR-0002). Success
recorded with no evidence is not the same as failure. The panel is a filtered
few-line slice, not the raw timeline; reading the contrast is yours, and triage at
scale belongs to a future agent layer, never the CLI. Design:
[ADR-0005](docs/adr/0005-claim-vs-reality-verification.md).

## Capability inventory: configured vs used

`inventory` accounts for your agent's **attack surface**: what MCP servers and
skills are *configured and reachable* versus what your sessions *actually
invoked*, plus the diff.

```bash
agent-witness inventory              # markdown, over all recorded history
agent-witness inventory --since 30d  # only count usage in the last 30 days
agent-witness inventory --json       # same data, machine-readable
```

It reads three configured MCP sources, source-tagged so you can see exactly
where a capability comes from:

1. `~/.claude.json` → `.mcpServers` (user-global)
2. `~/.claude.json` → `.projects[<cwd>].mcpServers` (per-project, this directory)
3. `<cwd>/.mcp.json` → `.mcpServers` (checked-in project file)

Configured skills are the immediate subdirectories containing a `SKILL.md` under
`~/.claude/skills/` (user) and `<cwd>/.claude/skills/` (project). The **used**
side aggregates `mcp__<server>__<tool>` and `Skill` tool calls from the recorded
store, with a per-item call count and last-used time.

**Honest time base — the two sides are never conflated.** *Configured* is a
snapshot read from your config files **at the moment you run the command**;
*used* is observed `ToolCall` evidence over a window (`[since, now]`, or all
recorded history without `--since`). The output states both explicitly. The diff
labels stay factual — "configured now, not observed used in `<window>`" and
"observed used in `<window>`, not configured now" — never a bare "unused". Each
configured source carries a read status (`read` / `missing` / `unreadable` /
`parse failed`), and the report is flagged `partial` if any source could not be
read, so an unreadable config never silently looks like "nothing configured".

**Gaps (documented, not papered over).**

- **MCP config values are never read** — only server *names* (the object keys).
  `~/.claude.json` holds env vars and tokens in the server values; those are
  never read, retained, serialized, or printed.
- **Plugin-provided skills** (via `enabledPlugins`) are not enumerated.
- **Used-but-not-configured is a neutral fact, not "misconfigured".** Dynamic /
  UUID-named or claude.ai-connected servers, and servers whose config was since
  removed, legitimately show up as observed-used without a current config entry.
- Nothing here is written to the store: `inventory` reads config files
  ephemerally and computes the report — configured state is never recorded as a
  session event.

## Your AI delegation ledger: `digest`

`digest` answers a question the raw record can't at a glance — *what did I delegate
to my agents today / this week, per project?* It aggregates **every recorded
session** in a time window, groups them by project (cwd), and prints one
shareable markdown (or `--json`) ledger.

```bash
agent-witness digest              # all recorded history
agent-witness digest --today      # sessions started in the current UTC calendar day
agent-witness digest --week       # last 7 UTC calendar days, including today
agent-witness digest --since 72h  # relative look-back (s/m/h/d/w)
agent-witness digest --json       # same data, machine-readable
```

Each project section reports session count and summed duration, user prompts,
tool calls, distinct files touched, Bash commands, destructive-class command-flag
counts by severity, and per-model token totals. The window flags are mutually
exclusive; the header states the exact bounds (e.g. `UTC today, 2026-07-11
00:00:00Z to 2026-07-11 14:32:10Z`).

**Facts only — judgments live elsewhere (deliberately).** The digest reports
counts, durations, and token totals and makes **no** claim about efficiency,
model choice, or command safety. Command-flag counts are pattern-matcher hits,
not verdicts (see [Flagged commands](#flagged-commands)). This boundary is
intentional: waste/oversized-model judgments belong to a future scheduled-audit
agent layer, never the CLI (see NON-GOALS.md).

**Honest time base and honest gaps.**

- **UTC calendar windows, not local time.** `--today` / `--week` use UTC day
  boundaries to match the rest of the tool's UTC-only time display, so the same
  command is deterministic regardless of where you run it.
- **Included by session start, whole totals counted.** A session is included when
  its *start* falls inside the window; its entire totals are then counted. Usage
  sidecars are per-session and cannot be sliced to a sub-window, so a long
  session that merely *touched* the window is not partially attributed.
- **Usage unavailable is not zero.** A session with no `usage.json` sidecar is
  counted as "usage unavailable" and contributes **zero** to token totals — a
  state kept distinct from a session that genuinely used zero tokens.
- **User prompts only.** The prompt count includes only hook-sourced prompts; the
  transcript adapter's `Prompt` events (assistant prose) are excluded.
- **Attribution is disclosed.** A session is grouped by its **first** observed
  cwd; sessions that span several directories are counted in `multi_cwd_sessions`
  (never silently attributed), a session with no observed cwd lands in the
  `unknown project` bucket, and skipped corrupt lines are surfaced.

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
