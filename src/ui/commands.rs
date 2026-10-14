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

use super::model::{ActionKind, Model, MoveKind, Row, WindowRow};
use super::worker::{Done, Job};
use crate::collect::{cmd, proc, tmux};
use crate::config;
use crate::layout::{point, restore, save};

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

/// One status line: what happened, then what did not.
fn report(done: String, failed: Vec<String>) -> String {
    if failed.is_empty() {
        done
    } else {
        format!("{done}; failed: {}", failed.join(", "))
    }
}

pub(super) fn save_point() -> Job {
    Job::new("saving", || {
        let panes = tmux::panes().unwrap_or_default();
        if panes.is_empty() {
            return Ok(Done::status("no tmux panes to save"));
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
            m.status = format!("saved {n} session(s) as {}", now.compact);
        });
        done.points = Some(points);
        Ok(done)
    })
}

pub(super) fn restore_marked(model: &mut Model) -> Option<Job> {
    let Some(p) = model.current_point() else {
        model.status = "no restore point selected".into();
        return None;
    };
    let reference = p.reference.clone();

    // With nothing marked, restore what is missing — the windows the point has
    // and the live server does not. Restoring everything would mean recreating
    // windows that are already open.
    let targets: Vec<String> = if model.marks.is_empty() {
        model
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Window(w) if w.gone => Some(w.target()),
                _ => None,
            })
            .collect()
    } else {
        model.marks.iter().cloned().collect()
    };
    if targets.is_empty() {
        model.status = "nothing to restore — mark a window with space".into();
        return None;
    }

    Some(Job::new("restoring", move || {
        let saved = point::read(&reference)?;
        // A config error stops the restore rather than the app: `r` is one
        // keystroke inside a popup, and quitting the TUI to report a typo in
        // a file the user can fix in the next pane is the wrong trade.
        let autorun = match config::load() {
            Ok(cfg) => cfg.restore,
            Err(e) => {
                return Ok(Done::status(format!("{}: {e:#}", config::path().display())));
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
                // Never forced from the TUI: `r` here restores windows the
                // point has and the server does not, so there is nothing live
                // to displace. Closing a running session is a CLI decision,
                // with the diff and confirmation that come with it.
                false,
            )?;
            windows += report.windows;
            ran += report.commands_run;
            notes.extend(report.notes);
        }

        Ok(Done::then(move |m: &mut Model| {
            // Only what this restore used: a mark placed while it ran is a
            // new selection, not part of this one.
            m.unmark(&targets);
            m.status = if notes.is_empty() {
                let mut line = format!("restored {windows} window(s)");
                // Worth a word even in a one-line status: the panes that
                // started on their own are the ones the user did not press a
                // key for.
                if ran > 0 {
                    line.push_str(&format!(", ran {ran} command(s)"));
                }
                line
            } else {
                notes.join("; ")
            };
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
        model.status = "that window is not running".into();
        return None;
    }

    Some(Job::new("moving", move || {
        let all = sessions()?;
        if all.len() < 2 {
            return Ok(Done::status("no other session to move to"));
        }
        if all.len() > 2 {
            return Ok(Done::status(format!(
                "several destinations ({}); use tmux directly",
                all.len() - 1
            )));
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

        // move-window leaves a hole; tmux only renumbers on close. Deferred
        // until every window has moved, because renumbering mid-loop would
        // invalidate the `session:index` targets still waiting their turn.
        // A failed renumber leaves a gap in the numbering, nothing worse.
        for session in &emptied {
            if let Err(e) = cmd::run("tmux", &["move-window", "-r", "-t", session], cmd::FAST) {
                failed.push(format!("renumber {session}: {e}"));
            }
        }

        Ok(Done::then(move |m: &mut Model| {
            m.unmark(&targets);
            m.status = report(format!("moved {moved} window(s)"), failed);
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
        model.status = "select a pane first — l expands a window, space marks one".into();
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
            m.status = report(done, failed);
        }))
    }))
}

/// Arguments that move one exact pane into one exact window.
///
/// `join-pane` calls its target a dst-pane, but tmux accepts a window target and
/// resolves that to the window's active pane. That gives the requested window
/// without relying on whichever pane launched the popup.
pub(super) fn join_pane_args<'a>(pane: &'a str, destination: &'a str) -> [&'a str; 6] {
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
            // The retry below reports the failure that matters; a tiling
            // that did not take shows up there as "no space" again.
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
/// The first press remembers them and turns the tree into destination
/// selection; Enter joins. A previous version always used `$TMUX_PANE`, which
/// meant "the window that opened tmc" rather than the window the user wanted.
fn move_panes(model: &mut Model) {
    let chosen = selected_panes(model);
    if chosen.is_empty() {
        model.status = "select a pane first — l expands a window, space marks one".into();
        return;
    }
    let what = match chosen.as_slice() {
        [(pane, _)] => pane.clone(),
        many => format!("{} panes", many.len()),
    };
    if model.begin_move(chosen, MoveKind::Panes) {
        model.status = format!("moving {what}: choose a window, then Enter");
    } else {
        model.status = "no other live window to move that into".into();
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
        model.status = "mark two or more windows with space, then x m".into();
        return;
    }

    let panes: Vec<(String, String)> = model
        .live
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
}

pub(super) fn cancel_move(model: &mut Model) {
    model.pending_move = None;
    model.search.clear();
    model.searching = false;
    model.status = "pane move cancelled".into();
}

/// Carry out the pending move into the window under the cursor.
pub(super) fn confirm_move(model: &mut Model) -> Option<Job> {
    let pending = model.pending_move.clone()?;
    let Some(destination) = model.destination_window().map(WindowRow::target) else {
        model.status = format!(
            "choose a live window: {}",
            super::keys::footer(super::keys::Ctx::Move, model, usize::MAX)
        );
        return None;
    };
    // The choice is made; the tree is the tree again while tmux catches up.
    model.pending_move = None;
    model.search.clear();
    model.searching = false;

    Some(Job::new("moving panes", move || {
        let mut moved = 0;
        let mut failed = Vec::new();
        for (pane, from) in &pending.panes {
            // The destination may be one of the merged windows; its own panes
            // are already home.
            if *from == destination {
                continue;
            }
            match join_one(pane, &destination) {
                Ok(()) => moved += 1,
                Err(e) => failed.push(format!("{pane}: {e}")),
            }
        }

        // Several panes arriving one at a time stack into ever-thinner slices
        // of whatever pane was active. Tiling once at the end is the only
        // arrangement that is readable without guessing what was wanted; a
        // single pane leaves a hand-made layout alone.
        if moved > 1
            && let Err(e) = cmd::run(
                "tmux",
                &["select-layout", "-t", &destination, "tiled"],
                cmd::FAST,
            )
        {
            failed.push(format!("tile {destination}: {e}"));
        }

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
            m.status = report(
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
        model.status = match model.current_window() {
            Some(_) => "that window is already gone".into(),
            None => "select a window first".into(),
        };
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
            m.status = report(format!("killed {killed} window(s){recovery}"), failed);
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
