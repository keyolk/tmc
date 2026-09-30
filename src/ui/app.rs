//! The terminal loop: poll, redraw only on change, handle keys.

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use std::collections::HashMap;

use super::model::{Model, MoveKind, Row, WindowRow};
use crate::collect::{cmd, notify, proc, tmux};
use crate::config;
use crate::layout::{point, restore, save};

/// How often the world is re-read. tmux exposes no event stream a foreign
/// process can subscribe to, so this polls; `fingerprint` keeps the redraw
/// from happening when nothing moved.
const TICK: Duration = Duration::from_secs(2);

/// What the caller should do once the terminal is restored.
pub enum Outcome {
    Quit,
    /// Switch to this tmux target, e.g. `projects:3`.
    Switch(String),
}

/// Open the TUI.
///
/// `search` decides which mode it starts in. Searching is the default,
/// because this runs as a popup: summoning it is already the decision to go
/// somewhere, and making you press `/` first is the same extra keystroke that
/// made tmux-fzf's two-level menu tiresome. `Esc` steps out to the tree when
/// the intent is to inspect rather than jump, and again to leave.
pub fn run(search: bool) -> Result<Outcome> {
    let points = point::list(&save::layout_dir());
    let mut model = Model::new(points);
    model.searching = search;

    let mut terminal = setup().context("enter alternate screen")?;
    let result = event_loop(&mut terminal, &mut model, summoning_window());
    teardown(&mut terminal)?;
    result?;

    Ok(match model.switch_to {
        Some(target) => Outcome::Switch(target),
        None => Outcome::Quit,
    })
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn teardown(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn event_loop(terminal: &mut Term, model: &mut Model, focus: Option<String>) -> Result<()> {
    // Paint the chrome before reading the world. This runs as a popup, and the
    // first reload costs ~0.1s of `ps` and `tmux` before it can say anything;
    // drawing after it meant the popup sat blank for that whole time and read
    // as slow to open. The frame drawn here is the real one with an empty
    // tree — the search line, the header and the panel are all already in
    // place, so the reload below fills the rows in rather than replacing the
    // screen.
    terminal.draw(|frame| super::render::draw(frame, model, now_secs()))?;

    reload(model)?;
    // The window this was summoned from is the context the user is already in,
    // so it is what the panel should describe first.
    if let Some(target) = &focus {
        model.focus(target);
    }
    let mut last = model.fingerprint();
    let mut needs_redraw = true;
    let mut next_tick = Instant::now() + TICK;

    loop {
        if needs_redraw {
            refresh_preview(model);
            let now = now_secs();
            terminal.draw(|frame| super::render::draw(frame, model, now))?;
            needs_redraw = false;
        }

        let wait = next_tick.saturating_duration_since(Instant::now());
        if event::poll(wait)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                handle_key(model, key.code, key.modifiers)?;
                if model.quit || model.switch_to.is_some() {
                    return Ok(());
                }
                needs_redraw = true;
            } else {
                needs_redraw = true; // resize
            }
            continue;
        }

        // Tick: re-read the world, but only redraw when the display would
        // differ. On an idle workspace this leaves the app at 0 fps.
        next_tick = Instant::now() + TICK;
        reload(model)?;
        let current = model.fingerprint();
        if current != last {
            last = current;
            needs_redraw = true;
        }
    }
}

