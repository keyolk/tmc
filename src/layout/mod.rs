//! The on-disk restore point format.
//!
//! Deliberately the same JSON `tmux.sh` writes, so the 24 autosaves already on
//! disk stay loadable and rolling back to the shell script during the
//! migration does not strand a snapshot. New fields are optional and are
//! skipped when empty, which keeps a tmc-written file byte-comparable to a
//! tmux.sh-written one for the fields both produce.

pub mod autosave;
pub mod point;
pub mod restore;
pub mod save;

use serde::{Deserialize, Serialize};

/// One session's layout at one moment. A restore point is a directory of
/// these, one file per session, captured together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub session: String,
    /// UTC, `%Y-%m-%dT%H:%M:%SZ`.
    pub saved_at: String,
    /// Summary of the windows, for the restore-point picker. `tmux.sh` has no
    /// such field; readers of an older file fall back to the timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub windows: Vec<Window>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub index: u32,
    pub name: String,
    /// tmux's own layout string — exact pane geometry.
    pub layout: String,
    pub panes: Vec<Pane>,
    /// Chrome tab group belonging to this window, when the bridge exported
    /// one. Passed through verbatim; tmc never interprets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chrome: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub index: u32,
    pub path: String,
    /// The command to prefill on restore. Empty means the pane held nothing
    /// but a shell, which is the correct thing to restore — see `shell_only`.
    pub command: String,
    /// Claude Code session to resume in this pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session: Option<String>,

    // ---- fields tmux.sh does not write ----
    /// `%N` at save time. The join key for diffing against a live server;
    /// absent in files written by tmux.sh, and meaningless across a tmux
    /// server restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// True when the pane really was just a shell, false when a command was
    /// found. Distinguishes "correctly empty" from "we failed to read one" —
    /// 16 of 56 panes on the reference machine are legitimately empty, and
    /// without this flag `doctor` cannot tell the two apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_only: Option<bool>,
    /// How far to trust `claude_session`: `exact` when the window held one
    /// claude, `ambiguous` when it held several and they share one published
    /// id. Absent when there is no session at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_confidence: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from ~/.config/tmux/layouts/autosave/projects/, the shape
    /// tmux.sh writes today.
    const TMUX_SH_FILE: &str = r#"{
      "session": "projects",
      "saved_at": "2026-08-29T20:40:43Z",
      "windows": [
        {
          "index": 1,
          "name": "cohome",
          "layout": "30dc,365x97,0,0{252x97,0,0}",
          "panes": [
            {
              "index": 1,
              "path": "/home/u/exp/cohome",
              "command": "ccproxy claude --intercept=mitm",
              "claude_session": "12f17a65-a326-4f2f-88a9-47a53e61de7f"
            },
            {
              "index": 4,
              "path": "/home/u/exp/cohome",
              "command": ""
            }
          ]
        }
      ]
    }"#;

    #[test]
    fn reads_a_file_written_by_tmux_sh() {
        let s: Session = serde_json::from_str(TMUX_SH_FILE).expect("should parse");
        assert_eq!(s.session, "projects");
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.windows[0].panes.len(), 2);
        assert_eq!(
            s.windows[0].panes[0].claude_session.as_deref(),
            Some("12f17a65-a326-4f2f-88a9-47a53e61de7f"),
        );
        // A shell-only pane: no claude_session key at all, empty command.
        assert_eq!(s.windows[0].panes[1].claude_session, None);
        assert!(s.windows[0].panes[1].command.is_empty());
        // Fields tmux.sh never wrote must default rather than fail the parse.
        assert_eq!(s.windows[0].panes[1].pane_id, None);
        assert_eq!(s.windows[0].panes[1].shell_only, None);
        assert_eq!(s.label, None);
    }

    #[test]
    fn round_trips_without_inventing_keys() {
        // Re-serializing a tmux.sh file must not add nulls that tmux.sh's jq
        // consumers would then have to handle. Checked on the emitted text,
        // not a `Value` — `Value`'s map sorts its keys and would hide both the
        // real order and a stray key's position.
        let s: Session = serde_json::from_str(TMUX_SH_FILE).unwrap();
        let text = serde_json::to_string(&s).unwrap();

        assert!(
            text.contains(
                r#"{"index":4,"path":"/home/u/exp/cohome","command":""}"#
            ),
            "an empty pane must serialize with exactly the keys tmux.sh writes, got: {text}",
        );
        assert!(
            !text.contains("null"),
            "no key may serialize as null: {text}"
        );
        assert!(!text.contains("\"label\""));
        assert!(!text.contains("\"chrome\""));
    }

    #[test]
    fn new_fields_survive_a_round_trip() {
        let mut s: Session = serde_json::from_str(TMUX_SH_FILE).unwrap();
        s.label = Some("cohome +12".into());
        s.windows[0].panes[0].pane_id = Some("%251".into());
        s.windows[0].panes[0].shell_only = Some(false);
        s.windows[0].panes[0].session_confidence = Some("ambiguous".into());

        let text = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn an_unknown_future_key_does_not_break_the_parse() {
        let text = TMUX_SH_FILE.replace(
            r#""session": "projects","#,
            r#""session": "projects", "written_by": "some-later-version","#,
        );
        assert!(serde_json::from_str::<Session>(&text).is_ok());
    }

    /// Every snapshot already on this machine must still load.
    ///
    /// Skipped when the directory is absent so the suite stays hermetic on a
    /// clean checkout or in CI; where the files do exist — 49 of them here —
    /// this is the real compatibility check, and a fixture cannot replace it.
    #[test]
    fn reads_every_snapshot_on_disk() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let dir = std::path::Path::new(&home).join(".config/tmux/layouts/autosave");
        let Ok(sessions) = std::fs::read_dir(&dir) else {
            return;
        };

        let mut checked = 0;
        for session in sessions.flatten() {
            let Ok(files) = std::fs::read_dir(session.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("readable");
                let parsed: Result<Session, _> = serde_json::from_str(&text);
                assert!(
                    parsed.is_ok(),
                    "{} failed to parse: {:?}",
                    path.display(),
                    parsed.err()
                );
                checked += 1;
            }
        }
        eprintln!("checked {checked} snapshot(s) on disk");
    }
}
