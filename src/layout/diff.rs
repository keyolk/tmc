//! Comparing a restore point against the live server.
//!
//! This is the reason the tool exists. 24 hourly snapshots accumulate but
//! there was no way to ask the only question that matters — *what has changed
//! since?* — so they were never used until something broke, and by then it was
//! too late to know which one to reach for.

use crate::collect::{command, proc, tmux};
use crate::layout::{Pane, Session, Window};

/// How a window or pane compares to the saved point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Present in both, and the same.
    Same,
    /// Present in both, but different.
    Modified,
    /// Live only — created since the point was taken.
    Added,
    /// Saved only — gone from the live server.
    Removed,
}

impl Change {
    /// The marker shown in the tree. Punctuation, not colour, so the display
    /// survives `NO_COLOR` and monochrome terminals.
    pub fn marker(self) -> char {
        match self {
            Change::Same => ' ',
            Change::Modified => '~',
            Change::Added => '+',
            Change::Removed => '-',
        }
    }
}

/// One window's comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowDiff {
    pub session: String,
    pub index: u32,
    pub name: String,
    pub change: Change,
    /// Why it is `Modified`, in the order a reader wants them.
    pub reasons: Vec<String>,
}

impl WindowDiff {
    pub fn target(&self) -> String {
        format!("{}:{}", self.session, self.index)
    }
}

/// The whole comparison.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diff {
    pub windows: Vec<WindowDiff>,
}

impl Diff {
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut modified = 0;
        let mut added = 0;
        let mut removed = 0;
        for w in &self.windows {
            match w.change {
                Change::Modified => modified += 1,
                Change::Added => added += 1,
                Change::Removed => removed += 1,
                Change::Same => {}
            }
        }
        (modified, added, removed)
    }

    /// Whether anything at all differs — the signal `autosave --if-drifted`
    /// acts on.
    pub fn has_drift(&self) -> bool {
        self.windows.iter().any(|w| w.change != Change::Same)
    }
}

/// Compare live panes against a set of saved sessions.
///
/// `tree` supplies the live command lines. Without it the live side would
/// carry no commands at all and every pane holding one — 45 of 63 on the
/// reference machine — would read as changed, which is worse than no diff.
pub fn compare(live: &[tmux::Pane], saved: &[Session], tree: &proc::Tree) -> Diff {
    let live_sessions = group_live(live, tree);
    let mut windows = Vec::new();

    // Sessions present in either side. A session missing from one is not an
    // error: the point may predate it, or hold one since closed.
    let mut names: Vec<&str> = live_sessions.iter().map(|(n, _)| *n).collect();
    for s in saved {
        if !names.contains(&s.session.as_str()) {
            names.push(&s.session);
        }
    }
    names.sort_unstable();

    for name in names {
        let live_windows = live_sessions
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, w)| w.as_slice())
            .unwrap_or(&[]);
        let saved_windows = saved
            .iter()
            .find(|s| s.session == name)
            .map(|s| s.windows.as_slice())
            .unwrap_or(&[]);
        windows.extend(compare_session(name, live_windows, saved_windows));
    }

    Diff { windows }
}

fn compare_session(session: &str, live: &[Window], saved: &[Window]) -> Vec<WindowDiff> {
    let mut out = Vec::new();

    for l in live {
        match find_match(l, saved) {
            Some(s) => {
                let reasons = window_reasons(l, s);
                out.push(WindowDiff {
                    session: session.to_string(),
                    index: l.index,
                    name: l.name.clone(),
                    change: if reasons.is_empty() {
                        Change::Same
                    } else {
                        Change::Modified
                    },
                    reasons,
                });
            }
            None => out.push(WindowDiff {
                session: session.to_string(),
                index: l.index,
                name: l.name.clone(),
                change: Change::Added,
                reasons: vec!["not in the restore point".into()],
            }),
        }
    }

    for s in saved {
        if find_match(s, live).is_none() {
            out.push(WindowDiff {
                session: session.to_string(),
                index: s.index,
                name: s.name.clone(),
                change: Change::Removed,
                reasons: vec!["only in the restore point".into()],
            });
        }
    }

    out.sort_by_key(|w| w.index);
    out
}

