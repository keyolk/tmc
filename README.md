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

type to search   ↑/↓ move  ⏎ switch  tab/esc commands   ctrl-u clear   ctrl-c quit
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
| `tmc config` | the effective configuration |
| `tmc window send <w>… --to <session>` | move windows to a session (`x s`) |
| `tmc window merge <w>… --into <w>` | merge windows into one (`x m`) |

`--dry-run` on `save` and `load` shows what would happen and touches nothing.

`window send` and `window merge` are the TUI's `x s` and `x m` for scripts and
other front ends, with the targets given instead of marked: windows as `@id` or
`session:index`. `send` renumbers the sessions the windows left; `merge` joins
every pane of the windows into `--into` (which may be one of them), tiles when
more than one arrives, and the emptied windows close. Each prints what moved,
names every failure on stderr and exits non-zero if anything did not move.

`load` skips a session that is already running, since appending its windows to
a live one would silently double the workspace. `--force` closes it and
rebuilds instead — it prints what would be lost first and asks, so the answer
is informed; `--yes` skips the asking for scripts.

## Autorun

Restore types each pane's command at its prompt and stops there: bringing a
workspace back should not start 30 processes unasked, and a line sitting at a
prompt is undone with `ctrl-c`.

But a few commands are the reason you restored at all. A `claude` pane you
still have to start by hand is a pane you have not recovered yet — and it is
the one that carries a `--resume <id>` you did not type and cannot retype.
So which commands run is a per-pattern choice, in
`~/.config/tmc/config.toml`:

```toml
[restore]
autorun = ["claude", "nvim *", "htop"]
```

`claude` is the default; everything else is typed and left. An explicit
`autorun = []` turns it off entirely. `tmc config` prints what is in force,
which is otherwise only observable by restoring and watching what starts.

Two spellings, because they answer different questions:

- **a plain word** matches any *token* of the command line. The program is
  rarely the interesting word — `claude` has to find `ccproxy claude
  --intercept=mitm`, and a substring match would also find `echo claudette`.
- **`*` or `?`** makes it a glob over the whole line, for when the token is
  not the unit you want: `nvim *`, `cargo watch*`, `*--resume*`.

Patterns see the *restored* line, `--resume` included. Per run, `--autorun
<pattern>` replaces the list and `--no-autorun` empties it; `--dry-run`
counts the two kinds separately, since only one of them is irreversible.

## Keys

It opens on the search line. Summoning a popup is already the decision to go
somewhere, so type — `bnp` reaches `binpack`. `Tab` or `Esc` steps out to the
tree while keeping the query as a filter; `tmc --browse` starts there instead.

The cursor starts on the window it was summoned from, so the panel opens
describing where you already are rather than some other session's first
window.

`Esc` pops one level each press: a destination filter, then a pending pane
move or the `x` menu, then the search line, then the app. Pressing it twice
from where the TUI opens leaves.

**Searching**

| | |
|---|---|
| any letter | narrow the list |
| `↑`/`↓`, `Ctrl-n`/`Ctrl-p` | move |
| `Enter` | switch to the window and exit |
| `Tab`, `Esc` | keep the filter, return to normal/tree mode |
| `Ctrl-u` | clear the query |

**Tree**

| | |
|---|---|
| `/` | back to searching |
| `j`/`k`, arrows | move |
| `g`/`G` | first / last |
| `Enter` | switch to the window and exit |
| `n` | next window waiting on you |
| `space` | mark the window — or, on a pane row, the pane; on a session header, the session |
| `a` / `c` | mark everything changed / clear |
| `r` | restore marked sessions and windows (or, with none marked, the missing windows) |
| `s` | save a point now |
| `p` / `P` | next / previous restore point |
| `l` / `h` | expand a window to its panes / collapse |
| `x` | the session, window and pane menu, below |
| `R` | re-read tmux now |
| `o` | open the first link in the output on screen |
| `?` | every key, in every mode |
| `q`, `Esc`, `Ctrl-C` | quit |

