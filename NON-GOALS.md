# NON-GOALS

- Sandboxing / blocking (observation & audit only; v0.2 policy layer is hooks-based and still not a security boundary)
- Being an agent or agent framework
- v0.1: PTY wrapping, fs watching, process polling, socket-level network observation (deferred: v0.2/v0.3)
- Catching side effects inside `Bash(...)` scripts in v0.1 — hooks cannot see them; we document this gap instead of hiding it
- Windows first-class support in v0.1
- Usage / token / cost aggregation in `top` and elsewhere — that turns the tool into a metrics dashboard (abtop's space) and away from honest observation. `top` shows what is *happening now*, not what it costs.
- In-app split panes for watching several sessions at once — use an external multiplexer (Cmux/tmux) with `show --follow` instead (see docs/recipes/cmux.md)
