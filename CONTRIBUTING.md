# Contributing to agent-witness

Thanks for your interest! This project is small and opinionated; this document
tells you what those opinions are so your PR lands smoothly.

## Ground rules

The project has three Core Values (see [CLAUDE.md](CLAUDE.md)); every change must
directly strengthen one:

1. **Honest observation** — never claim more than what was observed. Overclaiming
   is the one unforgivable bug. Don't weaken attribution, confidence, corrupt-line
   counts, or the report disclaimer.
2. **Zero-friction adoption** — `agent-witness init` and you're recording.
3. **Evidence you can share** — replayable records, pasteable reports.

Features outside these go to [NON-GOALS.md](NON-GOALS.md) — check it before
proposing: sandboxing/blocking, agent frameworks, and usage/token metering are
deliberate cuts, not oversights.

## Dev setup

```bash
rustup default stable
cargo install cargo-nextest just

just verify    # the merge gate: fmt + clippy -D warnings + build + nextest
```

If `just verify` is green, your change is mergeable — CI runs exactly the same gate.

## Conventions

- No `unsafe`. No `unwrap`/`expect` outside tests. `thiserror` in `witness-core`,
  `anyhow` in the binary. No magic numbers — name them.
- Determinism: no wall-clock or RNG inside pure paths — inject a `Clock`
  (see `witness-core/src/clock.rs`). TUI views are pure functions of app state,
  golden-tested against a fixed-size ratatui `TestBackend`.
- Code comments and commit messages in English. Commits: concise single line.
- PRs merge with a merge commit (no squash). Include a short design-decision note
  in the PR body — what you chose and why.

## Fixtures

`tests/fixtures/` contains **sanitized real-session captures** — they are both
test inputs and a canary for upstream hook-payload changes.

- Never commit unsanitized captures. Use `tools/fixtures/capture.sh` +
  `sanitize.py`, then walk [tests/fixtures/SANITIZE_CHECKLIST.md](tests/fixtures/SANITIZE_CHECKLIST.md).
- Record provenance honestly in `provenance.json`: real-captured vs synthetic.
  Never label synthetic data as a capture.
- `just test-fixtures` runs the mechanical sanitization lint.

## Reporting bugs & proposing features

Open a GitHub Issue. For features, include a **Core Value Alignment** section —
one step from the feature to a Core Value, no indirect reasoning. Security issues:
see [SECURITY.md](SECURITY.md) (email, not a public issue).
