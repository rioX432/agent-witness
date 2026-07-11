# Recipe: a weekly usage audit that arrives on a schedule

agent-witness records what your agents did; `digest` rolls it into a factual
cross-session ledger. This recipe wires a **weekly scheduled agent** that runs
that digest for you and narrates what it means — wasteful interaction loops,
oversized-model usage for mechanical work, top projects by delegated work,
notable destructive-command flags — so the audit lands in your lap every week
with zero commands to remember.

## The one thing to get right: facts vs. judgment

This split is the whole point, and it is load-bearing (NON-GOALS.md, Core Value
1 — honest observation):

- **`agent-witness digest --json` is DETERMINISTIC FACTS ONLY.** Counts,
  durations, per-model token totals, and destructive-class command-flag *counts*.
  The CLI never says a loop was "wasteful", a model was "oversized", or a command
  was "risky". Flag counts are pattern-matcher hits over command text, not
  verdicts.
- **The JUDGMENT is the agent's.** "Wasteful", "oversized", "worth a look" are
  interpretations layered on top of the facts by the scheduled agent. This recipe
  is exactly where that interpretation lives — outside the CLI, clearly owned by
  the agent, never presented as a CLI claim.

Keep that boundary visible in the report the agent writes: quote the numbers as
facts, and label every characterization as the agent's own reading.

## Set up the schedule

The scheduling mechanism is Claude Code's own — you do **not** invent a cron
wrapper or a new CLI flag. In a Claude Code session, run the `/schedule` skill
and ask for a weekly routine. It distills your request into a scheduled task
(a routine that runs autonomously on a cron schedule via `create_scheduled_task`
with a `cronExpression`); manage existing ones with `list_scheduled_tasks` /
`update_scheduled_task`. Consult `/schedule` for the exact cron format your
version accepts — a weekly cadence is a standard 5-field expression like
`0 9 * * 1` (09:00 every Monday).

The routine runs cold with no memory of the session that created it, so its
prompt must be entirely self-contained. Give `/schedule` a prompt along these
lines:

```
Objective: produce this week's AI-usage audit from the local agent-witness record.

Steps:
1. Run: agent-witness digest --week --json
2. Optionally run: agent-witness inventory --json   (configured vs. actually-used MCP servers / skills)
3. From the JSON FACTS only, write a short markdown report that narrates:
   - wasteful interaction loops (e.g. many near-identical commands / high tool-call
     counts for little delegated work)
   - oversized-model usage for mechanical work (large-model token totals against
     sessions that were mostly routine Bash/file edits)
   - top projects by delegated work (sessions, tool calls, tokens)
   - notable destructive-command flags (quote the count and severity)

Rules:
- The digest gives FACTS. Every "wasteful" / "oversized" / "worth reviewing" call
  is YOUR judgment — say so explicitly; never attribute it to the CLI.
- If a session's usage sidecar is unavailable, the digest says so — report that
  honestly (absent usage is NOT zero tokens), do not guess its model or tokens.
- This report is a convenience summary, not the trusted read path (see below).
```

## Trust boundary (do not drop this)

The scheduled audit is **an agent narrating an audit record** — a convenience
view, **not** the trusted read path. This matters twice over here, because the
audit agent is the *same kind of agent* the record observes: a compromised or
mistaken agent could narrate its own trail favorably. For anything that matters —
incident review, suspected prompt injection, a flag you need to actually trust —
open the record directly in your own terminal:

```bash
agent-witness show <selector>      # timeline you read yourself
agent-witness report <selector>    # the shareable record, not an agent's retelling
```

Have the weekly report end by pointing at those commands, so the reader is always
one step from the primary evidence.

## Example weekly report

The factual half below was **produced for real** by `agent-witness digest --week`
from a small, synthetic-by-construction store (placeholder projects `acme-web` /
`data-pipeline`, invented sessions and token counts — no real paths or secrets).
It is the deterministic CLI output the scheduled agent starts from:

