//! Recreating tmux windows and panes from a saved layout.
//!
//! The hard-won details from `tmux.sh` are preserved deliberately, because
//! each one was a bug once:
//!
//! - **Real pane indices.** With `pane-base-index 1` set, panes start at 1, so
//!   targeting `<win>.0` fails and the command is silently never typed. The
//!   live indices are read back rather than assumed.
//! - **Incremental re-tiling.** Past a handful of panes `split-window` runs out
//!   of cells; re-tiling between splits keeps room, and without it the trailing
//!   panes are dropped.
//! - **Layout only when the count matches.** `select-layout` requires the live
//!   pane count to equal the layout string's, and applying a mismatched one
//!   errors out. A `tiled` fallback beats a failed restore.
//! - **Prefill, don't execute — unless asked.** Commands are typed into the
//!   prompt and left there, because restoring a workspace should not start 30
//!   processes unasked. The exception is per-command and the user's:
//!   `config::Restore::autorun` names the patterns worth running, and a
//!   `claude` pane that has to be started by hand is a pane you have not
//!   actually recovered yet.

use std::time::Duration;

use anyhow::Result;

use crate::collect::cmd;
use crate::config;
use crate::layout::{Session, Window};

/// The tmux operations a restore performs.
///
/// Injected rather than called directly so the sequencing — which is where the
/// bugs live — can be tested without a tmux server. A restore mutates the
/// user's workspace; it is not something to verify only by running it.
pub trait Tmux {
    fn session_exists(&self, name: &str) -> bool;
    /// Returns the new window's index.
    fn new_session(&mut self, session: &str, window: &str, path: &str) -> Result<String>;
    fn new_window(&mut self, session: &str, name: &str, path: &str) -> Result<String>;
    fn split_window(&mut self, target: &str, path: &str) -> Result<()>;
    fn select_layout(&mut self, target: &str, layout: &str) -> Result<()>;
    /// Live pane indices, in on-screen order.
    fn pane_indices(&self, target: &str) -> Vec<String>;
    /// Type text at the prompt without executing it.
    fn send_literal(&mut self, target: &str, text: &str);
    /// Submit whatever is at the prompt.
    fn send_enter(&mut self, target: &str);
    /// Block until the pane's foreground process is a shell.
    fn wait_for_shell(&self, target: &str);
    /// Close a session and everything in it.
    fn kill_session(&mut self, name: &str) -> Result<()>;
}

/// The real thing.
pub struct Server;

impl Tmux for Server {
    fn session_exists(&self, name: &str) -> bool {
        // `=name` is an exact match; without it tmux prefix-matches and a
        // session named `proj` would answer for `projects`.
        cmd::run(
            "tmux",
            &["has-session", "-t", &format!("={name}")],
            cmd::FAST,
        )
        .is_ok()
    }

    fn new_session(&mut self, session: &str, window: &str, path: &str) -> Result<String> {
        let out = cmd::run(
            "tmux",
            &[
                "new-session",
                "-d",
                "-s",
                session,
                "-n",
                window,
                "-c",
                path,
                "-P",
                "-F",
                "#{window_index}",
            ],
            cmd::FAST,
        )?;
        Ok(out.trim().to_string())
    }

    fn new_window(&mut self, session: &str, name: &str, path: &str) -> Result<String> {
        let out = cmd::run(
            "tmux",
            &[
                "new-window",
                "-t",
                &format!("{session}:"),
                "-n",
                name,
                "-c",
                path,
                "-P",
                "-F",
                "#{window_index}",
            ],
            cmd::FAST,
        )?;
        Ok(out.trim().to_string())
    }

    fn split_window(&mut self, target: &str, path: &str) -> Result<()> {
        cmd::run(
            "tmux",
            &["split-window", "-t", target, "-c", path],
            cmd::FAST,
        )?;
        Ok(())
    }

    fn select_layout(&mut self, target: &str, layout: &str) -> Result<()> {
        cmd::run("tmux", &["select-layout", "-t", target, layout], cmd::FAST)?;
        Ok(())
    }

