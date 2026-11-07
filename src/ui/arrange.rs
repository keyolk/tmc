//! The layout picker: type to narrow the presets, see the window drawn the
//! way each would leave it, Enter to apply. Bound to prefix-Enter, which was
//! tmux's `next-layout` — a cycle through layouts that ignore what the panes
//! are (see `crate::arrange`).

use std::io;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::theme::Theme;
use crate::arrange::{self, PRESETS, Preset, Window};

pub struct Picker {
    pub window: Window,
    pub filter: String,
    pub cursor: usize,
    pub chosen: Option<&'static Preset>,
    pub quit: bool,
}

impl Picker {
    pub fn new(window: Window) -> Self {
        Self {
            window,
            filter: String::new(),
            cursor: 0,
            chosen: None,
            quit: false,
        }
    }

    /// Presets matching the filter, best first. Matched on name and blurb,
    /// so `big` finds the one that makes a pane big as well as `spot`.
    pub fn visible(&self) -> Vec<&'static Preset> {
        let text: Vec<String> = PRESETS
            .iter()
            .map(|p| format!("{} {}", p.name, p.about))
            .collect();
        crate::fuzzy::rank(&self.filter, &text, |s| s.as_str())
            .into_iter()
            .map(|i| &PRESETS[i])
            .collect()
    }

    pub fn current(&self) -> Option<&'static Preset> {
        self.visible().get(self.cursor).copied()
    }

    fn step(&mut self, delta: isize) {
        let n = self.visible().len();
        if n > 0 {
            self.cursor = (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        }
    }

    pub fn key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('u') if ctrl => {
                self.filter.clear();
                self.cursor = 0;
            }
            KeyCode::Enter => {
                self.chosen = self.current();
                self.quit = self.chosen.is_some();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.cursor = 0;
            }
            _ => {}
        }
    }
}

/// Pick a preset for `target`'s window and apply it.
pub fn run(target: &str) -> Result<()> {
    let window = arrange::window(target)?;
    if window.cells.len() < 2 {
        println!("one pane: nothing to arrange");
        return Ok(());
    }
    let mut picker = Picker::new(window);
    let theme = Theme::detect();

    super::app::restore_terminal_on_panic();
    let mut guard = super::app::TerminalSetupGuard::arm()?;
    let mut out = io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    guard.disarm();

    let result = loop {
        if let Err(e) = terminal.draw(|f| draw(f, &picker, &theme)) {
            break Err(anyhow::Error::from(e));
        }
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                picker.key(k.code, k.modifiers);
                if picker.quit {
                    break Ok(());
                }
            }
            Ok(_) => {}
            Err(e) => break Err(anyhow::Error::from(e)),
        }
    };

    let raw = disable_raw_mode();
    let screen = crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let cursor = terminal.show_cursor();
    raw?;
    screen?;
    cursor?;
    result?;

    if let Some(p) = picker.chosen {
        arrange::apply(&picker.window, p)?;
    }
    Ok(())
}

