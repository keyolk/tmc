use super::*;

fn cell(index: u32, kind: &str) -> Cell {
    Cell {
        index,
        id: format!("%{index}"),
        kind: kind.into(),
        label: kind.into(),
        active: false,
    }
}

/// The window from the request: two claude sessions and a browser.
fn two_claudes_and_a_page() -> Vec<Cell> {
    vec![cell(1, "claude"), cell(2, "claude"), cell(3, "web")]
}

fn win(cells: Vec<Cell>, w: u32, h: u32) -> Window {
    Window {
        id: "@1".into(),
        width: w,
        height: h,
        zoomed: false,
        cells,
    }
}

#[test]
fn kinds_see_through_wrappers() {
    assert_eq!(
        kind_of(Some("ccproxy claude --intercept=mitm"), false),
        "claude"
    );
    assert_eq!(kind_of(Some("/opt/homebrew/bin/tweb open"), false), "web");
    assert_eq!(kind_of(Some("nvim src/main.rs"), false), "editor");
    assert_eq!(kind_of(Some("slx"), false), "slx");
    assert_eq!(kind_of(None, false), "shell");
    // A claude pane whose process is between restarts still says so.
    assert_eq!(kind_of(None, true), "claude");
    // The command wins over a session id the pane still carries.
    assert_eq!(kind_of(Some("tweb open"), true), "web");
    // `echo claudette` is not claude.
    assert_eq!(kind_of(Some("echo claudette"), false), "echo");
}

#[test]
fn spotlight_puts_the_lone_page_right_and_the_sessions_left() {
    let cells = two_claudes_and_a_page();
    let placed = place(&spotlight(&cells), 365, 97).unwrap();
    let web = placed.iter().find(|(i, _)| *i == 2).unwrap().1;
    let first = placed.iter().find(|(i, _)| *i == 0).unwrap().1;
    let second = placed.iter().find(|(i, _)| *i == 1).unwrap().1;
    assert!(
        web.x > first.x && web.h == 97,
        "web full height on the right: {web:?}"
    );
    assert!(web.w > first.w, "the page is the big cell");
    assert_eq!(first.x, second.x, "the sessions share the left column");
    assert!(second.y > first.y);
}

#[test]
fn a_lone_shell_is_not_the_spotlight() {
    let mut cells = vec![cell(1, "claude"), cell(2, "claude"), cell(3, "shell")];
    cells[0].active = true;
    let placed = place(&spotlight(&cells), 200, 50).unwrap();
    let big = placed.iter().max_by_key(|(_, r)| r.w * r.h).unwrap().0;
    assert_eq!(big, 0, "the active pane, not the shell");
}

#[test]
fn cells_tile_the_window_exactly() {
    // Every preset, at an odd size: cells plus borders cover the window
    // and none overlap — what tmux checks before it takes a layout.
    let cells = vec![
        cell(1, "claude"),
        cell(2, "shell"),
        cell(3, "claude"),
        cell(4, "web"),
        cell(5, "shell"),
    ];
    for p in PRESETS {
        let placed = place(&p.tree(&cells), 201, 57).unwrap();
        assert_eq!(placed.len(), 5, "{}", p.name);
        let area: u32 = placed.iter().map(|(_, r)| r.w * r.h).sum();
        assert!(area <= 201 * 57, "{}", p.name);
        for (a, (_, ra)) in placed.iter().enumerate() {
            assert!(
                ra.x + ra.w <= 201 && ra.y + ra.h <= 57,
                "{} out of bounds",
                p.name
            );
            for (_, rb) in placed.iter().skip(a + 1) {
                let apart = ra.x + ra.w <= rb.x
                    || rb.x + rb.w <= ra.x
                    || ra.y + ra.h <= rb.y
                    || rb.y + rb.h <= ra.y;
                assert!(apart, "{} overlaps: {ra:?} {rb:?}", p.name);
            }
        }
    }
}

#[test]
fn kinds_stay_together_in_a_line() {
    let cells = vec![cell(1, "claude"), cell(2, "shell"), cell(3, "claude")];
    let placed = place(&side_by_side(&cells), 120, 40).unwrap();
    let order: Vec<usize> = placed.iter().map(|(i, _)| *i).collect();
    assert_eq!(
        order,
        [0, 2, 1],
        "the two claudes adjacent, the busier kind first"
    );
}

#[test]
fn grid_is_square_ish() {
    let cells: Vec<Cell> = (1..=5).map(|i| cell(i, "shell")).collect();
    let placed = place(&grid(&cells), 200, 60).unwrap();
    let rows: std::collections::BTreeSet<u32> = placed.iter().map(|(_, r)| r.y).collect();
    assert_eq!(rows.len(), 2, "5 panes in 3+2");
}

#[test]
fn json_names_pane_indexes_and_covers_the_window() {
    let w = win(two_claudes_and_a_page(), 365, 97);
    let layout: Value = serde_json::from_str(&to_json(&spotlight(&w.cells), &w).unwrap()).unwrap();
    assert_eq!(layout["V"], 2);
    assert_eq!(layout["L"]["t"], "h");
    assert_eq!(layout["L"]["w"], 365);
    let right = &layout["L"]["c"][1];
    assert_eq!(right["i"], 3, "pane index 3 (the page) is the right cell");
    assert_eq!(right["I"], "%3");
    let left = &layout["L"]["c"][0];
    assert_eq!(left["t"], "v");
    assert_eq!(
        left["w"].as_u64().unwrap() + 1 + right["w"].as_u64().unwrap(),
        365
    );
}

#[test]
fn a_window_too_small_is_refused_not_sent() {
    let cells: Vec<Cell> = (1..=6).map(|i| cell(i, "shell")).collect();
    assert!(to_json(&side_by_side(&cells), &win(cells, 8, 10)).is_err());
}

#[test]
fn one_pane_is_one_leaf() {
    let cells = vec![cell(1, "claude")];
    for p in PRESETS {
        assert_eq!(p.tree(&cells), Node::Leaf(0), "{}", p.name);
    }
}

#[test]
fn labels_skip_spinners_and_host_names() {
    assert_eq!(
        label("✳ Coder 내부 대체", Some("ccproxy claude"), "claude"),
        "Coder 내부 대체"
    );
    assert_eq!(label("GavinJeong-MacBook-Pro", None, "shell"), "shell");
    assert_eq!(label("~/s/k/tmc", Some("slx"), "slx"), "~/s/k/tmc");
}
