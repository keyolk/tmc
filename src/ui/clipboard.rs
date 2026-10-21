//! The paste-buffer picker — a small, self-contained screen.
//!
//! Separate from the tree because it is summoned for one action and exits.
//! Bound to `F`, replacing the tmux-fzf menu that took two keystrokes to
//! reach the same list.

use std::io;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::link;
use super::theme::Theme;
use crate::collect::clipboard::{self, Buffer};

/// State of the picker, kept apart from the terminal so it can be tested.
pub struct Picker {
    pub buffers: Vec<Buffer>,
    pub cursor: usize,
    /// Typed filter. Buffers are matched on their preview text.
    pub filter: String,
    pub chosen: Option<String>,
    pub quit: bool,
    /// What the last ctrl-o did, on the filter line until the next key.
    pub note: Option<String>,
    /// Opens a URL; tests put a recorder here.
    pub launcher: link::Launcher,
    /// Reads a buffer's whole text; tests read a fixture instead of tmux.
    pub read: fn(&str) -> Result<String>,
}

impl Picker {
    pub fn new(buffers: Vec<Buffer>) -> Self {
        Self {
            buffers,
            cursor: 0,
            filter: String::new(),
            chosen: None,
            quit: false,
            note: None,
            launcher: link::DEFAULT,
            read: clipboard::content,
        }
    }

    /// Indices of the buffers matching the current filter, best first.
    ///
    /// Fuzzy rather than substring: with 100 buffers you remember roughly what
    /// you copied, not its exact wording. `gcm` should find
    /// `git commit --amend`.
    pub fn visible(&self) -> Vec<usize> {
        crate::fuzzy::rank(&self.filter, &self.buffers, |b| b.preview.as_str())
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let next = self.cursor as isize + delta;
        self.cursor = next.clamp(0, visible.len() as isize - 1) as usize;
    }

    pub fn current(&self) -> Option<&Buffer> {
        self.visible()
            .get(self.cursor)
            .and_then(|&i| self.buffers.get(i))
    }

    pub fn key(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.note = None;
        match code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('n') if mods.contains(KeyModifiers::CONTROL) => self.move_cursor(1),
            KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => self.move_cursor(-1),
            // Not `o`, the fleet's open-link key: every printable key types
            // into the filter here, so the link takes the control chord.
            KeyCode::Char('o') if mods.contains(KeyModifiers::CONTROL) => self.open_link(),
            KeyCode::Enter => {
                self.chosen = self.current().map(|b| b.name.clone());
                self.quit = true;
            }
            KeyCode::Backspace => {
                self.filter.pop();
                // The list just grew; a cursor past the new end would point at
                // nothing.
                self.clamp();
            }
            // Every other printable character types into the filter. Plain
            // letters cannot double as navigation here — the whole point is to
            // search 100 buffers by content.
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.clamp();
            }
            _ => {}
        }
    }

    /// Open the first URL in the highlighted buffer. Read whole on demand:
    /// the row shows only tmux's sample, which cuts URLs.
    fn open_link(&mut self) {
        let Some(name) = self.current().map(|b| b.name.clone()) else {
            self.note = Some("no buffer selected".into());
            return;
        };
        let text = match (self.read)(&name) {
            Ok(text) => text,
            Err(e) => {
                self.note = Some(format!("read {name}: {e:#}"));
                return;
            }
        };
        let opened = link::open_first(self.launcher, &link::find(&text));
        self.note = Some(match opened.err {
            Some(link::OpenError::NoLink) => "no link in this buffer".into(),
            _ => opened.note(),
        });
    }

    pub fn mouse(&mut self, event: MouseEvent, area: Rect) {
        let rows = picker_rows(area);
        if event.column < rows[1].x
            || event.column >= rows[1].right()
            || event.row < rows[1].y
            || event.row >= rows[1].bottom()
        {
            return;
        }
        match event.kind {
            MouseEventKind::ScrollDown => self.move_cursor(3),
            MouseEventKind::ScrollUp => self.move_cursor(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                let visible = self.visible();
                let offset = list_offset(self.cursor, visible.len(), rows[1].height as usize);
                let position = offset + event.row.saturating_sub(rows[1].y) as usize;
                if position < visible.len() {
                    self.cursor = position;
                }
            }
            MouseEventKind::Down(MouseButton::Right | MouseButton::Middle) => {
                let visible = self.visible();
                let offset = list_offset(self.cursor, visible.len(), rows[1].height as usize);
                let position = offset + event.row.saturating_sub(rows[1].y) as usize;
                if position < visible.len() {
                    self.cursor = position;
                    self.chosen = self.current().map(|b| b.name.clone());
                    self.quit = self.chosen.is_some();
                }
            }
            _ => {}
        }
    }

    fn clamp(&mut self) {
        let visible = self.visible().len();
        if visible == 0 {
            self.cursor = 0;
        } else if self.cursor >= visible {
            self.cursor = visible - 1;
        }
    }
}

