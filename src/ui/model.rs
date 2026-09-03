//! What the TUI shows, independent of how it is drawn.
//!
//! Keeping the tree, cursor and marks here — with no ratatui types — is what
//! lets the interesting behaviour be tested: which row `n` jumps to, what a
//! mark selects, whether a refresh actually changed anything.

use std::collections::HashSet;

use crate::collect::{notify, proc, tmux};
use crate::layout::diff::{Change, Diff};
use crate::layout::{Session, point};

/// One line in the tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A session header. Not selectable for restore.
    Session {
        name: String,
        windows: usize,
    },
    Window(WindowRow),
    /// A pane inside an expanded window. Only these carry a `pane_id`, which
    /// is what `break-pane` and `join-pane` actually take — addressing them by
    /// window let tmux pick its active pane instead of the one on screen.
    Pane(PaneRow),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneRow {
    pub session: String,
    pub window_index: u32,
    pub index: u32,
    /// `%N`, the unambiguous target for pane commands.
    pub pane_id: String,
    /// What the pane is running, from the process tree — not
    /// `pane_current_command`, which names the shell.
    pub command: String,
    pub path: String,
    pub active: bool,
}

impl PaneRow {
    /// `%N`. Preferred over `session:window.index` because it survives the
    /// renumbering that closing a neighbouring pane causes.
    pub fn target(&self) -> &str {
        &self.pane_id
    }

    pub fn window_target(&self) -> String {
        format!("{}:{}", self.session, self.window_index)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowRow {
    pub session: String,
    pub index: u32,
    pub name: String,
    pub panes: usize,
    /// `working` / `waiting` / `done`, from the Claude hooks.
    pub state: String,
    /// Claude session id, when the window has one.
    pub cc_session: String,
    /// Whether a human is blocked on this window right now.
    pub waiting: bool,
    pub change: Change,
    pub reasons: Vec<String>,
    /// True when the window exists only in the restore point.
    pub gone: bool,
}

impl WindowRow {
    pub fn target(&self) -> String {
        format!("{}:{}", self.session, self.index)
    }

    /// The state glyph. Letters and punctuation rather than colour alone, so
    /// the tree reads under `NO_COLOR` and in monochrome — twm's rule, kept.
    pub fn state_glyph(&self) -> char {
        match self.state.as_str() {
            "waiting" => '?',
            "working" => '*',
            "done" => '+',
            _ => ' ',
        }
    }
}

/// A pane waiting for the user to choose its destination window.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneMove {
    /// `%N`, stable even if another pane disappears while choosing.
    pub pane: String,
    /// The window it currently belongs to; selecting this is never useful.
    pub from: String,
    /// The exact live window chosen, kept separately from the row number so a
    /// polling refresh cannot silently turn the same cursor position into a
    /// different destination.
    pub destination: Option<String>,
}

/// The TUI's whole state.
pub struct Model {
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// Targets marked for restore.
    pub marks: HashSet<String>,
    /// The restore point being compared against, and its position in the list.
    pub points: Vec<point::Point>,
    pub point_index: usize,
    pub counts: (usize, usize, usize),
    pub waiting: usize,
    /// Set when the user picks a window to switch to; the caller performs the
    /// switch after the terminal is restored.
    pub switch_to: Option<String>,
    pub quit: bool,
    pub status: String,
    /// Set after `J` on a pane, until a destination window is confirmed.
    pub pane_move: Option<PaneMove>,
    /// Typed search. Empty means the whole tree is shown.
    pub search: String,
    /// True while the search line is accepting keys.
    pub searching: bool,
    /// Set when the running binary predates the checked-out source.
    pub stale_build: Option<String>,
    /// Windows whose panes are listed, by target.
    pub expanded: std::collections::HashSet<String>,
    /// What the selected window is showing, and which window that was.
    ///
    /// Cached by target so moving the cursor back and forth over the same rows
    /// does not re-shell for a capture that has not changed.
    pub preview: Option<(String, String)>,
}

impl Model {
    pub fn new(points: Vec<point::Point>) -> Self {
        Self {
            rows: Vec::new(),
            cursor: 0,
            marks: HashSet::new(),
            points,
            point_index: 0,
            counts: (0, 0, 0),
            waiting: 0,
            switch_to: None,
            quit: false,
            status: String::new(),
            pane_move: None,
            search: String::new(),
            searching: false,
            stale_build: check_build(),
            expanded: std::collections::HashSet::new(),
            preview: None,
        }
    }

