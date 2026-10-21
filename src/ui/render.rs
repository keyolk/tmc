//! Drawing the tree and the diff panel.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::guard;
use super::keys::{self, Ctx};
use super::model::{Model, Row, actions};
use super::split::{Config, Split};
use super::theme::Theme;
use crate::clock;
use crate::layout::diff::Change;

/// Below this many columns the detail panel is dropped rather than squeezed.
const DETAIL_MIN_WIDTH: u16 = 90;
/// Below this the app says so instead of rendering garbage.
const MIN_WIDTH: u16 = 50;
const MIN_HEIGHT: u16 = 10;

/// The tree | detail split as it was before it could be resized: 55/45 from
/// `DETAIL_MIN_WIDTH` up, the tree alone below it. The divider is the one
/// column the tree's right border used to take, so the default look is
/// unchanged; the minimums keep a tree row and the detail readable.
pub(super) fn tree_split() -> Split {
    Split::new(Config {
        divider: 1,
        collapse: DETAIL_MIN_WIDTH,
        default: 0.55,
        min_a: 30,
        min_b: 30,
        lo: 0.2,
        hi: 0.8,
        ..Config::default()
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Regions {
    pub header: Rect,
    pub body: Rect,
    pub tree: Rect,
    /// Between the tree and the detail; in neither pane.
    pub divider: Rect,
    pub detail: Rect,
    pub keys: Rect,
}

/// Geometry shared by pure rendering, mouse hit testing and drag selection.
pub(super) fn regions(area: Rect, split: &Split) -> Regions {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return Regions::default();
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    let (tree, divider, detail) = split.layout(rows[1]);
    Regions {
        header: rows[0],
        body: rows[1],
        tree,
        divider,
        detail,
        keys: rows[2],
    }
}

pub fn draw(frame: &mut Frame, model: &Model, now_secs: u64) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("terminal too small").style(model.theme.error()),
            area,
        );
        return;
    }

    let regions = regions(area, &model.split);

    // Each pane is guarded: one that panics draws a line saying so, and the
    // rest of the frame -- the key row with the way out above all -- still
    // draws. See `guard`.
    let t = &model.theme;
    guard::pane(frame, regions.header, "header", t, false, |f| {
        render_header(f, regions.header, model, now_secs)
    });

    guard::pane(frame, regions.tree, "tree", t, false, |f| {
        render_tree(f, regions.tree, model)
    });
    if regions.detail.width > 0 {
        guard::pane(frame, regions.detail, "detail", t, false, |f| {
            render_detail(f, regions.detail, model)
        });
    }
    model.split.draw_divider(
        frame.buffer_mut(),
        regions.divider,
        t.glyphs.border.vertical_right,
        t.border(),
    );

    guard::pane(frame, regions.keys, "keys", t, false, |f| {
        render_keys(f, regions.keys, model)
    });

    // Last, so it sits over the tree and the panel both. A failed overlay
    // gives the tree back underneath its fallback line.
    if model.menu {
        guard::pane(frame, regions.body, "menu", t, true, |f| {
            render_menu(f, regions.body, model)
        });
    }
    if model.help {
        guard::pane(frame, regions.body, "help", t, true, |f| {
            render_help(f, regions.body, t, model.help_scroll)
        });
    }

    // Over everything, so the highlight shows whatever the panes drew.
    let panes = super::drag::panes(area, model);
    model.selection.paint(frame.buffer_mut(), &panes);
}

/// The `x` menu: every window and pane command, with what each would act on.
///
/// Floating rather than a panel swap, because it is momentary and because
/// covering the tree is the point — the commands act on the selection, which
/// the title line names, not on whatever the cursor is passing over.
pub(super) fn menu_area(body: Rect, model: &Model) -> Rect {
    let entries = actions(model);
    let title = format!(" x  {} ", menu_target(model));
    let footer = keys::footer(Ctx::Menu, model, usize::MAX);
    let widest_key = entries
        .iter()
        .map(|a| format!("{}:{}", a.key, a.label).width())
        .max()
        .unwrap_or(0);
    let widest_detail = entries.iter().map(|a| a.detail.width()).max().unwrap_or(0);
    const GAP: usize = 3;
    let inner = (widest_key + GAP + widest_detail)
        .max(title.width())
        .max(footer.width());
    let width = (inner as u16 + 4).min(body.width);
    let height = (entries.len() as u16 + 4).min(body.height);
    centred(body, width, height)
}

