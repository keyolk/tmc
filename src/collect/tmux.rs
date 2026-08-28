//! tmux topology: every pane on the server, with the window and session
//! context a layout snapshot needs.
//!
//! One `list-panes -a` fetches every field. The format language is the stable
//! contract, and a single exec beats a query per window — `tmux.sh` spawned a
//! subshell per window and a handful of processes per pane, which is most of
//! why saving took 10s on this machine.

use anyhow::Result;

use super::{cmd, proc};

/// Field separator for `tmux -F`. Tab is safe: pane paths and window names can
/// contain spaces, but not tabs.
const SEP: char = '\t';

const FORMAT: &str = concat!(
    "#{session_name}\t",
    "#{session_attached}\t",
    "#{window_index}\t",
    "#{window_id}\t",
    "#{window_name}\t",
    "#{window_layout}\t",
    "#{?window_active,1,0}\t",
    "#{pane_index}\t",
    "#{pane_id}\t",
    "#{pane_pid}\t",
    "#{pane_current_path}\t",
    "#{pane_current_command}\t",
    "#{?pane_active,1,0}\t",
    // Published by ~/.claude/hooks/cc_state.py as a *window* option, so every
    // pane in a window reports the same value. tmux has no pane-scoped user
    // options (`set-window-option -p` is rejected, and `set-option -p` is
    // denied by ~/.claude/hooks/tmux_pane_guard.py), and a claude process
    // exposes no session id through its cwd, environment or open files. A
    // window running several claudes is therefore ambiguous by construction —
    // `resolve` in this module narrows it as far as the data allows.
    "#{@cc_session}\t",
    "#{@cc_state}",
);

/// One pane as tmux reports it, before any process-tree work.
#[derive(Clone, Debug, PartialEq)]
pub struct Pane {
    pub session: String,
    pub session_attached: bool,
    pub window_index: u32,
    pub window_id: String,
    pub window_name: String,
    pub window_layout: String,
    pub window_active: bool,
    pub pane_index: u32,
    /// `%N` — stable for the life of the tmux server, so it is the join key
    /// for diffing a snapshot against the live server. It does *not* survive a
    /// server restart; see `layout::diff` for the fallback.
    pub pane_id: String,
    pub pid: u32,
    pub path: String,
    pub current_command: String,
    pub active: bool,
    /// Claude Code session id as published on the *window*. Shared by every
    /// pane in the window, so it identifies the pane only when the window runs
    /// a single claude. See `cc_session_is_exact`.
    pub cc_session: String,
    /// `working` / `waiting` / `done`, empty when unknown.
    pub cc_state: String,
}

impl Pane {
    /// Addresses this pane for `send-keys`, `select-pane` and friends.
    pub fn target(&self) -> String {
        format!("{}:{}.{}", self.session, self.window_index, self.pane_index)
    }
}

/// How much to trust a pane's `cc_session`.
///
/// `tmux.sh` collapsed this distinction: it guessed a per-pane id from file
/// birthtimes and wrote the guess into the snapshot as if it were fact, so a
/// wrong guess resumed the wrong conversation with nothing to signal it. The
/// ambiguity is real and unresolvable here, so it is carried in the type and
/// surfaced in the UI instead of being averaged away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionConfidence {
    /// No claude in this window.
    None,
    /// Exactly one pane in the window runs claude, so the window-scoped id is
    /// that pane's id.
    Exact,
    /// Several panes in the window run claude and share one published id. It
    /// names *a* session in this window, but not necessarily this pane's.
    Ambiguous,
}

/// Classify each pane's `cc_session` by how many panes in its window could
/// have produced it.
///
/// The pane's own foreground command is not enough to decide this: claude is
/// usually a *descendant* of the pane's shell, not the shell itself. On the
/// reference machine all three claude panes of `projects:1` report `fish`.
/// Classifying on the command alone called them unambiguous — the precise
/// mistake this type exists to prevent — so the process tree is consulted.
pub fn session_confidence(panes: &[Pane], tree: &proc::Tree) -> Vec<SessionConfidence> {
    let claude_panes: Vec<bool> = panes.iter().map(|p| runs_claude(p, tree)).collect();
    panes
        .iter()
        .map(|p| {
            if p.cc_session.is_empty() {
                return SessionConfidence::None;
            }
            let n = panes
                .iter()
                .zip(&claude_panes)
                .filter(|(o, is_claude)| o.window_id == p.window_id && **is_claude)
                .count();
            if n <= 1 {
                SessionConfidence::Exact
            } else {
                SessionConfidence::Ambiguous
            }
        })
        .collect()
}

