# Code Review Guidelines

## Always check

### Correctness
- Null/nil safety on external data (API responses, DB results, user input)
- Missing error handling (empty catch blocks, swallowed exceptions)
- Resource leaks (open connections, streams, listeners not cleaned up)
- Race conditions (shared mutable state without synchronization)
- Off-by-one errors in loops and boundary conditions

### Security
- Hardcoded secrets, API keys, passwords, tokens
- Sensitive data in logs or error messages
- User input not sanitized (SQL injection, XSS, path traversal)
- HTTP instead of HTTPS for sensitive data

### Architecture
- Violation of project architecture defined in CLAUDE.md
- Business logic in wrong layer (e.g., UI layer contains domain logic)
- Direct dependency on concrete implementations where abstraction is expected

## Style

<!-- Add project-specific style rules below -->
<!-- Examples: -->
<!-- - Prefer early returns over nested conditionals -->
<!-- - Use structured logging, not string interpolation -->
<!-- - All public functions must have doc comments -->

## Skip

- Issues already caught by linters (detekt, ktlint, ESLint, swiftlint, clippy, etc.)
- Pre-existing issues in unchanged code
- Generated code
- Formatting / import ordering
- Translation / localization content

## Severity Definitions

| Severity | Definition | Action |
|---|---|---|
| **Critical** | Will cause crash, data loss, security vulnerability, or incorrect behavior | Must fix before merge |
| **Warning** | Potential bug, performance issue, or architectural violation | Should fix before merge |
| **Suggestion** | Improvement opportunity, not blocking | Consider for future |
| **Nit** | Style/preference, purely optional | Author's discretion |

## False Positive Reduction

- Do NOT flag findings you cannot verify from the code context
- If unsure whether something is intentional, ask rather than flag
- Check if a pattern is established elsewhere in the codebase before flagging
- Prefer fewer, high-confidence findings over many uncertain ones
