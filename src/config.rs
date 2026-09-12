//! User configuration: `~/.config/tmc/config.toml`.
//!
//! One thing lives here today — which restored commands are allowed to run on
//! their own. Restore deliberately types commands at the prompt and stops
//! there (see `layout::restore`), because bringing a workspace back should not
//! start 30 processes unasked. But a handful of commands are the whole point
//! of restoring: a `claude` pane that has to be started by hand is a pane you
//! have not actually recovered yet.
//!
//! So the choice is per-command and the user's, by pattern:
//!
//! ```toml
//! [restore]
//! autorun = ["claude", "nvim", "htop"]
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Commands run on restore when there is no config file.
///
/// `claude` only. Resuming a conversation is the case that motivated autorun,
/// and a token match here also covers the wrapper forms (`ccproxy claude …`).
/// Everything else stays typed-but-not-run until the user says otherwise —
/// `nvim` and `htop` are per-taste, and guessing wrong opens editors in a
/// dozen panes.
const DEFAULT_AUTORUN: [&str; 1] = ["claude"];

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub restore: Restore,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Restore {
    /// Patterns whose matching commands are executed on restore instead of
    /// being left at the prompt. See [`matches`] for the matching rules.
    ///
    /// An explicit empty list turns autorun off; omitting the key entirely
    /// leaves [`DEFAULT_AUTORUN`] in place.
    pub autorun: Vec<String>,
}

impl Default for Restore {
    fn default() -> Self {
        Self {
            autorun: DEFAULT_AUTORUN.iter().map(|p| p.to_string()).collect(),
        }
    }
}

impl Restore {
    /// Whether this command line should be executed rather than only typed.
    pub fn should_run(&self, command: &str) -> bool {
        self.autorun.iter().any(|p| matches(p, command))
    }
}

pub fn path() -> PathBuf {
    home().join(".config/tmc/config.toml")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Read the config, falling back to the defaults when there is no file.
///
/// A file that exists but does not parse is an error rather than a silent
/// fallback: "autorun is off" and "your config did not parse" look identical
/// from the outside, and what hangs on the difference is whether a screenful
/// of commands runs.
pub fn load() -> Result<Config> {
    load_from(&path())
}

fn load_from(path: &std::path::Path) -> Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    Ok(toml::from_str(text)?)
}

/// Does `pattern` describe `command`?
///
/// Two spellings, because the two questions being asked are different:
///
/// - A plain word matches when any token of the command line equals it. The
///   program is rarely the interesting word — `claude` has to find `ccproxy
///   claude --intercept=mitm`, and matching a substring instead would also
///   find `echo claudette`.
/// - A pattern containing `*` or `?` is a glob over the whole line, for when
///   the token is not the unit you want (`nvim *`, `cargo watch*`).
pub fn matches(pattern: &str, command: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        return glob(pattern, command);
    }
    command.split_whitespace().any(|token| token == pattern)
}

/// `*` (any run, including empty) and `?` (one char), anchored at both ends.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();

    // Iterative backtracking rather than recursion: a pattern like `*a*b*c*`
    // against a long command line is exponential the naive way.
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            // Let the last `*` swallow one more character and retry.
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(patterns: &[&str]) -> Restore {
        Restore {
            autorun: patterns.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn a_word_matches_any_token_not_just_the_program() {
        // The case that motivated token matching: claude is behind a wrapper.
        assert!(matches("claude", "ccproxy claude --intercept=mitm"));
        assert!(matches("claude", "claude --resume abc"));
        assert!(matches("htop", "htop"));
    }

    #[test]
    fn a_word_does_not_match_a_longer_one() {
        assert!(!matches("claude", "echo claudette"));
        assert!(!matches("top", "htop"));
    }

    #[test]
    fn a_glob_spans_the_whole_line() {
        assert!(matches("nvim *", "nvim /home/u/.tmux.conf"));
        assert!(matches("cargo*", "cargo watch -x test"));
        assert!(!matches("nvim *", "vim /etc/hosts"));
    }

    #[test]
    fn a_glob_matches_the_empty_run_and_one_char() {
        assert!(glob("nvim*", "nvim"));
        assert!(glob("h?top", "hgtop"));
        assert!(!glob("h?top", "htop"));
    }

    #[test]
    fn a_glob_with_many_stars_still_terminates() {
        // Naive recursion is exponential here; the iterative form is not.
        let long = "a".repeat(64);
        assert!(!glob("*a*a*a*a*a*a*b", &long));
    }

    #[test]
    fn the_default_runs_claude_and_nothing_else() {
        let r = Restore::default();
        assert!(r.should_run("ccproxy claude --model default"));
        assert!(!r.should_run("nvim ~/.tmux.conf"));
        assert!(!r.should_run("kmd dashboard"));
    }

    #[test]
    fn an_empty_list_turns_autorun_off() {
        // Distinct from an absent key, which keeps the defaults — see below.
        assert!(!restore(&[]).should_run("ccproxy claude"));
    }

    #[test]
    fn an_absent_section_keeps_the_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(parse("[restore]\n").unwrap(), Config::default());
    }

    #[test]
    fn an_explicit_empty_list_survives_parsing() {
        let c = parse("[restore]\nautorun = []\n").unwrap();
        assert!(c.restore.autorun.is_empty(), "not replaced by the defaults");
    }

    #[test]
    fn parses_a_list() {
        let c = parse("[restore]\nautorun = [\"claude\", \"nvim *\"]\n").unwrap();
        assert!(c.restore.should_run("nvim /etc/hosts"));
        assert!(c.restore.should_run("ccproxy claude"));
        assert!(!c.restore.should_run("htop"));
    }

    #[test]
    fn a_typo_is_an_error_rather_than_a_silent_default() {
        // `autoruns` would otherwise parse as an unknown key and leave autorun
        // at its default, which is the one outcome the user cannot see.
        assert!(parse("[restore]\nautoruns = [\"claude\"]\n").is_err());
        assert!(parse("[restoer]\n").is_err());
        assert!(parse("[restore]\nautorun = \"claude\"\n").is_err());
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let c = load_from(std::path::Path::new("/nonexistent/tmc/config.toml")).unwrap();
        assert_eq!(c, Config::default());
    }
}