fn handle_key(model: &mut Model, code: KeyCode, mods: KeyModifiers) -> Result<()> {
    model.status.clear();

    // Under a Korean input source the shortcut keys arrive as jamo (`q` -> `ㅂ`)
    // and do nothing until the input source is switched back. Rewrite them to
    // the Latin key at the same physical position -- but not while searching,
    // where letters type and the jamo IS the input.
    let code = if model.searching {
        code
    } else {
        crate::keymap::normalize(code, mods)
    };

    // Panes have been picked and only a destination window is selectable. Deal
    // with this before the ordinary tree keys so Enter cannot switch windows
    // and Esc cannot quit while the user is in the middle of choosing.
    if model.pending_move.is_some() {
        // Typing filters the destinations. With 28 windows open, walking to
        // one with j/k is the slow half of the move — and the query is thrown
        // away on the way out, so it never leaks into the tree afterwards.
        if model.searching {
            match code {
                KeyCode::Esc | KeyCode::Tab => model.searching = false,
                KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => model.quit = true,
                KeyCode::Char('n') if mods.contains(KeyModifiers::CONTROL) => {
                    model.move_destination(1);
                }
                KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => {
                    model.move_destination(-1);
                }
                KeyCode::Enter => confirm_move(model)?,
                KeyCode::Down => {
                    model.move_destination(1);
                }
                KeyCode::Up => {
                    model.move_destination(-1);
                }
                KeyCode::Backspace => {
                    model.search_pop();
                    // The filter moved the rows under the cursor; land it back
                    // on a window that can actually receive the panes.
                    model.move_destination(0);
                }
                KeyCode::Char(c) => {
                    model.search_push(c);
                    model.move_destination(0);
                }
                _ => {}
            }
            return Ok(());
        }
        match code {
            KeyCode::Esc => cancel_move(model),
            KeyCode::Char('q') => model.quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => model.quit = true,
            KeyCode::Char('/') => model.searching = true,
            KeyCode::Char('j') | KeyCode::Down => {
                model.move_destination(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                model.move_destination(-1);
            }
            KeyCode::Enter | KeyCode::Char('J') | KeyCode::Char('M') => confirm_move(model)?,
            _ => {
                model.status =
                    "choose a window: j/k move, / filter, Enter confirm, Esc cancel".into();
            }
        }
        return Ok(());
    }

    // While searching, letters type. This is where the TUI starts, so the way
    // out has to be obvious and the way back cheap.
    if model.searching {
        match code {
            // Esc is a mode change, never an exit. Keep the query as a visible
            // filter, just as Tab does, so backing out does not discard typing.
            KeyCode::Esc => model.searching = false,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => model.quit = true,
            // Step out to the tree, keeping the filter. The single-key
            // commands (mark, restore, save, window surgery) live there.
            KeyCode::Tab => model.searching = false,
            // Control chords come before the catch-all: a `Char` arm placed
            // first swallows them and types the letter instead.
            KeyCode::Char('n') if mods.contains(KeyModifiers::CONTROL) => model.move_cursor(1),
            KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => model.move_cursor(-1),
            KeyCode::Backspace => model.search_pop(),
            KeyCode::Char(c) => model.search_push(c),
            KeyCode::Enter => {
                model.searching = false;
                if let Some(w) = model.current_window()
                    && !w.gone
                {
                    model.switch_to = Some(w.target());
                }
            }
            KeyCode::Down => model.move_cursor(1),
            KeyCode::Up => model.move_cursor(-1),
            _ => {}
        }
        return Ok(());
    }

    match code {
        KeyCode::Char('/') => model.searching = true,
        // Esc pops one level: the pane-move choice, then the search line, then
        // the app. Making it do nothing at the root meant the key that got you
        // out of everything else stopped working exactly once, which reads as
        // the popup being stuck rather than as a rule.
        KeyCode::Char('q') | KeyCode::Esc => model.quit = true,
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => model.quit = true,

        KeyCode::Char('j') | KeyCode::Down => model.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => model.move_cursor(-1),
        KeyCode::Char('g') | KeyCode::Home => {
            model.cursor = 0;
            model.move_cursor(0);
        }
        KeyCode::Char('G') | KeyCode::End => {
            model.cursor = model.rows.len().saturating_sub(1);
            model.move_cursor(-1);
        }

        KeyCode::Char(' ') => model.toggle_mark(),
        KeyCode::Char('a') => model.mark_all_changed(),
        KeyCode::Char('c') => model.clear_marks(),

        KeyCode::Char('n') => model.jump_waiting(),

        // p/P walk the restore points; the diff follows immediately.
        KeyCode::Char('p') => {
            if model.cycle_point(1) {
                reload(model)?;
            }
        }
        KeyCode::Char('P') => {
            if model.cycle_point(-1) {
                reload(model)?;
            }
        }

        KeyCode::Char('s') => save_point(model)?,
        KeyCode::Char('r') => restore_marked(model)?,

        KeyCode::Enter => {
            if let Some(w) = model.current_window() {
                if w.gone {
                    // Nothing to switch to; the window only exists in the point.
                    model.status = "that window is gone — press r to restore it".into();
                } else {
                    model.switch_to = Some(w.target());
                }
            }
        }
        // Window-level surgery, replacing `tmux.sh move/join/break`. These
        // act on the window under the cursor, which is what the tree is for —
        // tmux.sh made you pick from a second fzf list first.
        // Expand a window to its panes. `b` and `J` need a pane, and this is
        // how you get one.
        KeyCode::Char('l') | KeyCode::Right => {
            if model.set_expanded(Some(true)) {
                reload(model)?;
            }
        }
        KeyCode::Char('h') | KeyCode::Left => {
            if model.set_expanded(Some(false)) {
                reload(model)?;
            }
        }

        KeyCode::Char('m') => move_windows(model)?,
        KeyCode::Char('b') => break_panes(model)?,
        KeyCode::Char('J') => move_panes(model)?,
        KeyCode::Char('M') => merge_windows(model)?,
        KeyCode::Char('x') => kill_windows(model)?,

        KeyCode::Char('R') => reload(model)?,
        _ => {}
    }
    Ok(())
}

/// Re-read tmux, the process table, the queue and the selected point.
fn reload(model: &mut Model) -> Result<()> {
    let panes = tmux::panes().unwrap_or_default();
    let tree = proc::Tree::capture_with_args()?;
    let pending = notify::load();
    let saved = match model.current_point() {
        Some(p) => point::read(&p.reference).unwrap_or_default(),
        None => Vec::new(),
    };
    model.refresh(&panes, &saved, &tree, &pending);
    Ok(())
}

fn save_point(model: &mut Model) -> Result<()> {
    let panes = tmux::panes().unwrap_or_default();
    if panes.is_empty() {
        model.status = "no tmux panes to save".into();
        return Ok(());
    }
    let tree = proc::Tree::capture_with_args()?;
    let now = crate::clock::now();
    let sessions = save::snapshot(&panes, &tree, &now.timestamp);
    let dir = save::layout_dir().join(&now.compact);

    for session in &sessions {
        save::write(session, &dir.join(format!("{}.json", session.session)))?;
    }

    // The new point becomes the one being compared against, which is almost
    // always what the user wants next: they saved because this state is worth
    // keeping, so the diff should now read as empty.
    model.points = point::list(&save::layout_dir());
    model.point_index = 0;
    reload(model)?;
    model.status = format!("saved {} session(s) as {}", sessions.len(), now.compact);
    Ok(())
}

fn restore_marked(model: &mut Model) -> Result<()> {
    let Some(p) = model.current_point() else {
        model.status = "no restore point selected".into();
        return Ok(());
    };
    let saved = point::read(&p.reference)?;

    // With nothing marked, restore what is missing — the windows the point has
    // and the live server does not. Restoring everything would mean recreating
    // windows that are already open.
    let targets: Vec<String> = if model.marks.is_empty() {
        model
            .rows
            .iter()
            .filter_map(|r| match r {
                super::model::Row::Window(w) if w.gone => Some(w.target()),
                _ => None,
            })
            .collect()
    } else {
        model.marks.iter().cloned().collect()
    };

    if targets.is_empty() {
        model.status = "nothing to restore — mark a window with space".into();
        return Ok(());
    }

    // A config error stops the restore rather than the app: `r` is one
    // keystroke inside a popup, and quitting the TUI to report a typo in a
    // file the user can fix in the next pane is the wrong trade.
    let autorun = match config::load() {
        Ok(cfg) => cfg.restore,
        Err(e) => {
            model.status = format!("{}: {e:#}", config::path().display());
            return Ok(());
        }
    };
    let mut server = restore::Server;
    let mut windows = 0;
    let mut ran = 0;
    let mut notes = Vec::new();

    for session in &saved {
        let indices: Vec<u32> = targets
            .iter()
            .filter_map(|t| t.strip_prefix(&format!("{}:", session.session)))
            .filter_map(|i| i.parse().ok())
            .collect();
        if indices.is_empty() {
            continue;
        }
        let report = restore::session(
            &mut server,
            session,
            restore::Selection {
                windows: Some(&indices),
            },
            &autorun,
            false,
            // Never forced from the TUI: `r` here restores windows the point
            // has and the server does not, so there is nothing live to
            // displace. Closing a running session is a CLI decision, with the
            // diff and confirmation that come with it.
            false,
        )?;
        windows += report.windows;
        ran += report.commands_run;
        notes.extend(report.notes);
    }

    model.marks.clear();
    reload(model)?;
    model.status = if notes.is_empty() {
        let mut line = format!("restored {windows} window(s)");
        // Worth a word even in a one-line status: the panes that started on
        // their own are the ones the user did not press a key for.
        if ran > 0 {
            line.push_str(&format!(", ran {ran} command(s)"));
        }
        line
    } else {
        notes.join("; ")
    };
    Ok(())
}

/// `session:index` for a live pane.
fn window_of(p: &tmux::Pane) -> String {
    format!("{}:{}", p.session, p.window_index)
}

/// The panes a pane command acts on: the marked ones, or the one under the
/// cursor.
///
/// Marking is how you say "these"; the cursor fallback keeps the single-pane
/// case one keystroke. Resolved against the live pane list rather than the
/// tree, because a mark survives collapsing its window and by then the row it
/// came from is gone.
fn selected_panes(model: &Model, live: &[tmux::Pane]) -> Vec<(String, String)> {
    let marked = model.marked_panes();
    if marked.is_empty() {
        return model
            .current_pane()
            .map(|p| vec![(p.target().to_string(), p.window_target())])
            .unwrap_or_default();
    }
    live.iter()
        .filter(|p| marked.contains(&p.pane_id))
        .map(|p| (p.pane_id.clone(), window_of(p)))
        .collect()
}

/// The live windows a window command acts on: the marked ones, or the one
/// under the cursor.
fn selected_windows(model: &Model) -> Vec<String> {
    let marked = model.marked_live_windows();
    if !marked.is_empty() {
        return marked;
    }
    model
        .current_window()
        .filter(|w| !w.gone)
        .map(|w| vec![w.target()])
        .unwrap_or_default()
}

/// One status line: what happened, then what did not.
fn report(done: String, failed: Vec<String>) -> String {
    if failed.is_empty() {
        done
    } else {
        format!("{done}; failed: {}", failed.join(", "))
    }
}

/// Move the selected windows to the other session.
///
/// With two sessions the destination is unambiguous, which is the whole reason
/// this is one keystroke here and a prompt in tmux.sh. With more, the status
/// line says what to do instead of guessing.
fn move_windows(model: &mut Model) -> Result<()> {
    let targets = selected_windows(model);
    if targets.is_empty() {
        model.status = "that window is not running".into();
        return Ok(());
    }

    let all = sessions()?;
    if all.len() < 2 {
        model.status = "no other session to move to".into();
        return Ok(());
    }
    if all.len() > 2 {
        model.status = format!(
            "several destinations ({}); use tmux directly",
            all.len() - 1
        );
        return Ok(());
    }

    let mut moved = 0;
    let mut failed = Vec::new();
    let mut emptied: Vec<String> = Vec::new();
    for target in &targets {
        let here = target.split(':').next().unwrap_or_default().to_string();
        let Some(dest) = all.iter().find(|s| **s != here) else {
            continue;
        };
        match cmd::run(
            "tmux",
            &["move-window", "-s", target, "-t", &format!("{dest}:")],
            cmd::FAST,
        ) {
            Ok(_) => {
                moved += 1;
                if !emptied.contains(&here) {
                    emptied.push(here);
                }
            }
            Err(e) => failed.push(format!("{target}: {e}")),
        }
    }

    // move-window leaves a hole; tmux only renumbers on close. Deferred until
    // every window has moved, because renumbering mid-loop would invalidate
    // the `session:index` targets still waiting their turn.
    for session in &emptied {
        let _ = cmd::run("tmux", &["move-window", "-r", "-t", session], cmd::FAST);
    }

    model.unmark(&targets);
    reload(model)?;
    model.status = report(format!("moved {moved} window(s)"), failed);
    Ok(())
}

/// Break the selected panes out, each into a window of its own.
///
/// Requires panes to be selected. Addressing this by window — which an
/// earlier version did — makes tmux use that window's *active* pane, so the
/// thing that moved was not the thing on screen.
fn break_panes(model: &mut Model) -> Result<()> {
    let live = tmux::panes().unwrap_or_default();
    let chosen = selected_panes(model, &live);
    if chosen.is_empty() {
        model.status = "select a pane first — l expands a window, space marks one".into();
        return Ok(());
    }

    // tmux refuses to break a window's only pane, and rightly: that is a
    // rename, not a move. Counted down as panes leave, so breaking two of a
    // window's three still stops at the last one rather than trusting a count
    // the previous break already invalidated.
    let mut remaining: HashMap<String, usize> = HashMap::new();
    for p in &live {
        *remaining.entry(window_of(p)).or_default() += 1;
    }

    let mut broken = 0;
    let mut alone = 0;
    let mut failed = Vec::new();
    for (pane, from) in &chosen {
        let left = remaining.entry(from.clone()).or_insert(1);
        if *left < 2 {
            alone += 1;
            continue;
        }
        // `-d` leaves the focus where it is: this is a popup, and stealing the
        // client to the new window would drop the user somewhere they did not
        // ask to be.
        match cmd::run("tmux", &["break-pane", "-d", "-s", pane], cmd::FAST) {
            Ok(_) => {
                *left -= 1;
                broken += 1;
            }
            Err(e) => failed.push(format!("{pane}: {e}")),
        }
    }

    model.unmark(
        &chosen
            .iter()
            .map(|(pane, _)| pane.clone())
            .collect::<Vec<_>>(),
    );
    reload(model)?;
    let mut done = format!("broke out {broken} pane(s)");
    if alone > 0 {
        done.push_str(&format!(" — {alone} already alone in its window"));
    }
    model.status = report(done, failed);
    Ok(())
}

/// Arguments that move one exact pane into one exact window.
///
/// `join-pane` calls its target a dst-pane, but tmux accepts a window target and
/// resolves that to the window's active pane. That gives the requested window
/// without relying on whichever pane launched the popup.
fn join_pane_args<'a>(pane: &'a str, destination: &'a str) -> [&'a str; 6] {
    ["join-pane", "-d", "-s", pane, "-t", destination]
}

