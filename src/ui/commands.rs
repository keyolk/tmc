//! The commands that change tmux or the restore points.
//!
//! Each is split in two. The first half runs on the input thread against the
//! model — what is selected, whether the command applies, the message when it
//! does not — and touches nothing outside the process. The second half is a
//! `Job`: the tmux calls, run on the worker, ending in a `Finish` that puts
//! the outcome on screen once the tree has been re-read.
//!
//! Resolving the targets up front is deliberate: the command acts on what
//! was on screen when it was chosen, not on whatever a reload that lands
//! mid-command would put under the cursor.

use std::collections::HashMap;

use anyhow::Result;

use super::model::{ActionKind, Model, MoveKind, Row, WindowRow, session_mark};
use super::status::{Level, Status};
use super::worker::{Done, Job};
use crate::collect::{cmd, proc, tmux};
use crate::config;
use crate::layout::{point, restore, save};
use crate::rearrange;

/// Run a menu command. Exhaustive over `ActionKind`, which is what keeps the
/// menu and the commands from drifting apart.
pub(super) fn run_action(model: &mut Model, kind: ActionKind) -> Option<Job> {
    match kind {
        ActionKind::Break => break_panes(model),
        ActionKind::Join => {
            move_panes(model);
            None
        }
        ActionKind::Merge => {
            merge_windows(model);
            None
        }
        ActionKind::Send => move_windows(model),
        ActionKind::Kill => kill_windows(model),
        ActionKind::NewSession => {
            model.naming = Some(String::new());
            None
        }
        ActionKind::KillSession => kill_sessions(model),
    }
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
pub(super) fn selected_panes(model: &Model) -> Vec<(String, String)> {
    let marked = model.marked_panes();
    if marked.is_empty() {
        return model
            .current_pane()
            .map(|p| vec![(p.target().to_string(), p.window_target())])
            .unwrap_or_default();
    }
    model
        .live
        .iter()
        .filter(|p| marked.contains(&p.pane_id))
        .map(|p| (p.pane_id.clone(), window_of(p)))
        .collect()
}

/// The live windows a window command acts on: the marked ones, or the one
/// under the cursor.
pub(super) fn selected_windows(model: &Model) -> Vec<String> {
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

/// One status line: what happened, then what did not. A partial failure is
/// a warning: something did happen, and the user has to look at what did not.
fn report(status: &mut Status, done: String, failed: Vec<String>) {
    if failed.is_empty() {
        status.success(done);
    } else {
        status.warn(format!("{done}; failed: {}", failed.join(", ")));
    }
}

pub(super) fn save_point() -> Job {
    Job::new("saving", || {
        let panes = tmux::panes().unwrap_or_default();
        if panes.is_empty() {
            return Ok(Done::status(Level::Warn, "no tmux panes to save"));
        }
        let tree = proc::Tree::capture_with_args()?;
        let now = crate::clock::now();
        let sessions = save::snapshot(&panes, &tree, &now.timestamp);
        let dir = save::layout_dir().join(&now.compact);
        for session in &sessions {
            save::write(session, &dir.join(format!("{}.json", session.session)))?;
        }
        // The new point becomes the one being compared against, which is
        // almost always what the user wants next: they saved because this
        // state is worth keeping, so the diff should now read as empty.
        let points = point::list(&save::layout_dir());
        let n = sessions.len();
        let mut done = Done::then(move |m: &mut Model| {
            m.status
                .success(format!("saved {n} session(s) as {}", now.compact));
        });
        done.points = Some(points);
        Ok(done)
    })
}

/// The windows `r` would bring back, as `session:index` of the point, and
/// how many marked windows it leaves alone because they are running.
///
/// Only windows that exist solely in the point: the point's index of a live
/// window names whatever sits there now, and recreating a window that is open
/// would double it. A marked session stands for every one of its windows the
/// point has and the server does not — which, for a session that is not
/// running at all, is the whole session.
pub(super) fn restore_targets(model: &Model) -> (Vec<String>, usize) {
    let gone = |w: &WindowRow| w.gone;
    if model.marks.is_empty() {
        let all = model
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Window(w) if gone(w) => Some(w.target()),
                _ => None,
            })
            .collect();
        return (all, 0);
    }
    let sessions = model.marked_sessions();
    let mut targets = Vec::new();
    let mut running = 0;
    for row in &model.rows {
        let Row::Window(w) = row else { continue };
        let marked = model.marks.contains(&w.target()) || sessions.contains(&w.session);
        if !marked {
            continue;
        }
        if gone(w) {
            targets.push(w.target());
        } else if model.marks.contains(&w.target()) {
            // A window under a marked session is not counted: marking the
            // session did not ask for it specifically.
            running += 1;
        }
    }
    (targets, running)
}