fn render_menu(frame: &mut Frame, body: Rect, model: &Model) {
    let t = &model.theme;
    let ell = t.glyphs.ellipsis;
    let entries = actions(model);
    let title = format!(" x  {} ", menu_target(model));
    let footer = keys::footer(Ctx::Menu, model, usize::MAX);

    // Sized to its contents: the detail column is right-aligned, so the box
    // has to know the widest row before it can place anything.
    let keys: Vec<String> = entries
        .iter()
        .map(|a| format!("{}:{}", a.key, a.label))
        .collect();
    let widest_key = keys.iter().map(|k| k.width()).max().unwrap_or(0);
    const GAP: usize = 3;
    let area = menu_area(body, model);

    let usable = area.width.saturating_sub(4) as usize;
    // The box is capped by the terminal, so on a narrow one the detail is
    // what gives. Cut explicitly rather than letting the paragraph clip it:
    // `needs a pane - l expan` reads as a typo, `needs a pane…` as a cut.
    let budget = usable.saturating_sub(widest_key + GAP);
    let mut lines: Vec<Line> = entries
        .iter()
        .zip(&keys)
        .map(|(action, key)| {
            let detail = truncate(&action.detail, budget, ell);
            let pad = usable.saturating_sub(widest_key + detail.width()).max(1);
            let (key_style, label_style) = if action.enabled {
                (t.accent(), t.text())
            } else {
                // Greyed rather than hidden. With five commands the reason a
                // row is unavailable is the useful part — "needs two or more
                // marked" is how you learn merge takes a selection at all —
                // and a menu that changes shape teaches nothing.
                (t.subtle(), t.subtle())
            };
            let (k, label) = key.split_at(1);
            Line::from(vec![
                Span::styled(k.to_string(), key_style),
                Span::styled(label.to_string(), label_style),
                Span::raw(" ".repeat(widest_key - key.width() + pad)),
                Span::styled(detail, t.subtle()),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(footer, t.subtle()));

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(t.glyphs.border)
                .border_style(t.border_focus())
                .title(Span::styled(
                    truncate(&title, area.width.saturating_sub(4) as usize, ell),
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
    let t = &model.theme;
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
        summary.push(Span::raw("   "));
        // A restored query is drawn selected, the way a GUI search box
        // shows last time's text: typing replaces it.
        summary.push(Span::styled(
            model.search.clone(),
            if model.search_selected {
                t.replaceable()
            } else {
                t.accent()
            },
        ));
        summary.push(Span::styled(t.glyphs.caret, t.accent()));
    } else if !model.search.is_empty() {
        // Stepped out to the tree with a filter still applied. The slash marks
        // it as a filter rather than part of the summary, and the missing
        // cursor says typing will no longer land here.
        summary.push(Span::styled(format!("   /{}", model.search), t.accent()));
    }
    if model.waiting > 0 {
        summary.push(Span::styled(
            format!("  {} waiting", model.waiting),
            t.warn(),
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
            t.error(),
        ));
    }

    let drift = Line::from(vec![
        Span::styled(point, t.subtle()),
        Span::raw("   "),
        Span::styled(format!("~{modified}"), style_for(t, Change::Modified)),
        Span::raw(" "),
        Span::styled(format!("+{added}"), style_for(t, Change::Added)),
        Span::raw(" "),
        Span::styled(format!("-{removed}"), style_for(t, Change::Removed)),
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
            model.theme.subtle(),
        ));
    }

    // No border of its own: the split's divider is drawn beside it.
    frame.render_widget(Paragraph::new(lines), area);
}

/// Keep the cursor in view without jumping the list around.
pub(super) fn scroll_offset(cursor: usize, total: usize, height: usize) -> usize {
    if total <= height || height == 0 {
        return 0;
    }
    // Centre the cursor once it passes the midpoint, then stop at the end.
    let half = height / 2;
    cursor.saturating_sub(half).min(total - height)
}

fn render_row<'a>(row: &'a Row, selected: bool, model: &Model, width: usize) -> Line<'a> {
    let t = &model.theme;
    let ell = t.glyphs.ellipsis;
    match row {
        Row::Session { name, windows } => Line::from(vec![
            Span::styled(
                format!("{} {name}", t.glyphs.session),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {windows}w"), t.subtle()),
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
                Span::styled(format!("{:<5}", p.pane_id), t.subtle()),
                // `*` rather than `·`: the command column starts right after
                // this, and `·` is East Asian Ambiguous -- two cells on a CJK
                // terminal, which would push every active pane's command one
                // column right of its siblings'. tmux marks its current window
                // with `*` too.
                Span::styled(if p.active { "* " } else { "  " }, t.subtle()),
                Span::styled(
                    // The pane id and markers take a fixed prefix; the rest is
                    // the command, cut to what is left. Three claude panes in
                    // one window are indistinguishable otherwise.
                    truncate(&command, width.saturating_sub(13), ell),
                    if selected { t.selected() } else { t.muted() },
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
            // `-`, not `—`: the dash is East Asian Ambiguous and would shift
            // the name column on a CJK terminal.
            let index = if w.gone {
                "  -".to_string()
            } else {
                format!("{:>3}", w.index)
            };

            let mut spans = vec![
                Span::raw(format!("{cursor}{mark} ")),
                Span::styled(
                    w.state_glyph().to_string(),
                    if w.waiting { t.warn() } else { t.subtle() },
                ),
                Span::raw(format!(" {index} ")),
                Span::styled(
                    // Padded to the display width the name actually occupies,
                    // so a Korean window name does not shift the columns after
                    // it — `{:<14}` counts characters, not columns.
                    pad(&truncate(&w.name, 14, ell), 14),
                    if w.gone {
                        t.subtle().add_modifier(Modifier::CROSSED_OUT)
                    } else if selected {
                        t.selected()
                    } else {
                        t.text()
                    },
                ),
                Span::styled(format!("{:>2}p ", w.panes), t.subtle()),
                Span::styled(w.change.marker().to_string(), style_for(t, w.change)),
            ];
            if !w.cc_session.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", short_id(&w.cc_session)),
                    t.subtle(),
                ));
            }
            Line::from(spans)
        }
    }
}

pub(super) fn detail_preview_area(area: Rect, model: &Model) -> Rect {
    let Some(window) = model.current_window() else {
        return Rect::default();
    };
    let mut head = 1usize;
    match model.current_pane() {
        Some(_) => head += 1, // cwd
        None => head += window.reasons.len(),
    }
    if !window.state.is_empty() {
        head += 1;
    }
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(head as u16 + 1), Constraint::Min(0)])
        .split(area)[1]
}