/// Show the picker; paste the chosen buffer into `target`.
pub fn run(target: &str) -> Result<()> {
    let buffers = clipboard::buffers().context("list tmux buffers")?;
    if buffers.is_empty() {
        println!("no paste buffers");
        return Ok(());
    }
    let mut picker = Picker::new(buffers);
    let theme = Theme::detect();

    super::app::restore_terminal_on_panic();
    let mut guard = super::app::TerminalSetupGuard::arm()?;
    let mut out = io::stdout();
    crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    guard.disarm();

    let result = loop {
        if let Err(e) = terminal.draw(|frame| draw(frame, &picker, &theme)) {
            break Err(anyhow::Error::from(e));
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                picker.key(key.code, key.modifiers);
                if picker.quit {
                    break Ok(());
                }
            }
            Ok(Event::Mouse(mouse)) => match terminal.size() {
                Ok(size) => {
                    picker.mouse(mouse, Rect::from(size));
                    if picker.quit {
                        break Ok(());
                    }
                }
                Err(e) => break Err(anyhow::Error::from(e)),
            },
            Ok(_) => {}
            Err(e) => break Err(anyhow::Error::from(e)),
        }
    };

    let raw = disable_raw_mode();
    let screen = crossterm::execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let cursor = terminal.show_cursor();
    raw?;
    screen?;
    cursor?;
    result?;

    if let Some(name) = picker.chosen {
        clipboard::paste(&name, target)?;
    }
    Ok(())
}

fn picker_rows(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area)
}

fn list_offset(cursor: usize, total: usize, height: usize) -> usize {
    cursor
        .saturating_sub(height / 2)
        .min(total.saturating_sub(height))
}

