#!/usr/bin/env python3
"""Sanitize a raw Claude Code hook-dump JSONL into a shareable golden fixture.

The recorder's Core Value is honest observation: fixtures are *real* captured
hook payloads, only mechanically stripped of machine-, user-, and secret-
specific data. This script performs the mechanical pass; a human still runs the
checklist in tests/fixtures/SANITIZE_CHECKLIST.md before committing.

Usage:
    sanitize.py --scenario session-basic \
                --project-root /abs/path/used/as/cwd/during/capture \
                --in  dump-basic.jsonl \
                --out ../../tests/fixtures/session-basic/hooks.jsonl

What it does (mechanical rules, applied to every string in every payload):
  1. Real home dir(s) -> /home/user
  2. Capture project root (the session cwd) -> /home/user/project
  3. transcript_path -> canonical /home/user/.claude/projects/<scenario>/<sid>.jsonl
  4. Local temp/scratch prefixes (/private/tmp, /tmp, /var/folders) -> /home/user/project
  5. Real username / hostname tokens -> user / host
  6. Local command-proxy wrapper ("rtk ") stripped from command strings
  7. Known secret/token patterns -> *-REDACTED (defensive; capture had none)
  8. Non-deterministic ids (session_id, prompt_id, tool_use_id, UUIDs) ->
     stable placeholders, so downstream golden tests stay reproducible.

Output is compact one-JSON-per-line with sorted keys, so re-captures diff
cleanly (the fixture doubles as a canary for upstream payload-shape changes).
"""

from __future__ import annotations

import argparse
import getpass
import json
import re
import socket
import sys
from pathlib import Path

# --- Secret patterns (prefix-anchored; conservative to avoid false positives) --
SECRET_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"sk-ant-[A-Za-z0-9_\-]{20,}"), "sk-ant-REDACTED"),
    (re.compile(r"sk-[A-Za-z0-9]{20,}"), "sk-REDACTED"),
    (re.compile(r"gh[pousr]_[A-Za-z0-9]{20,}"), "ghTOKEN-REDACTED"),
    (re.compile(r"github_pat_[A-Za-z0-9_]{20,}"), "github_pat_REDACTED"),
    (re.compile(r"AKIA[0-9A-Z]{16}"), "AKIA-REDACTED"),
    (re.compile(r"xox[baprs]-[A-Za-z0-9\-]{10,}"), "xox-REDACTED"),
    (re.compile(r"(?i)bearer\s+[A-Za-z0-9._\-]{20,}"), "Bearer REDACTED"),
]

# Lowercase 8-4-4-4-12 hex UUID.
UUID_RE = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
TOOLU_RE = re.compile(r"toolu_[A-Za-z0-9]+")
# Local temp/scratch absolute-path prefixes to collapse to the project root.
SCRATCH_RE = re.compile(r"(/private/tmp|/tmp|/var/folders)/[^\s\"']*")

HOME_PLACEHOLDER = "/home/user"
PROJECT_PLACEHOLDER = "/home/user/project"


