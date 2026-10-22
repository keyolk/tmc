//! Drag selection wiring: which panes exist on screen, and what a selection
//! event does to the model.
//!
//! The pane rects come from the same helpers `render` draws with
//! (`regions`, `detail_preview_area`, `preview_window`, the overlay areas),
//! never from geometry recomputed here (E14).

use std::time::Instant;

use crossterm::event::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::model::Model;
use super::render;
use super::selection::{Kind, Pane};

/// The selectable panes of the frame drawn into `area`, in drawing order.
pub(super) fn panes(area: Rect, model: &Model) -> Vec<Pane> {
    let regions = render::regions(area, &model.split);
    if regions.body.width == 0 {
        return Vec::new();
    }
    // An open overlay is the only thing selectable: the base under a modal
    // must not be. Both boxes have a border and one column of padding.
    if model.help {
        let b = render::help_area(regions.body, &model.theme);
        return vec![Pane::new("help", inset(b, 2, 1), Kind::Text)];
    }
    if model.menu {
        let b = render::menu_area(regions.body, model);
        return vec![Pane::new("menu", inset(b, 2, 1), Kind::List)];
    }

    // The divider between tree and detail is in neither rect, so a drag
    // from the tree never copies it.
    let mut out = vec![
        Pane::new("header", regions.header, Kind::List),
        Pane::new("tree", regions.tree, Kind::List),
    ];
    if regions.detail.width > 0 {
        let preview = render::detail_preview_area(regions.detail, model);
        let head = if preview.height == 0 {
            regions.detail
        } else {
            Rect {
                height: preview.y.saturating_sub(regions.detail.y),
                ..regions.detail
            }
        };
        out.push(Pane::new("detail", head, Kind::Text));
        if preview.height > 1 {
            // Below the "output" rule.
            let rect = Rect {
                y: preview.y + 1,
                height: preview.height - 1,
                ..preview
            };
            let mut pane = Pane::new("preview", rect, Kind::Text);
            if let Some(body) = preview_body(model) {
                let (all, start, _) = render::preview_window(body, model.preview_scroll, preview);
                if !all.is_empty() {
                    pane = pane.with_lines(all.iter().map(|l| l.to_string()).collect(), start);
                }
            }
            out.push(pane);
        }
    }
    out.push(Pane::new("keys", regions.keys, Kind::List));
    out
}

fn inset(r: Rect, x: u16, y: u16) -> Rect {
    Rect {
        x: r.x + x,
        y: r.y + y,
        width: r.width.saturating_sub(2 * x),
        height: r.height.saturating_sub(2 * y),
    }
}

fn preview_body(model: &Model) -> Option<&str> {
    let w = model.current_window()?;
    model.preview_for(&render::preview_key(model, w))
}

/// What the event loop does after a selection event.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Fed {
    /// The selection owns the event; skip the click handling.
    pub consumed: bool,
    /// Text to put on the clipboard.
    pub copy: Option<String>,
}

/// Feed one mouse event to the selection, before the click handling.
/// `frame` is the last drawn buffer.
pub(super) fn feed(
    model: &mut Model,
    event: &MouseEvent,
    area: Rect,
    frame: &Buffer,
    now: Instant,
) -> Fed {
    let panes = panes(area, model);
    let out = model.selection.mouse(event, &panes, frame, now);
    if out.scroll != 0 && out.scroll_pane == "preview" {
        scroll_preview(model, &panes, out.scroll);
    }
    Fed {
        consumed: out.consumed,
        copy: out.copy.filter(|t| !t.is_empty()),
    }
}

/// What the event loop does with one mouse event.
pub(super) struct Dispatched {
    pub effect: super::app::Effect,
    pub copy: Option<String>,
    pub redraw: bool,
}

