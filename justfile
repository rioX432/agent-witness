# agent-witness task runner

# All checks: format, lint, build
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo build --workspace

fmt:
    cargo fmt --all

build:
    cargo build --workspace

# Tests (nextest)
test:
    cargo nextest run --workspace

# Golden fixture tests for the event pipeline (lands with issues #1/#8)
test-fixtures:
    cargo nextest run -p agent-witness-core

# TUI golden-screen tests via ratatui TestBackend (lands with issue #5)
test-tui:
    cargo nextest run -p agent-witness

# Local verification gate (primary gate; CI mirrors it for the public repo).
# just runs dependencies left-to-right and aborts on the first failure.
verify: check test
    @echo "verify: all checks passed"