/// Move one pane, making room first if the destination has none.
///
/// `join-pane` splits the destination's *active* pane, and `-d` keeps that the
/// same pane throughout — so each join halves what the last one halved, and a
/// merge fails partway with "no space for new pane". Tiling and retrying costs
/// an extra exec only when that actually happens, which leaves a hand-made
/// layout alone in the ordinary single-pane case.
fn join_one(pane: &str, destination: &str) -> Result<()> {
    match cmd::run("tmux", &join_pane_args(pane, destination), cmd::FAST) {
        Ok(_) => Ok(()),
        Err(_) => {
            let _ = cmd::run(
                "tmux",
                &["select-layout", "-t", destination, "tiled"],
                cmd::FAST,
            );
            cmd::run("tmux", &join_pane_args(pane, destination), cmd::FAST).map(|_| ())
        }
    }
}

/// Pick panes, then move them into an explicitly selected window.
///
/// The first `J` remembers them and turns the tree into destination selection;
/// the second `J` (or Enter) joins. A previous version always used
/// `$TMUX_PANE`, which meant "the window that opened tmc" rather than the
/// window the user actually wanted.
fn move_panes(model: &mut Model) -> Result<()> {
    if model.pending_move.is_some() {
        return confirm_move(model);
    }

    let live = tmux::panes().unwrap_or_default();
    let chosen = selected_panes(model, &live);
    if chosen.is_empty() {
        model.status = "select a pane first — l expands a window, space marks one".into();
        return Ok(());
    }
    let what = match chosen.as_slice() {
        [(pane, _)] => pane.clone(),
        many => format!("{} panes", many.len()),
    };
    if model.begin_move(chosen, MoveKind::Panes) {
        model.status = format!("moving {what}: choose a window, then Enter or J");
    } else {
        model.status = "no other live window to move that into".into();
    }
    Ok(())
}