fn render_detail(frame: &mut Frame, area: Rect, model: &Model) {
    let t = &model.theme;
    let Some(w) = model.current_window() else {
        frame.render_widget(
            Paragraph::new(Line::styled("select a window", t.subtle())),
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
            Span::styled(format!("{} {}", p.window_target(), w.name), t.subtle()),
        ]),
        None => Line::from(vec![
            Span::styled(w.target(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(w.name.clone(), t.subtle()),
            Span::raw("  "),
            Span::styled(format!("{}p", w.panes), t.subtle()),
        ]),
    }];

    match model.current_pane() {
        Some(p) => head.push(Line::from(vec![
            Span::styled("  cwd ", t.subtle()),
            Span::raw(p.path.clone()),
        ])),
        None => {
            for reason in &w.reasons {
                head.push(Line::styled(format!("  {reason}"), style_for(t, w.change)));
            }
        }
    }
    if !w.state.is_empty() {
        head.push(Line::from(vec![
            Span::styled("  claude ", t.subtle()),
            Span::raw(w.state.clone()),
            Span::styled(format!("  {}", short_id(&w.cc_session)), t.subtle()),
        ]));
    }
    let preview = detail_preview_area(area, model);
    let head_area = Rect {
        height: preview.y.saturating_sub(area.y),
        ..area
    };
    frame.render_widget(Paragraph::new(head), head_area);

    render_preview(frame, preview, model, w);
}