    fn pane_indices(&self, target: &str) -> Vec<String> {
        cmd::run(
            "tmux",
            &["list-panes", "-t", target, "-F", "#{pane_index}"],
            cmd::FAST,
        )
        .map(|out| out.lines().map(str::to_string).collect())
        .unwrap_or_default()
    }

    fn send_literal(&mut self, target: &str, text: &str) {
        // `-l` types the text literally and leaves it at the prompt.
        let _ = cmd::run(
            "tmux",
            &["send-keys", "-l", "-t", target, "--", text],
            cmd::FAST,
        );
    }

    fn send_enter(&mut self, target: &str) {
        // Separate from `send_literal` so the text is committed as one literal
        // chunk first. Sending `Enter` inside the same `-l` call would type the
        // characters "Enter"; sending the command without `-l` would let tmux
        // interpret words in it as key names.
        let _ = cmd::run("tmux", &["send-keys", "-t", target, "Enter"], cmd::FAST);
    }

    /// Wait until the pane's foreground process is a shell.
    ///
    /// A flat sleep is too short when a heavy rc file is still rendering: the
    /// text then lands before the prompt exists and the pane looks empty even
    /// though the command was sent.
    fn kill_session(&mut self, name: &str) -> Result<()> {
        cmd::run(
            "tmux",
            &["kill-session", "-t", &format!("={name}")],
            cmd::FAST,
        )?;
        Ok(())
    }

