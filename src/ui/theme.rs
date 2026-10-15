//! Colours by role, not by hue, and the glyphs the screens draw with.
//!
//! Render code asks for `warn()` or `subtle()`, never `Color::Yellow`, so the
//! palette, `NO_COLOR` and the ASCII fallback are decided here once. Mirrors
//! the Go tuikit `theme` package the other TUIs share (engine principle E10).
//!
//! The palette is the 16 ANSI colours rather than RGB: tmc runs as a tmux
//! popup inside whatever theme the terminal has, and the named colours are the
//! ones every theme remaps to something legible on its own background.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

use super::status::Level;

/// Every symbol drawn outside plain text.
///
/// Only the ones that need a fallback live here. Columns that must line up
/// (the pane marker, a gone window's index) use plain ASCII in every set:
/// `·` and `—` are East Asian Ambiguous and draw two cells wide on a CJK
/// terminal, which shifts every column after them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// This is the ASCII set.
    pub ascii: bool,
    /// Marks a cut. `…` is ambiguous too, but a cut line sits at the end of
    /// its budget, so under a CJK locale it becomes `~` (one cell everywhere)
    /// rather than overflowing the column by one.
    pub ellipsis: &'static str,
    /// The `─ output ───` rule over the preview.
    pub rule: &'static str,
    /// The text cursor after a typed query.
    pub caret: &'static str,
    /// Before an expanded session's name.
    pub session: &'static str,
    /// Box corners and edges for the overlays.
    pub border: border::Set<'static>,
    /// Keys are written as `⏎ ↑ ↓` rather than `enter up down`. Off in ASCII,
    /// and off under a CJK locale: the arrows are East Asian Ambiguous, and
    /// the `?` help lines its descriptions up after them.
    pub key_glyphs: bool,
    /// Before a status message, by level (info, success, warn, error). The
    /// level is in the words as well as the colour, so it survives NO_COLOR.
    pub levels: [&'static str; 4],
}

impl Glyphs {
    pub const UNICODE: Glyphs = Glyphs {
        ascii: false,
        ellipsis: "…",
        rule: "─",
        caret: "▌",
        session: "▾",
        border: border::ROUNDED,
        key_glyphs: true,
        levels: ["i", "✓", "!", "✗"],
    };

    /// For consoles that mangle box drawing (SSH into a minimal image, a
    /// non-UTF-8 locale). Every glyph is as wide as its Unicode counterpart,
    /// so switching sets never moves a column.
    pub const ASCII: Glyphs = Glyphs {
        ascii: true,
        ellipsis: "~",
        rule: "-",
        caret: "_",
        session: "v",
        border: border::Set {
            top_left: "+",
            top_right: "+",
            bottom_left: "+",
            bottom_right: "+",
            vertical_left: "|",
            vertical_right: "|",
            horizontal_top: "-",
            horizontal_bottom: "-",
        },
        key_glyphs: false,
        levels: ["info:", "ok:", "warn:", "error:"],
    };
}

/// The palette plus the glyph set, resolved once at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// `NO_COLOR` is set: no colour anywhere, attributes kept.
    pub no_color: bool,
    pub glyphs: Glyphs,
}

impl Default for Theme {
    /// Colour and Unicode, whatever the environment says. Tests build on this
    /// so a developer's `NO_COLOR` or Korean locale cannot change a snapshot.
    fn default() -> Self {
        Self {
            no_color: false,
            glyphs: Glyphs::UNICODE,
        }
    }
}

impl Theme {
    /// The prefix and style a status of `level` is drawn with.
    pub fn status(&self, level: Level) -> (&'static str, Style) {
        let [info, success, warn, error] = self.glyphs.levels;
        match level {
            Level::Info => (info, self.info()),
            Level::Success => (success, self.success()),
            Level::Warn => (warn, self.warn()),
            Level::Error => (error, self.error()),
        }
    }

    /// The theme the environment asks for.
    pub fn detect() -> Self {
        Self::from_env(|k| std::env::var(k).ok())
    }

