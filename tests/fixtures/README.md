# Golden Fixtures

Sanitized, **real-captured** Claude Code hook payloads. These are the shared
input for every event-pipeline test (store, hooks receiver, normalizer, TUI,
report) — see `docs/test-strategy.md`. One `hooks.jsonl` line == one hook stdin
payload, verbatim from a real session except for mechanical sanitization.

## Provenance: REAL captures (not synthetic)

Both scenarios were recorded from real headless Claude Code sessions
(`claude -p ... --settings <hook-dump> --max-turns N`), captured with
Claude Code **2.1.198**, then run through `tools/fixtures/sanitize.py`.
Per-scenario details live in each `provenance.json`.

If a fixture is ever added synthetically (e.g. capture unavailable), it MUST set
`"provenance": "synthetic"` in its `provenance.json` and say so here. Never label
a synthetic fixture as real — honest observation is the project's Core Value.

## Scenarios

| Directory | Flow | Why it matters |
|---|---|---|
| `session-basic/` | Write a file, run `rustc --version`; both succeed | Happy path: every PreToolUse has a matching PostToolUse |
| `session-with-failure/` | A Bash call fails (missing file), a diagnostic succeeds | Failure path: a failed Bash call fires **no** PostToolUse (see below) |

## Transcript fixture (issue #4)

`session-basic/transcript.jsonl` is a sanitized, minimal subset of the **same**
real session's transcript JSONL, used to test the best-effort transcript adapter
(`witness-core` `transcript` module). Unlike `hooks.jsonl` (the canonical hook
stdin record), the transcript's internal schema is **not a stable contract** — the
adapter is versioned and best-effort. The adapter extracts only the assistant
*text* lines (context hooks never surface) as `Observed` events; every other line
type is recognized-but-skipped and counted in `TranscriptStats`. Provenance and
line inventory live in `session-basic/provenance.json` under `transcript`.

### Observed failure behavior (Claude Code 2.1.198)

A Bash tool call that exits non-zero produces a `PreToolUse` with **no matching
`PostToolUse`**. The failure is only observable from the missing pair (correlate
by `tool_use_id`) plus the `Stop` message. This is the observation gap ADR-0001
calls out; the failure fixture preserves it deliberately.

## Schema version & canary

`hook_payload_schema` (see `provenance.json`) tracks the observed hook payload
shape, independent of `AgentEvent`'s `SCHEMA_VERSION`. The fixture lint test
(`crates/witness-core/tests/fixtures_lint.rs`) asserts the expected field set per
`hook_event_name`. If a future Claude Code release renames or drops a hook field,
re-capturing and re-running the lint surfaces the diff — the fixtures double as a
**canary** for upstream payload changes.

## How to regenerate / add a scenario

See `tools/fixtures/README.md` for the capture + sanitize workflow, and run the
manual checklist in `SANITIZE_CHECKLIST.md` before committing any new fixture.
