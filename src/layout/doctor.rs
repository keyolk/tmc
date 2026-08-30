//! Checking a restore point before it is needed.
//!
//! A snapshot only reveals its problems when you try to use it, which is
//! always the worst moment. `shell_only` is what makes this possible: without
//! it an empty command is ambiguous — 16 of 56 panes here are legitimately
//! just a shell, and files written by tmux.sh cannot tell that apart from a
//! command it failed to read.

use crate::layout::Session;

/// How serious a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing, restores fine.
    Note,
    /// The restore will be incomplete or may do the wrong thing.
    Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    /// `projects:3.2`, or `projects` for a whole-session finding.
    pub where_: String,
    pub what: String,
}

/// Examine every session in a restore point.
pub fn check(sessions: &[Session]) -> Vec<Finding> {
    let mut findings = Vec::new();

    if sessions.is_empty() {
        findings.push(Finding {
            severity: Severity::Warn,
            where_: "point".into(),
            what: "holds no sessions".into(),
        });
        return findings;
    }

    for session in sessions {
        if session.windows.is_empty() {
            findings.push(Finding {
                severity: Severity::Warn,
                where_: session.session.clone(),
                what: "no windows saved".into(),
            });
            continue;
        }

        for window in &session.windows {
            let at = format!("{}:{}", session.session, window.index);

            if window.panes.is_empty() {
                findings.push(Finding {
                    severity: Severity::Warn,
                    where_: at.clone(),
                    what: "no panes saved; restore would create an empty window".into(),
                });
                continue;
            }
            if window.layout.is_empty() {
                findings.push(Finding {
                    severity: Severity::Note,
                    where_: at.clone(),
                    what: "no layout string; panes will be tiled".into(),
                });
            }

            for pane in &window.panes {
                let at = format!("{}.{}", at, pane.index);

                // The distinction tmux.sh could not record.
                if pane.command.is_empty() {
                    match pane.shell_only {
                        Some(true) => {} // correct: the pane really was a shell
                        Some(false) => findings.push(Finding {
                            severity: Severity::Warn,
                            where_: at.clone(),
                            what: "a command was running but none was saved".into(),
                        }),
                        None => findings.push(Finding {
                            severity: Severity::Note,
                            where_: at.clone(),
                            what: "empty command, and no way to tell whether that is right \
                                   (saved by tmux.sh)"
                                .into(),
                        }),
                    }
                }

                if pane.path.is_empty() {
                    findings.push(Finding {
                        severity: Severity::Warn,
                        where_: at.clone(),
                        what: "no working directory; the pane would open in $HOME".into(),
                    });
                }

                // An ambiguous id resumes *a* session in that window, but not
                // necessarily this pane's. Worth restoring, worth knowing.
                if pane.claude_session.is_some()
                    && pane.session_confidence.as_deref() == Some("ambiguous")
                {
                    findings.push(Finding {
                        severity: Severity::Note,
                        where_: at.clone(),
                        what: "claude session is ambiguous — several ran in this window".into(),
                    });
                }
            }
        }
    }

    findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.where_.cmp(&b.where_)));
    findings
}

/// A one-line verdict for the point.
pub fn summarize(findings: &[Finding]) -> String {
    let warns = findings
        .iter()
        .filter(|f| f.severity == Severity::Warn)
        .count();
    let notes = findings.len() - warns;
    match (warns, notes) {
        (0, 0) => "healthy".into(),
        (0, n) => format!("{n} note(s)"),
        (w, 0) => format!("{w} warning(s)"),
        (w, n) => format!("{w} warning(s), {n} note(s)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Pane, Window};

    fn pane(index: u32, command: &str, shell_only: Option<bool>) -> Pane {
        Pane {
            index,
            path: "/tmp".into(),
            command: command.into(),
            claude_session: None,
            pane_id: None,
            shell_only,
            session_confidence: None,
        }
    }

    fn session(panes: Vec<Pane>) -> Vec<Session> {
        vec![Session {
            session: "projects".into(),
            saved_at: "2026-08-30T00:00:00Z".into(),
            label: None,
            windows: vec![Window {
                index: 1,
                name: "alpha".into(),
                layout: "abcd,80x24,0,0".into(),
                panes,
                chrome: None,
            }],
        }]
    }

    #[test]
    fn a_deliberately_empty_pane_is_not_a_problem() {
        // 16 of 56 panes here hold nothing but a shell. Flagging those would
        // bury the real findings.
        let findings = check(&session(vec![pane(1, "", Some(true))]));
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(summarize(&findings), "healthy");
    }

    #[test]
    fn a_command_that_was_running_but_not_saved_is_a_warning() {
        let findings = check(&session(vec![pane(1, "", Some(false))]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert_eq!(findings[0].where_, "projects:1.1");
    }

    #[test]
    fn a_tmux_sh_file_reports_the_ambiguity_rather_than_guessing() {
        // No shell_only field: the empty command may be correct or may be a
        // failure, and the file cannot say which.
        let findings = check(&session(vec![pane(1, "", None)]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Note);
        assert!(findings[0].what.contains("tmux.sh"));
    }

    #[test]
    fn a_missing_working_directory_is_a_warning() {
        let mut p = pane(1, "ghx", Some(false));
        p.path = String::new();
        let findings = check(&session(vec![p]));
        assert!(findings.iter().any(|f| f.what.contains("$HOME")));
    }

    #[test]
    fn an_ambiguous_claude_session_is_flagged_but_not_fatal() {
        let mut p = pane(1, "ccproxy claude", Some(false));
        p.claude_session = Some("abc-123".into());
        p.session_confidence = Some("ambiguous".into());
        let findings = check(&session(vec![p]));

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Note);
        assert!(findings[0].what.contains("ambiguous"));
    }

    #[test]
    fn an_exact_claude_session_is_silent() {
        let mut p = pane(1, "ccproxy claude", Some(false));
        p.claude_session = Some("abc-123".into());
        p.session_confidence = Some("exact".into());
        assert!(check(&session(vec![p])).is_empty());
    }

    #[test]
    fn a_window_with_no_panes_would_restore_empty() {
        let sessions = vec![Session {
            session: "projects".into(),
            saved_at: "2026-08-30T00:00:00Z".into(),
            label: None,
            windows: vec![Window {
                index: 1,
                name: "alpha".into(),
                layout: String::new(),
                panes: Vec::new(),
                chrome: None,
            }],
        }];
        let findings = check(&sessions);
        assert_eq!(
            findings.len(),
            1,
            "the empty window supersedes the layout note"
        );
        assert_eq!(findings[0].severity, Severity::Warn);
    }

    #[test]
    fn warnings_sort_above_notes() {
        let findings = check(&session(vec![
            pane(1, "", None),        // note
            pane(2, "", Some(false)), // warn
        ]));
        assert_eq!(findings[0].severity, Severity::Warn);
        assert_eq!(findings[1].severity, Severity::Note);
        assert_eq!(summarize(&findings), "1 warning(s), 1 note(s)");
    }

    #[test]
    fn an_empty_point_is_itself_the_finding() {
        let findings = check(&[]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warn);
    }
}
