//! Building a restore point from the live tmux server.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::collect::{command, proc::Tree, tmux};
use crate::layout::{Pane, Session, Window};

/// Where restore points live. Manual ones sit in `<dir>/<name>/`, autosaves in
/// `<dir>/autosave/<session>/<timestamp>.json` — the layout `tmux.sh` uses and
/// the 24 snapshots on disk already follow.
pub fn layout_dir() -> PathBuf {
    dirs_home().join(".config/tmux/layouts")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Group live panes into one `Session` per tmux session.
///
/// `now` is passed in rather than read here so a caller can produce a
/// deterministic snapshot in tests.
pub fn snapshot(panes: &[tmux::Pane], tree: &Tree, now: &str) -> Vec<Session> {
    let confidence = tmux::session_confidence(panes, tree);

    // BTreeMap keeps sessions and windows in index order without a later sort,
    // which is also the order tmux.sh writes them in.
    let mut sessions: BTreeMap<&str, BTreeMap<u32, Window>> = BTreeMap::new();

    for (p, conf) in panes.iter().zip(&confidence) {
        let windows = sessions.entry(p.session.as_str()).or_default();
        let window = windows.entry(p.window_index).or_insert_with(|| Window {
            index: p.window_index,
            name: p.window_name.clone(),
            layout: p.window_layout.clone(),
            panes: Vec::new(),
            chrome: None,
        });
        window.panes.push(build_pane(p, *conf, tree));
    }

    sessions
        .into_iter()
        .map(|(name, windows)| {
            let windows: Vec<Window> = windows.into_values().collect();
            Session {
                session: name.to_string(),
                saved_at: now.to_string(),
                label: Some(label_for(&windows)),
                windows,
            }
        })
        .collect()
}

fn build_pane(p: &tmux::Pane, conf: tmux::SessionConfidence, tree: &Tree) -> Pane {
    let command = command::for_pane(p.pid, tree, |pid| tree.args(pid).map(str::to_string));
    let shell_only = command.is_none();

    // The published id names a session in this window. When several claudes
    // share the window it is not necessarily *this* pane's, so the confidence
    // travels with it and the restore UI can ask rather than guess.
    let (claude_session, session_confidence) = match conf {
        tmux::SessionConfidence::None => (None, None),
        tmux::SessionConfidence::Exact => (non_empty(&p.cc_session), Some("exact".to_string())),
        tmux::SessionConfidence::Ambiguous => {
            (non_empty(&p.cc_session), Some("ambiguous".to_string()))
        }
    };

    Pane {
        index: p.pane_index,
        path: p.path.clone(),
        command: command.unwrap_or_default(),
        claude_session,
        pane_id: Some(p.pane_id.clone()),
        shell_only: Some(shell_only),
        session_confidence,
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// A human-readable summary of what a restore point holds.
///
/// `auto:20260828T143549Z` says nothing about its contents, so the picker had
/// no way to tell one hourly snapshot from another. Window names are what the
/// user actually recognizes.
fn label_for(windows: &[Window]) -> String {
    const SHOWN: usize = 3;
    let names: Vec<&str> = windows.iter().map(|w| w.name.as_str()).collect();
    let head = names
        .iter()
        .take(SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().saturating_sub(SHOWN) {
        0 => head,
        rest => format!("{head} +{rest}"),
    }
}

/// Write one session's layout, creating parent directories as needed.
pub fn write(session: &Session, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    // Pretty-printed to match what tmux.sh's `jq` emits, so a diff between a
    // tmux.sh file and a tmxx file shows real differences and not formatting.
    let text = serde_json::to_string_pretty(session)?;
    std::fs::write(path, text + "\n").with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-08-30T00:00:00Z";

    fn pane(session: &str, win: u32, win_name: &str, idx: u32, pid: u32) -> tmux::Pane {
        tmux::Pane {
            session: session.into(),
            session_attached: true,
            window_index: win,
            window_id: format!("@{win}"),
            window_name: win_name.into(),
            window_layout: "abcd,80x24,0,0".into(),
            window_active: false,
            pane_index: idx,
            pane_id: format!("%{pid}"),
            pid,
            path: "/home/u/src".into(),
            current_command: "fish".into(),
            active: idx == 1,
            cc_session: String::new(),
            cc_state: String::new(),
        }
    }

    /// pid 100 runs ccproxy under a shell; pid 400 is a bare shell.
    fn tree() -> Tree {
        Tree::parse_for_test_with_args(
            "1 0 launchd\n100 1 fish\n200 100 ccproxy\n400 1 fish\n",
            &[(200, "ccproxy claude --model default")],
        )
    }

    #[test]
    fn groups_panes_into_sessions_and_windows() {
        let panes = vec![
            pane("projects", 1, "cohome", 1, 100),
            pane("projects", 1, "cohome", 2, 400),
            pane("projects", 2, "mitm", 1, 400),
            pane("dashboard", 1, "coder", 1, 400),
        ];
        let sessions = snapshot(&panes, &tree(), NOW);

        assert_eq!(sessions.len(), 2);
        // BTreeMap orders sessions by name, windows by index.
        assert_eq!(sessions[0].session, "dashboard");
        assert_eq!(sessions[1].session, "projects");
        assert_eq!(sessions[1].windows.len(), 2);
        assert_eq!(sessions[1].windows[0].panes.len(), 2);
        assert_eq!(sessions[1].saved_at, NOW);
    }

    #[test]
    fn records_the_command_behind_a_shell() {
        let panes = vec![pane("projects", 1, "cohome", 1, 100)];
        let s = snapshot(&panes, &tree(), NOW);
        let p = &s[0].windows[0].panes[0];
        assert_eq!(p.command, "ccproxy claude --model default");
        assert_eq!(p.shell_only, Some(false));
    }

    #[test]
    fn marks_a_bare_shell_pane_rather_than_leaving_it_ambiguous() {
        // The distinction tmux.sh could not express: an empty command here is
        // correct, not a failure to read one.
        let panes = vec![pane("projects", 1, "cohome", 1, 400)];
        let s = snapshot(&panes, &tree(), NOW);
        let p = &s[0].windows[0].panes[0];
        assert!(p.command.is_empty());
        assert_eq!(p.shell_only, Some(true));
    }

    #[test]
    fn carries_session_confidence_next_to_the_id() {
        let mut panes = vec![
            pane("projects", 1, "cohome", 1, 100),
            pane("projects", 1, "cohome", 2, 100),
        ];
        for p in &mut panes {
            p.cc_session = "abc-123".into();
        }
        // Both panes resolve to a claude, so the window-scoped id is ambiguous.
        let s = snapshot(&panes, &tree(), NOW);
        for p in &s[0].windows[0].panes {
            assert_eq!(p.claude_session.as_deref(), Some("abc-123"));
            assert_eq!(p.session_confidence.as_deref(), Some("ambiguous"));
        }
    }

    #[test]
    fn labels_a_restore_point_by_its_window_names() {
        let panes: Vec<tmux::Pane> = ["cohome", "mitm", "istio", "kite", "cage"]
            .iter()
            .enumerate()
            .map(|(i, n)| pane("projects", i as u32 + 1, n, 1, 400))
            .collect();
        let s = snapshot(&panes, &tree(), NOW);
        assert_eq!(s[0].label.as_deref(), Some("cohome, mitm, istio +2"));
    }

    #[test]
    fn a_short_workspace_gets_a_label_without_a_tail() {
        let panes = vec![pane("projects", 1, "cohome", 1, 400)];
        let s = snapshot(&panes, &tree(), NOW);
        assert_eq!(s[0].label.as_deref(), Some("cohome"));
    }
}