fn draw(frame: &mut ratatui::Frame, picker: &Picker, t: &Theme) {
    let area = frame.area();
    let rows = picker_rows(area);

    let head = Line::from(vec![
        Span::styled("buffer  ", t.subtle()),
        Span::raw(picker.filter.clone()),
        Span::styled(t.glyphs.caret, t.accent()),
    ]);
    // The picker has no hint row; the filter line names the one key that is
    // not typing, and says what it did.
    let side = picker
        .note
        .clone()
        .unwrap_or_else(|| "ctrl-o open link".into());
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

    let visible = picker.visible();
    let height = rows[1].height as usize;
    let offset = list_offset(picker.cursor, visible.len(), height);

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(pos, &i)| {
            let b = &picker.buffers[i];
            let selected = pos == picker.cursor;
            // Cut with a marker rather than clipped by the paragraph: a
            // preview that stops mid-word at the edge reads as the whole
            // buffer. 2 for the cursor, 9 for the size column.
            let room = (rows[1].width as usize).saturating_sub(2 + 9);
            Line::from(vec![
                Span::raw(if selected { "> " } else { "  " }),
                Span::styled(format!("{:>6}b  ", b.bytes), t.subtle()),
                Span::styled(
                    super::render::truncate(&b.preview, room, t.glyphs.ellipsis),
                    if selected { t.selected() } else { t.text() },
                ),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), rows[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(name: &str, preview: &str) -> Buffer {
        Buffer {
            name: name.into(),
            bytes: preview.len(),
            preview: preview.into(),
        }
    }

    fn picker() -> Picker {
        Picker::new(vec![
            buffer("buffer3", "git commit --amend"),
            buffer("buffer2", "cargo test --release"),
            buffer("buffer1", "kubectl get pods"),
        ])
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn a_mouse_click_selects_without_pasting() {
        let mut p = picker();
        p.mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 2, 2),
            Rect::new(0, 0, 60, 10),
        );
        assert_eq!(p.cursor, 1);
        assert!(p.chosen.is_none());
        assert!(!p.quit);
    }

    #[test]
    fn wheel_moves_only_inside_the_buffer_list() {
        let mut p = picker();
        let area = Rect::new(0, 0, 60, 10);
        p.mouse(mouse(MouseEventKind::ScrollDown, 2, 2), area);
        assert_eq!(p.cursor, 2);
        p.mouse(mouse(MouseEventKind::ScrollUp, 2, 0), area);
        assert_eq!(p.cursor, 2, "the filter line does not own list scrolling");
    }

    #[test]
    fn middle_click_is_an_explicit_mouse_paste() {
        let mut p = picker();
        p.mouse(
            mouse(MouseEventKind::Down(MouseButton::Middle), 2, 2),
            Rect::new(0, 0, 60, 10),
        );
        assert_eq!(p.chosen.as_deref(), Some("buffer2"));
        assert!(p.quit);
    }

    #[test]
    fn typing_filters_on_the_preview_text() {
        let mut p = picker();
        for c in "cargo".chars() {
            p.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(p.current().unwrap().name, "buffer2");
    }

    #[test]
    fn a_fuzzy_query_finds_what_you_half_remember() {
        // The realistic case with 100 buffers: initials, not exact wording.
        let mut p = picker();
        for c in "gcm".chars() {
            p.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(
            p.current().unwrap().preview,
            "git commit --amend",
            "gcm should reach it",
        );
    }

    #[test]
    fn filtering_is_case_insensitive_until_you_type_a_capital() {
        // Smart case, as everywhere else here: a lowercase query ignores case,
        // an uppercase one means it.
        let mut lower = picker();
        for c in "kubectl".chars() {
            lower.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(lower.current().unwrap().name, "buffer1");

        let mut upper = picker();
        for c in "KUBECTL".chars() {
            upper.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(upper.visible().is_empty(), "an uppercase query is exact");
    }

    #[test]
    fn a_letter_types_rather_than_navigating() {
        // The list holds 100 entries here; searching by content is the point,
        // so j/k cannot be movement keys.
        let mut p = picker();
        p.key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(p.filter, "j");
        assert_eq!(p.cursor, 0, "the cursor did not move");
    }

    #[test]
    fn backspace_widens_the_list_without_stranding_the_cursor() {
        let mut p = picker();
        for c in "cargo".chars() {
            p.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(p.visible().len(), 1);

        p.key(KeyCode::Down, KeyModifiers::NONE);
        for _ in 0..5 {
            p.key(KeyCode::Backspace, KeyModifiers::NONE);
        }
        assert_eq!(p.visible().len(), 3);
        assert!(p.current().is_some(), "the cursor still points at a buffer");
    }

    #[test]
    fn a_filter_matching_nothing_leaves_no_selection() {
        let mut p = picker();
        for c in "zzz".chars() {
            p.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(p.visible().is_empty());
        assert!(p.current().is_none());

        // Enter must not choose a buffer that is not shown.
        p.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(p.chosen, None);
    }

    #[test]
    fn enter_chooses_the_highlighted_buffer() {
        let mut p = picker();
        p.key(KeyCode::Down, KeyModifiers::NONE);
        p.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(p.chosen.as_deref(), Some("buffer2"));
        assert!(p.quit);
    }

    #[test]
    fn ctrl_n_and_ctrl_p_move_without_typing() {
        let mut p = picker();
        p.key(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(p.cursor, 1);
        assert!(p.filter.is_empty(), "a control chord must not type");
        p.key(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(p.cursor, 0);
    }

    fn read_fixture(name: &str) -> Result<String> {
        Ok(match name {
            "buffer3" => "see https://github.com/keyolk/tmc/pull/15 and\nhttps://x.io/b".into(),
            "buffer2" => "javascript:alert(1) file:///etc/passwd".into(),
            _ => "kubectl get pods".into(),
        })
    }

    fn ctrl_o(p: &mut Picker) {
        p.key(KeyCode::Char('o'), KeyModifiers::CONTROL);
    }

    #[test]
    fn ctrl_o_opens_the_first_url_of_the_whole_buffer() {
        let mut p = picker();
        p.read = read_fixture;
        p.launcher = link::stub::record;
        ctrl_o(&mut p);
        assert_eq!(
            link::stub::take(),
            ["https://github.com/keyolk/tmc/pull/15"]
        );
        assert_eq!(
            p.note.as_deref(),
            Some("opened https://github.com/keyolk/tmc/pull/15 (the first of 2 links)")
        );
        assert!(p.filter.is_empty(), "the chord does not type");
        assert!(
            p.chosen.is_none() && !p.quit,
            "opening a link is not pasting"
        );
    }

    #[test]
    fn ctrl_o_without_an_http_url_launches_nothing_and_says_so() {
        let mut p = picker();
        p.read = read_fixture;
        p.launcher = link::stub::unexpected;
        p.key(KeyCode::Down, KeyModifiers::NONE);
        ctrl_o(&mut p);
        assert_eq!(p.note.as_deref(), Some("no link in this buffer"));
        p.key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(p.note, None, "the next key takes the note down");
        ctrl_o(&mut p);
        assert_eq!(p.note.as_deref(), Some("no link in this buffer"));
    }

    #[test]
    fn plain_o_still_types() {
        let mut p = picker();
        p.launcher = link::stub::unexpected;
        p.key(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(p.filter, "o");
    }

    #[test]
    fn the_filter_line_names_ctrl_o() {
        let p = picker();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 5)).unwrap();
        terminal.draw(|f| draw(f, &p, &Theme::default())).unwrap();
        let top: String = (0..60)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol().to_string())
            .collect();
        assert!(top.trim_end().ends_with("ctrl-o open link"), "{top:?}");
    }

    #[test]
    fn escape_leaves_without_pasting() {
        let mut p = picker();
        p.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(p.quit);
        assert_eq!(p.chosen, None);
    }
}
