---
name: pr
description: "Create a pull request for the current branch using the project's PR template"
user-invocable: true
disable-model-invocation: true
allowed-tools:
  - Read
  - Grep
  - Glob
  - Bash(git status)
  - Bash(git diff:*)
  - Bash(git log:*)
  - Bash(git push:*)
  - Bash(gh pr create:*)
  - Bash(gh issue view:*)
  - Bash(agent-witness report:*)
  - Bash(agent-witness ls:*)
---

# /pr — Pull Request Creation

Create a pull request for the current branch.

## Steps

1. `git status` and `git diff` to understand all changes
2. Check if the current branch has a remote; push with `-u` if needed
3. `git log` and `git diff <base>...HEAD` to understand ALL commits
4. Look up the related GitHub Issue from branch name or commit messages
5. Read `.github/pull_request_template.md` if it exists
6. Generate changelog entry from commits (see below)
7. Generate the session audit attachment (see below)
8. Create PR with `gh pr create`, include changelog entry and audit attachment in body

## Changelog Generation

From step 3 commits, generate a changelog entry categorized by type:

```
### Changelog
- **Added**: {new features}
- **Changed**: {modifications to existing features}
- **Fixed**: {bug fixes}
- **Removed**: {removed features}
```

Include this in the PR body after the description. If a `CHANGELOG.md` exists in the project root, prepend the entry under the `## [Unreleased]` section.

## Session Audit Attachment (dogfooding)

Attach the agent-witness record of the work session to the PR body — every PR
ships with the evidence of how it was made (Core Value: evidence you can share).

1. Run `agent-witness report` (no argument: the latest session for this
   directory). If it clearly isn't the session that produced this branch
   (check the tool calls against the diff), pick the right one via
   `agent-witness ls` and a selector; if none matches, skip the attachment and
   say so in the PR body — never attach an unrelated session's record.
2. **Sanitize check (required, human-visible):** read the report before
   attaching. It contains local paths and command lines. Remove or redact
   anything that must not be public (home-directory usernames are acceptable
   for this repo; secrets, tokens, or unrelated project paths are not). If in
   doubt, ask before attaching.
3. Append it to the PR body inside a collapsed block:

```markdown
<details>
<summary>Session audit (agent-witness)</summary>

{report markdown}

</details>
```

If `agent-witness` is not installed or no session was recorded, skip the
attachment and note that honestly in the PR body instead of omitting silently.

## Rules

- Title: `#{Issue} {concise description}` (under 70 chars)
- If no issue: omit the number
- Description: bullet points summarizing changes
- Other template sections: leave as-is
- No AI stamps, no Co-Authored-By
- Always set base branch explicitly
- Link issues with `Closes #XX` in body if applicable
