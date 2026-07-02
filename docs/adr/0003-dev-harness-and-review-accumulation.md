# ADR-0003: Development harness and review accumulation

- Status: Accepted / Date: 2026-07-02

## Context
Same rationale as avatar-core ADR-0010: grow Rust capability by accumulating valid review findings into rules/lints/skills, with an issue-driven autonomous harness.

## Decision
- Issue-driven development via `/dev` and `/dev-all`; harness self-contained in `.claude/`.
- Primary merge gate is `just verify` locally; GitHub Actions CI mirrors the same gate because this repository will become a public OSS where a green CI badge is a trust signal (deliberate divergence from avatar-core's PoC no-CI stance).
- Valid review findings are promoted into `.claude/rules/` and lints; promotion/retirement criteria: a finding recurs twice → rule; a rule unused for 3 months → retire.
- Golden fixtures (sanitized real session events) are the deterministic verification core; they double as canaries for upstream Claude Code payload changes.

## Consequences
Upside: precision grows over time, reusable across rioX432 repos. Downside: fixture sanitization needs discipline (machine check + checklist, issue #8).
