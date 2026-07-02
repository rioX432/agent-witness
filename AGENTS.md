# AGENTS.md

Read CLAUDE.md first — it is the source of truth for project overview, Core Values, Won't Do, commands, architecture, and gotchas.

Rules for agents:
- Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` before proposing a change as done
- Never weaken the attribution/honesty invariants described in CLAUDE.md Core Values
- Keep commits single-line English; no squash merges
