//! UI state that outlives one popup: which windows were expanded, the last
//! query, and the restore point being compared against.
//!
//! tmc is summoned and dismissed dozens of times a day, and each time it
//! used to forget all three — re-expanding the same window to reach its panes
//! was the usual first keystroke.
//!
//! Kept under `$XDG_STATE_HOME/tmc/ui.json` (`~/.local/state` by default):
//! this is state, not configuration, and losing it costs nothing but the
//! convenience. Written once, on the way out, atomically.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::model::{Model, Row};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Window targets whose panes were listed.
    pub expanded: BTreeSet<String>,
    pub search: String,
    /// The restore point's display name, e.g. `saved:work`. A name rather
    /// than an index: a new autosave shifts every index by one.
    pub point: Option<String>,
    /// The tree | detail split, only once the user has moved it: an unmoved
    /// split follows the default. A garbage value drops the ratio, not the
    /// file.
    #[serde(
        default,
        deserialize_with = "super::split::lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub split: Option<super::split::State>,
}

pub fn path() -> PathBuf {
    path_in(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// The XDG rule: an unset or empty `$XDG_STATE_HOME` means `~/.local/state`.
fn path_in(xdg: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    let base = xdg
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home.map(PathBuf::from)
                .unwrap_or_default()
                .join(".local/state")
        });
    base.join("tmc/ui.json")
}

/// The saved state, or the default when there is none.
///
/// An unreadable or corrupt file is an error the caller shows, not a reason
/// to refuse to open: the next save replaces it.
pub fn load(path: &Path) -> Result<UiState> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(UiState::default()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

/// Write via a temp file and rename, so a crash mid-write leaves the old
/// file rather than half a JSON document.
pub fn save(path: &Path, state: &UiState) -> Result<()> {
    let dir = path.parent().context("state path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let body = serde_json::to_vec_pretty(state)?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))
}

impl UiState {
    /// What is worth keeping from a model at exit.
    ///
    /// Expanded windows that no longer exist are dropped, or the set would
    /// grow with every window ever opened.
    pub fn capture(model: &Model) -> Self {
        let known: BTreeSet<String> = model
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Window(w) => Some(w.target()),
                _ => None,
            })
            .collect();
        // An empty tree means the last read failed; keeping everything is
        // safer than concluding every window is gone.
        let expanded = if known.is_empty() {
            model.expanded.iter().cloned().collect()
        } else {
            model
                .expanded
                .iter()
                .filter(|t| known.contains(*t))
                .cloned()
                .collect()
        };
        Self {
            expanded,
            search: model.search.clone(),
            point: model.current_point().map(|p| p.name.clone()),
            split: model.split.state(),
        }
    }

    /// Put this state back into a fresh model.
    pub fn restore(&self, model: &mut Model) {
        // Out of range (NaN, a percentage) is rejected and the default kept.
        if let Some(s) = self.split {
            model.split.restore(s);
        }
        model.expanded = self.expanded.iter().cloned().collect();
        model.search = self.search.clone();
        // A query from last time is a suggestion, not something to append
        // to: typing `b` after a restored `foo` means `b`, not `foob`.
        model.search_selected = !self.search.is_empty();
        // A point that has since been pruned falls back to the newest, which
        // is what a first run compares against too.
        if let Some(name) = &self.point
            && let Some(i) = model.points.iter().position(|p| &p.name == name)
        {
            model.point_index = i;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::point::{Point, Ref};
    use ratatui::layout::Rect;

    fn point(name: &str) -> Point {
        Point {
            reference: Ref::Dir(PathBuf::from(format!("/nowhere/{name}"))),
            name: name.into(),
            sort_key: String::new(),
            sessions: 1,
            is_auto: false,
        }
    }

    #[test]
    fn state_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("tmc-state-{}", std::process::id()));
        let file = dir.join("tmc/ui.json");
        let state = UiState {
            expanded: ["projects:1".to_string()].into(),
            search: "claude".into(),
            point: Some("saved:work".into()),
            split: Some(crate::ui::split::State { ratio: 0.4 }),
        };
        save(&file, &state).expect("save");
        assert_eq!(load(&file).expect("load"), state);
        // No temp file is left behind.
        let left: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_the_default_and_a_corrupt_one_says_so() {
        let dir = std::env::temp_dir().join(format!("tmc-state-bad-{}", std::process::id()));
        assert_eq!(load(&dir.join("absent.json")).unwrap(), UiState::default());
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("ui.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(load(&bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restoring_selects_the_point_by_name_not_position() {
        let mut m = Model::new(vec![point("auto:2"), point("auto:1"), point("saved:work")]);
        UiState {
            point: Some("saved:work".into()),
            search: "cl".into(),
            ..Default::default()
        }
        .restore(&mut m);
        assert_eq!(m.point_index, 2);
        assert_eq!(m.search, "cl");

        let mut m = Model::new(vec![point("auto:2")]);
        UiState {
            point: Some("pruned".into()),
            ..Default::default()
        }
        .restore(&mut m);
        assert_eq!(
            m.point_index, 0,
            "a vanished point falls back to the newest"
        );
    }

    #[test]
    fn a_moved_split_round_trips_and_an_unmoved_one_is_not_saved() {
        let r = Rect::new(0, 0, 120, 24);
        let mut m = Model::new(Vec::new());
        assert_eq!(
            UiState::capture(&m).split,
            None,
            "unmoved: follow the default"
        );
        m.split.handle(crate::ui::split::Action::Grow, r);
        let saved = UiState::capture(&m);
        let ratio = saved.split.expect("moved").ratio;
        let mut fresh = Model::new(Vec::new());
        saved.restore(&mut fresh);
        assert_eq!(fresh.split.ratio(), ratio);
        assert_eq!(fresh.split.layout(r), m.split.layout(r));
    }

    #[test]
    fn a_garbage_split_falls_back_to_the_default_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("tmc-state-split-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ui.json");
        let default = Model::new(Vec::new()).split.ratio();
        for split in [
            r#"{"ratio": 55}"#,
            r#"{"ratio": "wide"}"#,
            r#"{"ratio": -1}"#,
            "[]",
        ] {
            let raw = format!(r#"{{"search": "cl", "split": {split}}}"#);
            std::fs::write(&file, raw).unwrap();
            let state = load(&file).expect("the rest of the file still loads");
            assert_eq!(state.search, "cl");
            let mut m = Model::new(Vec::new());
            state.restore(&mut m);
            assert_eq!(m.split.ratio(), default, "{split}");
            assert!(!m.split.is_set(), "{split}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_state_path_follows_xdg_state_home() {
        assert_eq!(
            path_in(Some("/x/state".into()), Some("/home/u".into())),
            PathBuf::from("/x/state/tmc/ui.json"),
        );
        assert_eq!(
            path_in(Some("".into()), Some("/home/u".into())),
            PathBuf::from("/home/u/.local/state/tmc/ui.json"),
            "an empty value is unset",
        );
    }
}