    /// The capture for the selected window, if it is the one we hold.
    pub fn preview_for(&self, target: &str) -> Option<&str> {
        self.preview
            .as_ref()
            .filter(|(t, _)| t == target)
            .map(|(_, body)| body.as_str())
    }

    /// Row indices to display, in display order.
    ///
    /// With no search this is every row. With one, the matching windows ranked
    /// best-first — session headers drop out, since a header for a session
    /// whose windows all filtered away is noise, and the ranking is across the
    /// whole server anyway.
    pub fn visible(&self) -> Vec<usize> {
        if self.search.is_empty() {
            return (0..self.rows.len()).collect();
        }
        // Windows match on their name, panes on what they are running — the
        // pane is often the more precise thing to search for, since window
        // names repeat but `ccproxy claude --resume …` does not.
        let targets: Vec<(usize, String)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                Row::Window(w) => Some((i, w.name.clone())),
                Row::Pane(p) => Some((i, p.command.clone())),
                Row::Session { .. } => None,
            })
            .collect();
        crate::fuzzy::rank(&self.search, &targets, |(_, text)| text.as_str())
            .into_iter()
            .map(|pos| targets[pos].0)
            .collect()
    }

    /// The row the cursor points at, accounting for the search.
    fn cursor_row(&self) -> Option<usize> {
        self.visible().get(self.cursor).copied()
    }

    pub fn current_point(&self) -> Option<&point::Point> {
        self.points.get(self.point_index)
    }

    /// Rebuild the tree from a fresh reading of the world.
    pub fn refresh(
        &mut self,
        panes: &[tmux::Pane],
        saved: &[Session],
        tree: &proc::Tree,
        pending: &std::collections::HashMap<String, notify::Entry>,
    ) {
        let diff = crate::layout::diff::compare(panes, saved, tree);
        self.counts = diff.counts();
        self.rows = build_rows(panes, &diff, pending, tree, &self.expanded);
        self.waiting = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Window(w) if w.waiting))
            .count();

        // A polling refresh may insert, remove or reorder rows while a pane
        // destination is being chosen. Follow the remembered target rather
        // than leaving the same numeric cursor on a different window.
        if let Some(target) = self
            .pane_move
            .as_ref()
            .and_then(|moving| moving.destination.clone())
        {
            if let Some(row) = self
                .rows
                .iter()
                .position(|r| matches!(r, Row::Window(w) if !w.gone && w.target() == target))
            {
                self.cursor = row;
            } else {
                // Never substitute another window after the user picked one.
                // A refresh can remove or renumber it; silently keeping the row
                // number would move the pane somewhere they did not choose.
                self.pane_move = None;
                self.status = format!("pane move cancelled — {target} is no longer available");
                self.clamp_cursor();
            }
        } else {
            self.clamp_cursor();
        }
    }

    /// A cheap value that changes exactly when the display would.
    ///
    /// The tree is polled — tmux exposes no event stream a foreign process can
    /// subscribe to — so a redraw every tick would burn CPU on an idle
    /// workspace. Comparing this instead keeps the app at 0 fps when nothing
    /// moves. Taken from twm, where it is what makes a 2s poll acceptable.
    pub fn fingerprint(&self) -> String {
        let mut out = String::new();
        for row in &self.rows {
            match row {
                Row::Session { name, windows } => {
                    out.push_str(&format!("S:{name}:{windows}\n"));
                }
                Row::Pane(p) => {
                    out.push_str(&format!("P:{}:{}:{}\n", p.target(), p.command, p.active,))
                }
                Row::Window(w) => out.push_str(&format!(
                    "W:{}:{}:{}:{}:{}:{}\n",
                    w.target(),
                    w.name,
                    w.state,
                    w.panes,
                    w.change.marker(),
                    w.waiting,
                )),
            }
        }
        out
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let next = self.cursor as isize + delta;
        self.cursor = next.clamp(0, visible.len() as isize - 1) as usize;
        self.skip_header(delta.signum());
    }

    /// Type into the search, keeping the cursor on something real.
    pub fn search_push(&mut self, c: char) {
        self.search.push(c);
        self.cursor = 0;
        self.skip_header(1);
    }

    pub fn search_pop(&mut self) {
        self.search.pop();
        self.cursor = 0;
        self.skip_header(1);
    }

    /// Session headers are labels, not destinations.
    fn skip_header(&mut self, direction: isize) {
        let visible = self.visible();
        while matches!(
            self.cursor_row().and_then(|i| self.rows.get(i)),
            Some(Row::Session { .. })
        ) {
            let next = self.cursor as isize + if direction >= 0 { 1 } else { -1 };
            if next < 0 || next >= visible.len() as isize {
                // At an edge with a header under the cursor: turn around
                // rather than sit on an unselectable row.
                let back = self.cursor as isize - if direction >= 0 { 1 } else { -1 };
                if back >= 0 && back < visible.len() as isize {
                    self.cursor = back as usize;
                }
                return;
            }
            self.cursor = next as usize;
        }
    }

    fn clamp_cursor(&mut self) {
        let visible = self.visible().len();
        if visible == 0 {
            self.cursor = 0;
            return;
        }
        if self.cursor >= visible {
            self.cursor = visible - 1;
        }
        self.skip_header(1);
    }

    /// Put the cursor on `target`, e.g. `projects:1`. Returns whether it was
    /// found.
    ///
    /// The TUI opens on the window it was summoned from, so the first thing on
    /// screen — the preview, the diff reasons — describes where the user
    /// already is. Landing on row 0 instead showed some unrelated session's
    /// first window and made the panel say nothing about the current context.
    pub fn focus(&mut self, target: &str) -> bool {
        let visible = self.visible();
        let Some(pos) = visible.iter().position(
            |&i| matches!(&self.rows[i], Row::Window(w) if !w.gone && w.target() == target),
        ) else {
            return false;
        };
        self.cursor = pos;
        true
    }

    /// Jump to the next window blocked on a human.
    ///
    /// twm's `w`, and the whole point of showing state here: with 27 windows
    /// open, "which one is waiting on me" is the question that costs the most
    /// time.
    pub fn jump_waiting(&mut self) {
        let visible = self.visible();
        let n = visible.len();
        if n == 0 {
            return;
        }
        for step in 1..=n {
            let pos = (self.cursor + step) % n;
            if matches!(&self.rows[visible[pos]], Row::Window(w) if w.waiting) {
                self.cursor = pos;
                return;
            }
        }
        self.status = "no window is waiting".into();
    }

    /// Show or hide the selected window's panes.
    ///
    /// `want` of `None` toggles. With the cursor on a pane, collapsing acts on
    /// its parent window — otherwise `h` on a pane row would do nothing, which
    /// reads as a broken key.
    pub fn set_expanded(&mut self, want: Option<bool>) -> bool {
        let target = match self.cursor_row().and_then(|i| self.rows.get(i)) {
            Some(Row::Window(w)) if !w.gone => w.target(),
            Some(Row::Pane(p)) => p.window_target(),
            _ => return false,
        };
        let is_open = self.expanded.contains(&target);
        let open = want.unwrap_or(!is_open);
        if open == is_open {
            return false;
        }
        if open {
            self.expanded.insert(target);
        } else {
            self.expanded.remove(&target);
        }
        true
    }

    /// The pane under the cursor, when one is selected.
    pub fn current_pane(&self) -> Option<&PaneRow> {
        match self.cursor_row().and_then(|i| self.rows.get(i)) {
            Some(Row::Pane(p)) => Some(p),
            _ => None,
        }
    }

    pub fn current_window(&self) -> Option<&WindowRow> {
        match self.cursor_row().and_then(|i| self.rows.get(i)) {
            Some(Row::Window(w)) => Some(w),
            // A pane belongs to a window; commands that act on windows should
            // still work with the cursor on one of its panes.
            Some(Row::Pane(p)) => self.rows.iter().find_map(|r| match r {
                Row::Window(w) if w.target() == p.window_target() => Some(w),
                _ => None,
            }),
            _ => None,
        }
    }

    /// The exact window row under the cursor, excluding a pane's parent.
    ///
    /// A pane move needs an explicit destination. Treating a pane row as its
    /// parent here would let `J` confirm a window that is not visibly selected.
    pub fn destination_window(&self) -> Option<&WindowRow> {
        match self.cursor_row().and_then(|i| self.rows.get(i)) {
            Some(Row::Window(w)) if !w.gone => Some(w),
            _ => None,
        }
    }

    /// Remember a pane and put the cursor on the first usable destination.
    pub fn begin_pane_move(&mut self, pane: String, from: String) -> bool {
        self.search.clear();
        self.searching = false;
        self.pane_move = Some(PaneMove {
            pane,
            from,
            destination: None,
        });
        self.cursor = 0;
        if self.move_destination(1) {
            true
        } else {
            self.pane_move = None;
            false
        }
    }

    /// Move among live window rows while choosing a pane destination.
    pub fn move_destination(&mut self, delta: isize) -> bool {
        let Some(from) = self.pane_move.as_ref().map(|moving| moving.from.clone()) else {
            return false;
        };
        let candidates: Vec<(usize, String)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match row {
                Row::Window(w) if !w.gone && w.target() != from => Some((i, w.target())),
                _ => None,
            })
            .collect();
        if candidates.is_empty() {
            return false;
        }

        // Search is cleared when this mode starts, so visible positions and row
        // indices are identical. From a non-candidate row, choose the first one;
        // thereafter j/k wrap only across candidate windows.
        let next = match candidates.iter().position(|(i, _)| *i == self.cursor) {
            Some(pos) => (pos as isize + delta).rem_euclid(candidates.len() as isize) as usize,
            None => 0,
        };
        self.cursor = candidates[next].0;
        if let Some(moving) = &mut self.pane_move {
            moving.destination = Some(candidates[next].1.clone());
        }
        true
    }

    /// Mark or unmark the window under the cursor.
    pub fn toggle_mark(&mut self) {
        if let Some(w) = self.current_window() {
            let target = w.target();
            if !self.marks.remove(&target) {
                self.marks.insert(target);
            }
        }
    }

    /// Mark every window that differs from the point — the usual intent when
    /// restoring, and tedious to do row by row.
    pub fn mark_all_changed(&mut self) {
        for row in &self.rows {
            if let Row::Window(w) = row
                && w.change != Change::Same
            {
                self.marks.insert(w.target());
            }
        }
    }

    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }

    /// Move to the next or previous restore point.
    pub fn cycle_point(&mut self, delta: isize) -> bool {
        if self.points.is_empty() {
            return false;
        }
        let next =
            (self.point_index as isize + delta).clamp(0, self.points.len() as isize - 1) as usize;
        if next == self.point_index {
            return false;
        }
        self.point_index = next;
        true
    }
}