The hint row, the `x` menu, `?` and the key dispatch are all read off one
table (`src/ui/keys.rs`), so a key cannot be advertised and not answer, or
answer and not be listed.

**Mouse** augments the same model; every action remains reachable by keyboard.

| gesture | where | action |
|---|---|---|
| left click | header | focus search |
| left click | tree | select a session, window or pane |
| right / middle click | tree | select and toggle its mark |
| wheel | tree | move three rows; in destination mode, visit valid windows |
| wheel | detail | scroll captured output |
| click | `x` menu | run that row, or show why it is disabled |
| wheel / outside click | `?` help | scroll / close |
| left click | clipboard | select without pasting |
| middle / right click | clipboard | select and paste |

**Links.** A URL in the output preview is an OSC 8 hyperlink carrying the
whole URL, even where the panel cuts the line, so cmd/ctrl+click opens it
(never a plain click). `o` opens the first one on screen and says how many
there were; in the clipboard picker, where letters type, it is `Ctrl-o` and
reads the whole buffer. Only http(s) URLs are linked or opened, and over SSH
nothing is opened, since the browser would start on the remote host. Behind
tmux, links need `set -as terminal-features '*:hyperlinks'` (tmux 3.4+).

Reloads, previews and every command that changes tmux run off the input
thread. Navigation and `Esc` keep answering while a restore brings windows up;
another tmux-changing command waits until it finishes, and leaving waits for it
too rather than cutting it off halfway.

**Remembered between runs.** Expanded windows, the last query and the restore
point being compared against are kept in `$XDG_STATE_HOME/tmc/ui.json`
(`~/.local/state/tmc/ui.json` by default) and put back on the next open.
The restored query is shown selected: the first letter typed replaces it,
Backspace clears it, and Enter or a move keeps it.

**Status line.** Messages carry a level, drawn as a prefix and a colour
(`i` / `✓` / `!` / `✗`, or `info:` `ok:` `warn:` `error:` in ASCII). Info and
success clear themselves after 4 seconds; warnings and errors stay until a key.

**Terminal.** `NO_COLOR` drops every colour but keeps bold and reverse video.
`TMC_ASCII=1` (or a non-UTF-8 locale) draws with ASCII only; a CJK locale keeps
Unicode but cuts with `~` and names arrow keys in words, since `…` and the
arrows draw two cells there. If one pane panics while drawing, it shows a
one-line notice and the rest keeps working; the panic goes to `tmc.log` next to
`ui.json`.

**`x` — rearranging tmux**

Everything that creates, moves or closes a session, window or pane lives
behind `x`, and pressing it shows what is on offer:

```
╭ x  local:14  tmc ──────────────────────────────────╮
│ b:break out                needs a pane; l expands │
│ j:join into a window       needs a pane; l expands │
│ m:merge windows           needs two or more marked │
│ s:send to a session               no other session │
│ k:close                                   1 window │
│ n:new session                          type a name │
│ d:close session                          1 session │
│                                                    │
│ esc cancel                                         │
╰────────────────────────────────────────────────────╯
```

| | |
|---|---|
| `x b` | break the selected panes out, each into its own window |
| `x j` | join the selected panes into a window you pick |
| `x m` | merge the marked windows into one |
| `x s` | send the selected windows to another session — with three or more, pick which with `j`/`k` and `Enter` |
| `x k`, `x x` | close the selected windows |
| `x n` | create a session: type its name, `Enter` creates it, `Esc` cancels |
| `x d` | close the selected sessions — the marked ones, or the cursor's |

The title names what the commands would act on — the marked windows and panes,
or the row under the cursor when nothing is marked. The right column says what
each one would do with that, or what it is missing, and a greyed row says the
same thing if you press it anyway. Any other key closes the menu and does
nothing.

These were five top-level letters until they were not: they are the least
used keys in the app and had taken the most memorable ones — `x` meant kill,
and `m` and `M` meant two moves with nothing in common. Nothing is polled while
the menu is up, so what it says it will act on is still true when the key lands.