pub(super) fn restore_marked(model: &mut Model) -> Option<Job> {
    let Some(p) = model.current_point() else {
        model.status.warn("no restore point selected");
        return None;
    };
    let reference = p.reference.clone();

    // With nothing marked, restore what is missing — the windows the point has
    // and the live server does not. Restoring everything would mean recreating
    // windows that are already open.
    let (targets, running) = restore_targets(model);
    if targets.is_empty() {
        model.status.warn(if running > 0 {
            "the marked windows are running — r brings back only what is gone"
        } else if model.marks.is_empty() {
            "nothing is missing — mark a session or window with space"
        } else {
            "nothing marked is missing from the server"
        });
        return None;
    }
    // Consumed on success: the windows restored, and the session marks that
    // selected them. Window marks on running windows were not used and stay.
    let mut consumed = targets.clone();
    consumed.extend(model.marked_sessions().iter().map(|s| session_mark(s)));

    Some(Job::new("restoring", move || {
        let saved = point::read(&reference)?;
        // A config error stops the restore rather than the app: `r` is one
        // keystroke inside a popup, and quitting the TUI to report a typo in
        // a file the user can fix in the next pane is the wrong trade.
        let autorun = match config::load() {
            Ok(cfg) => cfg.restore,
            Err(e) => {
                return Ok(Done::status(
                    Level::Error,
                    format!("{}: {e:#}", config::path().display()),
                ));
            }
        };
        let mut server = restore::Server;
        let mut windows = 0;
        let mut sessions = 0;
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
            // One session failing — a path that no longer exists, a name tmux
            // refuses — must not strand the others behind it: several are
            // restored in one press now.
            let report = match restore::session(
                &mut server,
                session,
                restore::Selection {
                    windows: Some(&indices),
                    // Only windows the server does not have are targeted, so
                    // adding them to a running session doubles nothing.
                    append: true,
                },
                &autorun,
                false,
                // Never forced from the TUI: closing a running session is a
                // CLI decision, with the diff and confirmation that come
                // with it.
                false,
            ) {
                Ok(report) => report,
                Err(e) => {
                    notes.push(format!("{}: {e:#}", session.session));
                    continue;
                }
            };
            if report.windows > 0 {
                sessions += 1;
            }
            windows += report.windows;
            ran += report.commands_run;
            notes.extend(report.notes);
        }

        Ok(Done::then(move |m: &mut Model| {
            // Only what this restore used: a mark placed while it ran is a
            // new selection, not part of this one.
            m.unmark(&consumed);
            let mut line = format!("restored {windows} window(s)");
            if sessions > 1 {
                line.push_str(&format!(" in {sessions} sessions"));
            }
            // Worth a word even in a one-line status: the panes that
            // started on their own are the ones the user did not press a
            // key for.
            if ran > 0 {
                line.push_str(&format!(", ran {ran} command(s)"));
            }
            if running > 0 {
                line.push_str(&format!(", {running} marked already running"));
            }
            if notes.is_empty() {
                m.status.success(line);
            } else {
                m.status.warn(format!("{line}; {}", notes.join("; ")));
            }
        }))
    }))
}

