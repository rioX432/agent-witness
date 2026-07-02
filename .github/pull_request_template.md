## Description

<!-- What changed and why. -->

## Related Issue

Closes #

## Design decision note

<!-- The one non-obvious choice this PR makes, and why. (Launch-article material — one short paragraph.) -->

## Verification

- [ ] `just verify` is green (fmt + clippy `-D warnings` + build + nextest)
- [ ] New behavior is covered by tests (golden/TestBackend for TUI, fixture-driven where applicable)
- [ ] If fixtures were added/changed: sanitized via `tools/fixtures/`, checklist walked, `provenance.json` truthful
- [ ] Honesty invariants intact (attribution, corrupt-line counts, report disclaimer)
