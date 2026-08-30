//! Drawing the tree and the diff panel.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::model::{Model, Row};
use crate::clock;
use crate::layout::diff::Change;

/// Below this many columns the detail panel is dropped rather than squeezed.
const DETAIL_MIN_WIDTH: u16 = 90;
/// Below this the app says so instead of rendering garbage.
const MIN_WIDTH: u16 = 50;
const MIN_HEIGHT: u16 = 10;

pub fn draw(frame: &mut Frame, model: &Model, now_secs: u64) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("terminal too small").style(Style::default().fg(Color::Red)),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // keys
        ])
        .split(area);

    render_header(frame, rows[0], model, now_secs);

    if area.width >= DETAIL_MIN_WIDTH {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(rows[1]);
        render_tree(frame, columns[0], model);
        render_detail(frame, columns[1], model);
    } else {
        render_tree(frame, rows[1], model);
    }

    render_keys(frame, rows[2], model);
}

fn render_header(frame: &mut Frame, area: Rect, model: &Model, now_secs: u64) {
    let (modified, added, removed) = model.counts;
    let point = model
        .current_point()
        .map(|p| format!("{}  ({})", p.name, clock::age_of(&p.sort_key, now_secs),))
        .unwrap_or_else(|| "no restore point".into());

    let windows = model
        .rows
        .iter()
        .filter(|r| matches!(r, Row::Window(_)))
        .count();

    let mut summary = vec![
        Span::styled("tmc", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {windows} windows")),
    ];
    if model.waiting > 0 {
        summary.push(Span::styled(
            format!("  {} waiting", model.waiting),
            Style::default().fg(Color::Yellow),
        ));
    }
    if !model.marks.is_empty() {
        summary.push(Span::raw(format!("  {} marked", model.marks.len())));
    }

    let drift = Line::from(vec![
        Span::styled(point, Style::default().fg(Color::DarkGray)),
        Span::raw("   "),
        Span::styled(format!("~{modified}"), style_for(Change::Modified)),
        Span::raw(" "),
        Span::styled(format!("+{added}"), style_for(Change::Added)),
        Span::raw(" "),
        Span::styled(format!("-{removed}"), style_for(Change::Removed)),
    ]);

    frame.render_widget(Paragraph::new(vec![Line::from(summary), drift]), area);
}

fn render_tree(frame: &mut Frame, area: Rect, model: &Model) {
    let height = area.height.saturating_sub(2) as usize;
    let offset = scroll_offset(model.cursor, model.rows.len(), height);

    let lines: Vec<Line> = model
        .rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, row)| render_row(row, i == model.cursor, model))
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::RIGHT)),
        area,
    );
}

/// Keep the cursor in view without jumping the list around.
fn scroll_offset(cursor: usize, total: usize, height: usize) -> usize {
    if total <= height || height == 0 {
        return 0;
    }
    // Centre the cursor once it passes the midpoint, then stop at the end.
    let half = height / 2;
    cursor.saturating_sub(half).min(total - height)
}

fn render_row<'a>(row: &'a Row, selected: bool, model: &Model) -> Line<'a> {
    match row {
        Row::Session { name, windows } => Line::from(vec![
            Span::styled(
                format!("▾ {name}"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {windows}w"),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Row::Window(w) => {
            let mark = if model.marks.contains(&w.target()) {
                '#'
            } else {
                ' '
            };
            let cursor = if selected { '>' } else { ' ' };
            let index = if w.gone {
                "  —".to_string()
            } else {
                format!("{:>3}", w.index)
            };

            let mut spans = vec![
                Span::raw(format!("{cursor}{mark} ")),
                Span::styled(
                    w.state_glyph().to_string(),
                    if w.waiting {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
                Span::raw(format!(" {index} ")),
                Span::styled(
                    format!("{:<14}", truncate(&w.name, 14)),
                    if w.gone {
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::CROSSED_OUT)
                    } else if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("{:>2}p ", w.panes),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(w.change.marker().to_string(), style_for(w.change)),
            ];
            if !w.cc_session.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", &w.cc_session[..8.min(w.cc_session.len())]),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        }
    }
}

fn render_detail(frame: &mut Frame, area: Rect, model: &Model) {
    let mut lines: Vec<Line> = Vec::new();

    match model.current_window() {
        Some(w) => {
            lines.push(Line::from(vec![
                Span::styled(w.target(), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(w.name.clone(), Style::default().fg(Color::DarkGray)),
            ]));
            lines.push(Line::raw(""));

            if w.change == Change::Same {
                lines.push(Line::styled(
                    "unchanged since the restore point",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            for reason in &w.reasons {
                lines.push(Line::styled(format!("  {reason}"), style_for(w.change)));
            }

            if !w.state.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![
                    Span::styled("state    ", Style::default().fg(Color::DarkGray)),
                    Span::raw(w.state.clone()),
                ]));
            }
            if !w.cc_session.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("session  ", Style::default().fg(Color::DarkGray)),
                    Span::raw(w.cc_session.clone()),
                ]));
            }
        }
        None => lines.push(Line::styled(
            "select a window",
            Style::default().fg(Color::DarkGray),
        )),
    }

    if !model.status.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            model.status.clone(),
            Style::default().fg(Color::Yellow),
        ));
    }

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::NONE)),
        area,
    );
}

fn render_keys(frame: &mut Frame, area: Rect, model: &Model) {
    let restore = if model.marks.is_empty() {
        "r restore all"
    } else {
        "r restore marked"
    };
    let text = format!(
        "j/k move  space mark  a all  {restore}  s save  p/P point  n next waiting  ⏎ switch  q quit",
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn style_for(change: Change) -> Style {
    match change {
        Change::Modified => Style::default().fg(Color::Blue),
        Change::Added => Style::default().fg(Color::Green),
        Change::Removed => Style::default().fg(Color::Red),
        Change::Same => Style::default().fg(Color::DarkGray),
    }
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_list_never_scrolls() {
        assert_eq!(scroll_offset(0, 5, 20), 0);
        assert_eq!(scroll_offset(4, 5, 20), 0);
    }

    #[test]
    fn the_cursor_centres_then_stops_at_the_end() {
        // 100 rows in a 20-row window.
        assert_eq!(
            scroll_offset(5, 100, 20),
            0,
            "no scroll before the midpoint"
        );
        assert_eq!(scroll_offset(50, 100, 20), 40, "centred");
        assert_eq!(scroll_offset(99, 100, 20), 80, "clamped to the last page");
    }

    #[test]
    fn truncation_marks_what_it_cut() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a-very-long-window-name", 10), "a-very-lo…");
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // A window named in Korean must not be cut mid-codepoint.
        assert_eq!(truncate("한글이름테스트", 4), "한글이…");
    }
}
