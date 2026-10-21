//! The tree | detail split wired into the app: drawing, the mouse order
//! (split, then selection, then clicks), the keys and the hint row. Every
//! position comes from the rendered frame, never from layout arithmetic.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::app::handle_key;
use super::drag::dispatch;
use super::model::{Model, Row, WindowRow};
use super::render;
use crate::layout::diff::Change;

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

fn draw_at(m: &mut Model, w: u16, h: u16) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    // What app::draw records, so a key acts on the frame last drawn.
    m.screen = Rect::new(0, 0, w, h);
    t.draw(|f| render::draw(f, m, 0)).unwrap();
    t.backend().buffer().clone()
}

fn draw(m: &mut Model) -> Buffer {
    draw_at(m, W, H)
}

fn row_text(b: &Buffer, y: u16) -> String {
    (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
}

fn find(b: &Buffer, needle: &str) -> (u16, u16) {
    for y in 0..b.area.height {
        let row = row_text(b, y);
        if let Some(i) = row.find(needle) {
            return (row[..i].chars().count() as u16, y);
        }
    }
    panic!("{needle:?} not on screen");
}

/// The divider's column: the leftmost column drawn as a vertical line on
/// every row from the first tree row to the row above the key hints.
fn divider_x(b: &Buffer) -> Option<u16> {
    let top = find(b, "win0").1;
    let bottom = b.area.height - 1;
    (0..b.area.width)
        .find(|&x| (top..bottom).all(|y| matches!(b[(x, y)].symbol(), "│" | "┃" | "|")))
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

/// Each event against the frame drawn before it, as the event loop does.
fn gesture(m: &mut Model, events: &[MouseEvent], now: Instant) {
    for e in events {
        let frame = draw(m);
        dispatch(m, *e, Rect::new(0, 0, W, H), &frame, now);
    }
}

fn key(m: &mut Model, c: char) {
    handle_key(m, KeyCode::Char(c), KeyModifiers::NONE);
}

#[test]
fn dragging_the_divider_moves_it_and_the_click_targets_follow() {
    let mut m = model();
    let b = draw(&mut m);
    let x0 = divider_x(&b).expect("a divider between tree and detail");
    let y = find(&b, "win2").1;
    let to = x0 + 10;
    gesture(
        &mut m,
        &[down((x0, y)), drag((to, y)), up((to, y))],
        Instant::now(),
    );

    let b = draw(&mut m);
    assert_eq!(divider_x(&b), Some(to), "{}", row_text(&b, y));
    assert!(!m.selection.active(), "a divider drag selected text");
    // A press where the detail used to be is now a tree row: the click
    // handler sees the same rects the frame was drawn with.
    let at = (x0 + 3, find(&b, "win4").1);
    let t = Instant::now() + Duration::from_secs(1);
    gesture(&mut m, &[down(at), up(at)], t);
    assert_eq!(m.cursor, 4);
}

#[test]
fn a_pane_drag_still_selects_text_and_leaves_the_divider() {
    let mut m = model();
    let b = draw(&mut m);
    let x0 = divider_x(&b).unwrap();
    let from = find(&b, "out-58");
    let to = find(&b, "out-59");
    gesture(&mut m, &[down(from), drag(to)], Instant::now());
    assert!(
        m.selection.active(),
        "a press in the detail must reach selection"
    );
    assert_eq!(divider_x(&draw(&mut m)), Some(x0));
}

#[test]
fn a_divider_press_drops_the_old_highlight() {
    let mut m = model();
    let b = draw(&mut m);
    let from = find(&b, "out-58");
    let to = find(&b, "out-59");
    gesture(&mut m, &[down(from), drag(to), up(to)], Instant::now());
    assert!(m.selection.active());
    let x0 = divider_x(&draw(&mut m)).unwrap();
    gesture(&mut m, &[down((x0, from.1))], Instant::now());
    assert!(
        !m.selection.active(),
        "the split took the press, but the stale highlight stayed"
    );
}

#[test]
fn a_double_click_on_the_divider_resets_it() {
    let mut m = model();
    let x0 = divider_x(&draw(&mut m)).unwrap();
    key(&mut m, '>');
    key(&mut m, '>');
    let x1 = divider_x(&draw(&mut m)).unwrap();
    assert!(x1 > x0);
    let at = (x1, 5);
    let t = Instant::now();
    gesture(&mut m, &[down(at), up(at)], t);
    gesture(&mut m, &[down(at)], t + Duration::from_millis(100));
    assert_eq!(divider_x(&draw(&mut m)), Some(x0));
    assert!(!m.split.is_set());
}

#[test]
fn the_keys_move_the_divider_and_reset_it() {
    let mut m = model();
    let x0 = divider_x(&draw(&mut m)).unwrap();
    key(&mut m, '>');
    let grown = divider_x(&draw(&mut m)).unwrap();
    assert!(grown > x0, "> {x0} -> {grown}");
    key(&mut m, '<');
    key(&mut m, '<');
    let shrunk = divider_x(&draw(&mut m)).unwrap();
    assert!(shrunk < x0, "< {x0} -> {shrunk}");
    key(&mut m, '=');
    assert_eq!(divider_x(&draw(&mut m)), Some(x0));
}

#[test]
fn the_hint_row_names_the_split_keys() {
    let mut m = model();
    let b = draw_at(&mut m, 200, H);
    assert!(
        row_text(&b, H - 1).contains("</>/= split"),
        "{}",
        row_text(&b, H - 1)
    );
}

#[test]
fn one_shrink_after_growing_past_the_limit_moves_at_once() {
    let mut m = model();
    draw(&mut m);
    for _ in 0..40 {
        key(&mut m, '>');
    }
    let at_limit = divider_x(&draw(&mut m)).unwrap();
    key(&mut m, '<');
    let back = divider_x(&draw(&mut m)).unwrap();
    assert!(back < at_limit, "dead zone: {at_limit} -> {back}");
}

#[test]
fn below_the_collapse_width_the_tree_is_alone_and_nothing_overflows() {
    for (w, h) in [(60, 24), (80, 24), (89, 24)] {
        let mut m = model();
        let b = draw_at(&mut m, w, h);
        assert_eq!(divider_x(&b), None, "{w} cols drew a divider");
        assert!(
            !row_text(&b, find(&b, "win0").1).contains("out-"),
            "{w} cols drew the detail"
        );
        let r = render::regions(Rect::new(0, 0, w, h), &m.split);
        assert!(r.tree.right() <= w && r.detail.right() <= w);
        // A key at this size moves the stored ratio but cannot overflow.
        key(&mut m, '>');
        let r = render::regions(Rect::new(0, 0, w, h), &m.split);
        assert!(r.tree.right() <= w);
    }
    // And a split moved at a wide size survives the trip.
    let mut m = model();
    draw(&mut m);
    key(&mut m, '>');
    let wide = divider_x(&draw(&mut m)).unwrap();
    draw_at(&mut m, 80, 24);
    assert_eq!(divider_x(&draw(&mut m)), Some(wide));
}
