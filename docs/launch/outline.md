# Launch article — outline (DRAFT for rio's sign-off)

> Status: draft outline (issue #51). Final copy is rio's to write/approve — this
> is the narrative skeleton and beat sheet, not finished prose. Honesty guardrails
> below are load-bearing, not decoration; keep them when writing.

## Working titles

- "Did your agent actually do what it said?"
- "The audit trail for AI coding agents"
- "I don't distrust my coding agent. I just check." (verification-tax angle)

## The arc, in one line

Open on the visceral disaster wedge (a destructive command you have to
reconstruct), then turn: the real, *daily* value isn't disaster forensics — it's
the verification tax on almost-right output. Land on the daily surfaces and the
honest-observation identity.

---

## 1. Cold open — the wedge (high-virality, episodic)

- The scene: you come back to `rm -rf` in the scrollback (or a force-push, a
  dropped table). What actually happened, in what order, to which files?
- `agent-witness show @last` / `report` replays the recorded timeline: the exact
  tool calls, the flagged destructive command, critical-first.
- **Honesty beat (do not skip):** the record shows the *command*, not its side
  effects — `Bash("script.sh")` internals are invisible (ADR-0001). Say this out
  loud in the article. The wedge earns trust precisely by *not* overclaiming.
- Purpose of the section: emotional hook + memorable demo, explicitly framed as
  the *rare* case, so the pivot lands.

## 2. The turn — disasters are rare; the tax is daily

- Pull the research thread (docs/research/2026-07-11-solo-dev-pain.md): the #1
  pain in every 2025 survey is the *verification cost of almost-right output*,
  not catastrophe. "Almost right but not quite." Distrust > trust.
- Reframe the product: not a black-box recorder for when things explode — the
  ground-truth record you check every "done, tests pass" against.
- The honest scope of *today's* claim-check: the record shows which commands ran
  and their `ok`/`failed`/`no-result` status; **you** do the contrast. (Tease the
  automated claim-vs-reality section as a design direction — ADR-0005 — not a
  shipped feature. Never imply the tool adjudicates truth today.)

## 3. The daily surfaces (the body)

Each: what question it answers, one command, one honest caveat.

- **Statusline** (`init --statusline`) — *is it even recording?* Always-on
  `● witness 42ev` / `○ not recording`. A silently broken hook becomes visible,
  not discovered weeks later.
- **`/witness` skill** — *ask your agent what it just did.* Convenience view.
  **Trust-boundary caveat (load-bearing):** the agent narrating its own audit
  trail is the same kind of agent the record observes — for incident review, open
  the record yourself. Do not soften this.
- **`report` + flagged commands** — *the shareable audit you paste into a PR.*
  Flags are class-only ("worth a second look"), never intent or outcome.
- **`digest` — the delegation/usage ledger** — *where did my agent time and quota
  actually go?* Per-project sessions, duration, tool calls, per-model token
  totals. Ties to Pain #2 ("where did my quota go"). Honest boundary: facts only,
  no waste/efficiency verdicts; not a live cost meter (NON-GOALS.md).
- (Optional) **`inventory`** — configured vs actually-used MCP servers/skills;
  attack-surface accounting. Include only if the article isn't already long.

## 4. The identity — why honest observation is the whole moat

- The one differentiator: we never claim more than we observed. `direct |
  observed | inferred` on every event; overclaiming is the one unforgivable bug
  (ADR-0002).
- Position against the two failure modes on either side:
  - vs. overclaiming indie tools ("see everything", "AI slop"): we document every
    coverage gap instead of papering over it.
  - vs. prevention/sandboxing (Anthropic ships an official sandbox): we don't
    block, we observe and audit. Different job, stated plainly.
- The engineering-ownership tone: less punchy copy, on purpose.

## 5. Close — building in public

- Where it is: pre-v0.1, building in public; zero-privilege install (`init` and
  you're recording — no sudo, no entitlements).
- Install one-liner + repo link.
- Honest forward look: v0.2 direction (claim-vs-reality section, ADR-0005) framed
  as *where this is going*, not *what it does now*.

---

## Honesty guardrails (keep these while writing — non-negotiable)

- Never say or imply the tool detects lies or adjudicates "the agent lied". It
  shows the record; the reader judges.
- Never imply coverage the tool doesn't have: script internals, `rm` side
  effects, and non-hooked activity are invisible. State the gaps in-line.
- `no-result` is genuine ambiguity (a failed Bash fires no completion hook), never
  "failed".
- `digest` is retrospective facts, not a live cost/usage meter — don't blur it
  into a metrics-dashboard pitch.
- The `/witness` trust boundary must survive the edit: an agent narrating its own
  audit trail is not the trusted read path.
- Don't headline a v0.2 design spike (ADR-0005) as a shipped v0.1 capability.
