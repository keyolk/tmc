//! A panic while drawing one pane costs that pane, not the session (E4).
//!
//! Each pane's render runs inside `catch_unwind`. If it panics, the cells it
//! covers are put back as they were before it started -- for an overlay that
//! is the tree underneath -- and a one-line fallback is drawn in their place.
//! The panic is written to `tmc.log` beside `ui.json`, once per pane per run:
//! the frame redraws on every key, and a log line per frame would bury the
//! one that matters.
//!
//! The terminal-restoring panic hook in `app` runs before any unwinding, so a
//! caught panic would otherwise drop the alternate screen out from under a
//! TUI that is about to carry on. The hook asks `active()` first and, while a
//! guarded render runs, only records where the panic happened.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, PanicHookInfo, catch_unwind};
use std::path::PathBuf;
use std::sync::Mutex;

use ratatui::Frame;
use ratatui::buffer::Cell as BufCell;
use ratatui::layout::Rect;
use ratatui::widgets::{Clear, Paragraph};

use super::status::Level;
use super::theme::Theme;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// What the hook saw: the message with its location, which the payload
    /// `catch_unwind` returns does not carry.
    static NOTED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Whether a guarded render is running on this thread.
pub fn active() -> bool {
    ACTIVE.with(Cell::get)
}

/// Called by the panic hook while `active()`: keep the message for the log.
pub fn note(info: &PanicHookInfo<'_>) {
    NOTED.with(|n| *n.borrow_mut() = Some(info.to_string()));
}

/// Run `draw` for the pane `name` covering `area`.
///
/// `keep_base` is for overlays: the cells under the box are copied first so a
/// panic restores the tree rather than leaving half a menu over it. Panes
/// that own their area skip the copy -- the frame starts each draw blank.
pub fn pane(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    theme: &Theme,
    keep_base: bool,
    draw: impl FnOnce(&mut Frame),
) {
    let area = area.intersection(frame.area());
    let base = keep_base.then(|| cells(frame, area));

    ACTIVE.with(|a| a.set(true));
    let result = catch_unwind(AssertUnwindSafe(|| draw(frame)));
    ACTIVE.with(|a| a.set(false));

    let Err(payload) = result else {
        return;
    };
    let message = NOTED
        .with(|n| n.borrow_mut().take())
        .unwrap_or_else(|| payload_text(payload.as_ref()));
    let path = log_path();
    log_once(name, &message, &path);

    match base {
        Some(base) => restore(frame, area, base),
        None => frame.render_widget(Clear, area),
    }
    let (prefix, style) = theme.status(Level::Error);
    let line = format!("{prefix} {name} failed to draw; see {}", path.display());
    let first_row = Rect {
        height: area.height.min(1),
        ..area
    };
    frame.render_widget(Clear, first_row);
    frame.render_widget(
        Paragraph::new(super::render::truncate(
            &line,
            area.width as usize,
            theme.glyphs.ellipsis,
        ))
        .style(style),
        first_row,
    );
}

fn cells(frame: &mut Frame, area: Rect) -> Vec<BufCell> {
    let buf = frame.buffer_mut();
    area.positions().map(|p| buf[p].clone()).collect()
}

fn restore(frame: &mut Frame, area: Rect, base: Vec<BufCell>) {
    let buf = frame.buffer_mut();
    for (p, cell) in area.positions().zip(base) {
        buf[p] = cell;
    }
}

fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// `tmc.log` beside the remembered UI state.
pub fn log_path() -> PathBuf {
    super::state::path().with_file_name("tmc.log")
}

/// Panes already logged this run.
static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn log_once(name: &str, message: &str, path: &std::path::Path) {
    // A poisoned lock means another thread panicked holding it; logging
    // twice is better than not logging.
    let mut logged = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    if logged.iter().any(|n| n == name) {
        return;
    }
    logged.push(name.to_string());
    drop(logged);
    write_log(&format!("render {name} panicked: {message}"), path);
}

#[cfg(not(test))]
fn write_log(line: &str, path: &std::path::Path) {
    use std::io::Write;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Errors are dropped on purpose: this is already the failure path, the
    // fallback line is on screen whatever happens here, and there is no
    // terminal to print to while the TUI owns it.
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{secs} {line}");
    }
}

// Tests never touch the real state directory: they read what would have been
// written from here.
#[cfg(test)]
thread_local! {
    static WRITTEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn write_log(line: &str, _path: &std::path::Path) {
    WRITTEN.with(|w| w.borrow_mut().push(line.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn text(t: &Terminal<TestBackend>) -> String {
        let buf = t.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_panicking_pane_draws_a_fallback_and_the_others_survive() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(60, 4)).unwrap();
        term.draw(|f| {
            let top = Rect::new(0, 0, 60, 2);
            let bottom = Rect::new(0, 2, 60, 2);
            pane(f, top, "guard-test-tree", &theme, false, |f| {
                f.render_widget(Paragraph::new("half a tr"), top);
                panic!("index out of bounds");
            });
            pane(f, bottom, "guard-test-keys", &theme, false, |f| {
                f.render_widget(Paragraph::new("q quit"), bottom);
            });
        })
        .unwrap();

        let out = text(&term);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("✗ guard-test-tree failed to draw; see "),
            "{out}"
        );
        assert!(
            !out.contains("half a tr"),
            "the partial draw is gone:\n{out}"
        );
        assert_eq!(lines[2], "q quit", "the next pane still draws");
        assert!(!active(), "the guard is lowered after a panic");

        let logged = WRITTEN.with(|w| w.borrow().clone());
        assert!(
            logged
                .iter()
                .any(|l| l.contains("guard-test-tree") && l.contains("index out of bounds")),
            "{logged:?}"
        );
    }

    #[test]
    fn an_overlay_that_panics_leaves_the_base_underneath() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(40, 3)).unwrap();
        term.draw(|f| {
            let all = Rect::new(0, 0, 40, 3);
            f.render_widget(Paragraph::new("row one\nrow two\nrow three"), all);
            pane(f, all, "guard-test-menu", &theme, true, |f| {
                f.render_widget(Clear, all);
                f.render_widget(Paragraph::new("menu"), all);
                panic!("boom");
            });
        })
        .unwrap();

        let out = text(&term);
        assert!(
            out.lines().next().unwrap().contains("guard-test-menu"),
            "{out}"
        );
        assert!(
            out.contains("row two") && out.contains("row three"),
            "{out}"
        );
        assert!(!out.contains("menu\n"), "{out}");
    }

    #[test]
    fn a_pane_is_logged_once_per_run() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(40, 2)).unwrap();
        for _ in 0..3 {
            term.draw(|f| {
                let all = f.area();
                pane(f, all, "guard-test-repeat", &theme, false, |_| {
                    panic!("again")
                });
            })
            .unwrap();
        }
        let n = WRITTEN.with(|w| {
            w.borrow()
                .iter()
                .filter(|l| l.contains("guard-test-repeat"))
                .count()
        });
        assert_eq!(n, 1);
    }
}