/// One mouse event: the split first, then the selection, then, unless one
/// of them consumed the event, the click handling it had before. A press
/// that did not move still reaches `mouse::handle` as a click.
pub(super) fn dispatch(
    model: &mut Model,
    event: MouseEvent,
    area: Rect,
    frame: &Buffer,
    now: Instant,
) -> Dispatched {
    // Only the base screen has a divider: an overlay owns every press.
    if !model.help && !model.menu {
        let body = render::regions(area, &model.split).body;
        let split = model.split.mouse(&event, body, now);
        if split.consumed {
            // The press never reaches the selection, which would otherwise
            // have dropped its old highlight: drop it here, or a stale
            // highlight is painted over panes that just moved.
            if matches!(
                event.kind,
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            ) {
                model.selection.clear();
            }
            return Dispatched {
                effect: super::app::Effect::None,
                copy: None,
                redraw: true,
            };
        }
    }
    let fed = feed(model, &event, area, frame, now);
    let mut out = Dispatched {
        effect: super::app::Effect::None,
        copy: fed.copy,
        redraw: fed.consumed,
    };
    if !fed.consumed && super::mouse::actionable(event.kind) {
        model.status.on_key(now);
        out.effect = super::mouse::handle(model, event, area);
        out.redraw = true;
    } else if matches!(event.kind, crossterm::event::MouseEventKind::Up(_)) {
        // A press dropped the old highlight; draw it gone.
        out.redraw = true;
    }
    out
}