/// Merge the marked windows into one.
///
/// tmux has no merge: it is every pane of the sources joined into the window
/// that is being kept, which the sources then close themselves over, having
/// nothing left. Which window survives is chosen the same way `J` chooses a
/// destination — including one of the marked ones, which is the usual
/// intent — so the answer is never "whichever tmux considered active".
fn merge_windows(model: &mut Model) -> Result<()> {
    let sources = model.marked_live_windows();
    if sources.len() < 2 {
        model.status = "mark two or more windows with space, then M".into();
        return Ok(());
    }

    let live = tmux::panes().unwrap_or_default();
    let panes: Vec<(String, String)> = live
        .iter()
        .filter(|p| sources.contains(&window_of(p)))
        .map(|p| (p.pane_id.clone(), window_of(p)))
        .collect();
    if model.begin_move(panes, MoveKind::Merge) {
        model.status = format!(
            "merging {} windows: choose the one to keep, then Enter",
            sources.len()
        );
    } else {
        model.status = "those windows have no panes to merge".into();
    }
    Ok(())
}

fn cancel_move(model: &mut Model) {
    model.pending_move = None;
    model.search.clear();
    model.searching = false;
    model.status = "pane move cancelled".into();
}

/// Carry out the pending move into the window under the cursor.
fn confirm_move(model: &mut Model) -> Result<()> {
    let Some(pending) = model.pending_move.clone() else {
        return Ok(());
    };
    let Some(destination) = model.destination_window().map(WindowRow::target) else {
        model.status = "choose a live window: j/k move, Enter confirm, Esc cancel".into();
        return Ok(());
    };

    let mut moved = 0;
    let mut failed = Vec::new();
    for (pane, from) in &pending.panes {
        // The destination may be one of the merged windows; its own panes are
        // already home.
        if *from == destination {
            continue;
        }
        match join_one(pane, &destination) {
            Ok(()) => moved += 1,
            Err(e) => failed.push(format!("{pane}: {e}")),
        }
    }

    // Several panes arriving one at a time stack into ever-thinner slices of
    // whatever pane was active. Tiling once at the end is the only arrangement
    // that is readable without guessing what was wanted; a single pane leaves
    // a hand-made layout alone.
    if moved > 1 {
        let _ = cmd::run(
            "tmux",
            &["select-layout", "-t", &destination, "tiled"],
            cmd::FAST,
        );
    }

    let sources = pending.sources();
    let consumed: Vec<String> = pending
        .panes
        .iter()
        .map(|(pane, _)| pane.clone())
        .chain(sources.iter().map(|w| w.to_string()))
        .collect();
    let sources = sources.len();
    model.pending_move = None;
    model.search.clear();
    model.searching = false;
    model.unmark(&consumed);
    reload(model)?;
    model.status = report(
        match pending.kind {
            MoveKind::Merge => {
                format!("merged {sources} windows — {moved} pane(s) into {destination}")
            }
            MoveKind::Panes => format!("moved {moved} pane(s) into {destination}"),
        },
        failed,
    );
    Ok(())
}

