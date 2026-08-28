//! Process tree: one `ps` for the whole machine, indexed by parent.
//!
//! Needed because a pane's foreground command does not reveal what is actually
//! running in it. On the reference machine three panes of `projects:1` each run
//! a different claude, yet all three report `fish` as `pane_current_command` —
//! claude sits two levels down, under the shell. Classifying on the pane
//! command alone marked those panes as unambiguous when they are the exact
//! case that is ambiguous.
//!
//! One bulk `ps` costs ~0.05s for ~1100 processes. A `pgrep` per pane costs
//! that much *each*, which is most of why `tmux.sh save` took 10s.

use std::collections::HashMap;

use anyhow::Result;

use super::cmd;

/// Parent-indexed process table for one machine.
pub struct Tree {
    /// pid -> command name (`comm`, so no arguments).
    comm: HashMap<u32, String>,
    /// ppid -> children.
    children: HashMap<u32, Vec<u32>>,
}

impl Tree {
    /// Snapshot every process on the machine.
    pub fn capture() -> Result<Self> {
        let raw = cmd::run("ps", &["-axo", "pid=,ppid=,comm="], cmd::FAST)?;
        Ok(Self::parse(&raw))
    }

    /// Build a tree from raw `ps` text. Exposed for tests in sibling modules
    /// that need a tree shaped like a specific machine state.
    #[cfg(test)]
    pub fn parse_for_test(raw: &str) -> Self {
        Self::parse(raw)
    }

    fn parse(raw: &str) -> Self {
        let mut comm = HashMap::new();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for line in raw.lines() {
            // `comm` can contain spaces (a full path with one), so the two
            // numeric fields are taken by position and the remainder is kept
            // whole. Splitting on the ppid's text instead would misfire when
            // the same digits appear inside the command.
            let line = line.trim_start();
            let Some((pid, rest)) = split_field(line) else {
                continue;
            };
            let Some((ppid, comm_str)) = split_field(rest) else {
                continue;
            };
            let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
                continue;
            };
            comm.insert(pid, basename(comm_str.trim()).to_string());
            children.entry(ppid).or_default().push(pid);
        }
        Self { comm, children }
    }

    /// Find the first descendant of `root` whose command matches `pred`,
    /// breadth-first so the shallowest match wins.
    ///
    /// Depth is bounded: a pid table read from a live machine can contain a
    /// cycle if a pid was recycled between rows, and an unbounded walk would
    /// hang the collector rather than return a wrong answer.
    pub fn find_descendant(&self, root: u32, pred: impl Fn(&str) -> bool) -> Option<u32> {
        const MAX_DEPTH: usize = 16;
        let mut frontier = vec![root];
        let mut seen = vec![root];
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for pid in frontier {
                for &kid in self.children.get(&pid).into_iter().flatten() {
                    if seen.contains(&kid) {
                        continue;
                    }
                    if self.comm.get(&kid).is_some_and(|c| pred(c)) {
                        return Some(kid);
                    }
                    seen.push(kid);
                    next.push(kid);
                }
            }
            if next.is_empty() {
                return None;
            }
            frontier = next;
        }
        None
    }

    /// Command name for a pid, if it was in the snapshot.
    pub fn comm(&self, pid: u32) -> Option<&str> {
        self.comm.get(&pid).map(String::as_str)
    }
}

/// Strip a leading path so `/usr/bin/claude` and `claude` compare equal.
fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// Split one whitespace-delimited field off the front, returning it and the
/// untouched remainder.
fn split_field(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace)?;
    Some((&s[..end], &s[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // pid ppid comm — shaped like the panes of projects:1: a shell whose
    // grandchild is the claude that `pane_current_command` never shows.
    const PS: &str = "\
  1     0 /sbin/launchd
100     1 fish
200   100 ccproxy
300   200 claude
400     1 fish
500   400 ghx
";

    #[test]
    fn finds_a_grandchild_the_pane_command_hides() {
        let t = Tree::parse(PS);
        assert_eq!(t.find_descendant(100, |c| c == "claude"), Some(300));
    }

    #[test]
    fn reports_none_when_the_tree_holds_no_match() {
        let t = Tree::parse(PS);
        assert_eq!(t.find_descendant(400, |c| c == "claude"), None);
    }

    #[test]
    fn strips_the_path_from_a_command() {
        let t = Tree::parse(PS);
        assert_eq!(t.comm(1), Some("launchd"));
    }

    #[test]
    fn a_cycle_terminates_instead_of_hanging() {
        // pid 10 and 11 claim each other as parent, which a recycled pid can
        // produce between `ps` rows.
        let t = Tree::parse("10 11 a\n11 10 b\n");
        assert_eq!(t.find_descendant(10, |c| c == "nothing"), None);
    }

    #[test]
    fn malformed_rows_are_skipped() {
        let t = Tree::parse("garbage\n\n100 1 fish\n");
        assert_eq!(t.comm(100), Some("fish"));
    }
}