/// Pair a window with its counterpart.
///
/// Matching is by name first, index second. A tmux window keeps its name
/// across `move-window` and renumbering but not its index, and a workspace
/// where windows shift by one would otherwise read as entirely rebuilt.
fn find_match<'a>(needle: &Window, haystack: &'a [Window]) -> Option<&'a Window> {
    haystack
        .iter()
        .find(|w| w.name == needle.name)
        .or_else(|| haystack.iter().find(|w| w.index == needle.index))
}

/// Why two versions of a window differ, most significant first.
fn window_reasons(live: &Window, saved: &Window) -> Vec<String> {
    let mut reasons = Vec::new();

    if live.index != saved.index {
        reasons.push(format!(
            "moved from index {} to {}",
            saved.index, live.index
        ));
    }
    if live.panes.len() != saved.panes.len() {
        reasons.push(format!(
            "{} panes, was {}",
            live.panes.len(),
            saved.panes.len()
        ));
    }

    for (pos, l) in live.panes.iter().enumerate() {
        let Some(s) = match_pane(l, saved, pos) else {
            continue;
        };
        // Both sides through the same normalizer. Files written by tmux.sh
        // keep the `--resume <id>` / `--continue` that tmc strips at save
        // time, and comparing the raw strings flagged every claude pane as
        // changed — 9 of 27 windows on the reference machine, none of which
        // had actually moved.
        let (live_cmd, saved_cmd) = (
            command::normalize(&l.command),
            command::normalize(&s.command),
        );
        if live_cmd != saved_cmd {
            reasons.push(format!(
                "pane {} command: {} -> {}",
                l.index,
                show(&saved_cmd),
                show(&live_cmd),
            ));
        }
        if l.path != s.path {
            reasons.push(format!("pane {} cwd: {} -> {}", l.index, s.path, l.path));
        }
    }

    reasons
}

/// Pair a live pane with its saved counterpart.
///
/// `pane_id` is exact but only within one tmux server lifetime — it resets on
/// restart, and files written by tmux.sh never had it. Falling back to the
/// pane index, then to position, keeps a diff meaningful across a reboot,
/// which is precisely when a restore point matters most.
fn match_pane<'a>(live: &Pane, saved: &'a Window, position: usize) -> Option<&'a Pane> {
    if let Some(id) = live.pane_id.as_deref()
        && let Some(hit) = saved
            .panes
            .iter()
            .find(|p| p.pane_id.as_deref() == Some(id))
    {
        return Some(hit);
    }
    saved
        .panes
        .iter()
        .find(|p| p.index == live.index)
        .or_else(|| saved.panes.get(position))
}

fn show(command: &str) -> String {
    if command.is_empty() {
        "(shell)".into()
    } else {
        command.to_string()
    }
}

