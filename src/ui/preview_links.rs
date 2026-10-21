//! Links in the output preview: the one place tmc shows a URL.
//!
//! A pane's output often carries one (a PR `gh` just opened, a docs link a
//! command printed). The preview cuts long lines at the panel edge, so a URL
//! there is often not on screen in full and the terminal's own URL detection
//! would open the cut text. Each URL is made an OSC 8 link over the cells it
//! occupies, carrying the whole URL, and `o` opens the first one on screen.
//!
//! The cells come from the same helpers `render` draws with (`regions`,
//! `detail_preview_area`, `preview_window`, `truncate`'s budget), as the
//! drag selection does (E14), so the link lands on the text it belongs to.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use super::link;
use super::model::Model;
use super::render;

/// A URL and the cells of the frame it is drawn in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Region {
    pub y: u16,
    pub x: u16,
    pub width: u16,
    pub url: String,
}

/// The URLs on screen in the preview of the frame drawn into `area`, top to
/// bottom.
pub(super) fn regions(area: Rect, model: &Model) -> Vec<Region> {
    // An overlay covers the panel; a link under it would be clickable
    // through the box.
    if model.help || model.menu {
        return Vec::new();
    }
    let detail = render::regions(area, &model.split).detail;
    if detail.width == 0 {
        return Vec::new();
    }
    let preview = render::detail_preview_area(detail, model);
    if preview.height <= 1 {
        return Vec::new();
    }
    let Some(body) = model
        .current_window()
        .and_then(|w| model.preview_for(&render::preview_key(model, w)))
    else {
        return Vec::new();
    };
    let (all, start, end) = render::preview_window(body, model.preview_scroll, preview);
    let width = preview.width as usize;
    let marker = model.theme.glyphs.ellipsis.width();
    let mut out = Vec::new();
    for (row, line) in all[start..end].iter().enumerate() {
        // What `render_preview` drew: the line, or as much of it as fits
        // before the ellipsis.
        let shown = if line.width() <= width {
            width
        } else {
            width.saturating_sub(marker)
        };
        for (at, url) in link::spans(line) {
            let x0 = line[..at].width();
            let x1 = (x0 + url.width()).min(shown);
            if x0 >= x1 {
                continue;
            }
            out.push(Region {
                // Below the "output" rule.
                y: preview.y + 1 + row as u16,
                x: preview.x + x0 as u16,
                width: (x1 - x0) as u16,
                url: url.to_string(),
            });
        }
    }
    out
}

/// Turn the preview's URLs in a rendered frame into links. Called on the
/// frame after `render::draw`, so the pure render, the text snapshot and the
/// drag selection never see the escapes.
pub(super) fn apply(buf: &mut Buffer, model: &Model) {
    for r in regions(buf.area, model) {
        link::link_cells(buf, r.y, r.x, r.width, &r.url);
    }
}

