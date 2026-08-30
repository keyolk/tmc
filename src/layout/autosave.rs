//! The hourly snapshot: write, then prune.
//!
//! Pruning is not housekeeping, it is what keeps the picker usable. Before
//! `tmux.sh` grew this, 81 dead session directories — 6 MB, 1500 files from
//! throwaway sessions like `a9s-verify-*` — buried the two live sessions in
//! the list.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::collect::cmd;
use crate::layout::Session;

/// How many snapshots to keep per session.
pub const KEEP: usize = 24;

/// A session directory whose newest snapshot is older than this, and which is
/// not currently running, was a throwaway. These snapshots exist for crash
/// recovery; a session gone two weeks is not coming back.
pub const MAX_AGE_DAYS: u64 = 14;

/// Write one snapshot per session under `<root>/<session>/<stamp>.json`.
pub fn write(root: &Path, sessions: &[Session], stamp: &str) -> Result<usize> {
    for session in sessions {
        let dir = root.join(session.session.replace('/', "_"));
        crate::layout::save::write(session, &dir.join(format!("{stamp}.json")))?;
    }
    Ok(sessions.len())
}

/// Drop all but the newest `keep` snapshots in each session directory.
///
/// Ordering is by filename, not mtime: the names are UTC timestamps, so they
/// sort chronologically, and a file copied or touched later does not jump the
/// queue the way an mtime sort would let it.
pub fn prune_old(root: &Path, keep: usize) -> usize {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for dir in dirs.flatten().filter(|d| d.path().is_dir()) {
        let mut files = snapshot_files(&dir.path());
        if files.len() <= keep {
            continue;
        }
        files.sort();
        let cut = files.len() - keep;
        for old in files.into_iter().take(cut) {
            if std::fs::remove_file(&old).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Remove session directories for sessions that are gone and stale.
///
/// `is_live` is injected so this is testable without a tmux server.
pub fn prune_dead(
    root: &Path,
    now_secs: u64,
    max_age_days: u64,
    is_live: impl Fn(&str) -> bool,
) -> usize {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return 0;
    };
    let cutoff = now_secs.saturating_sub(max_age_days * 86_400);
    let mut removed = 0;

    for dir in dirs.flatten().filter(|d| d.path().is_dir()) {
        let path = dir.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // A running session is never stale, however old its newest snapshot.
        if is_live(&name) {
            continue;
        }
        let files = snapshot_files(&path);
        if files.is_empty() {
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
            continue;
        }
        let newest = files
            .iter()
            .filter_map(|f| modified_secs(f))
            .max()
            .unwrap_or(0);
        if newest < cutoff && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether a tmux session with this exact name is running.
pub fn session_is_live(name: &str) -> bool {
    cmd::run(
        "tmux",
        &["has-session", "-t", &format!("={name}")],
        cmd::FAST,
    )
    .is_ok()
}

fn snapshot_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .collect()
}

fn modified_secs(path: &Path) -> Option<u64> {
    Some(
        std::fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("tmc-autosave-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}").unwrap();
    }

    #[test]
    fn keeps_only_the_newest_snapshots_per_session() {
        let dir = Dir::new("prune-old");
        for i in 0..30 {
            touch(
                &dir.path()
                    .join(format!("projects/2026082{}T000000Z.json", i % 10)),
            );
        }
        // 10 distinct names (the modulo collapses the rest), keep 4.
        let removed = prune_old(dir.path(), 4);
        let left = snapshot_files(&dir.path().join("projects"));
        assert_eq!(left.len(), 4);
        assert_eq!(removed, 6);

        // The four that survive must be the chronologically newest.
        let mut names: Vec<String> = left
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "20260826T000000Z.json",
                "20260827T000000Z.json",
                "20260828T000000Z.json",
                "20260829T000000Z.json",
            ],
        );
    }

    #[test]
    fn leaves_a_session_under_the_limit_untouched() {
        let dir = Dir::new("prune-under");
        touch(&dir.path().join("projects/20260829T000000Z.json"));
        assert_eq!(prune_old(dir.path(), 24), 0);
        assert_eq!(snapshot_files(&dir.path().join("projects")).len(), 1);
    }

    #[test]
    fn removes_a_stale_directory_for_a_session_that_is_gone() {
        let dir = Dir::new("prune-dead");
        touch(&dir.path().join("throwaway/20260101T000000Z.json"));

        // now = far in the future, so the file is well past the cutoff.
        let removed = prune_dead(dir.path(), 4_000_000_000, 14, |_| false);
        assert_eq!(removed, 1);
        assert!(!dir.path().join("throwaway").exists());
    }

    #[test]
    fn never_removes_a_running_session_however_old() {
        let dir = Dir::new("prune-live");
        touch(&dir.path().join("projects/20260101T000000Z.json"));

        let removed = prune_dead(dir.path(), 4_000_000_000, 14, |name| name == "projects");
        assert_eq!(removed, 0, "a live session is never stale");
        assert!(dir.path().join("projects").exists());
    }

    #[test]
    fn keeps_a_recent_directory_even_when_the_session_is_gone() {
        let dir = Dir::new("prune-recent");
        touch(&dir.path().join("gone/20260829T000000Z.json"));

        // The file was just written, so `now` is only seconds past it.
        let now = modified_secs(&dir.path().join("gone/20260829T000000Z.json")).unwrap();
        assert_eq!(prune_dead(dir.path(), now, 14, |_| false), 0);
    }

    #[test]
    fn removes_an_empty_session_directory() {
        let dir = Dir::new("prune-empty");
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        assert_eq!(prune_dead(dir.path(), 4_000_000_000, 14, |_| false), 1);
    }
}
