//! The terminal loop: poll, redraw only on change, handle keys.

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use super::model::Model;
use crate::collect::{notify, proc, tmux};
use crate::layout::{point, restore, save};

/// How often the world is re-read. tmux exposes no event stream a foreign
/// process can subscribe to, so this polls; `fingerprint` keeps the redraw
/// from happening when nothing moved.
const TICK: Duration = Duration::from_secs(2);

/// What the caller should do once the terminal is restored.
pub enum Outcome {
    Quit,
    /// Switch to this tmux target, e.g. `projects:3`.
    Switch(String),
}

pub fn run() -> Result<Outcome> {
    let points = point::list(&save::layout_dir());
    let mut model = Model::new(points);

    let mut terminal = setup().context("enter alternate screen")?;
    let result = event_loop(&mut terminal, &mut model);
    teardown(&mut terminal)?;
    result?;

    Ok(match model.switch_to {
        Some(target) => Outcome::Switch(target),
        None => Outcome::Quit,
    })
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn teardown(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn event_loop(terminal: &mut Term, model: &mut Model) -> Result<()> {
    reload(model)?;
    let mut last = model.fingerprint();
    let mut needs_redraw = true;
    let mut next_tick = Instant::now() + TICK;

    loop {
        if needs_redraw {
            let now = now_secs();
            terminal.draw(|frame| super::render::draw(frame, model, now))?;
            needs_redraw = false;
        }

        let wait = next_tick.saturating_duration_since(Instant::now());
        if event::poll(wait)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                handle_key(model, key.code, key.modifiers)?;
                if model.quit || model.switch_to.is_some() {
                    return Ok(());
                }
                needs_redraw = true;
            } else {
                needs_redraw = true; // resize
            }
            continue;
        }

        // Tick: re-read the world, but only redraw when the display would
        // differ. On an idle workspace this leaves the app at 0 fps.
        next_tick = Instant::now() + TICK;
        reload(model)?;
        let current = model.fingerprint();
        if current != last {
            last = current;
            needs_redraw = true;
        }
    }
}

fn handle_key(model: &mut Model, code: KeyCode, mods: KeyModifiers) -> Result<()> {
    model.status.clear();
    match code {
        KeyCode::Char('q') | KeyCode::Esc => model.quit = true,
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => model.quit = true,

        KeyCode::Char('j') | KeyCode::Down => model.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => model.move_cursor(-1),
        KeyCode::Char('g') | KeyCode::Home => {
            model.cursor = 0;
            model.move_cursor(0);
        }
        KeyCode::Char('G') | KeyCode::End => {
            model.cursor = model.rows.len().saturating_sub(1);
            model.move_cursor(-1);
        }

        KeyCode::Char(' ') => model.toggle_mark(),
        KeyCode::Char('a') => model.mark_all_changed(),
        KeyCode::Char('c') => model.clear_marks(),

        KeyCode::Char('n') => model.jump_waiting(),

        // p/P walk the restore points; the diff follows immediately.
        KeyCode::Char('p') => {
            if model.cycle_point(1) {
                reload(model)?;
            }
        }
        KeyCode::Char('P') => {
            if model.cycle_point(-1) {
                reload(model)?;
            }
        }

        KeyCode::Char('s') => save_point(model)?,
        KeyCode::Char('r') => restore_marked(model)?,

        KeyCode::Enter => {
            if let Some(w) = model.current_window() {
                if w.gone {
                    // Nothing to switch to; the window only exists in the point.
                    model.status = "that window is gone — press r to restore it".into();
                } else {
                    model.switch_to = Some(w.target());
                }
            }
        }
        // Window-level surgery, replacing `tmux.sh move/join/break`. These
        // act on the window under the cursor, which is what the tree is for —
        // tmux.sh made you pick from a second fzf list first.
        KeyCode::Char('m') => move_window(model)?,
        KeyCode::Char('b') => break_pane(model)?,
        KeyCode::Char('J') => join_pane(model)?,
        KeyCode::Char('x') => kill_window(model)?,

        KeyCode::Char('R') => reload(model)?,
        _ => {}
    }
    Ok(())
}