/// `o`: open the first URL in the output on screen, and say what happened.
pub(super) fn open(model: &mut Model) {
    let urls: Vec<String> = regions(model.screen, model)
        .into_iter()
        .map(|r| r.url)
        .collect();
    let opened = link::open_first(model.launcher, &urls);
    match &opened.err {
        None => model.status.success(opened.note()),
        Some(link::OpenError::NoLink) => model.status.warn("no link in the output on screen"),
        Some(_) => model.status.error(opened.note()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::Change;
    use crate::ui::app::handle_key;
    use crate::ui::link::stub;
    use crate::ui::model::{Row, WindowRow};
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    const W: u16 = 110;
    const H: u16 = 16;

    fn model(output: &str) -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = vec![Row::Window(WindowRow {
            session: "projects".into(),
            index: 1,
            name: "alpha".into(),
            panes: 1,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            running_claude: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        })];
        m.preview = Some(("projects:1".into(), output.into()));
        m.screen = Rect::new(0, 0, W, H);
        m.launcher = stub::record;
        m
    }

    /// The frame as drawn, and with the links applied.
    fn frames(m: &Model) -> (Buffer, Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        let mut plain = None;
        let done = terminal
            .draw(|f| {
                render::draw(f, m, 0);
                plain = Some(f.buffer_mut().clone());
                apply(f.buffer_mut(), m);
            })
            .unwrap();
        let linked = done.buffer.clone();
        (plain.unwrap(), linked)
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    /// The URL each cell of row `y` links to, in column order.
    fn links_in_row(buf: &Buffer, y: u16) -> Vec<(u16, String, String)> {
        let mut out = Vec::new();
        for x in 0..buf.area.width {
            let s = buf[(x, y)].symbol();
            if let Some(rest) = s.strip_prefix("\x1b]8;;") {
                let (url, tail) = rest.split_once("\x1b\\").unwrap();
                let text = tail.strip_suffix("\x1b]8;;\x1b\\").unwrap();
                out.push((x, url.to_string(), text.to_string()));
            }
        }
        out
    }

    fn row_of(buf: &Buffer, needle: &str) -> u16 {
        (0..buf.area.height)
            .find(|&y| row_text(buf, y).contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen"))
    }

    #[test]
    fn a_url_in_the_output_is_linked_over_exactly_its_text() {
        let url = "https://github.com/keyolk/tmc/pull/15";
        let m = model(&format!("$ gh pr create\n{url}\n$ "));
        let (plain, linked) = frames(&m);
        let y = row_of(&plain, "github.com");
        let cells = links_in_row(&linked, y);
        let text: String = cells.iter().map(|(_, _, t)| t.as_str()).collect();
        assert_eq!(text, url, "the link covers the URL text and nothing else");
        assert!(cells.iter().all(|(_, u, _)| u == url), "{cells:?}");
        // Linking changes no cell's text: strip the escapes and the row is
        // the plain one, at the same columns.
        for x in 0..W {
            let cell = &linked[(x, y)];
            let shown = match links_in_row(&linked, y).iter().find(|(cx, _, _)| *cx == x) {
                Some((_, _, t)) => t.clone(),
                None => cell.symbol().to_string(),
            };
            assert_eq!(shown, plain[(x, y)].symbol(), "column {x}");
        }
    }

    #[test]
    fn a_url_cut_at_the_panel_edge_still_links_the_whole_url() {
        let url = format!("https://example.com/{}", "segment/".repeat(12));
        let m = model(&format!("see {url}\n"));
        let (plain, linked) = frames(&m);
        let y = row_of(&plain, "example.com");
        let cells = links_in_row(&linked, y);
        assert!(!cells.is_empty());
        assert!(cells.iter().all(|(_, u, _)| *u == url));
        let shown: String = cells.iter().map(|(_, _, t)| t.as_str()).collect();
        assert!(
            url.starts_with(&shown) && shown.len() < url.len(),
            "{shown}"
        );
        let ell = (0..W)
            .find(|&x| plain[(x, y)].symbol() == "…")
            .expect("the line was cut");
        assert!(
            cells.iter().all(|(x, _, _)| *x < ell),
            "the ellipsis is not part of the link"
        );
    }

    #[test]
    fn a_korean_prefix_keeps_the_link_on_the_url() {
        let url = "https://x.io/a";
        let m = model(&format!("한글 로그 {url} 끝\n"));
        let (plain, linked) = frames(&m);
        let y = row_of(&plain, "x.io");
        let text: String = links_in_row(&linked, y)
            .into_iter()
            .map(|(_, _, t)| t)
            .collect();
        assert_eq!(text, url);
    }

    #[test]
    fn a_javascript_or_file_url_is_neither_linked_nor_opened() {
        let mut m = model("javascript:alert(1)\nfile:///etc/passwd\n");
        let (_, linked) = frames(&m);
        for y in 0..H {
            assert!(links_in_row(&linked, y).is_empty(), "row {y} has a link");
        }
        m.launcher = stub::unexpected;
        handle_key(&mut m, KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(m.status.text(), "no link in the output on screen");
    }

    #[test]
    fn o_opens_exactly_the_url_on_screen() {
        let url = "https://github.com/keyolk/tmc/pull/15";
        let mut m = model(&format!("created {url}\n"));
        handle_key(&mut m, KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(stub::take(), [url]);
        assert_eq!(m.status.text(), format!("opened {url}"));
    }

    #[test]
    fn o_with_several_opens_the_first_and_says_how_many() {
        let mut m = model("https://a.io/1 and https://b.io/2\nhttps://c.io/3\n");
        handle_key(&mut m, KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(stub::take(), ["https://a.io/1"]);
        assert_eq!(
            m.status.text(),
            "opened https://a.io/1 (the first of 3 links)"
        );
    }

    #[test]
    fn o_with_no_url_launches_nothing_and_says_so() {
        let mut m = model("cargo test\ntest result: ok\n");
        m.launcher = stub::unexpected;
        handle_key(&mut m, KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(m.status.text(), "no link in the output on screen");
    }

    #[test]
    fn o_ignores_output_that_is_not_on_screen() {
        // Too narrow for the panel: the output is not shown, so a link in it
        // is not "here".
        let mut m = model("https://x.io/a\n");
        m.screen = Rect::new(0, 0, 80, H);
        m.launcher = stub::unexpected;
        handle_key(&mut m, KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(m.status.text(), "no link in the output on screen");
    }

    #[test]
    fn help_lists_o() {
        let help = crate::ui::keys::help_lines(true);
        let tree = help.iter().find(|(title, _)| title == "tree").unwrap();
        assert!(
            tree.1.iter().any(|(k, h)| k == "o" && h.contains("link")),
            "{tree:?}"
        );
    }

    #[test]
    fn an_overlay_hides_the_links_under_it() {
        let mut m = model("https://x.io/a\n");
        m.help = true;
        assert!(regions(m.screen, &m).is_empty());
    }
}
