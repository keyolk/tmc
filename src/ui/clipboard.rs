//! The paste-buffer picker — a small, self-contained screen.
//!
//! Separate from the tree because it is summoned for one action and exits.
//! Bound to `F`, replacing the tmux-fzf menu that took two keystrokes to
//! reach the same list.

use std::io;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::collect::clipboard::{self, Buffer};

/// State of the picker, kept apart from the terminal so it can be tested.
pub struct Picker {
    pub buffers: Vec<Buffer>,
    pub cursor: usize,
    /// Typed filter. Buffers are matched on their preview text.
    pub filter: String,
    pub chosen: Option<String>,
    pub quit: bool,
}

impl Picker {
    pub fn new(buffers: Vec<Buffer>) -> Self {
        Self {
            buffers,
            cursor: 0,
            filter: String::new(),
            chosen: None,
            quit: false,
        }
    }

    /// Indices of the buffers matching the current filter.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.buffers.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        self.buffers
            .iter()
            .enumerate()
            .filter(|(_, b)| b.preview.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
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
        match code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('n') if mods.contains(KeyModifiers::CONTROL) => self.move_cursor(1),
            KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => self.move_cursor(-1),
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

    enable_raw_mode()?;
    let mut out = io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let result = loop {
        if let Err(e) = terminal.draw(|frame| draw(frame, &picker)) {
            break Err(anyhow::Error::from(e));
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                picker.key(key.code, key.modifiers);
                if picker.quit {
                    break Ok(());
                }
            }
            Ok(_) => {}
            Err(e) => break Err(anyhow::Error::from(e)),
        }
    };

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result?;

    if let Some(name) = picker.chosen {
        clipboard::paste(&name, target)?;
    }
    Ok(())
}

fn draw(frame: &mut ratatui::Frame, picker: &Picker) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("buffer  ", Style::default().fg(Color::DarkGray)),
            Span::raw(picker.filter.clone()),
            Span::styled("▌", Style::default().fg(Color::Yellow)),
        ])),
        rows[0],
    );

    let visible = picker.visible();
    let height = rows[1].height as usize;
    let offset = picker
        .cursor
        .saturating_sub(height / 2)
        .min(visible.len().saturating_sub(height));

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(pos, &i)| {
            let b = &picker.buffers[i];
            let selected = pos == picker.cursor;
            Line::from(vec![
                Span::raw(if selected { "> " } else { "  " }),
                Span::styled(
                    format!("{:>6}b  ", b.bytes),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    b.preview.clone(),
                    if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
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

    #[test]
    fn typing_filters_on_the_preview_text() {
        let mut p = picker();
        for c in "cargo".chars() {
            p.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(p.visible().len(), 1);
        assert_eq!(p.current().unwrap().name, "buffer2");
    }

    #[test]
    fn filtering_is_case_insensitive() {
        let mut p = picker();
        for c in "KUBECTL".chars() {
            p.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(p.current().unwrap().name, "buffer1");
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

    #[test]
    fn escape_leaves_without_pasting() {
        let mut p = picker();
        p.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(p.quit);
        assert_eq!(p.chosen, None);
    }
}