    fn wait_for_shell(&self, target: &str) {
        const SHELLS: [&str; 6] = ["fish", "bash", "zsh", "sh", "dash", "ksh"];
        for _ in 0..40 {
            let current = cmd::run(
                "tmux",
                &[
                    "display-message",
                    "-p",
                    "-t",
                    target,
                    "#{pane_current_command}",
                ],
                cmd::FAST,
            )
            .unwrap_or_default();
            if SHELLS.contains(&current.trim()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// What a restore did, or would do.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub windows: usize,
    pub panes: usize,
    /// Panes that could not be created — the display ran out of room.
    pub missing_panes: usize,
    /// Typed at the prompt and left for the user to submit.
    pub commands_prefilled: usize,
    /// Typed and submitted, because a pattern in `autorun` matched.
    pub commands_run: usize,
    pub notes: Vec<String>,
}

/// Which parts of a saved session to bring back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection<'a> {
    /// Restore only these window indices; `None` restores all of them.
    pub windows: Option<&'a [u32]>,
}

impl Selection<'_> {
    pub fn all() -> Self {
        Self { windows: None }
    }

    fn includes(&self, index: u32) -> bool {
        self.windows.is_none_or(|w| w.contains(&index))
    }
}

/// Recreate a saved session.
///
/// A session that is already running is skipped: appending its windows to a
/// live one silently doubles the workspace, and for a recovery tool that is
/// the safe default. `force` closes it first instead — irreversible, so the
/// caller shows what would be lost before offering it.
pub fn session<T: Tmux>(
    tmux: &mut T,
    saved: &Session,
    select: Selection,
    autorun: &config::Restore,
    dry_run: bool,
    force: bool,
) -> Result<Report> {
    let mut report = Report::default();

    // A dry run still reports the clash, but goes on to describe what a real
    // restore would build. Stopping here would make the preview useless in the
    // common case — the user is usually looking at a point for a session that
    // is still open, deciding whether to close it.
    let clash = tmux.session_exists(&saved.session);
    if clash {
        if force {
            report
                .notes
                .push(format!("closed the running '{}'", saved.session));
            if !dry_run {
                // Everything in it goes. The caller is expected to have shown
                // the user what that costs — see `main::load`.
                tmux.kill_session(&saved.session)?;
            }
        } else {
            report.notes.push(format!(
                "session '{}' is already running — close it, or pass --force{}",
                saved.session,
                if dry_run { "" } else { "; skipped" },
            ));
            if !dry_run {
                return Ok(report);
            }
        }
    }

    let windows: Vec<&Window> = saved
        .windows
        .iter()
        .filter(|w| select.includes(w.index))
        .collect();
    if windows.is_empty() {
        report.notes.push("nothing selected".into());
        return Ok(report);
    }

    // What to type where, deferred until every window exists so the shells
    // have had the longest possible head start.
    let mut pending: Vec<Prefill> = Vec::new();

    for (i, window) in windows.iter().enumerate() {
        let first_path = window.panes.first().map(|p| p.path.as_str()).unwrap_or("");
        let target = if dry_run {
            report.windows += 1;
            report.panes += window.panes.len();
            continue;
        } else if i == 0 {
            tmux.new_session(&saved.session, &window.name, first_path)?
        } else {
            tmux.new_window(&saved.session, &window.name, first_path)?
        };

        report.windows += 1;
        let created = add_panes(tmux, &saved.session, &target, window);
        report.panes += created;
        report.missing_panes += window.panes.len() - created;

        apply_layout(tmux, &saved.session, &target, window, created, &mut report);

        // Map saved position -> live pane index. They differ under
        // `pane-base-index 1`, and guessing wrong types into nothing.
        let live = tmux.pane_indices(&format!("{}:{}", saved.session, target));
        for (pos, pane) in window.panes.iter().enumerate() {
            if pane.command.is_empty() {
                continue;
            }
            let Some(idx) = live.get(pos) else { continue };
            let command = resume_command(pane);
            pending.push(Prefill {
                target: format!("{}:{}.{}", saved.session, target, idx),
                run: autorun.should_run(&command),
                command,
            });
        }
    }

    if dry_run {
        // No panes were created, so there is nothing to read live indices
        // from; the counts come from the saved panes instead. A preview that
        // did not distinguish the two would hide the only irreversible part
        // of a restore — which commands start on their own.
        for pane in windows.iter().flat_map(|w| &w.panes) {
            if pane.command.is_empty() {
                continue;
            }
            if autorun.should_run(&resume_command(pane)) {
                report.commands_run += 1;
            } else {
                report.commands_prefilled += 1;
            }
        }
        return Ok(report);
    }

    for p in &pending {
        tmux.wait_for_shell(&p.target);
        tmux.send_literal(&p.target, &p.command);
        if p.run {
            tmux.send_enter(&p.target);
            report.commands_run += 1;
        } else {
            report.commands_prefilled += 1;
        }
    }

    Ok(report)
}

/// One command waiting on its pane's shell.
struct Prefill {
    target: String,
    command: String,
    /// Submit it, rather than leaving it at the prompt.
    run: bool,
}

/// Attach the pane's own Claude session to its command.
///
/// The saved command already had any stale `--resume` stripped at save time
/// (see `collect::command`), so this adds the id that was live *then*, which is
/// the session this pane actually held. An ambiguous id is still worth
/// attaching — the alternative is starting a fresh conversation and losing the
/// thread — but the UI shows the ambiguity before the user commits.
fn resume_command(pane: &crate::layout::Pane) -> String {
    let Some(id) = pane.claude_session.as_deref() else {
        return pane.command.clone();
    };
    if !mentions_claude(&pane.command) {
        return pane.command.clone();
    }
    // Appended, not spliced after the `claude` word. Inserting mid-command
    // has to reason about `--`: in `ccproxy claude --intercept=mitm --
    // --dangerously-skip-permissions`, everything after `--` belongs to
    // claude, and placing a flag before it hands it to the wrapper instead.
    // The end of the line is unambiguous in both forms.
    format!("{} --resume {id}", pane.command)
}

fn mentions_claude(command: &str) -> bool {
    command.split(' ').any(|t| t == "claude")
}

/// Split out the remaining panes, returning how many of the saved panes exist.
fn add_panes<T: Tmux>(tmux: &mut T, session: &str, window: &str, saved: &Window) -> usize {
    let target = format!("{session}:{window}");
    let mut created = 1; // the window came with one pane

    for pane in saved.panes.iter().skip(1) {
        if tmux.split_window(&target, &pane.path).is_err() {
            // Out of cells. Re-tile to maximize free space and try once more.
            let _ = tmux.select_layout(&target, "tiled");
            if tmux.split_window(&target, &pane.path).is_err() {
                continue; // the display genuinely cannot hold this pane
            }
        }
        created += 1;
        // Re-tile after each split so the next one sees even cells rather than
        // the newest pane being a sliver.
        let _ = tmux.select_layout(&target, "tiled");
    }
    created
}

fn apply_layout<T: Tmux>(
    tmux: &mut T,
    session: &str,
    window: &str,
    saved: &Window,
    created: usize,
    report: &mut Report,
) {
    let target = format!("{session}:{window}");
    if created != saved.panes.len() {
        report.notes.push(format!(
            "window '{}' restored with {}/{} panes (display too small); using tiled",
            saved.name,
            created,
            saved.panes.len(),
        ));
        let _ = tmux.select_layout(&target, "tiled");
        return;
    }
    if tmux.select_layout(&target, &saved.layout).is_err() {
        // The saved geometry does not fit this display. Even panes beat none.
        let _ = tmux.select_layout(&target, "tiled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Pane;

    fn pane(command: &str, claude: Option<&str>) -> Pane {
        Pane {
            index: 1,
            path: "/tmp".into(),
            command: command.into(),
            claude_session: claude.map(str::to_string),
            pane_id: None,
            shell_only: Some(command.is_empty()),
            session_confidence: None,
        }
    }

    #[test]
    fn appends_the_session_id() {
        let p = pane("ccproxy claude --model default", Some("abc-123"));
        assert_eq!(
            resume_command(&p),
            "ccproxy claude --model default --resume abc-123"
        );
    }

    #[test]
    fn appending_survives_a_separator_in_an_older_snapshot() {
        // `save` strips a bare `--` now, but snapshots written before that do
        // not, and those still have to restore. Appending works either way;
        // splicing after the `claude` word put the id before the separator,
        // where ccproxy took it and claude started a fresh conversation.
        let p = pane(
            "ccproxy claude --intercept=mitm -- --dangerously-skip-permissions",
            Some("abc-123"),
        );
        assert_eq!(
            resume_command(&p),
            "ccproxy claude --intercept=mitm -- --dangerously-skip-permissions --resume abc-123",
        );
    }

    #[test]
    fn leaves_a_non_claude_command_alone() {
        let p = pane("kmd dashboard", Some("abc-123"));
        assert_eq!(resume_command(&p), "kmd dashboard");
    }

    #[test]
    fn leaves_a_command_without_a_session_alone() {
        let p = pane("ccproxy claude --model default", None);
        assert_eq!(resume_command(&p), "ccproxy claude --model default");
    }

    #[test]
    fn does_not_match_a_word_that_merely_contains_claude() {
        let p = pane("echo claudette", Some("abc-123"));
        assert_eq!(resume_command(&p), "echo claudette");
    }

    /// A tmux that records what it was asked to do.
    ///
    /// `base_index` models `pane-base-index`, the setting that made tmux.sh
    /// type commands into nothing when it assumed panes start at 0.
    struct Fake {
        live_sessions: Vec<String>,
        base_index: u32,
        /// Splits refused after this many panes exist in a window, modelling a
        /// display that runs out of cells.
        pane_capacity: usize,
        panes: std::collections::BTreeMap<String, usize>,
        pub calls: Vec<String>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                live_sessions: Vec::new(),
                base_index: 0,
                pane_capacity: usize::MAX,
                panes: Default::default(),
                calls: Vec::new(),
            }
        }
    }

    impl Tmux for Fake {
        fn session_exists(&self, name: &str) -> bool {
            self.live_sessions.iter().any(|s| s == name)
        }
        fn new_session(&mut self, session: &str, window: &str, path: &str) -> Result<String> {
            self.calls
                .push(format!("new-session {session} {window} {path}"));
            self.panes.insert(format!("{session}:0"), 1);
            Ok("0".into())
        }
        fn new_window(&mut self, session: &str, name: &str, path: &str) -> Result<String> {
            let idx = self.panes.len();
            self.calls
                .push(format!("new-window {session} {name} {path}"));
            self.panes.insert(format!("{session}:{idx}"), 1);
            Ok(idx.to_string())
        }
        fn split_window(&mut self, target: &str, path: &str) -> Result<()> {
            let count = self.panes.entry(target.to_string()).or_insert(1);
            if *count >= self.pane_capacity {
                self.calls.push(format!("split-window {target} REFUSED"));
                anyhow::bail!("no space for new pane");
            }
            *count += 1;
            self.calls.push(format!("split-window {target} {path}"));
            Ok(())
        }
        fn select_layout(&mut self, target: &str, layout: &str) -> Result<()> {
            self.calls.push(format!("select-layout {target} {layout}"));
            Ok(())
        }
        fn pane_indices(&self, target: &str) -> Vec<String> {
            let n = *self.panes.get(target).unwrap_or(&1);
            (0..n)
                .map(|i| (i as u32 + self.base_index).to_string())
                .collect()
        }
        fn send_literal(&mut self, target: &str, text: &str) {
            self.calls.push(format!("send {target} [{text}]"));
        }
        fn send_enter(&mut self, target: &str) {
            self.calls.push(format!("enter {target}"));
        }
        fn wait_for_shell(&self, _target: &str) {}
        fn kill_session(&mut self, name: &str) -> Result<()> {
            self.calls.push(format!("kill-session {name}"));
            self.live_sessions.retain(|s| s != name);
            Ok(())
        }
    }

    /// Autorun off, which is what most of these tests want: they are about
    /// sequencing, and a command that submits itself adds noise to `calls`.
    fn no_autorun() -> config::Restore {
        config::Restore {
            autorun: Vec::new(),
        }
    }

    fn autorun(patterns: &[&str]) -> config::Restore {
        config::Restore {
            autorun: patterns.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn saved_session(windows: Vec<Window>) -> Session {
        Session {
            session: "projects".into(),
            saved_at: "2026-08-30T00:00:00Z".into(),
            label: None,
            windows,
        }
    }

    fn window(index: u32, name: &str, panes: Vec<Pane>) -> Window {
        Window {
            index,
            name: name.into(),
            layout: "abcd,80x24,0,0".into(),
            panes,
            chrome: None,
        }
    }

    #[test]
    fn creates_the_first_window_with_new_session_and_the_rest_with_new_window() {
        let mut tmux = Fake::new();
        let saved = saved_session(vec![
            window(1, "alpha", vec![pane("", None)]),
            window(2, "beta", vec![pane("", None)]),
        ]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.windows, 2);
        assert!(tmux.calls[0].starts_with("new-session projects alpha"));
        assert!(
            tmux.calls
                .iter()
                .any(|c| c.starts_with("new-window projects beta"))
        );
    }

    #[test]
    fn types_into_the_real_pane_index_under_pane_base_index_1() {
        // The tmux.sh bug: assuming panes start at 0 targeted `<win>.0`, which
        // does not exist, and the command was silently never typed.
        let mut tmux = Fake::new();
        tmux.base_index = 1;
        let saved = saved_session(vec![window(1, "alpha", vec![pane("ghx", None)])]);

        session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        assert!(
            tmux.calls.iter().any(|c| c == "send projects:0.1 [ghx]"),
            "should target pane 1, not 0; calls: {:?}",
            tmux.calls,
        );
    }

    #[test]
    fn retiles_between_splits_so_later_panes_still_fit() {
        let mut tmux = Fake::new();
        let panes: Vec<Pane> = (0..4).map(|_| pane("", None)).collect();
        let saved = saved_session(vec![window(1, "alpha", panes)]);

        session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        let tiled = tmux.calls.iter().filter(|c| c.ends_with("tiled")).count();
        assert!(tiled >= 3, "one re-tile per split; calls: {:?}", tmux.calls);
    }

    #[test]
    fn falls_back_to_tiled_when_panes_are_missing() {
        // select-layout requires the live pane count to match the layout
        // string. Applying a mismatched one errors; tiled beats a failed
        // restore.
        let mut tmux = Fake::new();
        tmux.pane_capacity = 2;
        let panes: Vec<Pane> = (0..4).map(|_| pane("", None)).collect();
        let saved = saved_session(vec![window(1, "alpha", panes)]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.panes, 2);
        assert_eq!(report.missing_panes, 2);
        assert!(report.notes.iter().any(|n| n.contains("2/4 panes")));
        assert!(
            !tmux.calls.iter().any(|c| c.contains("abcd,80x24")),
            "the saved layout must not be applied to a short window",
        );
    }

    #[test]
    fn refuses_a_session_that_is_already_running() {
        let mut tmux = Fake::new();
        tmux.live_sessions.push("projects".into());
        let saved = saved_session(vec![window(1, "alpha", vec![pane("", None)])]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        assert_eq!(
            report.windows, 0,
            "nothing may be appended to a live session"
        );
        assert!(tmux.calls.is_empty());
        assert!(report.notes[0].contains("already running"));
    }

    #[test]
    fn force_closes_the_running_session_before_rebuilding() {
        let mut tmux = Fake::new();
        tmux.live_sessions.push("projects".into());
        let saved = saved_session(vec![window(1, "alpha", vec![pane("ghx", None)])]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            true,
        )
        .unwrap();

        assert_eq!(report.windows, 1, "the rebuild happened");
        assert_eq!(
            tmux.calls.first().map(String::as_str),
            Some("kill-session projects"),
            "and the close came first; calls: {:?}",
            tmux.calls,
        );
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("closed the running"))
        );
    }

    #[test]
    fn without_force_a_live_session_is_left_alone() {
        let mut tmux = Fake::new();
        tmux.live_sessions.push("projects".into());
        let saved = saved_session(vec![window(1, "alpha", vec![pane("ghx", None)])]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.windows, 0);
        assert!(tmux.calls.is_empty(), "nothing was touched");
        assert!(
            report.notes[0].contains("--force"),
            "and the way forward is named: {}",
            report.notes[0],
        );
    }

    #[test]
    fn a_forced_dry_run_still_kills_nothing() {
        let mut tmux = Fake::new();
        tmux.live_sessions.push("projects".into());
        let saved = saved_session(vec![window(1, "alpha", vec![pane("ghx", None)])]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            true,
            true,
        )
        .unwrap();

        assert_eq!(report.windows, 1, "still previews the rebuild");
        assert!(tmux.calls.is_empty(), "a dry run mutates nothing");
    }

    #[test]
    fn force_on_a_session_that_is_not_running_is_a_plain_restore() {
        let mut tmux = Fake::new();
        let saved = saved_session(vec![window(1, "alpha", vec![pane("ghx", None)])]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            true,
        )
        .unwrap();

        assert_eq!(report.windows, 1);
        assert!(
            !tmux.calls.iter().any(|c| c.starts_with("kill-session")),
            "nothing to close: {:?}",
            tmux.calls,
        );
    }

    #[test]
    fn a_dry_run_previews_a_live_session_without_touching_it() {
        // The common case: the user is looking at a point for a session that
        // is still open, deciding whether to close it. Refusing to preview
        // would make dry-run useless exactly when it is wanted.
        let mut tmux = Fake::new();
        tmux.live_sessions.push("projects".into());
        let saved = saved_session(vec![window(1, "alpha", vec![pane("ghx", None)])]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            true,
            false,
        )
        .unwrap();

        assert_eq!(report.windows, 1);
        assert_eq!(report.commands_prefilled, 1);
        assert!(tmux.calls.is_empty(), "a dry run must not mutate anything");
    }

    #[test]
    fn restores_only_the_selected_windows() {
        let mut tmux = Fake::new();
        let saved = saved_session(vec![
            window(1, "alpha", vec![pane("", None)]),
            window(2, "beta", vec![pane("", None)]),
            window(3, "gamma", vec![pane("", None)]),
        ]);

        let report = session(
            &mut tmux,
            &saved,
            Selection {
                windows: Some(&[2]),
            },
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.windows, 1);
        assert!(tmux.calls[0].contains("beta"));
        assert!(!tmux.calls.iter().any(|c| c.contains("gamma")));
    }

    #[test]
    fn a_shell_only_pane_gets_no_typed_command() {
        let mut tmux = Fake::new();
        let saved = saved_session(vec![window(
            1,
            "alpha",
            vec![pane("", None), pane("ghx", None)],
        )]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &no_autorun(),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.commands_prefilled, 1, "only the non-empty command");
        assert_eq!(
            tmux.calls.iter().filter(|c| c.starts_with("send ")).count(),
            1
        );
    }

    #[test]
    fn a_matching_command_is_submitted_not_just_typed() {
        let mut tmux = Fake::new();
        let saved = saved_session(vec![window(
            1,
            "alpha",
            vec![pane("ccproxy claude --model default", Some("abc-123"))],
        )]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &autorun(&["claude"]),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.commands_run, 1);
        assert_eq!(report.commands_prefilled, 0, "counted as run, not both");
        let sends: Vec<&String> = tmux
            .calls
            .iter()
            .filter(|c| c.starts_with("send ") || c.starts_with("enter "))
            .collect();
        assert_eq!(
            sends,
            vec![
                "send projects:0.0 [ccproxy claude --model default --resume abc-123]",
                "enter projects:0.0",
            ],
            "the text lands first, then Enter commits it; calls: {:?}",
            tmux.calls,
        );
    }

    #[test]
    fn a_command_that_does_not_match_is_left_at_the_prompt() {
        let mut tmux = Fake::new();
        let saved = saved_session(vec![window(
            1,
            "alpha",
            vec![pane("nvim ~/.tmux.conf", None)],
        )]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &autorun(&["claude"]),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.commands_prefilled, 1);
        assert_eq!(report.commands_run, 0);
        assert!(
            !tmux.calls.iter().any(|c| c.starts_with("enter ")),
            "nothing was submitted: {:?}",
            tmux.calls,
        );
    }

    #[test]
    fn the_pattern_sees_the_resume_flag_that_was_appended() {
        // `--resume <id>` is added after the saved command, so a pattern
        // written against the restored line has to match the final form.
        let mut tmux = Fake::new();
        let saved = saved_session(vec![window(
            1,
            "alpha",
            vec![pane("ccproxy claude", Some("abc-123"))],
        )]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &autorun(&["*--resume*"]),
            false,
            false,
        )
        .unwrap();

        assert_eq!(report.commands_run, 1);
    }

    #[test]
    fn a_dry_run_says_which_commands_would_run_and_starts_none() {
        // The counts are the only warning the user gets before the one part of
        // a restore that cannot be undone by pressing ctrl-c at a prompt.
        let mut tmux = Fake::new();
        let saved = saved_session(vec![window(
            1,
            "alpha",
            vec![pane("ccproxy claude", None), pane("htop", None)],
        )]);

        let report = session(
            &mut tmux,
            &saved,
            Selection::all(),
            &autorun(&["claude"]),
            true,
            false,
        )
        .unwrap();

        assert_eq!(report.commands_run, 1);
        assert_eq!(report.commands_prefilled, 1);
        assert!(tmux.calls.is_empty(), "a dry run mutates nothing");
    }

    #[test]
    fn every_enter_follows_its_own_send() {
        // Submitting into a pane whose text has not landed yet runs a partial
        // line. With two identical commands the ordering is the only thing
        // that says the pairs did not interleave.
        let mut tmux = Fake::new();
        let saved = saved_session(vec![window(
            1,
            "alpha",
            vec![pane("claude", None), pane("claude", None)],
        )]);

        session(
            &mut tmux,
            &saved,
            Selection::all(),
            &autorun(&["claude"]),
            false,
            false,
        )
        .unwrap();

        let seq: Vec<&String> = tmux
            .calls
            .iter()
            .filter(|c| c.starts_with("send ") || c.starts_with("enter "))
            .collect();
        assert_eq!(seq.len(), 4);
        for pair in seq.chunks(2) {
            assert!(pair[0].starts_with("send "), "text first: {pair:?}");
            assert!(pair[1].starts_with("enter "), "then Enter: {pair:?}");
        }
    }

    #[test]
    fn selection_filters_by_window_index() {
        let all = Selection::all();
        assert!(all.includes(1) && all.includes(99));

        let some = Selection {
            windows: Some(&[2, 4]),
        };
        assert!(some.includes(2));
        assert!(!some.includes(3));
    }
}
