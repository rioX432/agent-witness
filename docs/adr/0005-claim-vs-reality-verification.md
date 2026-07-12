# ADR-0005: Claim-vs-reality — claim-aware presentation, not claim extraction

- Status: Proposed (design spike, issue #47) / Date: 2026-07-12

## Context

The top pain in every 2025 developer survey is the verification tax on
almost-right agent output (docs/research/2026-07-11-solo-dev-pain.md, Pain #1;
sharpened by Pain #3, reward hacking — mocked tests, swallowed errors, deleted
failing tests). The recorded hook stream is the ground truth of what actually
executed, so agent-witness is uniquely placed to help a human answer **"did the
agent actually do what it said?"** by putting the agent's final claim next to
the record.

This is the sharpest possible expression of Core Value 1 (honest observation)
and simultaneously its sharpest hazard. The failure mode is a hair's breadth
away: a section that reads the final message, sees no successful `cargo test`,
and concludes *"the agent lied about tests passing"* has committed the one
unforgivable bug — overclaiming (ADR-0002). Success recorded no evidence is not
the same as failure. Absence of a result is genuinely ambiguous (a non-zero-exit
Bash fires no completion hook — ADR-0001, issue #8), and side effects inside
`bash script.sh` are invisible. The design problem is therefore not "how do we
detect lies" but **"how do we present evidence next to a claim without ever
adjudicating the claim's truth."**

Feasibility (confirmed against the code, not assumed): **no new capture is
required.** The normalizer already persists everything this section reads —
`ToolCall` retains full `tool_input` (Bash `command`, Write `content`, Edit
`new_string`, `file_path`); `ToolResult` retains `tool_response` + `duration_ms`;
`Stop` retains `last_assistant_message`. `report`/`timeline` simply do not read
these fields yet. This is a presentation change over already-recorded data.

Design consulted with Codex (2026-07-12): the recommendation below is its
verdict, adopted.

## Decision

Ship a report section — **"Final message vs recorded evidence"** — that does
**claim-aware presentation, never claim extraction.** It places relevant
execution evidence beside the verbatim final message and lets the reader draw
the conclusion. It never converts prose into a normalized claim, and never
issues a contradiction verdict.

### Claim-extraction source

The agent's final claim is the `last_assistant_message` of the **last `Stop`
event** in the session (Stop fires per turn; the last one before `SessionEnd` is
the session's final word; resumed sessions may append more Stops after an END and
the rule still holds — last Stop wins). It is shown **verbatim**, never parsed
into structured claims. When absent (Claude Code did not populate it, or the
session has no Stop), the section says so plainly and shows the evidence panel
alone — absence of a final message is never treated as a claim.

### Detection list (evidence panel — directly/observedly recorded only)

Each item below is a **fact the record carries**, not an inference about truth:

| Evidence | Source | Honesty note |
|---|---|---|
| Test/build/lint commands seen | Bash `tool_input.command`, token-matched like `flags.rs` | Class match on recorded text; blind to script internals |
| Each command's outcome | timeline pairing → `ok` / `failed` / `no-result` | `no-result` stated as ambiguous, never as failure |
| Count of unpaired (`no-result`) calls | timeline | "outcome not observed", never "failed" |
| Test files written/edited | Write/Edit `tool_input.file_path` + path heuristic | A referenced path is not a confirmed change to test behavior |
| Observation gaps | constant, always shown | Script side effects, deletions via `rm`, are not observed |

**Deferred from the first cut: mock/skip signatures.** They are text-matchable
(`tool_input.content` is retained) but the meaning is noisy — `mock`, `skip`,
`#[ignore]`, `todo` are frequently legitimate, pre-existing, or unrelated.
Shipping them first would make the section read as a *suspicion engine*, which
directly conflicts with the honest-observation differentiator. They may return
later as a separately labelled, opt-in "text-pattern hints" panel if real demand
appears — never mixed into the factual evidence panel.

### Precision policy (the honest/dishonest line)

The CLI **may** state, as fact:
- what the final message **literally contained** ("Final message mentions tests");
- what the record **directly/observedly** contains ("Recorded test-like commands:
  `cargo test --workspace` — no-result");
- weak routing cues that are pure restatements of the two above ("No successful
  test-like command was recorded").

The CLI **must never**:
- parse prose into a normalized claim ("agent claimed all tests pass");
- say "the claim is false / contradicted / the agent lied / tests did not pass";
- infer command success from the **absence** of a result;
- infer filesystem side effects from shell text inside a script;
- treat "no successful test command recorded" as "tests did not pass".

The bright line: **the CLI can state what the final message literally said and
what the record literally contains; it cannot adjudicate the truth of the prose
unless that truth is exactly the recorded fact itself.** Acceptable —
*"Final message mentions tests. Recorded: `cargo test` had no result; outcome not
observed."* Forbidden — *"Agent claimed tests passed, but they were not run."*
Even where a human would reach the second sentence, the CLI stops at the first
and leaves the judgment to the reader.

### Output shape

A new optional `SessionReport` field (e.g. `claim_vs_reality: Option<...>`)
carried **identically** in the markdown and JSON exporters (the ADR-0002 parity
invariant tests already guard this). Rendered as a distinct section only when a
final message or test-like activity is present; each evidence row carries its
`attribution` like every other section. The existing observation-scope disclaimer
stays mandatory and unchanged. Later extends to `digest` (issue #45) on the same
facts-only footing.

## Consequences

- Upside: the feature the launch narrative promises (claim-vs-reality) ships on
  the durable, high-confidence half — actual-execution evidence beside the claim
  — with the honesty invariant intact and provable by golden tests.
- Upside: zero new capture; a presentation-only change over recorded data.
- Downside: deliberately *passive*. It will not tell the reader "the agent lied";
  some users will want that verdict. Refused on purpose — that verdict is exactly
  the overclaim ADR-0002 forbids. Accepted.
- Downside: test/build command matching is best-effort on command text (same
  documented gap as `flags.rs`); a novel test runner is missed until its pattern
  is added. Stated in the section caveat, never papered over.

## Follow-up implementation issues (to be filed after review)

1. **Evidence panel + verbatim final message** — the durable core: last-Stop
   extraction, test/build/lint command matcher (extend `flags.rs`-style module),
   outcome + no-result surfacing, test-file heuristic, section in both exporters,
   golden tests asserting the precision policy (no forbidden phrasings).
2. **`digest` extension** — same facts across sessions (issue #45 footing).
3. **(Deferred / demand-gated)** opt-in "text-pattern hints" panel for
   mock/skip signatures, separately labelled from the factual evidence.
