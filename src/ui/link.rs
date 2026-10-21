//! URLs reachable two ways, the fleet convention (tui-design E11): the text
//! on screen is an OSC 8 hyperlink, so the terminal opens it on cmd/ctrl+click
//! even while the app captures the mouse, and `o` opens the selected item's
//! URL.
//!
//! A port of tuikit `link` (Go), which Rust cannot import. Keep the two in
//! step: same validity rule, same escape, same notes.
//!
//! Only absolute http and https URLs are linked or opened. The URL comes from
//! data (a pane's output, a paste buffer), and a `file:`, `javascript:` or
//! custom-scheme URL is not something a key in a TUI should launch.
//!
//! Behind tmux, links need `set -as terminal-features '*:hyperlinks'` (3.4+);
//! a terminal that does not know OSC 8 ignores it and shows the text.

use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroU16;

use ratatui::buffer::{Buffer, CellDiffOption};
use unicode_width::UnicodeWidthStr;

/// The longest URL that is linked. Terminals cap OSC 8 URIs (VTE at 2083
/// bytes) and drop longer ones, sometimes with the text.
pub const MAX_URL: usize = 2048;

/// Whether `u` is an absolute http or https URL with a host and nothing a
/// terminal escape could be smuggled through (control characters, spaces).
pub fn valid(u: &str) -> bool {
    if u.is_empty() || u.len() > MAX_URL {
        return false;
    }
    if u.bytes().any(|c| c <= b' ' || c == 0x7f) {
        return false;
    }
    let Some((scheme, rest)) = u.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = match host_port.strip_prefix('[') {
        // An IPv6 literal: the port, if any, follows the bracket.
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host_port.split(':').next().unwrap_or(""),
    };
    !host.is_empty()
}

/// `text` as an OSC 8 hyperlink to `u`. A `u` that is not `valid` returns the
/// text unchanged, so a bad URL costs the link, not the label. An empty text
/// stays empty: a label cut to nothing by its column must not turn into the
/// URL.
///
/// Only for output that is written to the terminal as is. Inside ratatui a
/// span holding this escape is measured and cut as text; use `link_cells`.
pub fn wrap(u: &str, text: &str) -> String {
    if text.is_empty() || !valid(u) {
        return text.to_string();
    }
    format!("\x1b]8;;{}\x1b\\{text}\x1b]8;;\x1b\\", ascii_url(u))
}

/// `u` with every non-ASCII byte percent-encoded, as browsers do. Inside the
/// escape a non-ASCII byte would be measured as cells by width code that
/// skips the escape's ASCII; encoded, it is the same URL to a browser.
fn ascii_url(u: &str) -> Cow<'_, str> {
    if u.is_ascii() {
        return Cow::Borrowed(u);
    }
    let mut out = String::with_capacity(u.len() * 3);
    for c in u.bytes() {
        if c.is_ascii() {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    Cow::Owned(out)
}

/// Make the cells of row `y` from column `x` for `width` columns an OSC 8
/// link to `u`, after the frame is rendered.
///
/// Every cell carries a whole link (open, its own symbol, close) rather than
/// the first opening and the last closing: ratatui writes only the cells that
/// changed since the last frame, so an opening cell can be written without
/// its closing one, and everything drawn after it would become part of the
/// link. Each cell is told its real width (`ForcedWidth`): otherwise the diff
/// measures the escape as text, takes the cell for one ~40 columns wide and
/// skips writing the cells it thinks it covers.
pub fn link_cells(buf: &mut Buffer, y: u16, x: u16, width: u16, u: &str) {
    if !valid(u) {
        return;
    }
    let area = buf.area;
    if y < area.top() || y >= area.bottom() {
        return;
    }
    let end = x.saturating_add(width).min(area.right());
    let mut col = x.max(area.left());
    while col < end {
        let cell = &mut buf[(col, y)];
        let symbol = cell.symbol().to_string();
        let w = symbol.width().max(1) as u16;
        if !symbol.is_empty() && cell.diff_option == CellDiffOption::None {
            cell.set_symbol(&wrap(u, &symbol));
            cell.set_diff_option(CellDiffOption::ForcedWidth(
                NonZeroU16::new(w).expect("max(1)"),
            ));
        }
        col = col.saturating_add(w);
    }
}

/// The http and https URLs in `text`, in order, escapes ignored. Trailing
/// sentence punctuation is not part of a URL ("see https://x.io/a."), and
/// neither is a closing bracket that has no opening one in the URL
/// ("(https://x.io/a)").
pub fn find(text: &str) -> Vec<String> {
    spans(&strip_escapes(text))
        .into_iter()
        .map(|(_, u)| u.to_string())
        .collect()
}

/// `find` with each URL's byte offset in `text`, for placing it on screen.
/// `text` must be plain: an escape would be counted as part of a URL.
pub fn spans(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("http").map(|i| from + i) {
        let rest = &text[at..];
        if !rest.starts_with("http://") && !rest.starts_with("https://") {
            from = at + 4;
            continue;
        }
        let len = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
            .unwrap_or(rest.len());
        let u = trim_url(&rest[..len]);
        if valid(u) {
            out.push((at, u));
        }
        from = at + len.max(4);
    }
    out
}

fn trim_url(mut u: &str) -> &str {
    while let Some(last) = u.chars().last() {
        let count = |c: char| u.matches(c).count();
        let cut = ".,;:!?".contains(last)
            || (last == ')' && count('(') < count(')'))
            || (last == ']' && count('[') < count(']'));
        if !cut {
            break;
        }
        u = &u[..u.len() - 1];
    }
    u
}

/// `text` without CSI (`ESC [ … final`) and OSC (`ESC ] … BEL/ST`) escapes.
fn strip_escapes(text: &str) -> Cow<'_, str> {
    if !text.contains('\x1b') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Cow::Owned(out)
}