/// Create the session whose name was typed after `x n`.
///
/// Started in the directory of what the cursor is on, which is usually the
/// project the new session is for; tmux's own default is wherever the server
/// was started. Not switched to: the new session appears in the tree with
/// the cursor on it, and Enter is one key away.
pub(super) fn create_session(model: &mut Model) -> Option<Job> {
    let name = model
        .naming
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        model.status.warn("type a name for the session, or esc");
        return None;
    }
    // tmux rewrites both to `_` and the session would not have the name
    // that was typed — and `:` and `.` are what target syntax splits on.
    if name.contains([':', '.']) {
        model
            .status
            .warn("a session name cannot contain ':' or '.'");
        return None;
    }
    if model.session_is_live(&name) {
        model
            .status
            .warn(format!("session '{name}' already exists"));
        return None;
    }
    model.naming = None;

    let path = match (model.current_pane(), model.current_window()) {
        (Some(p), _) => Some(p.path.clone()),
        (None, Some(w)) => model
            .live
            .iter()
            .find(|p| p.session == w.session && p.window_index == w.index && p.active)
            .map(|p| p.path.clone()),
        _ => None,
    };

    Some(Job::new("creating session", move || {
        let mut args = vec![
            "new-session",
            "-d",
            "-s",
            &name,
            "-P",
            "-F",
            "#{session_name}:#{window_index}",
        ];
        if let Some(path) = &path {
            args.extend(["-c", path.as_str()]);
        }
        let target = match cmd::run("tmux", &args, cmd::FAST) {
            Ok(out) => out.trim().to_string(),
            Err(e) => {
                return Ok(Done::status(
                    Level::Error,
                    format!("new session {name}: {e:#}"),
                ));
            }
        };
        Ok(Done::then(move |m: &mut Model| {
            m.focus(&target);
            m.status
                .success(format!("created session {name} — enter switches to it"));
        }))
    }))
}

/// Close the selected sessions, every window in them included.
///
/// No confirmation, for the reason `kill_windows` has none: a session the
/// point holds comes back with `r`, and the status says whether that is so.
/// Closing every session is refused upstream, in the menu — tmux exits with
/// its last one, and this popup with it.
fn kill_sessions(model: &mut Model) -> Option<Job> {
    let targets = model.selected_sessions();
    if targets.is_empty() {
        model.status.warn("select a running session first");
        return None;
    }

    Some(Job::new("closing sessions", move || {
        // The session this popup was opened from goes last, after the client
        // is moved off it. Killing it first would detach the client — and
        // close the popup — with the rest of the list still to do.
        let own = super::app::summoning_window()
            .and_then(|t| t.split_once(':').map(|(s, _)| s.to_string()));
        let mut order: Vec<&String> = targets
            .iter()
            .filter(|s| Some(*s) != own.as_ref())
            .collect();
        let closing_own = targets.iter().find(|s| Some(*s) == own.as_ref());

        let mut killed = 0;
        let mut failed = Vec::new();
        if let Some(own) = closing_own {
            let survivor = sessions()?.into_iter().find(|s| !targets.contains(s));
            match survivor {
                Some(next) => {
                    if let Err(e) = cmd::run(
                        "tmux",
                        &["switch-client", "-t", &format!("={next}")],
                        cmd::FAST,
                    ) {
                        failed.push(format!("switch to {next}: {e}"));
                    }
                    order.push(own);
                }
                None => failed.push(format!("{own}: it is the last session")),
            }
        }
        for name in order {
            match cmd::run(
                "tmux",
                &["kill-session", "-t", &session_mark(name)],
                cmd::FAST,
            ) {
                Ok(_) => killed += 1,
                Err(e) => failed.push(format!("{name}: {e}")),
            }
        }

        let consumed: Vec<String> = targets.iter().map(|s| session_mark(s)).collect();
        Ok(Done::then(move |m: &mut Model| {
            // Runs after the re-read: a closed session the point holds is
            // still listed, with every window gone.
            let recoverable = targets
                .iter()
                .filter(|s| {
                    !m.session_is_live(s)
                        && m.rows
                            .iter()
                            .any(|r| matches!(r, Row::Window(w) if w.gone && &w.session == *s))
                })
                .count();
            // Session marks stay on what can be restored, so `r` right after
            // brings exactly these back.
            let keep: Vec<String> = targets
                .iter()
                .filter(|s| {
                    m.rows
                        .iter()
                        .any(|r| matches!(r, Row::Session { name, .. } if name == *s))
                })
                .map(|s| session_mark(s))
                .collect();
            let drop: Vec<String> = consumed.into_iter().filter(|k| !keep.contains(k)).collect();
            m.unmark(&drop);
            let recovery = match recoverable {
                0 => " — not in the restore point, so r cannot bring them back".to_string(),
                n if n == killed => " — r brings them back".to_string(),
                n => format!(" — r brings back {n} of them"),
            };
            report(
                &mut m.status,
                format!("closed {killed} session(s){recovery}"),
                failed,
            );
        }))
    }))
}