/// `claude×2 web` — what the window holds, for the header.
fn census(w: &Window) -> String {
    arrange::groups(&w.cells)
        .iter()
        .map(|(k, v)| {
            if v.len() > 1 {
                format!("{k}×{}", v.len())
            } else {
                k.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn draw(frame: &mut ratatui::Frame, p: &Picker, t: &Theme) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let head = Line::from(vec![
        Span::styled("layout  ", t.subtle()),
        Span::raw(p.filter.clone()),
        Span::styled(t.glyphs.caret, t.accent()),
    ]);
    let side = format!("{} · {}  ⏎ apply  esc", p.window.id, census(&p.window));
    let room = (rows[0].width as usize).saturating_sub(head.width() + 2);
    frame.render_widget(Paragraph::new(head), rows[0]);
    if room > 0 {
        frame.render_widget(
            Paragraph::new(super::render::truncate(&side, room, t.glyphs.ellipsis))
                .style(t.subtle())
                .alignment(ratatui::layout::Alignment::Right),
            rows[0],
        );
    }

    // The list beside the preview when there is room, above it when not.
    let wide = rows[1].width >= 70;
    let parts = Layout::default()
        .direction(if wide {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if wide {
            [
                Constraint::Length(rows[1].width.min(120) * 2 / 5),
                Constraint::Min(10),
            ]
        } else {
            [Constraint::Length(PRESETS.len() as u16), Constraint::Min(4)]
        })
        .split(rows[1]);

    let visible = p.visible();
    let list: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, preset)| {
            let on = i == p.cursor;
            let room = (parts[0].width as usize).saturating_sub(16);
            Line::from(vec![
                Span::raw(if on { "> " } else { "  " }),
                Span::styled(
                    format!("{:<14}", preset.name),
                    if on { t.selected() } else { t.text() },
                ),
                Span::styled(
                    super::render::truncate(preset.about, room, t.glyphs.ellipsis),
                    t.subtle(),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(list), parts[0]);

    if let Some(preset) = p.current() {
        preview(frame, parts[1], &p.window, preset, t);
    }
}

/// The window as `preset` would leave it, scaled into `area`: one box per
/// pane, titled by kind, holding its label.
fn preview(frame: &mut ratatui::Frame, area: Rect, w: &Window, preset: &Preset, t: &Theme) {
    let area = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };
    let placed = match arrange::place(&preset.tree(&w.cells), w.width, w.height) {
        Ok(placed) => placed,
        Err(e) => {
            frame.render_widget(Paragraph::new(e.to_string()).style(t.warn()), area);
            return;
        }
    };
    // Scale edges, not sizes, so neighbouring boxes meet without gaps.
    let sx = |x: u32| area.x + (x as u64 * area.width as u64 / (w.width as u64 + 1)) as u16;
    let sy = |y: u32| area.y + (y as u64 * area.height as u64 / (w.height as u64 + 1)) as u16;
    for (i, r) in placed {
        let (x0, y0) = (sx(r.x), sy(r.y));
        let (x1, y1) = (sx(r.x + r.w + 1), sy(r.y + r.h + 1));
        let rect = Rect {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0).max(2),
            height: y1.saturating_sub(y0).max(2),
        };
        let rect = rect.intersection(area);
        let cell = &w.cells[i];
        let style = if cell.active {
            t.border_focus()
        } else {
            t.border()
        };
        let title = format!(" {} ", cell.kind);
        let inner = (rect.width as usize).saturating_sub(2);
        let body = super::render::truncate(&cell.label, inner, t.glyphs.ellipsis);
        frame.render_widget(
            Paragraph::new(body).style(t.text()).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(style)
                    .title(Span::styled(title, t.accent())),
            ),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arrange::Cell;

    fn picker() -> Picker {
        let cell = |index: u32, kind: &str, label: &str| Cell {
            index,
            id: format!("%{index}"),
            kind: kind.into(),
            label: label.into(),
            active: index == 1,
        };
        Picker::new(Window {
            id: "@9".into(),
            width: 365,
            height: 97,
            zoomed: false,
            cells: vec![
                cell(1, "claude", "chat one"),
                cell(2, "claude", "chat two"),
                cell(3, "web", "red team"),
            ],
        })
    }

    fn typed(s: &str) -> Picker {
        let mut p = picker();
        for c in s.chars() {
            p.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        p
    }

    #[test]
    fn typing_narrows_fuzzily() {
        assert_eq!(typed("spt").current().unwrap().name, "spotlight");
        assert_eq!(typed("grd").current().unwrap().name, "grid");
        // The blurb is searchable too.
        assert_eq!(typed("stacked").current().unwrap().name, "columns");
    }

    #[test]
    fn enter_chooses_and_escape_does_not() {
        let mut p = typed("rows");
        p.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(p.chosen.map(|c| c.name), Some("rows"));
        assert!(p.quit);

        let mut p = picker();
        p.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(p.quit && p.chosen.is_none());
    }

    #[test]
    fn enter_on_no_match_chooses_nothing() {
        let mut p = typed("zzzz");
        p.key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(p.chosen.is_none() && !p.quit);
    }

    #[test]
    fn arrows_and_ctrl_chords_move_without_typing() {
        let mut p = picker();
        p.key(KeyCode::Down, KeyModifiers::NONE);
        p.key(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(p.cursor, 2);
        assert!(p.filter.is_empty());
        p.key(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(p.cursor, 1);
        for _ in 0..20 {
            p.key(KeyCode::Down, KeyModifiers::NONE);
        }
        assert_eq!(p.cursor, PRESETS.len() - 1, "stops at the end");
    }

    fn screen(p: &Picker, w: u16, h: u16) -> Vec<String> {
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, p, &Theme::default())).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    #[test]
    fn the_preview_draws_each_pane_where_the_preset_puts_it() {
        let lines = screen(&picker(), 100, 24);
        assert!(lines[0].contains("@9 · claude×2 web"), "{:?}", lines[0]);
        let body = lines.join("\n");
        assert!(
            body.contains("red team") && body.contains("chat one"),
            "{body}"
        );
        // Spotlight: the page's box starts right of the sessions' boxes.
        let col = |s: &str| lines.iter().find_map(|l| l.find(s)).unwrap();
        assert!(col(" web ") > col(" claude "), "{body}");
    }

    #[test]
    fn a_narrow_popup_stacks_the_list_over_the_preview() {
        let lines = screen(&picker(), 50, 20);
        assert!(lines[1].contains("spotlight"));
        assert!(
            lines
                .iter()
                .skip(1 + PRESETS.len())
                .any(|l| l.contains(" web "))
        );
    }
}
