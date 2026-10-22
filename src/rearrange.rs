//! Moving windows between sessions and merging windows: the tmux calls behind
//! the TUI's `x s` and `x m`, shared with `tmc window send` and `tmc window
//! merge` so a window moved from a script lands the way one moved by hand does.
//!
//! None of this decides *what* to move. The TUI resolves that from marks and
//! the cursor; the CLI takes it from its arguments. Both hand over exact
//! targets, and every call here reports what failed instead of stopping at the
//! first failure, because a half-done move is still a move the user must see.

use anyhow::{Result, bail};

use crate::collect::cmd;

/// What a rearrangement did: how many things moved, and what did not.
#[derive(Debug, Default)]
pub struct Outcome {
    pub moved: usize,
    pub failed: Vec<String>,
}

/// The session a window target belongs to, asked of tmux.
///
/// A `session:index` target names it, but a `@id` does not, and the CLI is
/// handed ids. Asking for both keeps one path.
fn session_of(target: &str) -> Result<String> {
    let name = cmd::run(
        "tmux",
        &["display-message", "-p", "-t", target, "#{session_name}"],
        cmd::FAST,
    )?;
    let name = name.trim();
    if name.is_empty() {
        bail!("{target}: no such window");
    }
    Ok(name.to_string())
}

/// The window id (`@N`) of a window target, so targets given in different
/// spellings compare equal.
pub fn window_id(target: &str) -> Result<String> {
    let id = cmd::run(
        "tmux",
        &["display-message", "-p", "-t", target, "#{window_id}"],
        cmd::FAST,
    )?;
    let id = id.trim();
    if id.is_empty() {
        bail!("{target}: no such window");
    }
    Ok(id.to_string())
}

/// Move windows to `dest`, then close the holes they left.
pub fn send_windows(targets: &[String], dest: &str) -> Outcome {
    let mut out = Outcome::default();
    let mut emptied: Vec<String> = Vec::new();
    for target in targets {
        // Resolved before the move: afterwards the target may name nothing,
        // or a different window that took its index.
        let here = match session_of(target) {
            Ok(s) => s,
            Err(e) => {
                out.failed.push(e.to_string());
                continue;
            }
        };
        if here == dest {
            continue;
        }
        match cmd::run(
            "tmux",
            &["move-window", "-s", target, "-t", &format!("{dest}:")],
            cmd::FAST,
        ) {
            Ok(_) => {
                out.moved += 1;
                if !emptied.contains(&here) {
                    emptied.push(here);
                }
            }
            Err(e) => out.failed.push(format!("{target}: {e}")),
        }
    }

    // move-window leaves a hole; tmux only renumbers on close. Deferred
    // until every window has moved, because renumbering mid-loop would
    // invalidate the `session:index` targets still waiting their turn.
    // A failed renumber leaves a gap in the numbering, nothing worse.
    for session in &emptied {
        if let Err(e) = cmd::run("tmux", &["move-window", "-r", "-t", session], cmd::FAST) {
            out.failed.push(format!("renumber {session}: {e}"));
        }
    }
    out
}

/// Arguments that move one exact pane into one exact window.
///
/// `join-pane` calls its target a dst-pane, but tmux accepts a window target and
/// resolves that to the window's active pane. That gives the requested window
/// without relying on whichever pane launched the popup.
pub fn join_pane_args<'a>(pane: &'a str, destination: &'a str) -> [&'a str; 6] {
    ["join-pane", "-d", "-s", pane, "-t", destination]
}

/// Move one pane, making room first if the destination has none.
///
/// `join-pane` splits the destination's *active* pane, and `-d` keeps that the
/// same pane throughout — so each join halves what the last one halved, and a
/// merge fails partway with "no space for new pane". Tiling and retrying costs
/// an extra exec only when that actually happens, which leaves a hand-made
/// layout alone in the ordinary single-pane case.
pub fn join_one(pane: &str, destination: &str) -> Result<()> {
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

/// Join panes into `destination`, skipping the ones already there.
///
/// `panes` pairs each pane with the window it is in, spelled the same way as
/// `destination`. A merge is this over every pane of the merged windows; the
/// windows left empty close themselves.
pub fn join_panes(panes: &[(String, String)], destination: &str) -> Outcome {
    let mut out = Outcome::default();
    for (pane, from) in panes {
        // The destination may be one of the merged windows; its own panes
        // are already home.
        if from == destination {
            continue;
        }
        match join_one(pane, destination) {
            Ok(()) => out.moved += 1,
            Err(e) => out.failed.push(format!("{pane}: {e}")),
        }
    }

    // Several panes arriving one at a time stack into ever-thinner slices
    // of whatever pane was active. Tiling once at the end is the only
    // arrangement that is readable without guessing what was wanted; a
    // single pane leaves a hand-made layout alone.
    if out.moved > 1
        && let Err(e) = cmd::run(
            "tmux",
            &["select-layout", "-t", destination, "tiled"],
            cmd::FAST,
        )
    {
        out.failed.push(format!("tile {destination}: {e}"));
    }
    out
}

/// Every pane of `windows` paired with its window id, for `join_panes`.
pub fn panes_of(windows: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for w in windows {
        let raw = cmd::run(
            "tmux",
            &["list-panes", "-t", w, "-F", "#{pane_id} #{window_id}"],
            cmd::FAST,
        )?;
        for line in raw.lines() {
            if let Some((pane, win)) = line.split_once(' ') {
                out.push((pane.to_string(), win.to_string()));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_join_names_the_exact_source_and_destination() {
        assert_eq!(
            join_pane_args("%1084", "tooling:3"),
            ["join-pane", "-d", "-s", "%1084", "-t", "tooling:3"],
        );
    }
}