/// `preview_scroll` counts lines above the newest frame, so a drag below
/// the pane (later lines) lowers it. Clamped first, as render clamps it, so
/// a stale large value does not swallow the step.
fn scroll_preview(model: &mut Model, panes: &[Pane], delta: i32) {
    let Some(p) = panes.iter().find(|p| p.id == "preview") else {
        return;
    };
    let total = p.lines.as_ref().map_or(0, Vec::len);
    let max = total.saturating_sub(p.rect.height as usize);
    let scroll = model.preview_scroll.min(max);
    model.preview_scroll = if delta > 0 {
        scroll.saturating_sub(delta as usize)
    } else {
        (scroll + delta.unsigned_abs() as usize).min(max)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::Change;
    use crate::ui::app::handle_key;
    use crate::ui::model::{Row, WindowRow};
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;
    use std::time::Duration;

    const W: u16 = 120;
    const H: u16 = 24;

    fn model() -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = (0..6)
            .map(|i| {
                Row::Window(WindowRow {
                    session: "s".into(),
                    index: i,
                    name: format!("win{i}"),
                    panes: 1,
                    state: String::new(),
                    cc_session: String::new(),
                    waiting: false,
                    running_claude: false,
                    change: Change::Same,
                    reasons: Vec::new(),
                    gone: false,
                })
            })
            .collect();
        m.preview = Some((
            "s:0".into(),
            (0..60).map(|i| format!("out-{i:02} tail\n")).collect(),
        ));
        m
    }

    fn draw(m: &Model) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(W, H)).unwrap();
        t.draw(|f| render::draw(f, m, 0)).unwrap();
        t.backend().buffer().clone()
    }

    fn row_text(b: &Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
    }

    /// The cell where `needle` was drawn, found in the rendered buffer.
    fn find(b: &Buffer, needle: &str) -> (u16, u16) {
        for y in 0..b.area.height {
            let row = row_text(b, y);
            if let Some(i) = row.find(needle) {
                return (row[..i].chars().count() as u16, y);
            }
        }
        panic!("{needle:?} not on screen");
    }

    fn ev(kind: MouseEventKind, (x, y): (u16, u16)) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn down(at: (u16, u16)) -> MouseEvent {
        ev(MouseEventKind::Down(MouseButton::Left), at)
    }
    fn drag(at: (u16, u16)) -> MouseEvent {
        ev(MouseEventKind::Drag(MouseButton::Left), at)
    }
    fn up(at: (u16, u16)) -> MouseEvent {
        ev(MouseEventKind::Up(MouseButton::Left), at)
    }

    /// Press, drag and release as the event loop does: the click handler only
    /// sees events the selection did not consume.
    fn gesture(m: &mut Model, events: &[MouseEvent], now: Instant) -> Option<String> {
        let area = Rect::new(0, 0, W, H);
        let mut copy = None;
        for e in events {
            let frame = draw(m);
            copy = dispatch(m, *e, area, &frame, now).copy.or(copy);
        }
        copy
    }

    #[test]
    fn a_drag_from_the_tree_toward_the_preview_stays_in_the_tree() {
        let mut m = model();
        let b = draw(&m);
        let from = find(&b, "win1");
        let into = find(&b, "out-59");
        let copy = gesture(&mut m, &[down(from), drag(into), up(into)], Instant::now()).unwrap();
        assert!(copy.contains("win1"), "{copy:?}");
        assert!(!copy.contains("out-"), "neighbor text leaked: {copy:?}");
        assert!(!copy.contains('│'), "border leaked: {copy:?}");

        let painted = draw(&m);
        let border_x = into.0 - 1; // the tree's right border sits left of the detail
        let (_, y) = find(&painted, "win2");
        for x in border_x..W {
            assert!(
                !painted[(x, y)].modifier.contains(Modifier::REVERSED),
                "highlight at {x},{y} past the tree: {:?}",
                row_text(&painted, y)
            );
        }
        assert!(painted[(from.0, y)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn a_drag_past_the_preview_bottom_scrolls_and_keeps_the_scrolled_out_lines() {
        let mut m = model();
        m.preview_scroll = 10;
        let b = draw(&m);
        let first = row_text(&b, find(&b, "out-").1);
        let first_id = first[first.find("out-").unwrap()..][..6].to_string();
        let from = find(&b, &first_id);
        let below = (from.0 + 3, H + 2);
        let now = Instant::now();
        gesture(
            &mut m,
            &[down(from), drag(below), drag(below), drag(below)],
            now,
        );
        assert_eq!(
            m.preview_scroll, 7,
            "three drag steps below scroll three lines"
        );
        let b = draw(&m);
        let last = find(&b, "out-52");
        let copy = gesture(&mut m, &[up(last)], now).unwrap();
        assert!(copy.starts_with(&first_id), "{copy:?}");
        assert!(copy.contains("out-50 tail\nout-51"), "{copy:?}");
        assert!(copy.lines().count() > 3, "{copy:?}");
    }

    #[test]
    fn a_press_without_motion_is_still_the_tree_click() {
        let mut m = model();
        let b = draw(&m);
        let at = find(&b, "win3");
        let copy = gesture(&mut m, &[down(at), up(at)], Instant::now());
        assert_eq!(copy, None);
        assert_eq!(m.cursor, 3);
        assert!(!m.selection.active());
    }

    #[test]
    fn a_second_click_on_a_tree_row_stays_a_click() {
        let mut m = model();
        let b = draw(&m);
        let at = find(&b, "win2");
        let t = Instant::now();
        gesture(&mut m, &[down(at), up(at)], t);
        m.cursor = 0;
        gesture(&mut m, &[down(at)], t + Duration::from_millis(100));
        assert_eq!(m.cursor, 2, "the list's second click must reach the app");
        assert!(!m.selection.active());
    }

    #[test]
    fn a_double_click_in_the_preview_copies_a_word() {
        let mut m = model();
        let b = draw(&m);
        let at = find(&b, "out-59");
        let t = Instant::now();
        gesture(&mut m, &[down(at), up(at)], t);
        let copy = gesture(&mut m, &[down(at)], t + Duration::from_millis(100));
        assert_eq!(copy.as_deref(), Some("out-59"));
    }

    #[test]
    fn a_key_clears_the_selection() {
        let mut m = model();
        let b = draw(&m);
        let from = find(&b, "out-58");
        let to = find(&b, "out-59");
        gesture(&mut m, &[down(from), drag(to), up(to)], Instant::now());
        assert!(m.selection.active());
        handle_key(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(!m.selection.active());
    }

    #[test]
    fn with_help_open_the_tree_is_not_selectable() {
        let mut m = model();
        m.help = true;
        let b = draw(&m);
        // The tree's first row is still visible left of the help box.
        let at = find(&b, "win0");
        let copy = gesture(
            &mut m,
            &[down(at), drag((at.0 + 3, at.1 + 1))],
            Instant::now(),
        );
        assert_eq!(copy, None);
        assert!(!m.selection.active(), "the base under help was selected");
    }

    #[test]
    fn a_drag_in_help_copies_help_text_only() {
        let mut m = model();
        m.help = true;
        let b = draw(&m);
        let (_, y) = find(&b, " keys ");
        let from = (0, y + 1);
        let to = (W - 1, y + 2);
        let copy = gesture(&mut m, &[down(from), drag(to), up(to)], Instant::now());
        // A press left of the box is outside every pane; it closes help as
        // before and selects nothing.
        assert_eq!(copy, None);
        assert!(!m.help);
    }
}
