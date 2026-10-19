//! Mouse hit testing and interaction.
//!
//! Rendering remains pure. This module reproduces only the geometry needed to
//! translate a terminal coordinate into the same model commands keys use.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::app::{Effect, run_cmd};
use super::keys::Cmd;
use super::model::{Model, actions};
use super::render;

const WHEEL_ROWS: isize = 3;

pub fn actionable(kind: MouseEventKind) -> bool {
    matches!(
        kind,
        MouseEventKind::Down(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
    )
}

/// Handle one mouse event. Unknown buttons and regions deliberately do nothing
/// so terminal selection modifiers remain available to the terminal emulator.
pub fn handle(model: &mut Model, event: MouseEvent, area: Rect) -> Effect {
    let point = (event.column, event.row);

    if model.help {
        return help(model, event.kind, point, area);
    }
    if model.menu {
        return menu(model, event.kind, point, area);
    }

    let regions = render::regions(area);
    if regions.header.contains(point.into()) {
        return match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                model.searching = true;
                model.search_selected = !model.search.is_empty();
                Effect::None
            }
            _ => Effect::None,
        };
    }
    if regions.detail.contains(point.into()) {
        let max_scroll = preview_body(model)
            .map(|body| {
                crate::collect::tmux::tail_lines(body, usize::MAX)
                    .len()
                    .saturating_sub(1)
            })
            .unwrap_or(0);
        match event.kind {
            MouseEventKind::ScrollUp => {
                model.preview_scroll = model
                    .preview_scroll
                    .saturating_add(WHEEL_ROWS as usize)
                    .min(max_scroll)
            }
            MouseEventKind::ScrollDown => {
                model.preview_scroll = model.preview_scroll.saturating_sub(WHEEL_ROWS as usize)
            }
            _ => {}
        }
        return Effect::None;
    }
    if !regions.tree.contains(point.into()) {
        return Effect::None;
    }

    match event.kind {
        MouseEventKind::ScrollDown => move_selection(model, WHEEL_ROWS),
        MouseEventKind::ScrollUp => move_selection(model, -WHEEL_ROWS),
        MouseEventKind::Down(MouseButton::Left) => {
            select_tree(model, event.row, regions.tree);
            Effect::None
        }
        MouseEventKind::Down(MouseButton::Right | MouseButton::Middle) => {
            if select_tree(model, event.row, regions.tree) {
                model.toggle_mark();
            }
            Effect::None
        }
        _ => Effect::None,
    }
}

fn preview_body(model: &Model) -> Option<&str> {
    let target = match (model.current_pane(), model.current_window()) {
        (Some(p), _) => p.target().to_string(),
        (None, Some(w)) if !w.gone => w.target(),
        _ => return None,
    };
    model.preview_for(&target)
}

fn move_selection(model: &mut Model, delta: isize) -> Effect {
    if model.pending_move.is_some() {
        model.move_destination(delta);
    } else {
        model.move_cursor(delta);
    }
    Effect::None
}

fn select_tree(model: &mut Model, row: u16, tree: Rect) -> bool {
    let height = tree.height.saturating_sub(2) as usize;
    let visible = model.visible();
    let offset = render::scroll_offset(model.cursor, visible.len(), height);
    let local = row.saturating_sub(tree.y) as usize;
    if local >= height {
        return false;
    }
    let position = offset + local;
    if model.pending_move.is_some() {
        model.select_destination_visible(position)
    } else {
        model.select_visible(position)
    }
}

fn help(model: &mut Model, kind: MouseEventKind, point: (u16, u16), area: Rect) -> Effect {
    let body = render::regions(area).body;
    let box_area = render::help_area(body, &model.theme);
    match kind {
        MouseEventKind::ScrollDown if box_area.contains(point.into()) => {
            model.help_scroll += WHEEL_ROWS as usize;
        }
        MouseEventKind::ScrollUp if box_area.contains(point.into()) => {
            model.help_scroll = model.help_scroll.saturating_sub(WHEEL_ROWS as usize);
        }
        MouseEventKind::Down(MouseButton::Left) if !box_area.contains(point.into()) => {
            model.help = false;
        }
        _ => {}
    }
    Effect::None
}

