# ADR-0002: Attribution honesty — direct / observed / inferred on every event

- Status: Accepted / Date: 2026-07-02

## Context
Audit tools lose all credibility the first time they overclaim. User-space observation cannot always prove the agent caused an effect (e.g. an fs change during a session window). The r/rust community specifically punishes tools whose claims exceed their implementation ("AI slop" pattern: grand claims, thin substance).

## Decision
Every AgentEvent carries `attribution: direct | observed | inferred` plus `confidence` and, for correlation-based events, `correlation_window_ms` and `raw_event_ref`. Marketing copy, README, and report output never claim more than the strongest attribution actually present. The report always includes an observation-scope disclaimer.

## Consequences
- Upside: differentiator vs. both overclaiming indie tools and prevention-focused official sandboxes; aligns the product with the launch narrative (engineering ownership).
- Downside: less punchy copy ("see everything" is forbidden). Accepted deliberately.