/// Re-read tmux, the process table, the queue and the selected point.
fn reload(model: &mut Model) -> Result<()> {
    let panes = tmux::panes().unwrap_or_default();
    let tree = proc::Tree::capture_with_args()?;
    let pending = notify::load();
    let saved = match model.current_point() {
        Some(p) => point::read(&p.reference).unwrap_or_default(),
        None => Vec::new(),
    };
    model.refresh(&panes, &saved, &tree, &pending);
    Ok(())
}

fn save_point(model: &mut Model) -> Result<()> {
    let panes = tmux::panes().unwrap_or_default();
    if panes.is_empty() {
        model.status = "no tmux panes to save".into();
        return Ok(());
    }
    let tree = proc::Tree::capture_with_args()?;
    let now = crate::clock::now();
    let sessions = save::snapshot(&panes, &tree, &now.timestamp);
    let dir = save::layout_dir().join(&now.compact);

    for session in &sessions {
        save::write(session, &dir.join(format!("{}.json", session.session)))?;
    }

    // The new point becomes the one being compared against, which is almost
    // always what the user wants next: they saved because this state is worth
    // keeping, so the diff should now read as empty.
    model.points = point::list(&save::layout_dir());
    model.point_index = 0;
    reload(model)?;
    model.status = format!("saved {} session(s) as {}", sessions.len(), now.compact);
    Ok(())
}

fn restore_marked(model: &mut Model) -> Result<()> {
    let Some(p) = model.current_point() else {
        model.status = "no restore point selected".into();
        return Ok(());
    };
    let saved = point::read(&p.reference)?;

    // With nothing marked, restore what is missing — the windows the point has
    // and the live server does not. Restoring everything would mean recreating
    // windows that are already open.
    let targets: Vec<String> = if model.marks.is_empty() {
        model
            .rows
            .iter()
            .filter_map(|r| match r {
                super::model::Row::Window(w) if w.gone => Some(w.target()),
                _ => None,
            })
            .collect()
    } else {
        model.marks.iter().cloned().collect()
    };

    if targets.is_empty() {
        model.status = "nothing to restore — mark a window with space".into();
        return Ok(());
    }

    let mut server = restore::Server;
    let mut windows = 0;
    let mut notes = Vec::new();

    for session in &saved {
        let indices: Vec<u32> = targets
            .iter()
            .filter_map(|t| t.strip_prefix(&format!("{}:", session.session)))
            .filter_map(|i| i.parse().ok())
            .collect();
        if indices.is_empty() {
            continue;
        }
        let report = restore::session(
            &mut server,
            session,
            restore::Selection {
                windows: Some(&indices),
            },
            false,
        )?;
        windows += report.windows;
        notes.extend(report.notes);
    }

    model.marks.clear();
    reload(model)?;
    model.status = if notes.is_empty() {
        format!("restored {windows} window(s)")
    } else {
        notes.join("; ")
    };
    Ok(())
}

/// Move the selected window to the other session.
///
/// With two sessions the destination is unambiguous, which is the whole reason
/// this is one keystroke here and a prompt in tmux.sh. With more, the status
/// line says what to do instead of guessing.
fn move_window(model: &mut Model) -> Result<()> {
    let Some(w) = model.current_window() else {
        return Ok(());
    };
    if w.gone {
        model.status = "that window is not running".into();
        return Ok(());
    }
    let target = w.target();
    let here = w.session.clone();

    let others: Vec<String> = sessions()?.into_iter().filter(|s| *s != here).collect();
    match others.as_slice() {
        [] => model.status = "no other session to move to".into(),
        [dest] => {
            crate::collect::cmd::run(
                "tmux",
                &["move-window", "-s", &target, "-t", &format!("{dest}:")],
                crate::collect::cmd::FAST,
            )?;
            // move-window leaves a hole; tmux only renumbers on close.
            let _ = crate::collect::cmd::run(
                "tmux",
                &["move-window", "-r", "-t", &here],
                crate::collect::cmd::FAST,
            );
            reload(model)?;
            model.status = format!("moved {target} to {dest}");
        }
        many => {
            model.status = format!("several destinations ({}); use tmux directly", many.len());
        }
    }
    Ok(())
}

