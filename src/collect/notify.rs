//! The pending-work queue written by `~/.claude/hooks/notify.py`.
//!
//! The hook owns the format; this only consumes it. Deliberately not shelling
//! out to `claude-waiting --json`: that CLI is a human-facing view over the
//! same file, and going through it would add a process spawn per refresh for
//! no extra information.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Why a session is blocked. Mirrors Claude Code's notification_type values.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub enum Kind {
    #[serde(rename = "permission_prompt")]
    Permission,
    #[serde(rename = "idle_prompt")]
    Idle,
    #[serde(rename = "agent_completed")]
    AgentDone,
    #[serde(other)]
    Other,
}

impl Kind {
    /// Whether a human is holding up this session right now.
    ///
    /// `agent_completed` is informational — it fires whenever background work
    /// finishes and would otherwise dominate the list.
    pub fn blocking(&self) -> bool {
        matches!(self, Kind::Permission | Kind::Idle)
    }
}

/// One row of `pending.jsonl`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Entry {
    pub at: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    #[serde(default)]
    pub message: String,
}

/// The newest entry per session, keyed by session id.
///
/// Claude Code re-fires `idle_prompt` for a still-waiting session about every
/// 30 minutes, so a session accumulates rows and only the latest describes its
/// current state. Keying by session id is what makes this joinable to
/// `@cc_session`; joining on the working directory instead would be ambiguous,
/// since several windows sit in `$HOME`.
pub fn load() -> HashMap<String, Entry> {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return HashMap::new();
    };
    parse(&text)
}

fn parse(text: &str) -> HashMap<String, Entry> {
    let mut out: HashMap<String, Entry> = HashMap::new();
    for line in text.lines() {
        // A truncated final line is normal — the hook appends while this reads.
        let Ok(entry) = serde_json::from_str::<Entry>(line) else {
            continue;
        };
        // Later lines win: the file is append-only, so the last row for a
        // session is its current state.
        out.insert(entry.session_id.clone(), entry);
    }
    out
}

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude/notify/pending.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim shape from ~/.claude/notify/pending.jsonl.
    const LINES: &str = r#"{"at":"2026-08-28T23:23:57","session_id":"aaa","cwd":"/tmp","type":"permission_prompt","message":"Claude needs your permission","prompt_id":"p1"}
{"at":"2026-08-29T00:06:55","session_id":"bbb","cwd":"/src","type":"idle_prompt","message":"Claude is waiting for your input","prompt_id":"p2"}
{"at":"2026-08-29T00:08:58","session_id":"aaa","cwd":"/tmp","type":"idle_prompt","message":"Claude is waiting for your input","prompt_id":"p3"}"#;

    #[test]
    fn keeps_only_the_newest_row_per_session() {
        let entries = parse(LINES);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries["aaa"].kind, Kind::Idle, "the later row wins");
        assert_eq!(entries["aaa"].at, "2026-08-29T00:08:58");
    }

    #[test]
    fn distinguishes_blocking_work_from_informational() {
        assert!(Kind::Permission.blocking());
        assert!(Kind::Idle.blocking());
        assert!(
            !Kind::AgentDone.blocking(),
            "agent_completed fires for background work and would dominate",
        );
    }

    #[test]
    fn an_unknown_kind_parses_rather_than_dropping_the_row() {
        let line = r#"{"at":"x","session_id":"ccc","type":"something_new"}"#;
        let entries = parse(line);
        assert_eq!(entries["ccc"].kind, Kind::Other);
        assert!(!entries["ccc"].kind.blocking());
    }

    #[test]
    fn a_truncated_line_does_not_lose_the_rest() {
        // The hook appends while this reads, so a partial final line is normal.
        let text = format!("{LINES}\n{{\"at\":\"2026-08-29T01:00");
        assert_eq!(parse(&text).len(), 2);
    }

    #[test]
    fn an_absent_file_is_an_empty_queue() {
        assert!(parse("").is_empty());
    }
}