/// What the window is showing right now.
fn render_preview(frame: &mut Frame, area: Rect, model: &Model, w: &super::model::WindowRow) {
    if area.height == 0 {
        return;
    }
    let t = &model.theme;
    let rule = format!(
        "{r} output {}",
        t.glyphs.rule.repeat(area.width.saturating_sub(9) as usize),
        r = t.glyphs.rule
    );

    let body = match model.preview_for(&preview_key(model, w)) {
        Some(body) => body,
        None if w.gone => {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "  not running — press r to restore it",
                    t.subtle(),
                )),
                area,
            );
            return;
        }
        // Still being captured. An empty body would say "(nothing on
        // screen)" for the moment it takes, which is a claim, not a wait.
        None => {
            frame.render_widget(Paragraph::new(Line::styled(rule, t.subtle())), area);
            return;
        }
    };

    let mut lines: Vec<Line> = vec![Line::styled(rule, t.subtle())];

    // Dim, because the preview is context rather than this app's own output —
    // and full-brightness terminal text next to the tree reads as a second UI.
    let (all, start, end) = preview_window(body, model.preview_scroll, area);
    let captured = &all[start..end];
    if captured.is_empty() {
        lines.push(Line::styled("  (nothing on screen)", t.subtle()));
    }
    for line in captured {
        lines.push(Line::styled(
            truncate(line, area.width as usize, t.glyphs.ellipsis),
            t.muted(),
        ));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Which capture the preview shows. Keyed by whatever `app::preview_target`
/// asked for: the pane id when one is selected, the window otherwise. Asking
/// by window while a pane is selected silently rendered "(nothing on screen)".
pub(super) fn preview_key(model: &Model, w: &super::model::WindowRow) -> String {
    match model.current_pane() {
        Some(p) => p.target().to_string(),
        None => w.target(),
    }
}

/// The captured lines, and the range of them drawn below the rule of the
/// preview `area`. Shared with drag selection, which needs the same offset.
pub(super) fn preview_window(body: &str, scroll: usize, area: Rect) -> (Vec<&str>, usize, usize) {
    let height = area.height.saturating_sub(1) as usize;
    let all = crate::collect::tmux::tail_lines(body, usize::MAX);
    let scroll = scroll.min(all.len().saturating_sub(height));
    let end = all.len().saturating_sub(scroll);
    let start = end.saturating_sub(height);
    (all, start, end)
}

fn render_keys(frame: &mut Frame, area: Rect, model: &Model) {
    let t = &model.theme;
    let ell = t.glyphs.ellipsis;
    // The status takes the key row while it is set. It used to live in the
    // detail panel, which is dropped below 90 columns and empty when no window
    // is selected -- exactly the popup sizes and the empty search where a
    // "killed 2 window(s)" or a failed reload most needs saying. The row is
    // always on screen, and any key clears the status, so the hints it covers
    // come back on the next keystroke.
    if !model.status.is_empty() {
        let (prefix, style) = t.status(model.status.level());
        let text = format!("{prefix} {}", model.status.text());
        frame.render_widget(
            Paragraph::new(truncate(&text, area.width as usize, ell)).style(style),
            area,
        );
        return;
    }

    let width = area.width as usize;
    let ctx = keys::context(model);

    // The hints are derived from the binding table for whatever context is
    // live: while the menu is up its letters mean the menu's commands, and
    // leaving the tree's hints on screen would advertise `j/k move` at the
    // moment `j` joins panes into a window.
    if let Some(moving) = &model.pending_move {
        let what = match moving.panes.as_slice() {
            [(pane, from)] => format!("moving {pane} from {from}"),
            panes => format!(
                "moving {} panes from {}",
                panes.len(),
                moving.sources().join(", ")
            ),
        };
        let what = if model.searching {
            format!("{what}   /{}{}", model.search, t.glyphs.caret)
        } else {
            what
        };
        let room = width.saturating_sub(what.width() + 3);
        let text = format!("{what}   {}", keys::footer(ctx, model, room));
        frame.render_widget(
            Paragraph::new(truncate(&text, width, ell)).style(t.warn()),
            area,
        );
        return;
    }

    // A command running on the worker says so ahead of the hints, which stay:
    // navigation still works while it runs.
    let mut lead = match model.busy {
        Some(label) => format!("{label}{ell}   "),
        None => String::new(),
    };
    match ctx {
        Ctx::Menu => lead.push_str("a letter runs it   "),
        // Searching is where the TUI starts, so its hint has to name the way
        // out and the way to everything else.
        Ctx::Search => lead.push_str("type to search   "),
        _ => {}
    }
    let room = width.saturating_sub(lead.width());
    let text = format!("{lead}{}", keys::footer(ctx, model, room));
    let style = if model.busy.is_some() {
        t.warn()
    } else {
        t.subtle()
    };
    frame.render_widget(
        Paragraph::new(truncate(&text, width, ell)).style(style),
        area,
    );
}

/// `?` help, derived from the binding table: every context, every key.
///
/// Drawn over the body like the `x` menu, so the tree it describes stays in
/// view around it. Scrolled with j/k on a short terminal rather than
/// squeezed; the hint row below names both.
pub(super) fn help_area(body: Rect, t: &Theme) -> Rect {
    let sections = keys::help_lines(t.glyphs.key_glyphs);
    let widest_keys = sections
        .iter()
        .flat_map(|(_, lines)| lines.iter().map(|(k, _)| k.width()))
        .max()
        .unwrap_or(0);
    let widest_help = sections
        .iter()
        .flat_map(|(_, lines)| lines.iter().map(|(_, h)| h.width()))
        .max()
        .unwrap_or(0);
    let width = ((widest_keys + 2 + widest_help) as u16 + 4).min(body.width);
    let rows: usize = sections.iter().map(|(_, l)| l.len() + 2).sum();
    let height = (rows as u16 + 2).min(body.height);
    centred(body, width, height)
}

fn render_help(frame: &mut Frame, body: Rect, t: &Theme, scroll: usize) {
    let sections = keys::help_lines(t.glyphs.key_glyphs);
    let widest_keys = sections
        .iter()
        .flat_map(|(_, lines)| lines.iter().map(|(k, _)| k.width()))
        .max()
        .unwrap_or(0);
    let area = help_area(body, t);
    let usable = area.width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = Vec::new();
    for (title, entries) in &sections {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        for (k, h) in entries {
            let pad = widest_keys.saturating_sub(k.width()) + 2;
            let help = truncate(h, usable.saturating_sub(widest_keys + 2), t.glyphs.ellipsis);
            lines.push(Line::from(vec![
                Span::styled(k.clone(), t.accent()),
                Span::raw(" ".repeat(pad)),
                Span::raw(help),
            ]));
        }
    }

    // Past the end would scroll the box empty; stop with the last line at
    // the bottom.
    let inner = area.height.saturating_sub(2) as usize;
    let scroll = scroll.min(lines.len().saturating_sub(inner));
    let lines: Vec<Line> = lines.into_iter().skip(scroll).collect();

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(t.glyphs.border)
                .border_style(t.border_focus())
                .title(Span::styled(
                    " keys ",
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}

fn style_for(t: &Theme, change: Change) -> Style {
    match change {
        Change::Modified => t.info(),
        Change::Added => t.added(),
        Change::Removed => t.removed(),
        Change::Same => t.subtle(),
    }
}

/// Cut a string to fit `width` terminal columns.
///
/// Measured in display width, not characters: CJK text is two columns per
/// character, so counting characters overflows the panel by up to double.
/// Window names and captured output here are routinely Korean.
///
/// `marker` is the theme's ellipsis: `…`, or `~` where `…` would draw two
/// cells (a CJK locale) or not at all (ASCII).
pub(super) fn truncate(s: &str, width: usize, marker: &str) -> String {
    use unicode_width::UnicodeWidthChar;

    if s.width() <= width {
        return s.to_string();
    }
    // Leave room for the marker.
    let budget = width.saturating_sub(marker.width());
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
    out.push_str(marker);
    out
}

/// The first eight characters of a session id.
///
/// By character, not byte: `@cc_session` is a tmux option anything can set,
/// and slicing `[..8]` through a multi-byte character panics the draw.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Pad to `width` terminal columns.
fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(s.width())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_regions_follow_the_responsive_layout() {
        let narrow = regions(Rect::new(0, 0, 89, 24), &tree_split());
        assert_eq!(narrow.tree, narrow.body);
        assert_eq!(narrow.detail, Rect::default());

        let wide = regions(Rect::new(0, 0, 90, 24), &tree_split());
        assert!(wide.tree.width > 0);
        assert!(wide.detail.width > 0);
        assert_eq!(wide.divider.width, 1);
        assert_eq!(
            wide.tree.width + wide.divider.width + wide.detail.width,
            wide.body.width
        );
    }

    #[test]
    fn a_tiny_terminal_has_no_mouse_targets() {
        assert_eq!(
            regions(Rect::new(0, 0, 49, 9), &tree_split()).tree,
            Rect::default()
        );
    }

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
        assert_eq!(truncate("short", 10, "…"), "short");
        assert_eq!(truncate("a-very-long-window-name", 10, "…"), "a-very-lo…");
    }

    #[test]
    fn truncation_counts_terminal_columns_not_characters() {
        use unicode_width::UnicodeWidthStr;
        // Korean is two columns per character. Counting characters would let
        // a name occupy twice its budget and push the panel border off.
        let cut = truncate("한글이름테스트", 8, "…");
        assert!(cut.width() <= 8, "{cut:?} is {} columns", cut.width());
        assert_eq!(cut, "한글이…");
    }

    #[test]
    fn a_session_id_is_cut_by_character_not_byte() {
        assert_eq!(short_id("e399ae98-489d-4cc7"), "e399ae98");
        assert_eq!(short_id("세션아이디가길다는뜻"), "세션아이디가길다");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn padding_measures_columns_too() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(pad("abc", 6).width(), 6);
        assert_eq!(pad("한글", 6).width(), 6, "two chars, four columns");
    }
}