/// Break the selected window's active pane into a window of its own.
fn break_pane(model: &mut Model) -> Result<()> {
    let Some(w) = model.current_window() else {
        return Ok(());
    };
    if w.gone {
        model.status = "that window is not running".into();
        return Ok(());
    }
    if w.panes < 2 {
        // Breaking the only pane just renames the window.
        model.status = "window has a single pane; nothing to break out".into();
        return Ok(());
    }
    let target = w.target();
    crate::collect::cmd::run(
        "tmux",
        &["break-pane", "-d", "-s", &target],
        crate::collect::cmd::FAST,
    )?;
    reload(model)?;
    model.status = format!("broke a pane out of {target}");
    Ok(())
}

/// Pull the selected window's pane into the window tmc was launched from.
///
/// The counterpart to break, and the reason `$TMUX_PANE` matters: the
/// destination is where the user was, not where the popup is.
fn join_pane(model: &mut Model) -> Result<()> {
    let Some(w) = model.current_window() else {
        return Ok(());
    };
    if w.gone {
        model.status = "that window is not running".into();
        return Ok(());
    }
    let Ok(here) = std::env::var("TMUX_PANE") else {
        model.status = "not running inside tmux".into();
        return Ok(());
    };
    let target = w.target();
    crate::collect::cmd::run(
        "tmux",
        &["join-pane", "-s", &target, "-t", &here],
        crate::collect::cmd::FAST,
    )?;
    reload(model)?;
    model.status = format!("joined a pane from {target}");
    Ok(())
}

/// Close the selected window.
///
/// No confirmation prompt: the workspace was snapshotted, and the point of
/// this tool is that closing something is recoverable. The status line says
/// how.
fn kill_window(model: &mut Model) -> Result<()> {
    let Some(w) = model.current_window() else {
        return Ok(());
    };
    if w.gone {
        model.status = "that window is already gone".into();
        return Ok(());
    }
    let target = w.target();
    crate::collect::cmd::run(
        "tmux",
        &["kill-window", "-t", &target],
        crate::collect::cmd::FAST,
    )?;
    reload(model)?;
    model.status = format!("killed {target} — press r to bring it back");
    Ok(())
}

fn sessions() -> Result<Vec<String>> {
    Ok(crate::collect::cmd::run(
        "tmux",
        &["list-sessions", "-F", "#{session_name}"],
        crate::collect::cmd::FAST,
    )?
    .lines()
    .map(str::to_string)
    .collect())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::Change;
    use crate::ui::model::{Row, WindowRow};

    fn model_with_window() -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = vec![Row::Window(WindowRow {
            session: "projects".into(),
            index: 1,
            name: "alpha".into(),
            panes: 2,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        })];
        m
    }

    /// Keys that only read state, so they can be exercised without touching
    /// tmux. The mutating ones (m/b/J/x/r/s) shell out and are covered by
    /// their own modules.
    fn press(model: &mut Model, c: char, mods: KeyModifiers) {
        let _ = handle_key(model, KeyCode::Char(c), mods);
    }

    #[test]
    fn ctrl_c_quits_rather_than_clearing_marks() {
        // Both are bound to `c`; the modifier guard has to win.
        let mut m = model_with_window();
        m.marks.insert("projects:1".into());

        press(&mut m, 'c', KeyModifiers::CONTROL);
        assert!(m.quit, "ctrl-c must quit");
        assert_eq!(m.marks.len(), 1, "and must not clear marks on the way out");
    }

    #[test]
    fn plain_c_clears_marks_without_quitting() {
        let mut m = model_with_window();
        m.marks.insert("projects:1".into());

        press(&mut m, 'c', KeyModifiers::NONE);
        assert!(m.marks.is_empty());
        assert!(!m.quit);
    }

    #[test]
    fn enter_on_a_window_that_only_exists_in_the_point_explains_itself() {
        let mut m = model_with_window();
        if let Row::Window(w) = &mut m.rows[0] {
            w.gone = true;
        }
        let _ = handle_key(&mut m, KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(m.switch_to, None, "there is nothing to switch to");
        assert!(m.status.contains("restore"), "status: {}", m.status);
    }

    #[test]
    fn enter_on_a_live_window_records_the_switch() {
        let mut m = model_with_window();
        let _ = handle_key(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.switch_to.as_deref(), Some("projects:1"));
    }

    #[test]
    fn a_new_keypress_clears_the_previous_status() {
        // Otherwise a stale message sits under an unrelated action.
        let mut m = model_with_window();
        m.status = "something happened".into();
        press(&mut m, 'j', KeyModifiers::NONE);
        assert!(m.status.is_empty());
    }
}
