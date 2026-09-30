//! Drawing the tree and the diff panel.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::model::{Model, Row, actions};
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

    // Last, so it sits over the tree and the panel both.
    if model.menu {
        render_menu(frame, rows[1], model);
    }
}

/// The `x` menu: every window and pane command, with what each would act on.
///
/// Floating rather than a panel swap, because it is momentary and because
/// covering the tree is the point — the commands act on the selection, which
/// the title line names, not on whatever the cursor is passing over.
fn render_menu(frame: &mut Frame, body: Rect, model: &Model) {
    let entries = actions(model);
    let title = format!(" x  {} ", menu_target(model));

    // Sized to its contents: the detail column is right-aligned, so the box
    // has to know the widest row before it can place anything.
    let keys: Vec<String> = entries
        .iter()
        .map(|a| format!("{}:{}", a.key, a.label))
        .collect();
    let widest_key = keys.iter().map(|k| k.chars().count()).max().unwrap_or(0);
    let widest_detail = entries
        .iter()
        .map(|a| a.detail.chars().count())
        .max()
        .unwrap_or(0);
    const GAP: usize = 3;
    let inner = (widest_key + GAP + widest_detail)
        .max(title.chars().count())
        .max("esc cancel".len());
    // +2 for the border, +2 for a column of padding either side.
    let width = (inner as u16 + 4).min(body.width);
    // One row per action, one for the footer, one blank above it.
    let height = (entries.len() as u16 + 2 + 2).min(body.height);
    let area = centred(body, width, height);

    let usable = area.width.saturating_sub(4) as usize;
    // The box is capped by the terminal, so on a narrow one the detail is
    // what gives. Cut explicitly rather than letting the paragraph clip it:
    // `needs a pane - l expan` reads as a typo, `needs a pane…` as a cut.
    let budget = usable.saturating_sub(widest_key + GAP);
    let mut lines: Vec<Line> = entries
        .iter()
        .zip(&keys)
        .map(|(action, key)| {
            let detail = truncate(&action.detail, budget);
            let pad = usable
                .saturating_sub(widest_key + detail.chars().count())
                .max(1);
            let (key_style, label_style) = if action.enabled {
                (
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    Style::default(),
                )
            } else {
                // Greyed rather than hidden. With five commands the reason a
                // row is unavailable is the useful part — "needs two or more
                // marked" is how you learn merge takes a selection at all —
                // and a menu that changes shape teaches nothing.
                (
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::DarkGray),
                )
            };
            let (k, label) = key.split_at(1);
            Line::from(vec![
                Span::styled(k.to_string(), key_style),
                Span::styled(label.to_string(), label_style),
                Span::raw(" ".repeat(widest_key - key.chars().count() + pad)),
                Span::styled(detail, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "esc cancel",
        Style::default().fg(Color::DarkGray),
    ));

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Span::styled(
                    truncate(&title, area.width.saturating_sub(4) as usize),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}

/// What the menu's commands would act on, for its title.
///
/// The selection, when there is one — the commands prefer it over the cursor,
/// so the title has to as well or the box would name a window it will not
/// touch.
fn menu_target(model: &Model) -> String {
    let (windows, panes) = model.mark_counts();
    let mut parts = Vec::new();
    if windows > 0 {
        parts.push(format!("{windows} window{}", plural(windows)));
    }
    if panes > 0 {
        parts.push(format!("{panes} pane{}", plural(panes)));
    }
    if !parts.is_empty() {
        return format!("{} marked", parts.join(", "));
    }
    match (model.current_pane(), model.current_window()) {
        (Some(p), _) => p.target().to_string(),
        (None, Some(w)) => format!("{}  {}", w.target(), w.name),
        _ => "nothing selected".to_string(),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A box of this size in the middle of `area`.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
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
    // The search sits in the header rather than a separate line: it is the
    // most important thing on screen while typing, and a mode line that
    // appears and disappears makes the tree jump.
    if model.searching {
        summary.push(Span::styled(
            format!("   {}", model.search),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        summary.push(Span::styled("▌", Style::default().fg(Color::Yellow)));
    } else if !model.search.is_empty() {
        // Stepped out to the tree with a filter still applied. The slash marks
        // it as a filter rather than part of the summary, and the missing
        // cursor says typing will no longer land here.
        summary.push(Span::styled(
            format!("   /{}", model.search),
            Style::default().fg(Color::Yellow),
        ));
    }
    if model.waiting > 0 {
        summary.push(Span::styled(
            format!("  {} waiting", model.waiting),
            Style::default().fg(Color::Yellow),
        ));
    }
    // Windows and panes are marked with the same key but act differently —
    // `M` merges the windows, `J` moves the panes — so the header counts them
    // apart rather than reporting one number for two selections.
    let (windows_marked, panes_marked) = model.mark_counts();
    if windows_marked > 0 {
        summary.push(Span::raw(format!("  {windows_marked} marked")));
    }
    if panes_marked > 0 {
        summary.push(Span::raw(format!("  {panes_marked} panes marked")));
    }
    // A binary older than the source is invisible otherwise: you notice it by
    // wondering why a feature you just wrote does nothing. Says so rather than
    // letting the reader debug the wrong thing.
    if let Some(newer) = &model.stale_build {
        summary.push(Span::styled(
            format!("   this build is {} — run make install", newer),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
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
    let visible = model.visible();
    let height = area.height.saturating_sub(2) as usize;
    let offset = scroll_offset(model.cursor, visible.len(), height);

    let mut lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(pos, &i)| {
            render_row(
                &model.rows[i],
                pos == model.cursor,
                model,
                area.width as usize,
            )
        })
        .collect();

    if visible.is_empty() && !model.search.is_empty() {
        lines.push(Line::styled(
            format!("  no window matches \"{}\"", model.search),
            Style::default().fg(Color::DarkGray),
        ));
    }

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

fn render_row<'a>(row: &'a Row, selected: bool, model: &Model, width: usize) -> Line<'a> {
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
        Row::Pane(p) => {
            // Indented under its window, addressed by pane id — that is what
            // the pane commands take, and showing it makes the target of
            // `b`/`J` unambiguous rather than "whichever tmux considers
            // active".
            let cursor = if selected { '>' } else { ' ' };
            // Same column as a window's mark, so a mixed selection reads as
            // one list rather than two conventions.
            let mark = if model.marks.contains(p.target()) {
                '#'
            } else {
                ' '
            };
            let command = if p.command.is_empty() {
                "(shell)".to_string()
            } else {
                p.command.clone()
            };
            Line::from(vec![
                Span::raw(format!("{cursor}{mark}   ")),
                Span::styled(
                    format!("{:<5}", p.pane_id),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    if p.active { "· " } else { "  " },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    // The pane id and markers take a fixed prefix; the rest is
                    // the command, cut to what is left. Three claude panes in
                    // one window are indistinguishable otherwise.
                    truncate(&command, width.saturating_sub(13)),
                    if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    },
                ),
            ])
        }
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
                    // Padded to the display width the name actually occupies,
                    // so a Korean window name does not shift the columns after
                    // it — `{:<14}` counts characters, not columns.
                    pad(&truncate(&w.name, 14), 14),
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
    let Some(w) = model.current_window() else {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "select a window",
                Style::default().fg(Color::DarkGray),
            )),
            area,
        );
        return;
    };

    // Identity and diff first — they are why the panel is here — then whatever
    // the window is showing. The capture gets the remaining height because it
    // is what tells one `fish` prompt from another; the metadata above is a
    // handful of lines and fixed.
    // Name the pane when one is selected: with three identical claude
    // commands in a window, the id is the only thing that says which output
    // below belongs to which row above.
    let mut head: Vec<Line> = vec![match model.current_pane() {
        Some(p) => Line::from(vec![
            Span::styled(
                p.target().to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{} {}", p.window_target(), w.name),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        None => Line::from(vec![
            Span::styled(w.target(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(w.name.clone(), Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                format!("{}p", w.panes),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    }];

    match model.current_pane() {
        Some(p) => head.push(Line::from(vec![
            Span::styled("  cwd ", Style::default().fg(Color::DarkGray)),
            Span::raw(p.path.clone()),
        ])),
        None => {
            for reason in &w.reasons {
                head.push(Line::styled(format!("  {reason}"), style_for(w.change)));
            }
        }
    }
    if !w.state.is_empty() {
        head.push(Line::from(vec![
            Span::styled("  claude ", Style::default().fg(Color::DarkGray)),
            Span::raw(w.state.clone()),
            Span::styled(
                format!("  {}", &w.cc_session[..8.min(w.cc_session.len())]),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    if !model.status.is_empty() {
        head.push(Line::styled(
            model.status.clone(),
            Style::default().fg(Color::Yellow),
        ));
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(head.len() as u16 + 1),
            Constraint::Min(0),
        ])
        .split(area);
    frame.render_widget(Paragraph::new(head), rows[0]);

    render_preview(frame, rows[1], model, w);
}

/// What the window is showing right now.
fn render_preview(frame: &mut Frame, area: Rect, model: &Model, w: &super::model::WindowRow) {
    if area.height == 0 {
        return;
    }

    // Keyed by whatever `app::refresh_preview` captured: the pane id when one
    // is selected, the window otherwise. Asking by window while a pane is
    // selected silently rendered "(nothing on screen)".
    let key = match model.current_pane() {
        Some(p) => p.target().to_string(),
        None => w.target(),
    };
    let body = match model.preview_for(&key) {
        Some(body) => body,
        None if w.gone => {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "  not running — press r to restore it",
                    Style::default().fg(Color::DarkGray),
                )),
                area,
            );
            return;
        }
        None => "",
    };

    let mut lines: Vec<Line> = vec![Line::styled(
        "─ output ".to_string() + &"─".repeat(area.width.saturating_sub(9) as usize),
        Style::default().fg(Color::DarkGray),
    )];

    // Dim, because the preview is context rather than this app's own output —
    // and full-brightness terminal text next to the tree reads as a second UI.
    let captured = crate::collect::tmux::tail_lines(body, area.height.saturating_sub(1) as usize);
    if captured.is_empty() {
        lines.push(Line::styled(
            "  (nothing on screen)",
            Style::default().fg(Color::DarkGray),
        ));
    }
    for line in captured {
        lines.push(Line::styled(
            truncate(line, area.width as usize),
            Style::default().fg(Color::Gray),
        ));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_keys(frame: &mut Frame, area: Rect, model: &Model) {
    // While the menu is up its letters mean the menu's commands. Leaving the
    // tree's hints on screen would advertise `j/k move` at the moment `j`
    // joins panes into a window.
    if model.menu {
        frame.render_widget(
            Paragraph::new("a letter runs it   esc cancel")
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    if let Some(moving) = &model.pending_move {
        let what = match moving.panes.as_slice() {
            [(pane, from)] => format!("moving {pane} from {from}"),
            panes => format!(
                "moving {} panes from {}",
                panes.len(),
                moving.sources().join(", ")
            ),
        };
        let text = if model.searching {
            format!("{what}   /{}▌   ⏎ confirm   esc filter off", model.search)
        } else {
            format!("{what}   j/k window   / filter   ⏎ confirm   esc cancel")
        };
        frame.render_widget(
            Paragraph::new(truncate(&text, area.width as usize))
                .style(Style::default().fg(Color::Yellow)),
            area,
        );
        return;
    }

    // Searching is where the TUI starts, so its hint has to name the way out
    // and the way to everything else — otherwise the other twelve keys are
    // undiscoverable from the screen you land on.
    if model.searching {
        let text = "type to search   ↑↓ move   ⏎ switch   tab/esc commands   ctrl-c quit";
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let restore = if model.marks.is_empty() {
        "r restore all"
    } else {
        "r restore marked"
    };
    // Ordered by how often each is reached for. A truncated hint line is
    // worse than a short one — it cuts mid-word and hides `q quit` — so the
    // tail is dropped group by group until it fits.
    let groups = [
        "/ search  j/k move  ⏎ switch".to_string(),
        format!("space mark  a all  {restore}"),
        // Expanding comes before the rarer commands: it is how you reach a
        // pane at all, and a hint that drops it first leaves `b`/`J` looking
        // like they act on nothing.
        "l/h panes".to_string(),
        "s save  n waiting".to_string(),
        // One entry now, because the menu behind it is self-describing --
        // and because a hint line that spelled all five was the thing that
        // pushed `p/P point` off the end of a narrow terminal.
        "x window/pane".to_string(),
        "p/P point".to_string(),
    ];

    let mut text = String::new();
    for group in &groups {
        let candidate = if text.is_empty() {
            group.clone()
        } else {
            format!("{text}   {group}")
        };
        // Leave room for the quit hint, which is always shown: leaving without
        // a visible way out is the one thing the hint line must never do.
        const QUIT: &str = "   esc/q quit";
        if candidate.chars().count() + QUIT.chars().count() > area.width as usize {
            break;
        }
        text = candidate;
    }
    text.push_str("   esc/q quit");

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

/// Cut a string to fit `width` terminal columns.
///
/// Measured in display width, not characters: CJK text is two columns per
/// character, so counting characters overflows the panel by up to double.
/// Window names and captured output here are routinely Korean.
fn truncate(s: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    if s.width() <= width {
        return s.to_string();
    }
    // Leave a column for the ellipsis.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Pad to `width` terminal columns.
fn pad(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut out = s.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(s.width())));
    out
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
    fn truncation_counts_terminal_columns_not_characters() {
        use unicode_width::UnicodeWidthStr;
        // Korean is two columns per character. Counting characters would let
        // a name occupy twice its budget and push the panel border off.
        let cut = truncate("한글이름테스트", 8);
        assert!(cut.width() <= 8, "{cut:?} is {} columns", cut.width());
        assert_eq!(cut, "한글이…");
    }

    #[test]
    fn padding_measures_columns_too() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(pad("abc", 6).width(), 6);
        assert_eq!(pad("한글", 6).width(), 6, "two chars, four columns");
    }
}
