//! Finding and reading restore points on disk.
//!
//! Two kinds live under `~/.config/tmux/layouts`, and the difference is not
//! cosmetic:
//!
//! - **manual** — `<dir>/<name>/<session>.json`. One directory per point.
//! - **autosave** — `<dir>/autosave/<session>/<timestamp>.json`. Grouped by
//!   *session* on disk, but a restore point is the set of files sharing one
//!   timestamp across every session. The point is virtual; it has no directory.
//!
//! That asymmetry is why a point is addressed by `Ref` rather than a path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::Session;

/// Where a restore point's files live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ref {
    /// One directory holding every session's file.
    Dir(PathBuf),
    /// Files named `<timestamp>.json` scattered under per-session directories.
    Autosave { root: PathBuf, timestamp: String },
}

/// One restore point, as the picker shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub reference: Ref,
    /// `saved:mypoint` or `auto:20260829T204041Z`.
    pub name: String,
    /// Newest-first ordering key. For autosaves this is the timestamp; for a
    /// manual point it is the newest file's modification time, formatted the
    /// same way so the two sort together.
    pub sort_key: String,
    pub sessions: usize,
    pub is_auto: bool,
}

/// Every restore point, newest first.
pub fn list(layout_dir: &Path) -> Vec<Point> {
    let mut points = manual_points(layout_dir);
    points.extend(autosave_points(&layout_dir.join("autosave")));
    // Newest first. Ties keep manual points ahead of autosaves: a point the
    // user named deliberately is the more likely target.
    points.sort_by(|a, b| b.sort_key.cmp(&a.sort_key).then(a.is_auto.cmp(&b.is_auto)));
    points
}

fn manual_points(layout_dir: &Path) -> Vec<Point> {
    let Ok(entries) = std::fs::read_dir(layout_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if !path.is_dir() || path.file_name()? == "autosave" {
                return None;
            }
            let files = json_files(&path);
            if files.is_empty() {
                return None; // a directory with no layouts is not a point
            }
            let newest = files
                .iter()
                .filter_map(|f| modified_stamp(f))
                .max()
                .unwrap_or_default();
            Some(Point {
                name: format!("saved:{}", path.file_name()?.to_string_lossy()),
                sort_key: newest,
                sessions: files.len(),
                is_auto: false,
                reference: Ref::Dir(path),
            })
        })
        .collect()
}

/// Group autosave files by the timestamp they share.
fn autosave_points(root: &Path) -> Vec<Point> {
    let Ok(sessions) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut by_stamp: BTreeMap<String, usize> = BTreeMap::new();
    for session in sessions.flatten() {
        for file in json_files(&session.path()) {
            if let Some(stamp) = file.file_stem().map(|s| s.to_string_lossy().into_owned()) {
                *by_stamp.entry(stamp).or_default() += 1;
            }
        }
    }
    by_stamp
        .into_iter()
        .map(|(timestamp, sessions)| Point {
            name: format!("auto:{timestamp}"),
            sort_key: timestamp.clone(),
            sessions,
            is_auto: true,
            reference: Ref::Autosave {
                root: root.to_path_buf(),
                timestamp,
            },
        })
        .collect()
}

/// The layout files belonging to one restore point, sorted by session name.
pub fn files(reference: &Ref) -> Vec<PathBuf> {
    let mut out = match reference {
        Ref::Dir(dir) => json_files(dir),
        Ref::Autosave { root, timestamp } => {
            let Ok(sessions) = std::fs::read_dir(root) else {
                return Vec::new();
            };
            sessions
                .flatten()
                .map(|s| s.path().join(format!("{timestamp}.json")))
                .filter(|p| p.is_file())
                .collect()
        }
    };
    out.sort();
    out
}

/// Read every session in a restore point.
pub fn read(reference: &Ref) -> Result<Vec<Session>> {
    files(reference)
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?;
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
        })
        .collect()
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .collect();
    out.sort();
    out
}

/// A file's mtime as `YYYYMMDDTHHMMSSZ`, so manual points sort against
/// autosave timestamps without a second format to parse.
fn modified_stamp(path: &Path) -> Option<String> {
    let secs = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let (y, mo, d, h, mi, s) = crate::clock::civil_from_unix(secs);
    Some(format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn session_json(name: &str) -> String {
        format!(r#"{{"session":"{name}","saved_at":"2026-08-29T20:40:43Z","windows":[]}}"#)
    }

    /// A layout dir holding one manual point and two autosave points.
    fn fixture() -> tempdir::TempDir {
        let dir = tempdir::TempDir::new();
        let root = dir.path();
        write(
            &root.join("mypoint/projects.json"),
            &session_json("projects"),
        );
        write(
            &root.join("mypoint/dashboard.json"),
            &session_json("dashboard"),
        );
        for stamp in ["20260829T204041Z", "20260829T214041Z"] {
            write(
                &root.join(format!("autosave/projects/{stamp}.json")),
                &session_json("projects"),
            );
            write(
                &root.join(format!("autosave/dashboard/{stamp}.json")),
                &session_json("dashboard"),
            );
        }
        dir
    }

    #[test]
    fn lists_manual_and_autosave_points() {
        let dir = fixture();
        let points = list(dir.path());

        assert_eq!(points.len(), 3, "one manual, two autosave");
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"saved:mypoint"));
        assert!(names.contains(&"auto:20260829T204041Z"));
        assert!(names.contains(&"auto:20260829T214041Z"));
    }

    #[test]
    fn counts_the_sessions_captured_together() {
        let dir = fixture();
        for p in list(dir.path()) {
            assert_eq!(p.sessions, 2, "{} should span both sessions", p.name);
        }
    }

    #[test]
    fn orders_newest_first() {
        let dir = fixture();
        let points = list(dir.path());
        // The manual point was written last, so it sorts first; between the
        // autosaves the later timestamp wins.
        let autos: Vec<&str> = points
            .iter()
            .filter(|p| p.is_auto)
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(
            autos,
            vec!["auto:20260829T214041Z", "auto:20260829T204041Z"],
        );
    }

    #[test]
    fn resolves_an_autosave_point_to_files_across_session_dirs() {
        let dir = fixture();
        let reference = Ref::Autosave {
            root: dir.path().join("autosave"),
            timestamp: "20260829T204041Z".into(),
        };
        let files = files(&reference);
        assert_eq!(
            files.len(),
            2,
            "one file per session, gathered by timestamp"
        );
        assert!(files.iter().all(|f| f.ends_with("20260829T204041Z.json")));
    }

    #[test]
    fn reads_every_session_in_a_point() {
        let dir = fixture();
        let sessions = read(&Ref::Dir(dir.path().join("mypoint"))).unwrap();
        let mut names: Vec<&str> = sessions.iter().map(|s| s.session.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["dashboard", "projects"]);
    }

    #[test]
    fn ignores_a_directory_with_no_layouts() {
        let dir = fixture();
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        assert!(list(dir.path()).iter().all(|p| p.name != "saved:empty"));
    }

    #[test]
    fn an_absent_layout_dir_is_empty_not_an_error() {
        assert!(list(Path::new("/nonexistent/tmc-test")).is_empty());
    }
}

/// Minimal scoped temp directory, so the suite stays hermetic without adding a
/// dependency for one helper.
#[cfg(test)]
mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = std::env::temp_dir().join(format!("tmc-test-{pid}-{n}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