**Choosing a destination** (after `x j` or `x m`)

| | |
|---|---|
| `j`/`k`, arrows | move among live windows |
| `/` | narrow them by typing; `Esc` closes the filter |
| `Enter` | confirm |
| `Esc` | cancel the move |

**Sessions**

Session headers are rows like any other. `Enter` on one switches to the
session, `space` marks it, and the panel says what it holds — how many
windows are running and how many exist only in the restore point.

`x n` starts the new session in the directory of the row under the cursor,
which is usually the project it is for, and puts the cursor on it rather
than switching: `Enter` is one key away. A name with `:` or `.` is refused,
since tmux would rewrite it and those are what target syntax splits on.

`x d` acts on the marked sessions, or the one the cursor is in — from its
header, a window or a pane. It does not ask, for the reason `x k` does not:
a session the point holds comes back with `r`, and the status says whether
that is so. Its mark stays on a closed session that the point holds, so `r`
straight after brings exactly that back. Closing every session is refused:
tmux exits with its last one, and the popup with it. The session the popup
was opened from goes last, after the client is switched off it.

## Marks are a selection, not a queue

`space` marks whatever is under the cursor, and the next command decides what
that meant: `r` restores the marked sessions and windows, `x m` merges them, `x s` sends
them to the other session, `x k` closes them, and with panes marked instead,
`x b` breaks them out and `x j` moves them. Only the marks a command consumed
are dropped, so windows staged for `r` survive a pane move made on the way.

With nothing marked, the pane and window commands act on the cursor — the
one-window case stays one keystroke.

`r` brings back only what the server does not have. A marked session stands
for every one of its windows that exists only in the point — the whole
session, when it is not running — so several sessions come back in one
press. The windows go into a running session alongside what is already
there; a marked window that is still running is left alone and counted in
the status, since recreating it would double it. One session failing does
not stop the others.

Marking a pane requires expanding its window (`l`), which is also what makes
the selection honest: `break-pane` and `join-pane` take a pane, and a window
target makes tmux use whichever pane it considers active rather than the one
you were looking at.

`M` is the one command tmux has no verb for. Merging is every pane of the
marked windows joined into the one you keep, which the sources then close
themselves over, having nothing left. The window that receives them is picked
in the same step — and it can be one of the marked ones, since "fold `alpha`
into `beta`" is the usual reason to ask.

Panes arriving one at a time each split whatever pane was active, so a merge
of more than one is tiled at the end. A single pane leaves a hand-made layout
alone; when tmux runs out of room mid-move, tiling is what makes room.

## State

Windows carry two independent marks. Both are punctuation rather than colour
alone, so the tree reads under `NO_COLOR` and in monochrome.

| glyph | |
|---|---|
| `?` | waiting on you |
| `*` | working |
| `+` | done, output unread |
| `.` | running claude, no state published |

| marker | |
|---|---|
| `~` | changed since the restore point |
| `+` | live only — created since |
| `-` | in the point only — gone from the server |

Claude state is not inferred: `~/.claude/hooks/cc_state.py` publishes it into
the `@cc_state` window option, and `notify.py` records *why* a session is
blocked. A window shows as waiting only when both agree, so a stale state
cannot claim someone is holding it up.

Both are hook output, and hooks miss in both directions, so the process table
is the tiebreaker. A published state with no claude running behind it is a
leftover — nothing retires `@cc_state` when a session is killed or lost to a
reboot, and windows were found still advertising `waiting` 35 hours on — so it
is dropped rather than shown. The reverse, a claude with no state, gets `.`:
it is alive, and how it is doing is simply not known.

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

`snapshot --tree` renders the tree instead of the opening search line, and
`--menu` draws the `x` menu over it.

`snapshot` renders one frame without an interactive terminal, which is how the
layout is reviewed. It has already caught a real bug: every window rendering
as removed, because a session had been renamed and the diff paired sessions by
name alone.
