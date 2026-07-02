---
name: investigate
description: "Investigate codebase: trace data flows, map dependencies, assess impact — report only, no implementation"
argument-hint: "<topic, feature, or issue>"
user-invocable: true
allowed-tools:
  - Read
  - Glob
  - Grep
  - Agent
  - Bash(git log:*)
  - Bash(git diff:*)
  - Bash(git blame:*)
  - Bash(gh issue view:*)
  - TaskCreate
  - TaskUpdate
  - TaskList
  - TaskGet
  - AskUserQuestion
---

# /investigate — Codebase Investigation

Investigate how something works, trace data flows, map dependencies, and assess impact. Report findings only — no implementation, no proposals.

**Use when:** "How does X work?", "What would be affected if we change Y?", "Trace the auth flow end-to-end", "What's the current state of module Z?"

**Not for:** Web research (`/think`), pre-implementation ambiguity resolution (`/dig`), finding bugs (`/audit`)

## Process

```
/investigate <topic>
  │
  Phase 1: Scoping
  │  └─ Define investigation axes and entry points
  │
  Phase 2: Deep Investigation (Explore agents, parallel)
  │  ├─ Code structure & architecture
  │  ├─ Data flow tracing
  │  ├─ Dependency mapping
  │  └─ Test coverage assessment
  │
  Phase 3: Structured Report
  │  ├─ Architecture overview
  │  ├─ Data flow
  │  ├─ Affected files & dependencies
  │  ├─ Existing patterns
  │  ├─ Risks / concerns
  │  └─ Open questions
  │
  ★ Done. No implementation, no proposals.
```

---

## Phase 1: Scoping

**Goal**: Define what to investigate and where to start.

1. Parse `$ARGUMENTS`:
   - **Issue reference** (`#42`, `PGR-1234`): Fetch issue details, extract keywords and affected areas
   - **Feature/module name**: Identify entry points via Grep/Glob
   - **Free-form question**: Extract key terms, identify likely code areas

2. Decompose into 2-4 investigation axes. Examples:
   - Architecture: Where does this code live? What layer/module?
   - Data flow: How does data enter, transform, and exit?
   - Dependencies: What depends on this? What does this depend on?
   - History: How has this evolved? Recent changes?

3. Identify entry points for each axis (files, classes, functions)

4. TaskCreate for each axis to track progress

---

## Phase 2: Deep Investigation

**Goal**: Thoroughly read and trace relevant code. No speculation.

Launch Explore agents in parallel for each axis:

### Agent template

```
Agent(
  subagent_type: "Explore",
  prompt: "Investigate {axis} for {topic} in this codebase.

  Entry points: {identified files/symbols}

  You MUST:
  1. Read every relevant file — no guessing
  2. Trace calls and data flow through actual code paths
  3. Note public API surfaces and internal boundaries
  4. Check for tests — list what's tested and what's not
  5. List all files involved with their role

  Report: structured findings with file:line references."
)
```

### Investigation checklist

For each axis, the agent must cover:

- [ ] **Read the code**: Every involved file, not just entry points
- [ ] **Trace the flow**: Follow function calls, event handlers, data transformations
- [ ] **Map boundaries**: Module boundaries, public vs internal APIs
- [ ] **Check tests**: Existing test files, what's covered, what's missing
- [ ] **Check history**: `git log` / `git blame` for recent changes and context

### Think Twice

After receiving agent reports:
1. Did the agents actually read the code, or did they speculate?
2. Are there code paths or edge cases not covered?
3. Do the findings from different axes contradict each other?

If gaps remain, launch follow-up Explore agents for specific areas.

---

## Phase 3: Structured Report

**Goal**: Organize findings into a clear, actionable report.

Present to the user:

```markdown
## Investigation: {topic}

### Architecture Overview
- Component/module structure relevant to the topic
- Layer boundaries and responsibilities
- Key abstractions and interfaces

### Data Flow
- Entry point → processing → output (with file:line references)
- State management involved
- Side effects (DB writes, API calls, file I/O)

### Affected Files
| File | Role | Lines | Notes |
|------|------|-------|-------|
| `src/...` | Entry point | 45-120 | Handles X |
| `src/...` | Data layer | 10-80 | Queries Y |

### Dependencies
- **Upstream** (what calls this): [list with file:line]
- **Downstream** (what this calls): [list with file:line]
- **External** (libraries, APIs, services): [list]

### Existing Patterns
- How similar features are implemented in this codebase
- Conventions observed (naming, error handling, testing)

### Test Coverage
- Existing tests: [list with file paths]
- Covered scenarios: [list]
- Missing coverage: [list]

### Risks / Concerns
- Potential issues discovered during investigation
- Complexity hotspots
- Missing error handling or edge cases

### Open Questions
- Things that could not be determined from code alone
- Areas that need user clarification
```

**Rules:**
- Every claim must reference a specific `file:line`
- Distinguish facts (read from code) from inferences
- If something is unclear, put it in Open Questions — do not guess

---

## Error Handling

| Situation | Action |
|-----------|--------|
| Issue not found | Report error, ask for clarification |
| Entry points unclear | Grep broadly, ask user if ambiguous |
| Agent returns shallow results | Re-launch with more specific prompts |
| Codebase too large for full trace | Scope down, report what was covered and what was skipped |
