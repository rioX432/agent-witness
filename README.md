# agent-witness

> A session recorder and audit log for AI coding agents.

`agent-witness init` → use Claude Code as usual → `agent-witness show` replays what the session actually did (tool calls, files, commands) as a TUI timeline, backed by a persistent JSONL audit trail.

**Status**: pre-v0.1, private development. Launch checklist lives in docs/launch.md.

- Observation only — this is not a sandbox and not a security boundary (see SECURITY.md)
- Every event is labeled `direct | observed | inferred` — we record what we saw, not what we guess
- Planned interop: export to Cursor's agent-trace format

## v0.1 scope

hooks receiver + JSONL session store + TUI timeline/replay + markdown report. See NON-GOALS.md for deliberate cuts.