    /// `detect` with the environment passed in, so tests stay hermetic.
    ///
    /// - `NO_COLOR` set to anything non-empty (no-color.org) drops colour.
    /// - `TMC_ASCII` truthy forces the ASCII glyphs; otherwise a non-UTF-8
    ///   locale does.
    /// - A CJK locale (or `RUNEWIDTH_EASTASIAN`) keeps Unicode but swaps the
    ///   ellipsis, which is the one glyph used to fit a line to its width.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let set = |k: &str| get(k).filter(|v| !v.is_empty());
        let no_color = set("NO_COLOR").is_some();
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|k| set(k))
            .map(|v| v.to_ascii_lowercase());

        let ascii = set("TMC_ASCII").is_some_and(|v| truthy(&v))
            // No locale at all is the macOS default, which is UTF-8.
            || locale
                .as_deref()
                .is_some_and(|l| !l.contains("utf-8") && !l.contains("utf8"));
        let cjk = match set("RUNEWIDTH_EASTASIAN") {
            Some(v) => truthy(&v),
            None => locale
                .as_deref()
                .is_some_and(|l| ["ko", "ja", "zh"].iter().any(|p| l.starts_with(p))),
        };

        let mut glyphs = if ascii {
            Glyphs::ASCII
        } else {
            Glyphs::UNICODE
        };
        if cjk {
            glyphs.ellipsis = "~";
            glyphs.key_glyphs = false;
        }
        Self { no_color, glyphs }
    }

    fn fg(&self, c: Color) -> Style {
        if self.no_color {
            Style::default()
        } else {
            Style::default().fg(c)
        }
    }

    /// Body text: the terminal's own foreground.
    pub fn text(&self) -> Style {
        Style::default()
    }
    /// Secondary content: captured output, a pane's command.
    pub fn muted(&self) -> Style {
        self.fg(Color::Gray)
    }
    /// Metadata, hints, rules: present but out of the way.
    pub fn subtle(&self) -> Style {
        self.fg(Color::DarkGray)
    }
    /// What the eye should land on: key letters, the typed query.
    pub fn accent(&self) -> Style {
        self.fg(Color::Yellow).add_modifier(Modifier::BOLD)
    }
    pub fn info(&self) -> Style {
        self.fg(Color::Cyan)
    }
    pub fn success(&self) -> Style {
        self.fg(Color::Green)
    }
    pub fn warn(&self) -> Style {
        self.fg(Color::Yellow)
    }
    /// Bold as well as red, so it still outranks a warning without colour.
    pub fn error(&self) -> Style {
        self.fg(Color::Red).add_modifier(Modifier::BOLD)
    }
    /// The row under the cursor. Bold rather than a background: a background
    /// is the first thing `NO_COLOR` removes, and the `>` beside it already
    /// carries the position.
    pub fn selected(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
    /// Text that typing will replace. Reverse video is an attribute, so it
    /// survives `NO_COLOR`.
    pub fn replaceable(&self) -> Style {
        self.accent().add_modifier(Modifier::REVERSED)
    }
    pub fn border(&self) -> Style {
        self.subtle()
    }
    /// The border of a box that owns the keyboard (the `x` menu, `?` help).
    pub fn border_focus(&self) -> Style {
        self.fg(Color::Cyan)
    }
    pub fn added(&self) -> Style {
        self.fg(Color::Green)
    }
    pub fn removed(&self) -> Style {
        self.fg(Color::Red)
    }
}

fn truthy(v: &str) -> bool {
    !matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Theme {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Theme::from_env(|k| {
            pairs
                .iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn no_color_drops_colour_but_keeps_attributes() {
        let t = env(&[("NO_COLOR", "1")]);
        assert!(t.no_color);
        assert_eq!(t.subtle().fg, None);
        assert_eq!(t.error().fg, None);
        assert!(t.error().add_modifier.contains(Modifier::BOLD));
        assert!(t.selected().add_modifier.contains(Modifier::BOLD));
        assert!(t.replaceable().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn an_empty_no_color_is_unset() {
        assert!(!env(&[("NO_COLOR", "")]).no_color);
    }

    #[test]
    fn the_glyph_set_follows_the_override_then_the_locale() {
        assert_eq!(env(&[]).glyphs, Glyphs::UNICODE, "no locale is UTF-8");
        assert_eq!(env(&[("LANG", "en_US.UTF-8")]).glyphs, Glyphs::UNICODE);
        assert_eq!(env(&[("LANG", "C")]).glyphs, Glyphs::ASCII);
        assert_eq!(
            env(&[("LANG", "en_US.UTF-8"), ("TMC_ASCII", "1")]).glyphs,
            Glyphs::ASCII
        );
        assert_eq!(
            env(&[("LANG", "en_US.UTF-8"), ("TMC_ASCII", "0")]).glyphs,
            Glyphs::UNICODE
        );
        // LC_ALL outranks LANG, as POSIX says.
        assert_eq!(
            env(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]).glyphs,
            Glyphs::ASCII
        );
    }

    #[test]
    fn a_cjk_locale_swaps_only_the_ambiguous_glyphs() {
        let g = env(&[("LANG", "ko_KR.UTF-8")]).glyphs;
        assert_eq!(g.ellipsis, "~");
        assert!(
            !g.key_glyphs,
            "arrows are ambiguous; help aligns after them"
        );
        assert_eq!(g.rule, Glyphs::UNICODE.rule);
        assert_eq!(
            env(&[("LANG", "en_US.UTF-8"), ("RUNEWIDTH_EASTASIAN", "1")])
                .glyphs
                .ellipsis,
            "~"
        );
    }

    #[test]
    fn every_ascii_glyph_is_as_wide_as_its_unicode_one() {
        use unicode_width::UnicodeWidthStr;
        let (u, a) = (Glyphs::UNICODE, Glyphs::ASCII);
        for (x, y) in [
            (u.ellipsis, a.ellipsis),
            (u.rule, a.rule),
            (u.caret, a.caret),
            (u.session, a.session),
        ] {
            assert_eq!(x.width(), y.width(), "{x:?} vs {y:?}");
            assert!(y.is_ascii());
        }
        for (x, y) in u.levels.iter().zip(a.levels) {
            assert!(y.is_ascii(), "{y:?}");
            // Only the Unicode prefix has to be one cell; the words are
            // longer on purpose and nothing is aligned after them.
            assert_eq!(x.width(), 1, "{x:?}");
        }
    }
}