/// Build the tree: a session header followed by its windows, plus any windows
/// that exist only in the restore point.
fn build_rows(
    panes: &[tmux::Pane],
    diff: &Diff,
    pending: &std::collections::HashMap<String, notify::Entry>,
    tree: &proc::Tree,
    expanded: &std::collections::HashSet<String>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut sessions: Vec<&str> = Vec::new();
    for w in &diff.windows {
        if !sessions.contains(&w.session.as_str()) {
            sessions.push(&w.session);
        }
    }

    for session in sessions {
        let windows: Vec<&crate::layout::diff::WindowDiff> = diff
            .windows
            .iter()
            .filter(|w| w.session == session)
            .collect();
        rows.push(Row::Session {
            name: session.to_string(),
            windows: windows.len(),
        });

        for w in windows {
            // A removed window has no live panes to read state from.
            let live: Vec<&tmux::Pane> = panes
                .iter()
                .filter(|p| p.session == w.session && p.window_index == w.index)
                .collect();
            let state = live.first().map(|p| p.cc_state.clone()).unwrap_or_default();
            let cc_session = live
                .first()
                .map(|p| p.cc_session.clone())
                .unwrap_or_default();

            // Waiting is the join of two sources: the hook publishes the state
            // on the window, and notify.py records *why* it is blocked. Both
            // must agree, so a stale state does not show a window as waiting
            // when its queue entry is gone.
            let waiting =
                state == "waiting" && pending.get(&cc_session).is_some_and(|e| e.kind.blocking());

            let target = format!("{}:{}", w.session, w.index);
            rows.push(Row::Window(WindowRow {
                session: w.session.clone(),
                index: w.index,
                name: w.name.clone(),
                panes: live.len(),
                state,
                cc_session,
                waiting,
                change: w.change,
                reasons: w.reasons.clone(),
                gone: w.change == Change::Removed,
            }));

            if expanded.contains(&target) {
                for p in &live {
                    rows.push(Row::Pane(PaneRow {
                        session: p.session.clone(),
                        window_index: p.window_index,
                        index: p.pane_index,
                        pane_id: p.pane_id.clone(),
                        // Through the process tree, like `save` — the shell
                        // name a pane reports says nothing about what is
                        // running in it.
                        command: crate::collect::command::for_pane(p.pid, tree, |pid| {
                            tree.args(pid).map(str::to_string)
                        })
                        .unwrap_or_default(),
                        path: p.path.clone(),
                        active: p.active,
                    }));
                }
            }
        }
    }
    rows
}

