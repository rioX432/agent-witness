# NON-GOALS

- Sandboxing / blocking (observation & audit only; v0.2 policy layer is hooks-based and still not a security boundary)
- Being an agent or agent framework
- v0.1: PTY wrapping, fs watching, process polling, socket-level network observation (deferred: v0.2/v0.3)
- Catching side effects inside `Bash(...)` scripts in v0.1 — hooks cannot see them; we document this gap instead of hiding it
- Windows first-class support in v0.1
