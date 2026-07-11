# NON-GOALS

- Sandboxing / blocking (observation & audit only; v0.2 policy layer is hooks-based and still not a security boundary)
- Being an agent or agent framework
- v0.1: PTY wrapping, fs watching, process polling, socket-level network observation (deferred: v0.2/v0.3)
- Catching side effects inside `Bash(...)` scripts in v0.1 — hooks cannot see them; we document this gap instead of hiding it
- Windows first-class support in v0.1
- Live cost metering in `top` — `top` stays an activity view (what is *happening now*), not a spend dashboard. **Revised 2026-07-11:** retrospective usage digests over the recorded store (tokens, models, per-project delegation reports — "what did my agents build, on which model, at what cost") are IN scope; the facts come from the transcript with `observed` attribution, and any wasteful/oversized-model *judgment* stays in the agent layer, never asserted by the CLI as fact.
- In-app split panes for watching several sessions at once — use an external multiplexer (Cmux/tmux) with `show --follow` instead (see docs/recipes/cmux.md)