```text
# Delegation digest

_A factual, cross-session ledger of what your agents did, grouped by project. Observation only: every number derives from recorded hook events._

Digest window: last 7 UTC calendar days, 2026-07-05 00:00:00Z to 2026-07-11 14:54:10Z

## Overall totals

- Projects: 2
- Sessions: 3
- Duration (summed): 18m00s
- Prompts (user, hook-sourced): 3
- Tool calls: 15
- Commands (Bash): 8
- Files touched (distinct): 2
- Command flags by matcher severity: critical 0, warning 1
- Token totals from available usage sidecars:
  - claude-fable-5: input 128000, output 24000, cache-creation 5000, cache-read 210000
  - claude-opus-4-8: input 45000, output 9000, cache-creation 12000, cache-read 60000

## Projects

### acme-web (`/work/acme-web`)

- Sessions: 2
- Duration (summed): 9m00s
- Prompts (user, hook-sourced): 2
- Tool calls: 7
- Commands (Bash): 3
- Files touched (distinct): 1
- Command flags by matcher severity: critical 0, warning 1
- Models used: claude-opus-4-8
- Token totals from available usage sidecars:
  - claude-opus-4-8: input 45000, output 9000, cache-creation 12000, cache-read 60000
- Usage unavailable: 1 session(s)
- Multi-cwd sessions: 0 (attributed to their first observed cwd)

### data-pipeline (`/work/data-pipeline`)

- Sessions: 1
- Duration (summed): 9m00s
- Prompts (user, hook-sourced): 1
- Tool calls: 8
- Commands (Bash): 5
- Files touched (distinct): 1
- Command flags by matcher severity: critical 0, warning 0
- Models used: claude-fable-5
- Token totals from available usage sidecars:
  - claude-fable-5: input 128000, output 24000, cache-creation 5000, cache-read 210000
- Usage unavailable: 0 session(s)
- Multi-cwd sessions: 0 (attributed to their first observed cwd)

## Honesty surfaces

- Usage unavailable: 1 session(s) — token totals exclude these; an absent sidecar is not zero tokens.
- Unknown-project sessions: 0 — no cwd was observed in the session's events.
- Multi-cwd sessions: 0 — attributed to their first observed cwd; the count is disclosed per project.
- Corrupt/skipped lines: 0 — unreadable JSONL lines across included sessions.

## Scope

- This digest is FACTS ONLY: counts, durations, and token totals. It makes no judgment about efficiency, model choice, or command class — that belongs to the agent/audit layer, not the CLI.
- Command flag counts are destructive-class pattern-matcher hits over recorded command text, not policy judgments or verdicts.
- A session is included when its start falls inside the window; its whole totals are counted. Usage sidecars are per-session and cannot be sliced to a sub-window.
- All times are UTC.
```

The agent then layers its **judgment** on those facts. Everything below is the
agent's own reading, not CLI output (illustrative):

> **Weekly AI-usage audit** — 2 projects, 3 sessions this week.
>
> - **Oversized model, worth a look (my judgment):** `data-pipeline` ran one
>   session of mostly mechanical Bash (5 commands, a config read, one file write)
>   on `claude-fable-5` — 128k input / 210k cache-read tokens. The digest only
>   reports the model and totals; the "oversized for this work" call is mine.
> - **Top project by delegated work:** `acme-web` (2 sessions, 7 tool calls),
>   though `data-pipeline` carried the heavier token load.
> - **Destructive-command flag:** 1 warning-severity hit in `acme-web` — the
>   matcher flagged a command's *class* (a force-push), not intent. Confirm it
>   yourself: `agent-witness report acme-web-sess-01`.
> - **Coverage caveat:** 1 `acme-web` session had no usage sidecar, so its tokens
>   are excluded — that is unavailable, not zero.
>
> This is a convenience summary. For incident review, read the record directly
> with `agent-witness show` / `report`.

## Notes

- `digest --week` is the last 7 UTC calendar days including today; a session is
  included by its **start** time and contributes its whole totals (usage sidecars
  are per-session and can't be sliced to a sub-window).
- Prefer `--json` for the routine (stable to parse); `digest --week` markdown, as
  shown above, is the human-readable equivalent of the same facts.
- No daemon is involved — `digest` reads the on-disk store, so the routine works
  whether or not `agent-witness watch` is running.