/// Close the selected windows.
///
/// No confirmation prompt: the workspace was snapshotted, and the point of
/// this tool is that closing something is recoverable. The status line says
/// how.
fn kill_windows(model: &mut Model) -> Result<()> {
    let targets = selected_windows(model);
    if targets.is_empty() {
        model.status = match model.current_window() {
            Some(_) => "that window is already gone".into(),
            None => "select a window first".into(),
        };
        return Ok(());
    }

    let mut killed = 0;
    let mut failed = Vec::new();
    for target in &targets {
        match cmd::run("tmux", &["kill-window", "-t", target], cmd::FAST) {
            Ok(_) => killed += 1,
            Err(e) => failed.push(format!("{target}: {e}")),
        }
    }

    model.unmark(&targets);
    reload(model)?;

    // Whether `r` can undo this is a fact about the selected point, not a
    // promise the tool gets to make. A window created since the last save is
    // in no point at all, and telling the user to press `r` after closing
    // several of those is worse than saying nothing.
    let recoverable = targets
        .iter()
        .filter(|t| {
            model
                .rows
                .iter()
                .any(|r| matches!(r, Row::Window(w) if w.gone && &w.target() == *t))
        })
        .count();
    let recovery = match recoverable {
        0 => " — not in the restore point, so r cannot bring them back".to_string(),
        n if n == killed => " — press r to bring them back".to_string(),
        n => format!(" — r brings back {n} of them"),
    };
    model.status = report(format!("killed {killed} window(s){recovery}"), failed);
    Ok(())
}