/// Move the selected windows to the other session.
///
/// With two sessions the destination is unambiguous, which is the whole reason
/// this is one keystroke here and a prompt in tmux.sh. With more, the status
/// line says what to do instead of guessing.
fn move_windows(model: &mut Model) -> Option<Job> {
    let targets = selected_windows(model);
    if targets.is_empty() {
        model.status.warn("that window is not running");
        return None;
    }

    Some(Job::new("moving", move || {
        let all = sessions()?;
        if all.len() < 2 {
            return Ok(Done::status(Level::Warn, "no other session to move to"));
        }
        if all.len() > 2 {
            return Ok(Done::status(
                Level::Warn,
                format!(
                    "several destinations ({}); use tmux directly",
                    all.len() - 1
                ),
            ));
        }

        // With two sessions each window goes to the one it is not in. Grouped
        // by where they are so each group is one `send_windows`, which
        // renumbers only after its whole group has moved.
        let mut by_here: Vec<(String, Vec<String>)> = Vec::new();
        for target in &targets {
            let here = target.split(':').next().unwrap_or_default().to_string();
            match by_here.iter_mut().find(|(h, _)| *h == here) {
                Some((_, v)) => v.push(target.clone()),
                None => by_here.push((here, vec![target.clone()])),
            }
        }
        let mut moved = 0;
        let mut failed = Vec::new();
        for (here, group) in &by_here {
            let Some(dest) = all.iter().find(|s| *s != here) else {
                continue;
            };
            let out = rearrange::send_windows(group, dest);
            moved += out.moved;
            failed.extend(out.failed);
        }

        Ok(Done::then(move |m: &mut Model| {
            m.unmark(&targets);
            report(&mut m.status, format!("moved {moved} window(s)"), failed);
        }))
    }))
}

/// Break the selected panes out, each into a window of its own.
///
/// Requires panes to be selected. Addressing this by window — which an
/// earlier version did — makes tmux use that window's *active* pane, so the
/// thing that moved was not the thing on screen.
fn break_panes(model: &mut Model) -> Option<Job> {
    let chosen = selected_panes(model);
    if chosen.is_empty() {
        model
            .status
            .warn("select a pane first — l expands a window, space marks one");
        return None;
    }

    Some(Job::new("breaking out", move || {
        // tmux refuses to break a window's only pane, and rightly: that is a
        // rename, not a move. Counted from a fresh read and down as panes
        // leave, so breaking two of a window's three still stops at the last
        // one rather than trusting a count the previous break invalidated.
        let mut remaining: HashMap<String, usize> = HashMap::new();
        for p in &tmux::panes().unwrap_or_default() {
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
            // `-d` leaves the focus where it is: this is a popup, and stealing
            // the client to the new window would drop the user somewhere they
            // did not ask to be.
            match cmd::run("tmux", &["break-pane", "-d", "-s", pane], cmd::FAST) {
                Ok(_) => {
                    *left -= 1;
                    broken += 1;
                }
                Err(e) => failed.push(format!("{pane}: {e}")),
            }
        }

        let consumed: Vec<String> = chosen.into_iter().map(|(pane, _)| pane).collect();
        Ok(Done::then(move |m: &mut Model| {
            m.unmark(&consumed);
            let mut done = format!("broke out {broken} pane(s)");
            if alone > 0 {
                done.push_str(&format!(" — {alone} already alone in its window"));
            }
            report(&mut m.status, done, failed);
        }))
    }))
}

/// Pick panes, then move them into an explicitly selected window.
///
/// The first press remembers them and turns the tree into destination
/// selection; Enter joins. A previous version always used `$TMUX_PANE`, which
/// meant "the window that opened tmc" rather than the window the user wanted.
fn move_panes(model: &mut Model) {
    let chosen = selected_panes(model);
    if chosen.is_empty() {
        model
            .status
            .warn("select a pane first — l expands a window, space marks one");
        return;
    }
    let what = match chosen.as_slice() {
        [(pane, _)] => pane.clone(),
        many => format!("{} panes", many.len()),
    };
    if model.begin_move(chosen, MoveKind::Panes) {
        model
            .status
            .info(format!("moving {what}: choose a window, then Enter"));
    } else {
        model.status.warn("no other live window to move that into");
    }
}