/// Why a URL was not opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Not an http or https URL (see `valid`).
    Invalid,
    /// Running over SSH: the browser would open on the remote host, not in
    /// front of the user.
    Remote,
    /// The platform has no known URL opener.
    NoOpener,
    /// The opener could not be started.
    Spawn(String),
    /// There was no URL to open. Said rather than doing nothing, so the key
    /// always answers.
    NoLink,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Invalid => f.write_str("not an http(s) URL"),
            OpenError::Remote => f.write_str("no local browser over SSH"),
            OpenError::NoOpener => f.write_str("no URL opener on this platform"),
            OpenError::Spawn(e) => write!(f, "start the opener: {e}"),
            OpenError::NoLink => f.write_str("no link"),
        }
    }
}

/// Opens a URL in the browser. The app holds one so tests can put a recorder
/// in its place; `DEFAULT` is the real one, except under `cfg(test)`.
pub type Launcher = fn(&str) -> Result<(), OpenError>;

/// What the app starts with. Under test a launch nobody stubbed fails the test
/// instead of opening a browser on the developer's screen.
#[cfg(not(test))]
pub const DEFAULT: Launcher = launch;
#[cfg(test)]
pub const DEFAULT: Launcher = stub::unexpected;

/// The result of `open_first`, and the status line it earns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub url: String,
    pub err: Option<OpenError>,
    /// How many URLs it chose from; above 1 the note says "the first of N" so
    /// the user knows to look.
    pub of: usize,
}

impl Opened {
    /// "opened <url>", "opened <url> (the first of 3 links)", or why not.
    pub fn note(&self) -> String {
        match &self.err {
            Some(OpenError::NoLink) => "no link here".into(),
            Some(e) => format!("open failed: {e}"),
            None if self.of > 1 => format!("opened {} (the first of {} links)", self.url, self.of),
            None => format!("opened {}", self.url),
        }
    }
}

/// Open the first valid URL of `urls` with `launcher` and say how many there
/// were. Invalid URLs are skipped, never handed to the launcher; with none
/// left the result is `NoLink`.
pub fn open_first(launcher: Launcher, urls: &[String]) -> Opened {
    let valid: Vec<&String> = urls.iter().filter(|u| valid(u)).collect();
    let Some(first) = valid.first() else {
        return Opened {
            url: String::new(),
            err: Some(OpenError::NoLink),
            of: 0,
        };
    };
    Opened {
        url: first.to_string(),
        err: launcher(first).err(),
        of: valid.len(),
    }
}

/// The real launcher: the platform opener with the URL as one argument,
/// never through a shell, with no terminal attached so it cannot draw over
/// the TUI.
// Unreferenced under test, where `DEFAULT` is the stub that fails instead.
#[cfg_attr(test, allow(dead_code))]
pub fn launch(u: &str) -> Result<(), OpenError> {
    launch_in(u, &|key| {
        std::env::var_os(key).is_some_and(|v| !v.is_empty())
    })
}

