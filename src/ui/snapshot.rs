//! Render one frame at a fixed size, without a terminal.
//!
//! How the layout is actually reviewed: an interactive TUI cannot be inspected
//! in CI or from a script, and eyeballing it in a live terminal does not catch
//! what a narrow window does to the columns.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::model::Model;

/// A frame as plain text, one line per row.
pub fn render(model: &Model, width: u16, height: u16, now_secs: u64) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("test backend is infallible");
    terminal
        .draw(|frame| super::render::draw(frame, model, now_secs))
        .expect("draw");

    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let line: String = (0..width)
                .map(|x| {
                    buffer
                        .cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect();
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::Change;
    use crate::ui::model::{Row, WindowRow};

    fn window(index: u32, name: &str, state: &str, change: Change, waiting: bool) -> Row {
        Row::Window(WindowRow {
            session: "projects".into(),
            index,
            name: name.into(),
            panes: 2,
            state: state.into(),
            cc_session: if state.is_empty() {
                String::new()
            } else {
                "e399ae98-489d-4cc7".into()
            },
            waiting,
            change,
            reasons: match change {
                Change::Modified => vec!["pane 2 command: ccx -> ccx --watch".into()],
                Change::Added => vec!["not in the restore point".into()],
                Change::Removed => vec!["only in the restore point".into()],
                Change::Same => Vec::new(),
            },
            gone: change == Change::Removed,
        })
    }

    fn model() -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = vec![
            Row::Session {
                name: "dashboard".into(),
                windows: 2,
            },
            window(1, "coder", "waiting", Change::Same, true),
            window(8, "ccx", "done", Change::Modified, false),
            Row::Session {
                name: "projects".into(),
                windows: 2,
            },
            window(12, "binpack", "working", Change::Added, false),
            window(6, "firewall", "", Change::Removed, false),
        ];
        m.cursor = 2;
        m.counts = (1, 1, 1);
        m.waiting = 1;
        m
    }

    #[test]
    fn wide_terminal_shows_the_tree_and_the_diff_panel() {
        let out = render(&model(), 100, 14, 0);
        assert!(out.contains("dashboard"), "{out}");
        assert!(out.contains("ccx"), "{out}");
        assert!(
            out.contains("pane 2 command"),
            "the detail panel explains the selected window:\n{out}",
        );
        assert!(out.contains("1 waiting"), "{out}");
    }

    #[test]
    fn narrow_terminal_drops_the_panel_rather_than_squeezing_it() {
        let out = render(&model(), 70, 14, 0);
        assert!(out.contains("ccx"), "the tree survives:\n{out}");
        assert!(
            !out.contains("pane 2 command"),
            "the panel is dropped below 90 columns:\n{out}",
        );
    }

    #[test]
    fn a_tiny_terminal_says_so_instead_of_rendering_garbage() {
        let out = render(&model(), 40, 8, 0);
        assert!(out.contains("terminal too small"), "{out}");
    }

    #[test]
    fn change_markers_and_state_glyphs_are_both_present() {
        let out = render(&model(), 100, 14, 0);
        // Punctuation, so the tree reads under NO_COLOR.
        assert!(out.contains('~'), "modified marker:\n{out}");
        assert!(out.contains('+'), "added marker:\n{out}");
        assert!(out.contains('-'), "removed marker:\n{out}");
        assert!(out.contains('?'), "waiting glyph:\n{out}");
        assert!(out.contains('*'), "working glyph:\n{out}");
    }

    #[test]
    fn a_window_that_only_exists_in_the_point_has_no_index() {
        let out = render(&model(), 100, 14, 0);
        let line = out
            .lines()
            .find(|l| l.contains("firewall"))
            .expect("the removed window is listed");
        assert!(
            line.contains('—'),
            "a gone window shows no live index: [{line}]",
        );
    }

    #[test]
    fn an_active_search_shows_the_query_and_narrows_the_tree() {
        let mut m = model();
        for c in "bnp".chars() {
            m.search_push(c);
        }
        let out = render(&m, 100, 14, 0);

        assert!(out.contains("/bnp"), "the query is visible:\n{out}");
        assert!(out.contains("binpack"), "the match is shown:\n{out}");
        assert!(!out.contains("firewall"), "non-matches are gone:\n{out}");
    }

    #[test]
    fn a_search_matching_nothing_says_so() {
        let mut m = model();
        for c in "zzz".chars() {
            m.search_push(c);
        }
        let out = render(&m, 100, 14, 0);
        assert!(out.contains("no window matches"), "{out}");
    }

    #[test]
    fn the_key_hint_always_shows_the_way_out() {
        // Truncating mid-word would hide `q quit`, which is the one thing the
        // hint line must never do.
        for width in [60, 70, 90, 104, 140] {
            let out = render(&model(), width, 12, 0);
            let last = out.lines().last().unwrap_or_default();
            assert!(
                last.contains("q quit"),
                "at {width} columns the quit key vanished: [{last}]",
            );
            assert!(
                last.chars().count() <= width as usize,
                "at {width} columns the hint overflowed: [{last}]",
            );
        }
    }

    #[test]
    fn a_wide_terminal_shows_the_window_surgery_keys_a_narrow_one_drops() {
        let wide = render(&model(), 140, 12, 0);
        let narrow = render(&model(), 70, 12, 0);
        assert!(wide.contains("x kill"), "{wide}");
        assert!(!narrow.contains("x kill"), "dropped when it will not fit");
    }

    #[test]
    fn the_key_hint_names_what_r_will_actually_do() {
        let mut m = model();
        let plain = render(&m, 100, 14, 0);
        assert!(plain.contains("restore all"), "{plain}");

        m.marks.insert("projects:12".into());
        let marked = render(&m, 100, 14, 0);
        assert!(marked.contains("restore marked"), "{marked}");
        assert!(marked.contains("1 marked"), "{marked}");
    }
}