/// Merge the marked windows into one.
///
/// tmux has no merge: it is every pane of the sources joined into the window
/// that is being kept, which the sources then close themselves over, having
/// nothing left. Which window survives is chosen the same way a pane move
/// chooses a destination — including one of the marked ones, which is the
/// usual intent — so the answer is never "whichever tmux considered active".
fn merge_windows(model: &mut Model) {
    let sources = model.marked_live_windows();
    if sources.len() < 2 {
        model
            .status
            .warn("mark two or more windows with space, then x m");
        return;
    }

    let panes: Vec<(String, String)> = model
        .live
        .iter()
        .filter(|p| sources.contains(&window_of(p)))
        .map(|p| (p.pane_id.clone(), window_of(p)))
        .collect();
    if model.begin_move(panes, MoveKind::Merge) {
        model.status.info(format!(
            "merging {} windows: choose the one to keep, then Enter",
            sources.len()
        ));
    } else {
        model.status.warn("those windows have no panes to merge");
    }
}

pub(super) fn cancel_move(model: &mut Model) {
    model.pending_move = None;
    model.search.clear();
    model.searching = false;
    model.status.info("pane move cancelled");
}

/// Carry out the pending move into the window under the cursor.
pub(super) fn confirm_move(model: &mut Model) -> Option<Job> {
    let pending = model.pending_move.clone()?;
    let Some(destination) = model.destination_window().map(WindowRow::target) else {
        let hint = super::keys::footer(super::keys::Ctx::Move, model, usize::MAX);
        model.status.warn(format!("choose a live window: {hint}"));
        return None;
    };
    // The choice is made; the tree is the tree again while tmux catches up.
    model.pending_move = None;
    model.search.clear();
    model.searching = false;

    Some(Job::new("moving panes", move || {
        let out = rearrange::join_panes(&pending.panes, &destination);
        let (moved, failed) = (out.moved, out.failed);

        let sources = pending.sources();
        let consumed: Vec<String> = pending
            .panes
            .iter()
            .map(|(pane, _)| pane.clone())
            .chain(sources.iter().map(|w| w.to_string()))
            .collect();
        let n_sources = sources.len();
        let kind = pending.kind;
        Ok(Done::then(move |m: &mut Model| {
            m.unmark(&consumed);
            report(
                &mut m.status,
                match kind {
                    MoveKind::Merge => {
                        format!("merged {n_sources} windows — {moved} pane(s) into {destination}")
                    }
                    MoveKind::Panes => format!("moved {moved} pane(s) into {destination}"),
                },
                failed,
            );
        }))
    }))
}

/// Close the selected windows.
///
/// No confirmation prompt: the workspace was snapshotted, and the point of
/// this tool is that closing something is recoverable. The status line says
/// how.
fn kill_windows(model: &mut Model) -> Option<Job> {
    let targets = selected_windows(model);
    if targets.is_empty() {
        model.status.warn(match model.current_window() {
            Some(_) => "that window is already gone",
            None => "select a window first",
        });
        return None;
    }

    Some(Job::new("closing", move || {
        let mut killed = 0;
        let mut failed = Vec::new();
        for target in &targets {
            match cmd::run("tmux", &["kill-window", "-t", target], cmd::FAST) {
                Ok(_) => killed += 1,
                Err(e) => failed.push(format!("{target}: {e}")),
            }
        }

        Ok(Done::then(move |m: &mut Model| {
            m.unmark(&targets);
            // Whether `r` can undo this is a fact about the selected point,
            // not a promise the tool gets to make. A window created since the
            // last save is in no point at all, and telling the user to press
            // `r` after closing several of those is worse than saying nothing.
            // Runs after the re-read, so `gone` already reflects the kill.
            let recoverable = targets
                .iter()
                .filter(|t| {
                    m.rows
                        .iter()
                        .any(|r| matches!(r, Row::Window(w) if w.gone && &w.target() == *t))
                })
                .count();
            let recovery = match recoverable {
                0 => " — not in the restore point, so r cannot bring them back".to_string(),
                n if n == killed => " — press r to bring them back".to_string(),
                n => format!(" — r brings back {n} of them"),
            };
            report(
                &mut m.status,
                format!("killed {killed} window(s){recovery}"),
                failed,
            );
        }))
    }))
}

fn sessions() -> Result<Vec<String>> {
    Ok(cmd::run(
        "tmux",
        &["list-sessions", "-F", "#{session_name}"],
        cmd::FAST,
    )?
    .lines()
    .map(str::to_string)
    .collect())
}