fn sessions() -> Result<Vec<String>> {
    Ok(crate::collect::cmd::run(
        "tmux",
        &["list-sessions", "-F", "#{session_name}"],
        crate::collect::cmd::FAST,
    )?
    .lines()
    .map(str::to_string)
    .collect())
}

/// Fetch the capture for whatever the cursor is on, if we do not have it.
///
/// Done at draw time rather than on every keystroke: a held-down `j` would
/// otherwise shell out once per repeat for panes that scroll past unseen.
fn refresh_preview(model: &mut Model) {
    // A pane row previews that pane; a window row previews its active one.
    // Without this, expanding a window of three identical claude commands
    // shows the same output whichever you select — and the whole reason to
    // expand is to tell them apart.
    let target = match (model.current_pane(), model.current_window()) {
        (Some(p), _) => p.target().to_string(),
        (None, Some(w)) if !w.gone => w.target(),
        _ => {
            model.preview = None;
            return;
        }
    };
    if model.preview_for(&target).is_some() {
        return;
    }
    let body = tmux::capture_pane(&target, 40).unwrap_or_default();
    model.preview = Some((target, body));
}

/// The window that ran this, as `session:index`.
///
/// Public so `tmc snapshot` renders the same frame the TUI opens on — a
/// snapshot that skipped the focus would review a layout nobody sees.
///
/// Asked of tmux rather than derived from `$TMUX_PANE`, because a popup is
/// itself a pane and `display-message` without a target would name the popup.
/// `-t $TMUX_PANE` pins the answer to the pane the key was pressed in.
pub fn summoning_window() -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let out = crate::collect::cmd::run(
        "tmux",
        &[
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{session_name}:#{window_index}",
        ],
        crate::collect::cmd::FAST,
    )
    .ok()?;
    let target = out.trim().to_string();
    (!target.is_empty()).then_some(target)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::Change;
    use crate::ui::model::WindowRow;

    fn model_with_window() -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = vec![Row::Window(WindowRow {
            session: "projects".into(),
            index: 1,
            name: "alpha".into(),
            panes: 2,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            running_claude: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        })];
        m
    }

    /// Keys that only read state, so they can be exercised without touching
    /// tmux. The mutating ones (m/b/J/x/r/s) shell out and are covered by
    /// their own modules.
    fn press(model: &mut Model, c: char, mods: KeyModifiers) {
        let _ = handle_key(model, KeyCode::Char(c), mods);
    }

    #[test]
    fn esc_leaves_search_for_normal_mode_without_quitting() {
        let mut m = model_with_window();
        m.searching = true;
        for c in "alpha".chars() {
            m.search_push(c);
        }

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.searching, "Esc returns to normal mode");
        assert_eq!(m.search, "alpha", "the query remains as a filter");
        assert!(!m.quit, "the first Esc is a mode change, not an exit");
    }

    #[test]
    fn esc_in_the_tree_quits() {
        // One level per press. The tree is the last one, so Esc there leaves.
        let mut m = model_with_window();
        assert!(!m.searching, "already at the root");

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);

        assert!(m.quit);
    }

    #[test]
    fn two_escapes_from_the_search_line_leave() {
        // The path the TUI actually opens on: search -> tree -> out.
        let mut m = model_with_window();
        m.searching = true;

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.quit, "the first lands in the tree");
        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(m.quit, "the second leaves");
    }

    #[test]
    fn esc_during_a_pane_move_cancels_rather_than_quitting() {
        // The level below the tree still has to be popped first, or a cancel
        // costs the whole session.
        let mut m = model_with_window();
        m.pending_move = Some(crate::ui::model::PendingMove {
            panes: vec![("%1084".into(), "projects:2".into())],
            kind: MoveKind::Panes,
            destination: None,
        });

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.quit, "the move was cancelled, not the program");

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(m.quit, "and the next press is at the root");
    }

    #[test]
    fn tab_hands_the_keys_to_the_tree_but_keeps_the_filter() {
        // The single-key commands live in the tree; the filter is what got you
        // to the right handful of windows, so it survives the switch.
        let mut m = model_with_window();
        m.searching = true;
        for c in "alp".chars() {
            m.search_push(c);
        }

        let _ = handle_key(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert!(!m.searching);
        assert_eq!(m.search, "alp", "the filter is still applied");

        // And a letter is a command again rather than search input.
        let _ = handle_key(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(m.marks.len(), 1, "space marked instead of typing");
    }

    #[test]
    fn typing_in_search_mode_does_not_trigger_commands() {
        // `s` saves and `x` kills a window in the tree. Landing on the search
        // line means a stray keystroke cannot do either.
        let mut m = model_with_window();
        m.searching = true;
        for c in "sx".chars() {
            let _ = handle_key(&mut m, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(m.search, "sx");
        assert!(m.marks.is_empty());
        assert!(!m.quit);
    }

    #[test]
    fn control_chords_move_instead_of_typing() {
        // A `Char(c)` catch-all placed before these swallowed them and typed
        // the letter — clippy caught it as an unreachable arm, but the visible
        // symptom would have been `n` appearing in the query.
        let mut m = Model::new(Vec::new());
        m.rows = vec![
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: "alpha".into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                running_claude: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            }),
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 2,
                name: "beta".into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                running_claude: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            }),
        ];
        m.searching = true;

        let _ = handle_key(&mut m, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(m.search.is_empty(), "nothing was typed");
        assert_eq!(
            m.current_window().map(|w| w.name.clone()),
            Some("beta".into())
        );

        let _ = handle_key(&mut m, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(
            m.current_window().map(|w| w.name.clone()),
            Some("alpha".into())
        );
    }

    #[test]
    fn pane_move_names_the_exact_source_and_destination() {
        assert_eq!(
            join_pane_args("%1084", "tooling:3"),
            ["join-pane", "-d", "-s", "%1084", "-t", "tooling:3"],
        );
    }

    #[test]
    fn esc_cancels_a_pane_move_without_quitting() {
        let mut m = model_with_window();
        m.pending_move = Some(pending_move());

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);

        assert!(m.pending_move.is_none(), "the pending move is gone");
        assert!(!m.quit, "Esc cancels the mode, not the program");
        assert_eq!(m.status, "pane move cancelled");
    }

    /// Two live windows, so the destination picker has something to offer.
    fn model_with_two_windows() -> Model {
        let mut m = model_with_window();
        m.rows.push(Row::Window(WindowRow {
            session: "projects".into(),
            index: 2,
            name: "beta".into(),
            panes: 1,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            running_claude: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        }));
        m
    }

    #[test]
    fn merge_says_what_it_needs_rather_than_guessing() {
        // One window is not a merge, and picking a second for the user would
        // be picking which of 28 to close.
        let mut m = model_with_two_windows();
        m.marks.insert("projects:1".into());

        press(&mut m, 'M', KeyModifiers::NONE);

        assert!(m.pending_move.is_none(), "nothing was started");
        assert!(m.status.contains("two or more"), "status: {}", m.status);
    }

    #[test]
    fn slash_filters_the_destinations_instead_of_cancelling() {
        let mut m = model_with_two_windows();
        assert!(m.begin_move(vec![("%11".into(), "projects:1".into())], MoveKind::Panes));

        press(&mut m, '/', KeyModifiers::NONE);
        assert!(m.searching, "typing now narrows the choices");
        assert!(m.pending_move.is_some(), "the move is still pending");

        // `x` kills a window in the tree. Inside the filter it is a letter.
        press(&mut m, 'x', KeyModifiers::NONE);
        assert_eq!(m.search, "x");
        assert!(m.pending_move.is_some());

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.searching, "Esc closes the filter");
        assert!(
            m.pending_move.is_some(),
            "and only the filter — the move survives one more Esc",
        );
    }

    #[test]
    fn cancelling_a_move_takes_its_filter_with_it() {
        // The query was typed to find a destination; leaving it applied to
        // the tree would hide most of the workspace with no obvious cause.
        let mut m = model_with_two_windows();
        assert!(m.begin_move(vec![("%11".into(), "projects:1".into())], MoveKind::Panes));
        m.searching = true;
        m.search_push('b');

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);

        assert!(m.pending_move.is_none());
        assert!(m.search.is_empty(), "search: {:?}", m.search);
        assert!(!m.quit, "cancelling is not leaving");
    }

    #[test]
    fn a_window_command_prefers_the_marked_windows_over_the_cursor() {
        let mut m = model_with_two_windows();
        m.cursor = 0; // projects:1
        m.marks.insert("projects:2".into());

        assert_eq!(
            selected_windows(&m),
            vec!["projects:2".to_string()],
            "a selection is an explicit answer; the cursor is only a fallback",
        );

        m.clear_marks();
        assert_eq!(selected_windows(&m), vec!["projects:1".to_string()]);
    }

    #[test]
    fn a_pane_command_leaves_the_restore_selection_alone() {
        // Windows staged for `r` and panes staged for `J` coexist in one set.
        // A move that cleared everything would quietly undo the staging.
        let mut m = model_with_two_windows();
        m.marks.insert("projects:1".into());
        m.marks.insert("%11".into());

        m.unmark(&["%11".to_string()]);

        assert_eq!(
            m.marks.iter().cloned().collect::<Vec<_>>(),
            vec!["projects:1".to_string()],
        );
    }

    #[test]
    fn ctrl_c_leaves_from_the_search_line_too() {
        let mut m = model_with_window();
        m.searching = true;
        m.search_push('a');
        let _ = handle_key(&mut m, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(m.quit, "ctrl-c is an unconditional way out");
    }

    #[test]
    fn ctrl_c_quits_rather_than_clearing_marks() {
        // Both are bound to `c`; the modifier guard has to win.
        let mut m = model_with_window();
        m.marks.insert("projects:1".into());

        press(&mut m, 'c', KeyModifiers::CONTROL);
        assert!(m.quit, "ctrl-c must quit");
        assert_eq!(m.marks.len(), 1, "and must not clear marks on the way out");
    }

    #[test]
    fn plain_c_clears_marks_without_quitting() {
        let mut m = model_with_window();
        m.marks.insert("projects:1".into());

        press(&mut m, 'c', KeyModifiers::NONE);
        assert!(m.marks.is_empty());
        assert!(!m.quit);
    }

    #[test]
    fn enter_on_a_window_that_only_exists_in_the_point_explains_itself() {
        let mut m = model_with_window();
        if let Row::Window(w) = &mut m.rows[0] {
            w.gone = true;
        }
        let _ = handle_key(&mut m, KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(m.switch_to, None, "there is nothing to switch to");
        assert!(m.status.contains("restore"), "status: {}", m.status);
    }

    #[test]
    fn enter_on_a_live_window_records_the_switch() {
        let mut m = model_with_window();
        let _ = handle_key(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.switch_to.as_deref(), Some("projects:1"));
    }

    #[test]
    fn a_new_keypress_clears_the_previous_status() {
        // Otherwise a stale message sits under an unrelated action.
        let mut m = model_with_window();
        m.status = "something happened".into();
        press(&mut m, 'j', KeyModifiers::NONE);
        assert!(m.status.is_empty());
    }
    // --- CJK input source ---------------------------------------------------

    // Under a Korean input source every shortcut arrives as a jamo. tmc starts
    // in search mode, so the first thing a user does is Tab out to the tree --
    // where, without this mapping, the whole keyboard was dead.
    #[test]
    fn hangul_clears_marks_like_latin_c() {
        let mut m = model_with_window();
        m.searching = false;
        m.marks.insert("projects:1".into());

        // `ㅊ` is the physical `c` under the 2-set layout.
        press(&mut m, 'ㅊ', KeyModifiers::NONE);
        assert!(m.marks.is_empty(), "ㅊ (physical c) must clear marks");
        assert!(!m.quit);
    }

    #[test]
    fn hangul_quits_like_latin_q() {
        let mut m = model_with_window();
        m.searching = false;
        // `ㅂ` sits on the physical `q` key.
        press(&mut m, 'ㅂ', KeyModifiers::NONE);
        assert!(m.quit, "ㅂ (physical q) must quit");
    }

    // Search mode is where letters type, so a jamo there is the query -- a
    // Korean window name would otherwise be unsearchable.
    #[test]
    fn search_mode_keeps_hangul_verbatim() {
        let mut m = model_with_window();
        m.searching = true;
        press(&mut m, 'ㅂ', KeyModifiers::NONE);
        assert_eq!(m.search, "ㅂ", "searching must not rewrite the jamo");
        assert!(!m.quit);
    }

    fn pending_move() -> crate::ui::model::PendingMove {
        crate::ui::model::PendingMove {
            panes: vec![("%1084".into(), "projects:2".into())],
            kind: MoveKind::Panes,
            destination: Some("tooling:3".into()),
        }
    }

    // The pane-move picker is its own mode ahead of the tree keys, and it is
    // all shortcuts, so it normalizes too.
    #[test]
    fn hangul_moves_the_destination_cursor_in_pane_move() {
        let mut with_jamo = model_with_window();
        with_jamo.searching = false;
        with_jamo.pending_move = Some(pending_move());
        // `ㅓ` is the physical `j`.
        press(&mut with_jamo, 'ㅓ', KeyModifiers::NONE);

        let mut with_latin = model_with_window();
        with_latin.searching = false;
        with_latin.pending_move = Some(pending_move());
        press(&mut with_latin, 'j', KeyModifiers::NONE);

        // j is a bound navigation key, so it leaves no "choose a window" hint;
        // an unmapped key would. Asserting both are empty AND equal makes the
        // comparison meaningful rather than two identical error strings.
        assert_eq!(with_latin.status, "", "j should navigate, not fall through");
        assert_eq!(
            with_jamo.status, with_latin.status,
            "ㅓ must reach the same handler j does"
        );
    }
}