/// Whether this binary is older than the source it was built from.
///
/// Only meaningful when run from a checkout — a released binary has no
/// repository to compare against and reports nothing. Cheap: one `git` call
/// at startup, never on the polling path.
fn check_build() -> Option<String> {
    let built = env!("TMC_COMMIT");
    if built == "unknown" {
        return None;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if !manifest.join(".git").exists() {
        return None;
    }
    let head = crate::collect::cmd::run(
        "git",
        &["-C", manifest.to_str()?, "describe", "--always", "--dirty"],
        crate::collect::cmd::FAST,
    )
    .ok()?;
    staleness(built, head.trim())
}

/// Describe the gap between the binary's commit and the source's, if any.
///
/// Compares commits, not strings: `-dirty` means uncommitted edits, which is
/// the normal state while working and says nothing about whether the binary is
/// current. Only a different commit is real staleness.
fn staleness(built: &str, source: &str) -> Option<String> {
    fn commit_of(s: &str) -> &str {
        s.trim_end_matches("-dirty")
    }
    let (built_at, source_at) = (commit_of(built), commit_of(source));
    (built_at != source_at).then(|| format!("{built_at}, source is {source_at}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::WindowDiff;
    use std::collections::HashMap;

    fn window_diff(session: &str, index: u32, name: &str, change: Change) -> WindowDiff {
        WindowDiff {
            session: session.into(),
            index,
            name: name.into(),
            change,
            reasons: Vec::new(),
        }
    }

    fn live_pane(session: &str, win: u32, name: &str, state: &str, cc: &str) -> tmux::Pane {
        tmux::Pane {
            session: session.into(),
            session_attached: true,
            window_index: win,
            window_id: format!("@{win}"),
            window_name: name.into(),
            window_layout: "abcd,80x24,0,0".into(),
            window_active: false,
            pane_index: 1,
            pane_id: format!("%{win}"),
            pid: 100,
            path: "/tmp".into(),
            current_command: "fish".into(),
            active: false,
            cc_session: cc.into(),
            cc_state: state.into(),
        }
    }

    fn pending_with(session: &str, kind: notify::Kind) -> HashMap<String, notify::Entry> {
        let mut map = HashMap::new();
        map.insert(
            session.to_string(),
            notify::Entry {
                at: "2026-08-30T00:00:00".into(),
                session_id: session.into(),
                cwd: "/tmp".into(),
                kind,
                message: String::new(),
            },
        );
        map
    }

    fn empty_tree() -> proc::Tree {
        proc::Tree::parse_for_test("1 0 launchd\n")
    }

    fn model_with(rows: Vec<Row>) -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = rows;
        m
    }

    #[test]
    fn a_session_header_precedes_its_windows() {
        let panes = vec![live_pane("projects", 1, "alpha", "", "")];
        let diff = Diff {
            windows: vec![window_diff("projects", 1, "alpha", Change::Same)],
        };
        let rows = build_rows(
            &panes,
            &diff,
            &HashMap::new(),
            &empty_tree(),
            &Default::default(),
        );

        assert!(matches!(&rows[0], Row::Session { name, windows: 1 } if name == "projects"));
        assert!(matches!(&rows[1], Row::Window(w) if w.name == "alpha"));
    }

    #[test]
    fn waiting_needs_both_the_state_and_a_queue_entry() {
        // A stale @cc_state alone must not show a window as blocked; the queue
        // is what records that a human is actually holding it up.
        let panes = vec![live_pane("projects", 1, "alpha", "waiting", "sess-a")];
        let diff = Diff {
            windows: vec![window_diff("projects", 1, "alpha", Change::Same)],
        };

        let without = build_rows(
            &panes,
            &diff,
            &HashMap::new(),
            &empty_tree(),
            &Default::default(),
        );
        assert!(matches!(&without[1], Row::Window(w) if !w.waiting));

        let with = build_rows(
            &panes,
            &diff,
            &pending_with("sess-a", notify::Kind::Idle),
            &empty_tree(),
            &Default::default(),
        );
        assert!(matches!(&with[1], Row::Window(w) if w.waiting));
    }

    #[test]
    fn a_finished_agent_does_not_count_as_waiting() {
        let panes = vec![live_pane("projects", 1, "alpha", "waiting", "sess-a")];
        let diff = Diff {
            windows: vec![window_diff("projects", 1, "alpha", Change::Same)],
        };
        let rows = build_rows(
            &panes,
            &diff,
            &pending_with("sess-a", notify::Kind::AgentDone),
            &empty_tree(),
            &Default::default(),
        );
        assert!(matches!(&rows[1], Row::Window(w) if !w.waiting));
    }

    #[test]
    fn the_cursor_never_rests_on_a_session_header() {
        let mut m = model_with(vec![
            Row::Session {
                name: "projects".into(),
                windows: 1,
            },
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: "alpha".into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            }),
        ]);
        m.cursor = 0;
        m.move_cursor(0);
        assert_eq!(m.cursor, 1, "a header is a label, not a destination");
    }

    #[test]
    fn focus_lands_on_the_window_it_names() {
        let win = |session: &str, index: u32, name: &str, gone: bool| {
            Row::Window(WindowRow {
                session: session.into(),
                index,
                name: name.into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone,
            })
        };
        let mut m = model_with(vec![
            Row::Session {
                name: "projects".into(),
                windows: 2,
            },
            win("projects", 1, "alpha", false),
            win("projects", 2, "beta", false),
        ]);

        assert!(m.focus("projects:2"));
        assert_eq!(m.cursor, 2);
    }

    #[test]
    fn focus_reports_a_miss_and_leaves_the_cursor_alone() {
        // The summoning window can be absent from the tree — the popup was
        // opened from a session the search has filtered away. Moving the
        // cursor somewhere arbitrary would be worse than not moving it.
        let mut m = model_with(vec![Row::Window(WindowRow {
            session: "projects".into(),
            index: 1,
            name: "alpha".into(),
            panes: 1,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        })]);
        m.cursor = 0;

        assert!(!m.focus("other:9"));
        assert_eq!(m.cursor, 0);
    }

    #[test]
    fn focus_ignores_a_window_that_only_exists_in_the_point() {
        // A `gone` row cannot be switched to, so focusing it would put the
        // panel on a window the user cannot reach.
        let mut m = model_with(vec![
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: "alpha".into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
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
                change: Change::Removed,
                reasons: Vec::new(),
                gone: true,
            }),
        ]);
        m.cursor = 0;

        assert!(!m.focus("projects:2"));
        assert_eq!(m.cursor, 0);
    }

    #[test]
    fn jump_waiting_wraps_around_the_list() {
        let mk = |name: &str, waiting: bool| {
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: name.into(),
                panes: 1,
                state: if waiting {
                    "waiting".into()
                } else {
                    String::new()
                },
                cc_session: String::new(),
                waiting,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            })
        };
        let mut m = model_with(vec![mk("a", true), mk("b", false), mk("c", false)]);
        m.cursor = 1;
        m.jump_waiting();
        assert_eq!(
            m.cursor, 0,
            "should wrap past the end to find the waiting one"
        );
    }

    #[test]
    fn jump_waiting_says_so_when_nothing_is_blocked() {
        let mut m = model_with(vec![Row::Window(WindowRow {
            session: "projects".into(),
            index: 1,
            name: "alpha".into(),
            panes: 1,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        })]);
        m.jump_waiting();
        assert_eq!(m.cursor, 0);
        assert!(m.status.contains("no window is waiting"));
    }

    #[test]
    fn marking_toggles_and_mark_all_takes_only_the_changed() {
        let mk = |index: u32, change: Change| {
            Row::Window(WindowRow {
                session: "projects".into(),
                index,
                name: format!("w{index}"),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change,
                reasons: Vec::new(),
                gone: false,
            })
        };
        let mut m = model_with(vec![
            mk(1, Change::Same),
            mk(2, Change::Modified),
            mk(3, Change::Added),
        ]);

        m.cursor = 0;
        m.toggle_mark();
        assert!(m.marks.contains("projects:1"));
        m.toggle_mark();
        assert!(m.marks.is_empty(), "toggling twice clears it");

        m.mark_all_changed();
        assert_eq!(m.marks.len(), 2, "the unchanged window is left alone");
        assert!(m.marks.contains("projects:2") && m.marks.contains("projects:3"));
    }

    #[test]
    fn the_fingerprint_changes_only_when_the_display_would() {
        let row = |state: &str| {
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: "alpha".into(),
                panes: 1,
                state: state.into(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            })
        };
        let a = model_with(vec![row("working")]);
        let same = model_with(vec![row("working")]);
        let changed = model_with(vec![row("waiting")]);

        assert_eq!(a.fingerprint(), same.fingerprint());
        assert_ne!(a.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn expanding_a_window_lists_its_panes_with_their_ids() {
        // `break-pane` and `join-pane` take a pane, not a window. Addressing
        // them by window made tmux use its active pane, so the thing that
        // moved was not the thing on screen.
        let panes = vec![
            live_pane("projects", 1, "alpha", "", ""),
            live_pane("projects", 1, "alpha", "", ""),
        ];
        let diff = Diff {
            windows: vec![window_diff("projects", 1, "alpha", Change::Same)],
        };
        let mut expanded = std::collections::HashSet::new();
        expanded.insert("projects:1".to_string());

        let rows = build_rows(&panes, &diff, &HashMap::new(), &empty_tree(), &expanded);
        let pane_rows: Vec<&PaneRow> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Pane(p) => Some(p),
                _ => None,
            })
            .collect();

        assert_eq!(pane_rows.len(), 2);
        assert!(
            pane_rows[0].target().starts_with('%'),
            "addressed by pane id"
        );
        assert_eq!(pane_rows[0].window_target(), "projects:1");
    }

    #[test]
    fn a_collapsed_window_lists_no_panes() {
        let panes = vec![live_pane("projects", 1, "alpha", "", "")];
        let diff = Diff {
            windows: vec![window_diff("projects", 1, "alpha", Change::Same)],
        };
        let rows = build_rows(
            &panes,
            &diff,
            &HashMap::new(),
            &empty_tree(),
            &Default::default(),
        );
        assert!(!rows.iter().any(|r| matches!(r, Row::Pane(_))));
    }

    #[test]
    fn collapsing_from_a_pane_row_acts_on_its_window() {
        // Otherwise `h` with the cursor on a pane does nothing, which reads as
        // a broken key.
        let mut m = model_with(vec![
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: "alpha".into(),
                panes: 2,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            }),
            Row::Pane(PaneRow {
                session: "projects".into(),
                window_index: 1,
                index: 1,
                pane_id: "%1".into(),
                command: "ghx".into(),
                path: "/tmp".into(),
                active: true,
            }),
        ]);
        m.expanded.insert("projects:1".into());
        m.cursor = 1; // on the pane

        assert!(m.set_expanded(Some(false)));
        assert!(!m.expanded.contains("projects:1"));
    }

    #[test]
    fn a_window_command_still_works_with_the_cursor_on_a_pane() {
        // Marking, restoring and killing act on windows; the cursor sitting on
        // one of its panes should not disable them.
        let mut m = model_with(vec![
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: "alpha".into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            }),
            Row::Pane(PaneRow {
                session: "projects".into(),
                window_index: 1,
                index: 1,
                pane_id: "%1".into(),
                command: "ghx".into(),
                path: "/tmp".into(),
                active: true,
            }),
        ]);
        m.cursor = 1;
        assert_eq!(
            m.current_window().map(|w| w.name.clone()),
            Some("alpha".into()),
        );
        assert_eq!(
            m.current_pane().map(|p| p.pane_id.clone()),
            Some("%1".into())
        );
    }

    #[test]
    fn searching_matches_a_pane_on_what_it_is_running() {
        // Window names repeat; `ccproxy claude --resume …` does not.
        let mut m = model_with(vec![
            Row::Pane(PaneRow {
                session: "projects".into(),
                window_index: 1,
                index: 1,
                pane_id: "%1".into(),
                command: "ccproxy claude".into(),
                path: "/tmp".into(),
                active: true,
            }),
            Row::Pane(PaneRow {
                session: "projects".into(),
                window_index: 1,
                index: 2,
                pane_id: "%2".into(),
                command: "ghx".into(),
                path: "/tmp".into(),
                active: false,
            }),
        ]);
        for c in "ccp".chars() {
            m.search_push(c);
        }
        assert_eq!(
            m.current_pane().map(|p| p.pane_id.clone()),
            Some("%1".into())
        );
    }

    #[test]
    fn searching_ranks_windows_and_drops_the_headers() {
        let mk = |name: &str| {
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: name.into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            })
        };
        let mut m = model_with(vec![
            Row::Session {
                name: "projects".into(),
                windows: 3,
            },
            mk("cohome"),
            mk("right-sizing"),
            mk("binpack"),
        ]);

        // Initials, which is how a 27-window list is actually navigated.
        for c in "bnp".chars() {
            m.search_push(c);
        }
        let visible = m.visible();
        assert_eq!(visible.len(), 1, "only binpack matches bnp");
        assert!(matches!(&m.rows[visible[0]], Row::Window(w) if w.name == "binpack"));
        assert_eq!(
            m.current_window().map(|w| w.name.clone()),
            Some("binpack".into())
        );
    }

    #[test]
    fn the_cursor_follows_the_filtered_list_not_the_full_one() {
        // The cursor indexes visible rows; treating it as an index into `rows`
        // would select whatever happens to sit at that position instead.
        let mk = |name: &str| {
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: name.into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            })
        };
        let mut m = model_with(vec![mk("alpha"), mk("beta"), mk("gamma")]);
        m.cursor = 2;

        for c in "be".chars() {
            m.search_push(c);
        }
        assert_eq!(
            m.current_window().map(|w| w.name.clone()),
            Some("beta".into()),
            "the cursor reset onto the one match",
        );
    }

    #[test]
    fn a_binary_from_an_older_commit_is_reported() {
        // The failure this exists to catch: fuzzy search was written, tested
        // and committed, but ~/.local/bin held the previous build, so pressing
        // `/` did nothing and the code looked wrong.
        assert_eq!(
            staleness("211e537", "d029676"),
            Some("211e537, source is d029676".into()),
        );
    }

    #[test]
    fn uncommitted_edits_alone_are_not_staleness() {
        // The normal state while working; warning here would train the eye to
        // ignore the line.
        assert_eq!(staleness("211e537", "211e537-dirty"), None);
        assert_eq!(staleness("211e537-dirty", "211e537"), None);
        assert_eq!(staleness("211e537", "211e537"), None);
    }

    #[test]
    fn pane_move_visits_only_other_live_windows() {
        let window = |session: &str, index: u32, name: &str, gone: bool| {
            Row::Window(WindowRow {
                session: session.into(),
                index,
                name: name.into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone,
            })
        };
        let mut m = model_with(vec![
            Row::Session {
                name: "projects".into(),
                windows: 3,
            },
            window("projects", 1, "source", false),
            Row::Pane(PaneRow {
                session: "projects".into(),
                window_index: 1,
                index: 1,
                pane_id: "%11".into(),
                command: "claude".into(),
                path: "/src".into(),
                active: true,
            }),
            window("projects", 2, "first", false),
            window("projects", 3, "gone", true),
            Row::Session {
                name: "tooling".into(),
                windows: 1,
            },
            window("tooling", 1, "second", false),
        ]);

        assert!(m.begin_pane_move("%11".into(), "projects:1".into()));
        assert_eq!(
            m.destination_window().map(WindowRow::target),
            Some("projects:2".into()),
            "the source, its pane row and the gone window are skipped",
        );
        assert_eq!(
            m.pane_move
                .as_ref()
                .and_then(|moving| moving.destination.as_deref()),
            Some("projects:2"),
            "the target is remembered independently of the cursor row",
        );

        m.move_destination(1);
        assert_eq!(
            m.destination_window().map(WindowRow::target),
            Some("tooling:1".into()),
            "a window in another session is a valid destination",
        );
        assert_eq!(
            m.pane_move
                .as_ref()
                .and_then(|moving| moving.destination.as_deref()),
            Some("tooling:1"),
        );
        m.move_destination(1);
        assert_eq!(
            m.destination_window().map(WindowRow::target),
            Some("projects:2".into()),
            "destination selection wraps",
        );
    }

    #[test]
    fn pane_move_is_not_started_without_another_live_window() {
        let mut m = model_with(vec![Row::Window(WindowRow {
            session: "projects".into(),
            index: 1,
            name: "only".into(),
            panes: 1,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        })]);

        assert!(!m.begin_pane_move("%11".into(), "projects:1".into()));
        assert!(
            m.pane_move.is_none(),
            "no half-entered selection mode remains"
        );
    }

    #[test]
    fn state_glyphs_are_readable_without_colour() {
        let mk = |state: &str| WindowRow {
            session: "s".into(),
            index: 1,
            name: "w".into(),
            panes: 1,
            state: state.into(),
            cc_session: String::new(),
            waiting: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        };
        assert_eq!(mk("waiting").state_glyph(), '?');
        assert_eq!(mk("working").state_glyph(), '*');
        assert_eq!(mk("done").state_glyph(), '+');
        assert_eq!(mk("").state_glyph(), ' ');
    }
}