fn menu(model: &mut Model, kind: MouseEventKind, point: (u16, u16), area: Rect) -> Effect {
    let body = render::regions(area).body;
    let box_area = render::menu_area(body, model);
    if matches!(kind, MouseEventKind::Down(MouseButton::Left)) && !box_area.contains(point.into()) {
        model.menu = false;
        return Effect::None;
    }
    if !matches!(kind, MouseEventKind::Down(MouseButton::Left)) || !box_area.contains(point.into())
    {
        return Effect::None;
    }

    // Border + one horizontal padding column. Each action owns one row.
    let action_row = point.1.saturating_sub(box_area.y + 1) as usize;
    let entries = actions(model);
    let Some(action) = entries.get(action_row) else {
        return Effect::None;
    };
    model.menu = false;
    run_cmd(model, Cmd::Run(action.kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::Change;
    use crate::ui::model::{MoveKind, PendingMove, Row, WindowRow};
    use crossterm::event::KeyModifiers;

    fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn model() -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = (0..12)
            .map(|i| {
                Row::Window(WindowRow {
                    session: "s".into(),
                    index: i,
                    name: format!("w{i}"),
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
        m
    }

    #[test]
    fn a_tree_click_selects_the_row_under_it() {
        let mut m = model();
        let area = Rect::new(0, 0, 80, 20);
        let tree = render::regions(area).tree;
        handle(
            &mut m,
            event(MouseEventKind::Down(MouseButton::Left), 2, tree.y + 4),
            area,
        );
        assert_eq!(m.cursor, 4);
    }

    #[test]
    fn the_wheel_moves_three_rows_only_over_the_tree() {
        let mut m = model();
        let area = Rect::new(0, 0, 100, 20);
        let regions = render::regions(area);
        handle(
            &mut m,
            event(MouseEventKind::ScrollDown, 2, regions.tree.y + 1),
            area,
        );
        assert_eq!(m.cursor, 3);
        handle(
            &mut m,
            event(
                MouseEventKind::ScrollDown,
                regions.detail.x + 1,
                regions.detail.y + 1,
            ),
            area,
        );
        assert_eq!(m.cursor, 3);
    }

    #[test]
    fn right_click_on_a_session_header_does_not_mark_the_old_cursor() {
        let mut m = model();
        m.rows.insert(
            0,
            Row::Session {
                name: "s".into(),
                windows: 12,
            },
        );
        m.cursor = 1;
        let area = Rect::new(0, 0, 80, 20);
        let tree = render::regions(area).tree;
        handle(
            &mut m,
            event(MouseEventKind::Down(MouseButton::Right), 2, tree.y),
            area,
        );
        assert!(m.marks.is_empty());
        assert_eq!(m.cursor, 1);
    }

    #[test]
    fn detail_wheel_scrolls_preview_without_moving_the_tree() {
        let mut m = model();
        m.preview = Some(("s:0".into(), "one\ntwo\nthree\nfour\nfive\n".into()));
        let area = Rect::new(0, 0, 100, 20);
        let detail = render::regions(area).detail;
        handle(
            &mut m,
            event(MouseEventKind::ScrollUp, detail.x + 1, detail.y + 1),
            area,
        );
        assert_eq!(m.preview_scroll, 3);
        assert_eq!(m.cursor, 0);
        handle(
            &mut m,
            event(MouseEventKind::ScrollDown, detail.x + 1, detail.y + 1),
            area,
        );
        assert_eq!(m.preview_scroll, 0);
    }

    #[test]
    fn clicking_a_source_does_not_replace_the_move_destination() {
        let mut m = model();
        m.cursor = 1;
        m.pending_move = Some(PendingMove {
            panes: vec![("%1".into(), "s:0".into())],
            kind: MoveKind::Panes,
            destination: Some("s:1".into()),
        });
        let area = Rect::new(0, 0, 80, 20);
        let tree = render::regions(area).tree;
        handle(
            &mut m,
            event(MouseEventKind::Down(MouseButton::Left), 2, tree.y),
            area,
        );
        assert_eq!(m.cursor, 1);
        assert_eq!(
            m.pending_move
                .as_ref()
                .and_then(|moving| moving.destination.as_deref()),
            Some("s:1")
        );
    }

    #[test]
    fn help_owns_the_wheel_over_the_tree() {
        let mut m = model();
        m.help = true;
        let area = Rect::new(0, 0, 100, 20);
        let help = render::help_area(render::regions(area).body, &m.theme);
        handle(
            &mut m,
            event(MouseEventKind::ScrollDown, help.x + 1, help.y + 1),
            area,
        );
        assert_eq!(m.help_scroll, 3);
        assert_eq!(m.cursor, 0, "the tree beneath the overlay did not move");
    }

    #[test]
    fn a_disabled_menu_row_reports_its_reason() {
        let mut m = model();
        m.menu = true;
        let area = Rect::new(0, 0, 100, 30);
        let menu = render::menu_area(render::regions(area).body, &m);
        handle(
            &mut m,
            event(
                MouseEventKind::Down(MouseButton::Left),
                menu.x + 2,
                menu.y + 1,
            ),
            area,
        );
        assert!(!m.menu);
        assert!(m.status.contains("needs a pane"), "{}", m.status);
    }

    #[test]
    fn clicking_outside_help_closes_it() {
        let mut m = model();
        m.help = true;
        let area = Rect::new(0, 0, 100, 30);
        handle(
            &mut m,
            event(MouseEventKind::Down(MouseButton::Left), 0, 2),
            area,
        );
        assert!(!m.help);
    }
}