class Sanitizer:
    def __init__(self, scenario: str, project_root: str, real_homes: list[str], real_users: list[str], real_hosts: list[str]):
        self.scenario = scenario
        # Longest-first so nested prefixes replace before their parents.
        self.project_root = project_root.rstrip("/")
        self.real_homes = sorted({h.rstrip("/") for h in real_homes if h}, key=len, reverse=True)
        self.real_users = sorted({u for u in real_users if u}, key=len, reverse=True)
        self.real_hosts = sorted({h for h in real_hosts if h}, key=len, reverse=True)
        # Deterministic id maps, populated in first-seen order.
        self._prompt_ids: dict[str, str] = {}
        self._toolu_ids: dict[str, str] = {}
        self._uuids: dict[str, str] = {}
        self._session_id: str | None = None
        self._session_placeholder = scenario

    def learn_session_id(self, sid: str) -> None:
        if self._session_id is None:
            self._session_id = sid

    def _map_prompt(self, value: str) -> str:
        return self._prompt_ids.setdefault(value, f"prompt-{len(self._prompt_ids) + 1:04d}")

    def _map_toolu(self, value: str) -> str:
        return self._toolu_ids.setdefault(value, f"toolu_{len(self._toolu_ids) + 1:016d}")

    def _map_uuid(self, value: str) -> str:
        if self._session_id is not None and value == self._session_id:
            return self._session_placeholder
        return self._uuids.setdefault(value, f"uuid-{len(self._uuids) + 1:04d}")

    def scrub_string(self, s: str, *, is_command: bool = False) -> str:
        # 1. Secrets first (before any structural rewriting can split them).
        for pat, repl in SECRET_PATTERNS:
            s = pat.sub(repl, s)
        # 2. Path prefixes. Known homes/roots are boundary-anchored so a sibling
        #    account that is a superstring of the runner's (/Users/rioX432 vs
        #    /Users/rio) is never partially rewritten — the generic per-user
        #    rules below take such paths whole.
        for home in self.real_homes:
            s = re.sub(rf"{re.escape(home)}(?![\w.-])", HOME_PLACEHOLDER, s)
        if self.project_root:
            s = re.sub(
                rf"{re.escape(self.project_root)}(?![\w.-])", PROJECT_PLACEHOLDER, s
            )
        s = SCRATCH_RE.sub(PROJECT_PLACEHOLDER, s)
        # Generic home forms for ANY user (macOS + Linux). The Linux rule is
        # idempotent for the /home/user placeholder itself.
        s = re.sub(r"/Users/[^/\s\"']+", HOME_PLACEHOLDER, s)
        s = re.sub(r"/home/[^/\s\"']+", HOME_PLACEHOLDER, s)
        # Claude Code's hyphen-encoded projects-dir form (~/.claude/projects/
        # -Users-<name>-...) can appear inside tool output / prose where the
        # slash-based rules never match. Mask the encoded home generically
        # (best effort: usernames containing hyphens still need the manual
        # checklist).
        s = re.sub(r"-Users-[A-Za-z0-9_.]+", "-home-user", s)
        s = re.sub(r"-home-[A-Za-z0-9_.]+", "-home-user", s)
        # 3. Deterministic ids.
        s = TOOLU_RE.sub(lambda m: self._map_toolu(m.group(0)), s)
        s = UUID_RE.sub(lambda m: self._map_uuid(m.group(0)), s)
        # 4. Host / user tokens (word-bounded to avoid mangling unrelated text,
        #    e.g. a machine named "mac" must not rewrite substrings of
        #    "Homebrew" internals or English prose).
        for host in self.real_hosts:
            s = re.sub(rf"(?<![\w-]){re.escape(host)}(?![\w-])", "host", s)
        for user in self.real_users:
            s = re.sub(rf"(?<![\w-]){re.escape(user)}(?![\w-])", "user", s)
        # 5. Local command-proxy wrapper.
        if is_command:
            s = re.sub(r"^\s*rtk\s+", "", s)
        return s

    def scrub_value(self, value, *, key: str | None = None):
        if isinstance(value, str):
            if key == "transcript_path":
                sid = self._session_placeholder
                return f"{HOME_PLACEHOLDER}/.claude/projects/{self.scenario}/{sid}.jsonl"
            if key == "cwd":
                return PROJECT_PLACEHOLDER
            return self.scrub_string(value, is_command=(key == "command"))
        if isinstance(value, list):
            return [self.scrub_value(v) for v in value]
        if isinstance(value, dict):
            return {k: self.scrub_value(v, key=k) for k, v in value.items()}
        return value

    def process_line(self, obj: dict) -> dict:
        sid = obj.get("session_id")
        if isinstance(sid, str):
            self.learn_session_id(sid)
        # Pre-map prompt ids so they normalize consistently across events.
        pid = obj.get("prompt_id")
        if isinstance(pid, str):
            self._map_prompt(pid)
        cleaned = self.scrub_value(obj)
        if isinstance(cleaned.get("prompt_id"), str):
            cleaned["prompt_id"] = self._map_prompt(obj["prompt_id"])
        if isinstance(cleaned.get("session_id"), str):
            cleaned["session_id"] = self._session_placeholder
        return cleaned


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--scenario", required=True, help="fixture scenario id, e.g. session-basic")
    ap.add_argument("--project-root", required=True, help="absolute cwd used during capture")
    ap.add_argument("--in", dest="infile", required=True, type=Path)
    ap.add_argument("--out", dest="outfile", required=True, type=Path)
    ap.add_argument("--home", action="append", default=[], help="extra real home dir to mask")
    ap.add_argument("--user", action="append", default=[], help="extra real username to mask")
    ap.add_argument("--host", action="append", default=[], help="extra real hostname to mask")
    args = ap.parse_args()

    real_homes = [str(Path.home()), *args.home]
    real_users = [getpass.getuser(), *args.user]
    hostname = socket.gethostname()
    real_hosts = [hostname, hostname.split(".")[0], *args.host]

    sanitizer = Sanitizer(args.scenario, args.project_root, real_homes, real_users, real_hosts)

    lines_out: list[str] = []
    for lineno, raw in enumerate(args.infile.read_text(encoding="utf-8").splitlines(), start=1):
        if not raw.strip():
            continue
        obj = json.loads(raw)
        if not isinstance(obj, dict):
            # Hook payloads are always objects; skip (and report) anything else
            # instead of crashing mid-file.
            print(f"warning: line {lineno} is not a JSON object; skipped", file=sys.stderr)
            continue
        cleaned = sanitizer.process_line(obj)
        lines_out.append(json.dumps(cleaned, sort_keys=True, ensure_ascii=False, separators=(",", ":")))

    args.outfile.parent.mkdir(parents=True, exist_ok=True)
    args.outfile.write_text("\n".join(lines_out) + "\n", encoding="utf-8")
    print(f"sanitized {len(lines_out)} events -> {args.outfile}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