/// Whether a claude session runs in this pane, directly or under its shell.
///
/// `cc_state` cannot stand in for this. It is a *window* option inherited by
/// every pane in the window — on the reference machine 41 of the panes
/// carrying a `cc_state` run nothing but `fish`.
fn runs_claude(p: &Pane, tree: &proc::Tree) -> bool {
    tree.comm(p.pid).is_some_and(is_claude) || tree.find_descendant(p.pid, is_claude).is_some()
}

/// Claude runs bare or behind a wrapper (`ccproxy claude …`, `happy`). The
/// wrapper spawns the real binary as a child, so matching `claude` alone would
/// still find it — the wrappers are listed so a pane counts as claude even
/// while the child is still starting up.
fn is_claude(comm: &str) -> bool {
    matches!(comm, "claude" | "ccproxy" | "happy")
}

/// Every pane across every session on the tmux server.
pub fn panes() -> Result<Vec<Pane>> {
    let raw = cmd::run("tmux", &["list-panes", "-a", "-F", FORMAT], cmd::FAST)?;
    Ok(parse_panes(&raw))
}

fn parse_panes(raw: &str) -> Vec<Pane> {
    raw.lines().filter_map(parse_pane).collect()
}

fn parse_pane(line: &str) -> Option<Pane> {
    // The pane path is user data and the only field that could plausibly hold
    // a tab, so bounding the split keeps a pathological path from shifting
    // every later column.
    let f: Vec<&str> = line.splitn(15, SEP).collect();
    if f.len() < 15 {
        return None; // a malformed row must not kill the whole listing
    }
    Some(Pane {
        session: f[0].to_string(),
        session_attached: f[1] == "1",
        window_index: f[2].parse().ok()?,
        window_id: f[3].to_string(),
        window_name: f[4].to_string(),
        window_layout: f[5].to_string(),
        window_active: f[6] == "1",
        pane_index: f[7].parse().ok()?,
        pane_id: f[8].to_string(),
        pid: f[9].parse().ok()?,
        path: f[10].to_string(),
        current_command: f[11].to_string(),
        active: f[12] == "1",
        cc_session: f[13].to_string(),
        cc_state: f[14].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(fields: &[&str]) -> String {
        fields.join("\t")
    }

    const OK: &[&str] = &[
        "projects",
        "1",
        "1",
        "@12",
        "cohome",
        "b89a,365x97,0,0",
        "1",
        "2",
        "%251",
        "4242",
        "/home/u/exp/cohome",
        "ccproxy",
        "1",
        "12f17a65-a326-4f2f-88a9-47a53e61de7f",
        "working",
    ];

    #[test]
    fn parses_a_full_row() {
        let p = parse_pane(&row(OK)).expect("row should parse");
        assert_eq!(p.session, "projects");
        assert_eq!(p.window_index, 1);
        assert_eq!(p.pane_id, "%251");
        assert_eq!(p.cc_session, "12f17a65-a326-4f2f-88a9-47a53e61de7f");
        assert_eq!(p.cc_state, "working");
        assert_eq!(p.target(), "projects:1.2");
    }

    #[test]
    fn empty_cc_fields_mean_no_claude() {
        // A pane running only a shell: the hook never stamped it. This is the
        // common case (41 of 58 panes on the reference machine) and must not
        // be confused with a parse failure.
        let mut f = OK.to_vec();
        f[13] = "";
        f[14] = "";
        let p = parse_pane(&row(&f)).expect("row should parse");
        assert!(p.cc_session.is_empty());
        assert!(p.cc_state.is_empty());
    }

    #[test]
    fn a_path_with_spaces_keeps_later_columns_aligned() {
        let mut f = OK.to_vec();
        f[10] = "/home/u/my project/sub dir";
        let p = parse_pane(&row(&f)).expect("row should parse");
        assert_eq!(p.path, "/home/u/my project/sub dir");
        assert_eq!(
            p.cc_state, "working",
            "columns after the path must not shift"
        );
    }

    #[test]
    fn short_and_unparseable_rows_are_dropped_not_fatal() {
        assert!(parse_pane("projects\t1\t1").is_none());
        let mut f = OK.to_vec();
        f[2] = "not-a-number";
        assert!(parse_pane(&row(&f)).is_none());
    }

    #[test]
    fn one_bad_row_does_not_kill_the_listing() {
        let raw = format!("{}\nbroken\n{}", row(OK), row(OK));
        assert_eq!(parse_panes(&raw).len(), 2);
    }

    /// A process tree where each pane pid runs claude under a shell — the
    /// shape `pane_current_command` hides (see `proc`).
    fn tree_with_claude_under(pids: &[u32]) -> proc::Tree {
        let mut ps = String::from("1 0 launchd\n");
        for (i, pid) in pids.iter().enumerate() {
            let child = 9000 + i as u32;
            ps.push_str(&format!("{pid} 1 fish\n{child} {pid} claude\n"));
        }
        proc::Tree::parse_for_test(&ps)
    }

    /// A tree where no pid runs claude at all.
    fn tree_without_claude(pids: &[u32]) -> proc::Tree {
        let mut ps = String::from("1 0 launchd\n");
        for pid in pids {
            ps.push_str(&format!("{pid} 1 fish\n"));
        }
        proc::Tree::parse_for_test(&ps)
    }

    /// Build a pane in window `win` running `cmd`, stamped with `sess`.
    fn pane(win: &str, idx: u32, cmd: &str, sess: &str) -> Pane {
        let mut f = OK.to_vec();
        f[3] = win;
        f[7] = "1";
        f[11] = cmd;
        f[13] = sess;
        let mut p = parse_pane(&row(&f)).expect("fixture should parse");
        p.pane_index = idx;
        p
    }

    const SESS: &str = "12f17a65-a326-4f2f-88a9-47a53e61de7f";

    #[test]
    fn a_lone_claude_in_a_window_is_exact() {
        // dashboard:12 on the reference machine: one claude, some shells.
        let mut panes = vec![pane("@1", 1, "fish", SESS), pane("@1", 2, "fish", SESS)];
        panes[0].pid = 100;
        panes[1].pid = 200;
        // Only pane 1 has a claude beneath it; pane 2 is a bare shell.
        let tree =
            proc::Tree::parse_for_test("1 0 launchd\n100 1 fish\n9001 100 claude\n200 1 fish\n");
        assert_eq!(
            session_confidence(&panes, &tree),
            vec![SessionConfidence::Exact, SessionConfidence::Exact],
        );
    }

    #[test]
    fn several_claudes_sharing_a_window_are_ambiguous() {
        // projects:1 on the reference machine: panes 1 and 2 are different
        // claude processes, but the window option publishes a single id, so
        // neither pane can claim it.
        // Every pane reports `fish`; the claudes are grandchildren. This is
        // the exact shape that a command-only check got wrong.
        let mut panes = vec![
            pane("@2", 1, "fish", SESS),
            pane("@2", 2, "fish", SESS),
            pane("@2", 3, "fish", SESS),
        ];
        panes[0].pid = 100;
        panes[1].pid = 200;
        panes[2].pid = 300;
        let tree = tree_with_claude_under(&[100, 200]);
        assert_eq!(
            session_confidence(&panes, &tree),
            vec![SessionConfidence::Ambiguous; 3],
        );
    }

    #[test]
    fn an_unstamped_pane_has_no_session() {
        let panes = vec![pane("@3", 1, "fish", "")];
        let tree = tree_without_claude(&[panes[0].pid]);
        assert_eq!(
            session_confidence(&panes, &tree),
            vec![SessionConfidence::None],
        );
    }

    #[test]
    fn confidence_does_not_leak_across_windows() {
        // Two windows each holding one claude must both stay Exact; counting
        // claude panes server-wide instead of per-window would call both
        // ambiguous.
        let mut panes = vec![pane("@4", 1, "fish", SESS), pane("@5", 1, "fish", SESS)];
        panes[0].pid = 100;
        panes[1].pid = 200;
        let tree = tree_with_claude_under(&[100, 200]);
        assert_eq!(
            session_confidence(&panes, &tree),
            vec![SessionConfidence::Exact, SessionConfidence::Exact],
        );
    }
}