fn launch_in(u: &str, env_set: &dyn Fn(&str) -> bool) -> Result<(), OpenError> {
    use std::process::{Command, Stdio};

    if !valid(u) {
        return Err(OpenError::Invalid);
    }
    if env_set("SSH_TTY") || env_set("SSH_CONNECTION") {
        return Err(OpenError::Remote);
    }
    let opener = match std::env::consts::OS {
        "macos" => "open",
        "linux" | "freebsd" | "openbsd" | "netbsd" => "xdg-open",
        _ => return Err(OpenError::NoOpener),
    };
    // `valid` holds, so the argument starts with "http" and cannot be read
    // as a flag.
    let mut child = Command::new(opener)
        .arg(ascii_url(u).as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| OpenError::Spawn(e.to_string()))?;
    // Reaped off the input thread so the key never waits on the browser. The
    // exit status is not reported: the opener has already been handed the
    // URL, and a status arriving seconds later would land on whatever the
    // user is doing by then.
    std::thread::spawn(move || child.wait().map(drop));
    Ok(())
}

/// Test launchers: one that records, one that fails the test.
#[cfg(test)]
pub mod stub {
    use std::cell::RefCell;

    use super::OpenError;

    thread_local! {
        static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Records the URL instead of opening it. Launches happen on the thread
    /// that handled the key, which is the test's own.
    pub fn record(u: &str) -> Result<(), OpenError> {
        OPENED.with(|o| o.borrow_mut().push(u.to_string()));
        Ok(())
    }

    /// The URLs `record` saw, emptying the record.
    pub fn take() -> Vec<String> {
        OPENED.with(|o| std::mem::take(&mut *o.borrow_mut()))
    }

    pub fn unexpected(u: &str) -> Result<(), OpenError> {
        panic!("a test reached the real launcher with {u}; set the launcher to stub::record")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn valid_takes_only_http_with_a_host() {
        let long = format!("https://x.io/{}", "a".repeat(MAX_URL));
        for (u, want) in [
            ("https://github.com/keyolk/tuikit/pull/9", true),
            ("http://localhost:8080/x?a=1&b=2", true),
            ("https://x.io/path;with;semis", true),
            ("HTTPS://x.io/a", true),
            ("https://user@x.io:8443/a", true),
            ("http://[::1]:8080/", true),
            ("", false),
            ("github.com/keyolk", false),
            ("https://", false),
            ("https:///path", false),
            ("file:///etc/passwd", false),
            ("javascript:alert(1)", false),
            ("ssh://host/x", false),
            ("https://x.io/\x1b]8;;evil\x1b\\", false),
            ("https://x.io/a b", false),
            (long.as_str(), false),
        ] {
            assert_eq!(valid(u), want, "valid({u:?})");
        }
    }

    #[test]
    fn wrap_is_an_osc8_link_around_exactly_the_text() {
        let u = "https://github.com/keyolk/tuikit/pull/9";
        assert_eq!(
            wrap(u, "#9 link package"),
            format!("\x1b]8;;{u}\x1b\\#9 link package\x1b]8;;\x1b\\")
        );
    }

    #[test]
    fn wrap_leaves_bad_urls_as_plain_text() {
        assert_eq!(wrap("javascript:alert(1)", "click"), "click");
        assert_eq!(wrap("file:///etc/passwd", "f"), "f");
        assert_eq!(
            wrap("https://x.io/\x1b\\x", "t"),
            "t",
            "an escape in the URL must not reach the terminal"
        );
        assert_eq!(
            wrap("https://x.io/a", ""),
            "",
            "a label cut to nothing must stay empty, not become the URL"
        );
    }

    #[test]
    fn a_non_ascii_url_is_percent_encoded() {
        let got = wrap("https://x.io/한글-서비스?q=값", "ab");
        assert_eq!(
            got,
            "\x1b]8;;https://x.io/%ED%95%9C%EA%B8%80-%EC%84%9C%EB%B9%84%EC%8A%A4?q=%EA%B0%92\x1b\\ab\x1b]8;;\x1b\\"
        );
        assert!(got.is_ascii());
    }

    #[test]
    fn find_strips_punctuation_unmatched_brackets_and_escapes() {
        let text = "see https://x.io/a. and (https://en.wikipedia.org/wiki/Go_(game)) or \
                    \x1b[1mhttps://y.io/b?q=1\x1b[0m, not ftp://z.io or file:///x; [https://z.io/c]";
        assert_eq!(
            find(text),
            [
                "https://x.io/a",
                "https://en.wikipedia.org/wiki/Go_(game)",
                "https://y.io/b?q=1",
                "https://z.io/c",
            ]
        );
        assert!(find("httpx http:// nothing here").is_empty());
    }

    #[test]
    fn spans_give_byte_offsets() {
        let text = "한글 https://x.io/a 끝";
        assert_eq!(spans(text), [(7, "https://x.io/a")]);
    }

    #[test]
    fn open_first_says_how_many_and_skips_bad_urls() {
        let urls = ["javascript:x", "https://x.io/a", "https://x.io/b"].map(String::from);
        let m = open_first(stub::record, &urls);
        assert_eq!(m.url, "https://x.io/a");
        assert_eq!(m.of, 2);
        assert_eq!(m.err, None);
        assert_eq!(m.note(), "opened https://x.io/a (the first of 2 links)");
        assert_eq!(stub::take(), ["https://x.io/a"]);

        let one = open_first(stub::record, &["https://x.io/c".into()]);
        assert_eq!(one.note(), "opened https://x.io/c");
        stub::take();

        // Nothing valid: the launcher is never reached (it would panic).
        let none = open_first(stub::unexpected, &["file:///etc/passwd".into()]);
        assert_eq!(none.err, Some(OpenError::NoLink));
        assert_eq!(none.note(), "no link here");
    }

    #[test]
    fn launch_refuses_bad_urls_and_ssh_before_running_anything() {
        let never = |_: &str| false;
        for u in ["file:///etc/passwd", "-a Calculator", "x-man-page://ls", ""] {
            assert_eq!(launch_in(u, &never), Err(OpenError::Invalid), "{u:?}");
        }
        let ssh = |k: &str| k == "SSH_CONNECTION";
        assert_eq!(
            launch_in("https://x.io/a", &ssh),
            Err(OpenError::Remote),
            "the browser would open on the remote host"
        );
        assert_eq!(
            Opened {
                url: "https://x.io/a".into(),
                err: Some(OpenError::Remote),
                of: 1
            }
            .note(),
            "open failed: no local browser over SSH"
        );
    }

    /// The columns ratatui's diff would write for `next` over `prev`.
    fn written(prev: &Buffer, next: &Buffer, y: u16) -> Vec<u16> {
        prev.diff(next)
            .into_iter()
            .filter(|(_, row, _)| *row == y)
            .map(|(x, _, _)| x)
            .collect()
    }

    #[test]
    fn linked_cells_keep_their_width_so_every_cell_is_still_written() {
        let area = Rect::new(0, 0, 12, 1);
        let mut plain = Buffer::empty(area);
        plain.set_string(0, 0, "> 한 title x", ratatui::style::Style::default());
        let mut linked = plain.clone();
        link_cells(&mut linked, 0, 2, 8, "https://x.io/a");

        // The marker and the trailing badge stay outside the link.
        assert_eq!(linked[(0, 0)].symbol(), ">");
        assert_eq!(linked[(11, 0)].symbol(), "x");
        assert_eq!(
            linked[(2, 0)].symbol(),
            "\x1b]8;;https://x.io/a\x1b\\한\x1b]8;;\x1b\\"
        );
        assert_eq!(
            linked[(9, 0)].symbol(),
            "\x1b]8;;https://x.io/a\x1b\\e\x1b]8;;\x1b\\"
        );
        // Drawn from blank, every column the plain row writes is still
        // written, plus the linked space at 4: no cell is skipped as if
        // covered by a wide escape. 3 is the right half of 한.
        let blank = Buffer::empty(area);
        assert_eq!(written(&blank, &plain, 0), [0, 2, 5, 6, 7, 8, 9, 11]);
        assert_eq!(written(&blank, &linked, 0), [0, 2, 4, 5, 6, 7, 8, 9, 11]);
    }

    #[test]
    fn link_cells_ignores_a_bad_url_and_the_edges() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "abcd", ratatui::style::Style::default());
        let before = buf.clone();
        link_cells(&mut buf, 0, 0, 4, "javascript:alert(1)");
        assert_eq!(buf, before);
        link_cells(&mut buf, 3, 0, 4, "https://x.io/a");
        assert_eq!(buf, before, "a row off the buffer is ignored");
        link_cells(&mut buf, 0, 2, 99, "https://x.io/a");
        assert!(buf[(3, 0)].symbol().contains("https://x.io/a"));
        assert_eq!(buf[(1, 0)].symbol(), "b");
    }
}