/// Reshape live panes into the saved layout's structure so one comparison
/// serves both sides.
fn group_live<'a>(panes: &'a [tmux::Pane], tree: &proc::Tree) -> Vec<(&'a str, Vec<Window>)> {
    let mut out: Vec<(&str, Vec<Window>)> = Vec::new();

    for p in panes {
        let session = out
            .iter_mut()
            .find(|(name, _)| *name == p.session.as_str())
            .map(|(_, w)| w);
        let windows = match session {
            Some(w) => w,
            None => {
                out.push((p.session.as_str(), Vec::new()));
                &mut out.last_mut().expect("just pushed").1
            }
        };

        let window = match windows.iter_mut().find(|w| w.index == p.window_index) {
            Some(w) => w,
            None => {
                windows.push(Window {
                    index: p.window_index,
                    name: p.window_name.clone(),
                    layout: p.window_layout.clone(),
                    panes: Vec::new(),
                    chrome: None,
                });
                windows.last_mut().expect("just pushed")
            }
        };

        // Read through the same path `save` uses, so the two sides are
        // directly comparable. `pane_current_command` would not be: it names
        // the shell, while the saved side holds a full command line.
        let live_command = command::for_pane(p.pid, tree, |pid| tree.args(pid).map(str::to_string));

        window.panes.push(Pane {
            index: p.pane_index,
            path: p.path.clone(),
            command: live_command.unwrap_or_default(),
            claude_session: None,
            pane_id: Some(p.pane_id.clone()),
            shell_only: None,
            session_confidence: None,
        });
    }

    for (_, windows) in &mut out {
        windows.sort_by_key(|w| w.index);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_pane(index: u32, command: &str, path: &str, id: Option<&str>) -> Pane {
        Pane {
            index,
            path: path.into(),
            command: command.into(),
            claude_session: None,
            pane_id: id.map(str::to_string),
            shell_only: Some(command.is_empty()),
            session_confidence: None,
        }
    }

    fn saved_window(index: u32, name: &str, panes: Vec<Pane>) -> Window {
        Window {
            index,
            name: name.into(),
            layout: "abcd,80x24,0,0".into(),
            panes,
            chrome: None,
        }
    }

    fn saved(windows: Vec<Window>) -> Vec<Session> {
        vec![Session {
            session: "projects".into(),
            saved_at: "2026-08-30T00:00:00Z".into(),
            label: None,
            windows,
        }]
    }

    /// A tree where pid 100 is a bare shell — the live panes in these tests
    /// hold no command unless a test says otherwise.
    fn tree() -> proc::Tree {
        proc::Tree::parse_for_test("1 0 launchd\n100 1 fish\n")
    }

    fn live_pane(win: u32, win_name: &str, idx: u32, path: &str, id: &str) -> tmux::Pane {
        tmux::Pane {
            session: "projects".into(),
            session_attached: true,
            window_index: win,
            window_id: format!("@{win}"),
            window_name: win_name.into(),
            window_layout: "abcd,80x24,0,0".into(),
            window_active: false,
            pane_index: idx,
            pane_id: id.into(),
            pid: 100,
            path: path.into(),
            current_command: "fish".into(),
            active: false,
            cc_session: String::new(),
            cc_state: String::new(),
        }
    }

    #[test]
    fn an_unchanged_workspace_reports_no_drift() {
        let live = vec![live_pane(1, "alpha", 1, "/tmp", "%1")];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "", "/tmp", Some("%1"))],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert!(!diff.has_drift());
        assert_eq!(diff.counts(), (0, 0, 0));
    }

    #[test]
    fn a_window_created_since_the_point_is_added() {
        let live = vec![
            live_pane(1, "alpha", 1, "/tmp", "%1"),
            live_pane(2, "beta", 1, "/tmp", "%2"),
        ];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "", "/tmp", Some("%1"))],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert_eq!(diff.counts(), (0, 1, 0));
        let beta = diff.windows.iter().find(|w| w.name == "beta").unwrap();
        assert_eq!(beta.change, Change::Added);
    }

    #[test]
    fn a_window_closed_since_the_point_is_removed() {
        let live = vec![live_pane(1, "alpha", 1, "/tmp", "%1")];
        let saved = saved(vec![
            saved_window(1, "alpha", vec![saved_pane(1, "", "/tmp", Some("%1"))]),
            saved_window(2, "gone", vec![saved_pane(1, "", "/tmp", Some("%9"))]),
        ]);

        let diff = compare(&live, &saved, &tree());
        assert_eq!(diff.counts(), (0, 0, 1));
        let gone = diff.windows.iter().find(|w| w.name == "gone").unwrap();
        assert_eq!(gone.change, Change::Removed);
        assert_eq!(gone.target(), "projects:2");
    }

    #[test]
    fn a_pane_added_to_a_window_makes_it_modified() {
        let live = vec![
            live_pane(1, "alpha", 1, "/tmp", "%1"),
            live_pane(1, "alpha", 2, "/tmp", "%2"),
        ];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "", "/tmp", Some("%1"))],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert_eq!(diff.counts(), (1, 0, 0));
        assert!(
            diff.windows[0]
                .reasons
                .iter()
                .any(|r| r == "2 panes, was 1")
        );
    }

    #[test]
    fn a_changed_cwd_is_reported_with_both_sides() {
        let live = vec![live_pane(1, "alpha", 1, "/new/path", "%1")];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "", "/old/path", Some("%1"))],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert_eq!(diff.windows[0].change, Change::Modified);
        assert!(
            diff.windows[0]
                .reasons
                .iter()
                .any(|r| r == "pane 1 cwd: /old/path -> /new/path"),
            "reasons: {:?}",
            diff.windows[0].reasons,
        );
    }

    #[test]
    fn a_renumbered_window_is_matched_by_name_not_rebuilt() {
        // move-window renumbers but keeps the name. Matching on index alone
        // would report the whole workspace as added-and-removed.
        let live = vec![live_pane(5, "alpha", 1, "/tmp", "%1")];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "", "/tmp", Some("%1"))],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert_eq!(diff.counts(), (1, 0, 0), "modified, not added+removed");
        assert!(
            diff.windows[0]
                .reasons
                .iter()
                .any(|r| r == "moved from index 1 to 5"),
        );
    }

    #[test]
    fn panes_still_pair_up_when_ids_are_absent() {
        // Files written by tmux.sh have no pane_id, and ids reset on a tmux
        // restart — exactly when a restore point matters most.
        let live = vec![live_pane(1, "alpha", 1, "/new", "%77")];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "", "/old", None)],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert!(
            diff.windows[0]
                .reasons
                .iter()
                .any(|r| r.contains("cwd: /old -> /new")),
            "index should pair the panes when ids cannot",
        );
    }

    #[test]
    fn a_session_only_in_the_point_still_appears() {
        let live: Vec<tmux::Pane> = Vec::new();
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "", "/tmp", None)],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert_eq!(diff.counts(), (0, 0, 1));
    }

    #[test]
    fn a_command_that_exited_reads_as_a_real_change() {
        // The saved pane held a claude; the live pane is back to a shell. That
        // is a genuine difference and the reason to reach for the point.
        let live = vec![live_pane(1, "alpha", 1, "/tmp", "%1")];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(1, "ccproxy claude", "/tmp", Some("%1"))],
        )]);

        let diff = compare(&live, &saved, &tree());
        assert_eq!(diff.counts(), (1, 0, 0));
        assert!(
            diff.windows[0]
                .reasons
                .iter()
                .any(|r| r == "pane 1 command: ccproxy claude -> (shell)"),
            "reasons: {:?}",
            diff.windows[0].reasons,
        );
    }

    #[test]
    fn an_unchanged_command_is_read_through_the_same_path_as_save() {
        // Both sides walk the process tree, so a pane still running what it
        // ran at save time compares equal. Reading `pane_current_command`
        // instead would report the shell and flag every such pane.
        let live = vec![live_pane(1, "alpha", 1, "/tmp", "%1")];
        let saved = saved(vec![saved_window(
            1,
            "alpha",
            vec![saved_pane(
                1,
                "ccproxy claude --model default",
                "/tmp",
                Some("%1"),
            )],
        )]);
        let tree = proc::Tree::parse_for_test_with_args(
            "1 0 launchd\n100 1 fish\n200 100 ccproxy\n",
            &[(200, "ccproxy claude --model default")],
        );

        let diff = compare(&live, &saved, &tree);
        assert!(!diff.has_drift(), "reasons: {:?}", diff.windows[0].reasons);
    }

    #[test]
    fn markers_are_punctuation_so_they_survive_no_color() {
        assert_eq!(Change::Modified.marker(), '~');
        assert_eq!(Change::Added.marker(), '+');
        assert_eq!(Change::Removed.marker(), '-');
        assert_eq!(Change::Same.marker(), ' ');
    }
}
