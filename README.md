# tmc

Snapshot your tmux workspace, see what has changed since, and put back only
the parts you want.

```
tmc  28 windows   ▌  18 waiting
saved:20260830T150700Z  (33m ago)   ~19 +3 -3
▾ projects  13w                                     │projects:1  cohome  4p
>  ?   1 cohome         4p ~ 12f17a65               │  4 panes, was 5
   ?   2 right-sizing   2p ~ c7e375b9               │  pane 4 command: ghx -> (shell)
   *   3 kite           2p ~ 58f63e71               │  claude waiting  12f17a65
   ?   4 firewall       2p ~ 0e5b5427               │
      —  binpack        2p -                        │─ output ──────────────────────────────────
                                                    │  cargo test
                                                    │     Compiling tmc v0.1.0
                                                    │  test result: ok. 166 passed
                                                    │  ❯

type to search   ↑↓ move   ⏎ switch   tab/esc commands   ctrl-c quit
```

Replaces `tmux.sh`, `twm` and the `tmux-fzf` plugin.

## Why

An hourly job had been writing workspace snapshots for months — 24 of them,
per session. They went unused, because there was no way to ask the only
question that matters: *what has changed since?* By the time you want a
restore point you have no idea which one to reach for.

`tmc` puts that comparison on screen. The left pane is the live server, the
right says why the selected window differs and shows what it is currently
running, and `r` restores only what you marked.

## Commands

| | |
|---|---|
| `tmc` | the TUI |
| `tmc save [name]` | write a restore point |
| `tmc load [point]` | bring one back |
| `tmc diff [point]` | what changed since |
| `tmc list` | the points on disk |
| `tmc doctor [point]` | check a point before you need it |
| `tmc autosave [--if-drifted]` | the periodic snapshot |
| `tmc clipboard` | paste-buffer picker |
| `tmc copy-mode` | send a copy-mode command |
| `tmc snapshot` | render one frame as text |

`--dry-run` on `save` and `load` shows what would happen and touches nothing.

`load` skips a session that is already running, since appending its windows to
a live one would silently double the workspace. `--force` closes it and
rebuilds instead — it prints what would be lost first and asks, so the answer
is informed; `--yes` skips the asking for scripts.

## Keys

It opens on the search line. Summoning a popup is already the decision to go
somewhere, so type — `bnp` reaches `binpack`. `Tab` or `Esc` steps out to the
tree while keeping the query as a filter; `tmc --browse` starts there instead.

**Searching**

| | |
|---|---|
| any letter | narrow the list |
| `↑`/`↓`, `Ctrl-n`/`Ctrl-p` | move |
| `Enter` | switch to the window and exit |
| `Tab`, `Esc` | keep the filter, return to normal/tree mode |

**Tree**

| | |
|---|---|
| `/` | back to searching |
| `j`/`k`, arrows | move |
| `g`/`G` | first / last |
| `Enter` | switch to the window and exit |
| `n` | next window waiting on you |
| `space` | mark for restore |
| `a` / `c` | mark everything changed / clear |
| `r` | restore marked (or, with none marked, the missing windows) |
| `s` | save a point now |
| `p` / `P` | next / previous restore point |
| `l` / `h` | expand a window to its panes / collapse |
| `b` | break the selected pane out into its own window |
| `J` | choose a destination window for the selected pane; `j`/`k`, then `Enter`/`J` |
| `m` / `x` | move window to the other session / close it |
| `Esc` | cancel pane destination selection; otherwise stay in normal mode |
| `q`, `Ctrl-C` | quit |

## State

Windows carry two independent marks. Both are punctuation rather than colour
alone, so the tree reads under `NO_COLOR` and in monochrome.

| glyph | |
|---|---|
| `?` | waiting on you |
| `*` | working |
| `+` | done, output unread |

| marker | |
|---|---|
| `~` | changed since the restore point |
| `+` | live only — created since |
| `-` | in the point only — gone from the server |

Claude state is not inferred: `~/.claude/hooks/cc_state.py` publishes it into
the `@cc_state` window option, and `notify.py` records *why* a session is
blocked. A window shows as waiting only when both agree, so a stale state
cannot claim someone is holding it up.

## Restore points

```
~/.config/tmux/layouts/
  <name>/<session>.json              a point you named
  autosave/<session>/<stamp>.json    the periodic ones
```

An autosave point is the set of files sharing one timestamp across every
session — a workspace is captured as a whole, so a `dashboard` + `projects`
pair comes back as it was at one moment.

The format is the one `tmux.sh` wrote, so every snapshot already on disk still
loads. Three fields are new and optional:

- **`pane_id`** — the join key for diffing. Resets when the tmux server does.
- **`shell_only`** — whether an empty command is correct. 16 of 56 panes here
  legitimately hold nothing but a shell; without this flag `doctor` cannot
  tell those apart from a command that was lost.
- **`session_confidence`** — `exact` or `ambiguous`. See below.

## Two things it will not pretend to know

**Which Claude session a pane holds.** `cc_state.py` publishes the id as a
*window* option, and tmux has no pane-scoped user options — `set-window-option
-p` is rejected, and `set-option -p` is denied by the pane guard. A claude
process exposes no session id through its cwd, environment or open files. So
when a window runs several claudes, the id names *a* session in that window
and not necessarily that pane's. `tmc` records which case it is and shows it,
rather than guessing and resuming the wrong conversation silently.

**Which pane you meant.** `break-pane` and `join-pane` take a pane, and a
window target makes tmux use whichever pane it considers active — not the one
you were looking at. `l` expands a window to its panes, each addressed by
`%id`, and the preview follows the selection so three identical `ccproxy
claude` rows can still be told apart.

**Whether panes match after a tmux restart.** `pane_id` is exact within one
server lifetime and meaningless across a restart — which is precisely when a
restore point matters most. The diff falls back to the pane index, then to
position.

## Speed

`save` takes 0.25s here against `tmux.sh`'s 10.25s, on 2 sessions / 26 windows
/ 61 panes. The difference is not optimization, it is not forking: three
execs — one `list-panes`, two `ps` — against a subshell per window and around
seven processes per pane.

That changes what autosave can be. `--if-drifted` compares before writing, so
a tmux hook can fire it on every window change instead of a timer:

```tmux
set-hook -g after-new-window 'run -b "tmc autosave --if-drifted"'
set-hook -g after-kill-pane  'run -b "tmc autosave --if-drifted"'
```

## Scope

`tmc` is workspace structure. Process detail — what is running, its sockets,
its stack — belongs to [`tpx`](../tpx), which shares no code but the same
collection approach. Chrome tab groups stay in `tmux-chrome`; `tmc` calls it.

## Install

```sh
make install     # fmt, clippy, tests, then -> ~/.local/bin/tmc
```

```tmux
bind-key w display-popup -w 80% -h 80% -E "tmc"
bind-key F display-popup -w 70% -h 60% -E "tmc clipboard"
```

## Development

```sh
make check       # fmt + clippy + test
tmc snapshot --width 100 --height 24
```

Search is fuzzy everywhere — the tree and the buffer picker share one matcher.
With 27 windows and 100 paste buffers, a picker that wants the exact letters
in order is a filter, not a search. Smart case: a lowercase query ignores
case, an uppercase one means it.

`snapshot --tree` renders the tree instead of the opening search line.

`snapshot` renders one frame without an interactive terminal, which is how the
layout is reviewed. It has already caught a real bug: every window rendering
as removed, because a session had been renamed and the diff paired sessions by
name alone.
