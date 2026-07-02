# Test Strategy

- **Golden fixtures first** (issue #8): sanitized hook-event JSONL from real Claude Code sessions is the shared input for store, receiver, TUI, and report tests. Fixtures are versioned and act as canaries for upstream payload changes.
- **Unit**: event model round-trip, settings.json merge (3 states), corrupt-line tolerance.
- **TUI**: ratatui TestBackend golden screens (empty / basic / failure / huge-payload sessions).
- **Integration**: emit → socket → store; emit fallback path without daemon.
- **Output lint**: report must always contain the observation-scope disclaimer (ADR-0002).
- Run everything via `just verify`.
