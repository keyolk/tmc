//! Recovering the command a pane was running, for replay on restore.
//!
//! `pane_current_command` is not enough: it reports the shell whenever a
//! long-running process sits behind one (`fish` → `ccproxy claude …`), and it
//! carries no arguments. The full command line has to come from the process
//! tree.

use super::proc::Tree;

/// Shells to walk *through* when looking for the real foreground process.
/// Both the plain and login-dash spellings appear in `ps`.
const SHELLS: [&str; 12] = [
    "fish", "bash", "zsh", "sh", "dash", "ksh", "-fish", "-bash", "-zsh", "-sh", "-dash", "-ksh",
];

fn is_shell(comm: &str) -> bool {
    SHELLS.contains(&comm)
}

/// What a pane was running, as a command line to prefill on restore.
///
/// Returns `None` when the pane held nothing but a shell — the correct thing
/// to restore then is an empty prompt, and on the reference machine that is 16
/// of 56 panes. The caller records this as `shell_only` so `doctor` can tell
/// it apart from a command it failed to read.
pub fn for_pane(
    pane_pid: u32,
    tree: &Tree,
    args_of: impl Fn(u32) -> Option<String>,
) -> Option<String> {
    let pid = tree.find_descendant(pane_pid, |c| !is_shell(c))?;
    let raw = args_of(pid)?;
    let cmd = normalize(raw.trim());
    if cmd.is_empty() { None } else { Some(cmd) }
}

/// Tidy a raw `ps` command line into something worth typing back.
fn normalize(raw: &str) -> String {
    let cmd = collapse_leading_path(raw);
    strip_stale_resume(&cmd)
}

/// `/opt/homebrew/bin/nvim ~/.tmux.conf` → `nvim ~/.tmux.conf`.
///
/// Only the program word is touched, and only when it is an absolute path:
/// what the user actually types is the basename, and the absolute form is an
/// artifact of how the shell resolved it. Arguments are left alone — a path
/// there is the user's own.
fn collapse_leading_path(raw: &str) -> String {
    let Some(first) = raw.split(' ').next() else {
        return raw.to_string();
    };
    if !first.starts_with('/') {
        return raw.to_string();
    }
    let Some(base) = first.rsplit('/').next().filter(|b| !b.is_empty()) else {
        return raw.to_string();
    };
    format!("{base}{}", &raw[first.len()..])
}

/// Drop a `--resume <id>` (and `--continue`) left over from a previous
/// restore.
///
/// The id in a running process's command line names the session that pane was
/// restored *into last time*, not the one it is in now. Keeping it walked the
/// workspace one generation backwards on every save/restore cycle — a bug
/// `tmux.sh` hit and fixed the same way. The caller re-attaches the current
/// session id from tmux, which is read live and is therefore the truth.
fn strip_stale_resume(cmd: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skip_next = false;
    for tok in cmd.split(' ') {
        if skip_next {
            skip_next = false;
            continue;
        }
        match tok {
            "--resume" | "-r" => skip_next = true,
            "--continue" | "-c" => {}
            t if t.starts_with("--resume=") => {}
            t => out.push(t),
        }
    }
    out.join(" ").trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::proc::Tree;

    /// pane 100 runs a shell with `ccproxy` under it; pane 400 is a bare shell.
    fn tree() -> Tree {
        Tree::parse_for_test(
            "1 0 launchd\n100 1 fish\n200 100 ccproxy\n300 200 claude\n400 1 fish\n",
        )
    }

    #[test]
    fn finds_the_command_behind_a_shell() {
        let cmd = for_pane(100, &tree(), |pid| {
            assert_eq!(pid, 200, "the shallowest non-shell wins");
            Some("ccproxy claude --model default".into())
        });
        assert_eq!(cmd.as_deref(), Some("ccproxy claude --model default"));
    }

    #[test]
    fn a_bare_shell_pane_has_no_command() {
        // Not a failure: restoring an empty prompt is correct here.
        assert_eq!(for_pane(400, &tree(), |_| Some("fish".into())), None);
    }

    #[test]
    fn collapses_an_absolute_program_path() {
        assert_eq!(
            normalize("/opt/homebrew/bin/nvim /home/u/.tmux.conf"),
            "nvim /home/u/.tmux.conf",
            "the program word folds; a path in the arguments does not",
        );
    }

    #[test]
    fn leaves_a_relative_program_alone() {
        assert_eq!(normalize("kmd dashboard"), "kmd dashboard");
        assert_eq!(normalize("ghx"), "ghx");
    }

    #[test]
    fn drops_a_stale_resume_id() {
        // Observed verbatim on the reference machine.
        assert_eq!(
            normalize(
                "ccproxy claude --intercept=mitm -- --model default \
                 --resume cf997492-ca4f-4fc7-9236-563d78b7149d"
            ),
            "ccproxy claude --intercept=mitm -- --model default",
        );
    }

    #[test]
    fn drops_the_other_resume_spellings() {
        assert_eq!(normalize("claude --resume=abc -c"), "claude");
        assert_eq!(normalize("claude -r abc --continue"), "claude");
    }

    #[test]
    fn keeps_a_flag_that_merely_looks_similar() {
        assert_eq!(
            normalize("cargo run --release --continue-on-error"),
            "cargo run --release --continue-on-error",
        );
    }
}
