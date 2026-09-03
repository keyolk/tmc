//! Render one frame at a fixed size, without a terminal.
//!
//! How the layout is actually reviewed: an interactive TUI cannot be inspected
//! in CI or from a script, and eyeballing it in a live terminal does not catch
//! what a narrow window does to the columns.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use unicode_width::UnicodeWidthStr;

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
            // A double-width character occupies two cells: the symbol sits in
            // the first and the second is left empty. Reading every cell would
            // emit a stray space after each one, which is what made a Korean
            // preview line measure wider here than it renders in a terminal.
            let mut line = String::new();
            let mut x = 0;
            while x < width {
                let Some(cell) = buffer.cell((x, y)) else {
                    x += 1;
                    continue;
                };
                let symbol = cell.symbol();
                if symbol.is_empty() {
                    line.push(' ');
                    x += 1;
                } else {
                    line.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
            }
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
            running_claude: !state.is_empty(),
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

    /// A pane of `projects:8` — `window()` builds every fixture window in the
    /// `projects` session, whatever header sits above it.
    fn pane_row(id: &str, command: &str) -> Row {
        Row::Pane(crate::ui::model::PaneRow {
            session: "projects".into(),
            window_index: 8,
            index: 1,
            pane_id: id.into(),
            command: command.into(),
            path: "/home/u/src".into(),
            active: false,
        })
    }

    #[test]
    fn an_expanded_window_lists_panes_by_id() {
        // Three identical claude commands in one window: the id is the only
        // thing that tells the rows apart.
        let mut m = model();
        m.rows
            .insert(3, pane_row("%1084", "ccproxy claude --model default"));
        m.rows
            .insert(4, pane_row("%1087", "ccproxy claude --model default"));
        let out = render(&m, 110, 16, 0);

        assert!(out.contains("%1084"), "{out}");
        assert!(out.contains("%1087"), "{out}");
    }

    #[test]
    fn selecting_a_pane_names_it_in_the_panel() {
        let mut m = model();
        // model() row 2 is dashboard:8; its pane goes directly after.
        m.rows.insert(3, pane_row("%1084", "ccproxy claude"));
        m.cursor = 3;
        assert!(
            matches!(&m.rows[m.cursor], Row::Pane(_)),
            "the cursor must land on the pane for this test to mean anything",
        );
        m.preview = Some(("%1084".into(), "the pane output\n".into()));
        // Tall enough that the capture is not cut off below the fold.
        let out = render(&m, 110, 20, 0);

        // The panel titles the pane, not its window — otherwise the output
        // below has nothing identifying it.
        assert!(out.contains("%1084  projects:8"), "{out}");
        assert!(out.contains("/home/u/src"), "the pane cwd:\n{out}");
        assert!(out.contains("the pane output"), "{out}");
    }

    #[test]
    fn a_long_pane_command_is_cut_at_the_column() {
        use unicode_width::UnicodeWidthStr;
        let mut m = model();
        m.rows.insert(
            3,
            pane_row(
                "%1084",
                &"ccproxy claude --intercept=mitm -- --model default".repeat(3),
            ),
        );
        for width in [90u16, 110, 140] {
            for line in render(&m, width, 16, 0).lines() {
                assert!(line.width() <= width as usize, "at {width}: {line:?}");
            }
        }
    }

    #[test]
    fn the_panel_shows_what_the_window_is_running() {
        // Choosing between windows means seeing their output — the name is
        // already in the tree. tmux.sh put this behind every one of its fzf
        // switchers; leaving it out made the panel a list of things already
        // on screen.
        let mut m = model();
        m.preview = Some((
            "projects:8".into(),
            "cargo test\n   Compiling tmc\ntest result: ok. 158 passed\n".into(),
        ));
        let out = render(&m, 110, 16, 0);

        assert!(out.contains("─ output"), "the section is labelled:\n{out}");
        assert!(out.contains("158 passed"), "the capture is shown:\n{out}");
    }

    #[test]
    fn a_window_only_in_the_restore_point_says_why_it_has_no_output() {
        let mut m = model();
        // The removed window; nothing is running to capture.
        m.cursor = 5;
        m.preview = None;
        let out = render(&m, 110, 16, 0);
        assert!(out.contains("not running"), "{out}");
    }

    #[test]
    fn a_korean_preview_stays_inside_the_panel() {
        use unicode_width::UnicodeWidthStr;
        // CJK is two columns per character. Measuring in characters let a
        // preview line run past the border — and reading the render cell by
        // cell added a stray space after every wide character on top of that.
        let mut m = model();
        m.preview = Some((
            "projects:8".into(),
            "마지막 항목은 이번에 실제로 필요했습니다 — dataexport 경로입니다\n".into(),
        ));
        for width in [90u16, 110, 140] {
            for line in render(&m, width, 16, 0).lines() {
                assert!(
                    line.width() <= width as usize,
                    "at {width} columns a line ran to {}: {line:?}",
                    line.width(),
                );
            }
        }
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
    fn the_opening_screen_is_the_search_line() {
        // What `prefix w` lands on. The hint has to name the way out and the
        // way to the other commands, or they are undiscoverable from here.
        let mut m = model();
        m.searching = true;
        let out = render(&m, 110, 12, 0);
        let last = out.lines().last().unwrap_or_default();

        assert!(last.contains("type to search"), "{last}");
        assert!(last.contains("tab/esc commands"), "{last}");
        assert!(last.contains("ctrl-c quit"), "{last}");
        assert!(
            !last.contains("esc quit"),
            "Esc changes mode, never exits: {last}"
        );
    }

    #[test]
    fn with_a_query_typed_esc_still_offers_normal_mode() {
        let mut m = model();
        m.searching = true;
        m.search_push('b');
        let last = render(&m, 110, 12, 0);
        let last = last.lines().last().unwrap_or_default();
        assert!(last.contains("tab/esc commands"), "{last}");
        assert!(
            !last.contains("esc clear"),
            "the query remains as a filter: {last}"
        );
    }

    #[test]
    fn an_active_search_shows_the_query_and_narrows_the_tree() {
        let mut m = model();
        for c in "bnp".chars() {
            m.search_push(c);
        }
        let out = render(&m, 100, 14, 0);

        // Stepped out to the tree, the query shows as a filter with a slash.
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
    fn expanding_survives_narrower_than_the_rare_commands_do() {
        // `l` is how you reach a pane at all; dropping it before `b`/`J` would
        // leave those looking like they act on nothing.
        let out = render(&model(), 100, 12, 0);
        assert!(out.contains("l/h panes"), "{out}");
        assert!(
            !out.contains("x kill"),
            "the rarer group goes first:\n{out}"
        );
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
