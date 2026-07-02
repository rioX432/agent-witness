# ADR-0001: Claude Code hooks as the canonical event source (v0.1)

- Status: Accepted / Date: 2026-07-02

## Context
An agent session recorder can observe via OS-level APIs (Endpoint Security, eBPF), PTY wrapping, fs watching, process polling, or the agent's own hook system. OS-level APIs require entitlements/root; PTY+polling misses short-lived processes; fs watchers have documented event-loss cases. Design review (Codex, 2026-07-02) rated the original 4-source v0.1 as infeasible for a part-time solo developer.

## Decision
v0.1 records **only** what Claude Code hooks report (PreToolUse/PostToolUse/PostToolUseFailure/Stop), received over a unix socket via an `emit` bridge. Raw hook JSON is persisted alongside normalized events. The transcript file (path taken from hook input, never globbed) is a best-effort, versioned, feature-flagged secondary adapter. PTY wrapping, fs watching, and process observation are deferred to v0.2; eBPF/Endpoint Security to v0.3.

## Consequences
- Upside: zero-privilege install, one-command adoption, 4-6 week v0.1 is realistic.
- Downside: side effects inside `Bash(...)` scripts are invisible in v0.1. We document this gap prominently (README, report footer) instead of hiding it — see ADR-0002.
