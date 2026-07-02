# Recipe: watch several live sessions with Cmux / tmux

agent-witness deliberately has **no in-app split pane** (see NON-GOALS.md). When
you run agents across several projects at once — the real workflow this feature
was built for — tile the timelines with an external multiplexer and one
`agent-witness show --follow` per pane.

This keeps agent-witness a single-session viewer and lets your multiplexer do
what it is already good at: layout, focus, and resize.

## Start from `top`, then split out the ones you care about

```bash
agent-witness top          # see everything running; note the session ids/projects
```

`top` is the dashboard; the split panes below are the detail views.

## tmux

Open a window and split it into panes, each following one session:

```bash
# one live session per project, selected by cwd substring
tmux new-session -d -s witness 'agent-witness show --follow @project:agent-witness'
tmux split-window -h            'agent-witness show --follow @project:avvy'
tmux split-window -v            'agent-witness show --follow @live:1'
tmux select-layout tiled
tmux attach -t witness
```

Selectors that pair well with `--follow`:

- `@project:<substring>` — the latest session whose working directory matches a
  project. Stable per project, so each pane stays pinned to "that project".
- `@live:1`, `@live:2`, … — the n-th most recent **live** session. Handy for an
  ad-hoc "show me whatever is running now" pane.
- a session-id prefix (e.g. `9f8c`) — pin a pane to one exact session.

## Cmux

Cmux drives panes the same way; give each split a `show --follow` command scoped
to a project or a live ordinal:

```bash
cmux split --cmd 'agent-witness show --follow @project:agent-witness'
cmux split --cmd 'agent-witness show --follow @project:avvy'
cmux split --cmd 'agent-witness show --follow @live:1'
```

(Consult `cmux --help` for the exact split/layout flags your version ships; the
only agent-witness-specific part is the `show --follow <selector>` command.)

## Notes

- Each pane polls its own `events.jsonl` for appended lines (~250 ms) — there is
  no shared daemon and no fs-watcher; live tail works whether or not `watch` is
  running.
- `--follow` starts in tail mode; inside any pane you can still press `f` to
  toggle following, `Enter` for the detail pane, and `q`/`Esc` to quit.
- Liveness is inferred from a store scan (no daemon). A session with no `Stop`
  stays "live" until its recency window (default 5 min, `--window`) lapses.
