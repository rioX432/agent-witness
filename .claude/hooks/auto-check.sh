#!/bin/bash
# Auto-run cargo check after file edits to catch compile errors early.
# Runs asynchronously to not block the agent.

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

# Only check if Rust files were modified
if git diff --name-only HEAD 2>/dev/null | grep -qE '\.rs$|Cargo\.toml$'; then
  cargo check --workspace 2>&1 | tail -5
fi
