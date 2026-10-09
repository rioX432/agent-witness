# Fixture tooling

Scripts to capture real Claude Code sessions and turn them into sanitized golden
fixtures under `tests/fixtures/`. This tooling (re)generates **real captures**;
hand-written, doc-derived scenarios must be marked `synthetic` in provenance
and listed in `tests/fixtures/README.md`.

## 1. Capture (`capture.sh`)

Points Claude Code hooks at a file-append command and runs a real headless
session, so each hook's stdin JSON is recorded verbatim (one JSON per line). The
dump is raw and UNSANITIZED.

```bash
# session-basic
tools/fixtures/capture.sh /tmp/dump-basic.jsonl /tmp/proj-basic \
  "Create hello.rs printing Hello, then run 'rustc --version'. Keep it minimal." 8

# session-with-failure (a genuinely failing Bash call + a succeeding diagnostic)
tools/fixtures/capture.sh /tmp/dump-failure.jsonl /tmp/proj-failure \
  "Run exactly 'cat missing-config.toml' (it will fail; do not chain commands). \
Then run 'ls -la'. Report that step 1 failed and stop." 10
```

Note: `--settings` adds to the user's global settings rather than replacing them,
so local hooks (e.g. a command proxy) may also fire in the dump. `sanitize.py`
normalizes those.

## 2. Sanitize (`sanitize.py`)

Mechanically strips machine/user/secret data and normalizes ids for
determinism. See the module docstring for the full rule list.

```bash
python3 tools/fixtures/sanitize.py --scenario session-basic \
  --project-root /tmp/proj-basic \
  --in  /tmp/dump-basic.jsonl \
  --out tests/fixtures/session-basic/hooks.jsonl
```

`--project-root` is the working directory used during capture (its `cwd`); it is
replaced with `/home/user/project`. Real home, username, and hostname are derived
automatically; add more with `--home/--user/--host`.

## 3. Verify

1. Run the manual checklist: `tests/fixtures/SANITIZE_CHECKLIST.md`
2. Update the scenario's `provenance.json`
3. `just test-fixtures` (the `fixtures_lint.rs` gate)

The capture hook set matches Claude `init`, including Notification,
PermissionRequest and PermissionDenied. `capture.sh` uses permission bypass
for its headless run, so registering those hooks does not guarantee that a
waiting signal is captured. The waiting fixtures are hand-written synthetic
examples; this script does not capture Codex sessions.
