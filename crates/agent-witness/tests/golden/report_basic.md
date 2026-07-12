# Session report: session-basic

_Observation only — records what Claude Code hooks reported. See the scope note at the end._

## Summary

- Started: 2023-11-14 22:13:20Z
- Duration: 6.0s
- Tool calls: 2
- Touched files: 1
- Commands: 1
- Events: 7
- Corrupt lines: 0

## Final message vs recorded evidence

_The agent's final message beside the facts the record holds. This section states what was recorded — it never judges whether the message is true; that is yours to read._

**Final message**

> 完了しました。`hello.rs` を作成し(`main` で `Hello` を出力)、コンパイラは `rustc 1.92.0` (Homebrew) が入っていることを確認しました。

**Test-like commands recorded**

_None recorded._

## Touched files

_From `tool_input.file_path`; attribution noted._

- `/home/user/project/hello.rs` — Write (direct)

## Executed commands

_From Bash `tool_input.command`._

- `rustc --version` — ok, 136ms (direct)

## Timeline

- `+0.000s` · SESSION · startup · direct
- `+1.000s` · PROMPT · Create a file hello.rs containing a main function that prints Hello, then run 'rustc --version' to check the compiler. Keep it minimal. · direct
- `+2.000s` · Write [ok] (6ms) · /home/user/project/hello.rs · direct
- `+4.000s` · Bash [ok] (136ms) · rustc --version · direct
- `+6.000s` · STOP · 完了しました。`hello.rs` を作成し(`main` で `Hello` を出力)、コンパイラは `rustc 1.92.0` (Homebrew) が入っていることを確認しました。 · direct

## Observation scope

agent-witness records only what Claude Code hooks report: tool calls and their inputs, not their side effects.
A command run via Bash is recorded by its command line only; what it does internally (e.g. `bash script.sh`) is not observed.
A failed Bash call fires no completion hook, so it appears as a call with no result — never as a success.
Flagged commands, when present, describe the command class only — never intent, outcome, or whether any damage occurred.
0 corrupt/unreadable line(s) were skipped while reading this session.
