# Fixture Sanitization Checklist

`tools/fixtures/sanitize.py` does the mechanical pass. A human MUST still walk
this list before committing any fixture — leaking a real path, username, or
secret is a **Critical** bug (Core Value: honest observation).

The `fixtures_lint.rs` test enforces items 1-5 mechanically; the rest need eyes.

## Mechanical (also enforced by the lint test)

- [ ] No `/Users/...` or `/home/<name>` other than `/home/user`
- [ ] No `/private/tmp`, `/tmp/...`, or `/var/folders` absolute paths
- [ ] No real username, hostname, or machine name (`whoami`, `hostname`)
- [ ] No raw hex UUIDs — session/prompt/tool ids are stable placeholders
- [ ] No known secret/token patterns (`sk-`, `sk-ant-`, `gh*_`, `AKIA`, `xox*-`, Bearer)

## Manual (human review required)

- [ ] `prompt` / `last_assistant_message` free text: no real names, emails,
      company/project names, internal URLs, or ticket ids
- [ ] `tool_input.command` and `tool_response.stdout/stderr`: no environment
      variables, API responses, git remotes, or IPs that identify a person/org
- [ ] `tool_input.content` (file writes): no proprietary code or credentials
- [ ] Every JSON line still parses and the payload shape is unchanged
      (sanitization must not drop or rename hook fields)
- [ ] `provenance.json` states `real-captured` vs `synthetic` truthfully
