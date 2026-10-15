//! The status line: one message, with a severity and an age.
//!
//! A bare `status: String` failed the way it fails in every TUI that has one:
//! no severity (a failed reload and a finished save were the same yellow),
//! and it went away only when a key arrived, so "saved 3 session(s)" sat over
//! the hints until the next keystroke. This mirrors the Go tuikit `toast`
//! contract (engine principle E6) at the size tmc needs — one seat, not a
//! queue:
//!
//! - info and success expire after `TTL`, checked by the event loop, which
//!   wakes for the deadline instead of redrawing on a timer;
//! - warnings and errors stay until a key, and an error must have been on
//!   screen for `MIN_DWELL` first, so a key already in flight when it lands
//!   does not dismiss it unread;
//! - the level is drawn as a prefix as well as a colour, so it survives
//!   `NO_COLOR` and colour blindness.

use std::ops::Deref;
use std::time::{Duration, Instant};

/// How long info and success stay.
pub const TTL: Duration = Duration::from_secs(4);

/// How long an error is shown before a key may clear it.
pub const MIN_DWELL: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// A prompt or a neutral fact: "moving %3: choose a window".
    Info,
    /// A command did what was asked.
    Success,
    /// Nothing happened, and why: "select a pane first".
    Warn,
    /// Something failed.
    Error,
}

impl Level {
    /// Whether the message waits for a key rather than timing out.
    fn sticky(self) -> bool {
        matches!(self, Level::Warn | Level::Error)
    }
}

#[derive(Clone, Debug)]
pub struct Status {
    text: String,
    level: Level,
    set_at: Instant,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            text: String::new(),
            level: Level::Info,
            set_at: Instant::now(),
        }
    }
}

impl Status {
    pub fn set(&mut self, level: Level, text: impl Into<String>) {
        self.set_at_time(level, text, Instant::now());
    }

    /// `set` with the clock passed in, for tests.
    pub fn set_at_time(&mut self, level: Level, text: impl Into<String>, now: Instant) {
        self.text = text.into();
        self.level = level;
        self.set_at = now;
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.set(Level::Info, text);
    }
    pub fn success(&mut self, text: impl Into<String>) {
        self.set(Level::Success, text);
    }
    pub fn warn(&mut self, text: impl Into<String>) {
        self.set(Level::Warn, text);
    }
    pub fn error(&mut self, text: impl Into<String>) {
        self.set(Level::Error, text);
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// A key was pressed. Everything clears except an error that has not yet
    /// been on screen for `MIN_DWELL`.
    pub fn on_key(&mut self, now: Instant) {
        if self.level == Level::Error && now.duration_since(self.set_at) < MIN_DWELL {
            return;
        }
        self.clear();
    }

    /// When the message times out, if it does.
    pub fn deadline(&self) -> Option<Instant> {
        (!self.text.is_empty() && !self.level.sticky()).then(|| self.set_at + TTL)
    }

    /// Take the message down if its time is up. Returns whether it did, which
    /// is the one redraw expiry costs.
    pub fn expire(&mut self, now: Instant) -> bool {
        match self.deadline() {
            Some(at) if now >= at => {
                self.clear();
                true
            }
            _ => false,
        }
    }
}

/// The text, so `status.contains(..)` and `status.is_empty()` read as before.
impl Deref for Status {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Same message, same level. The time is left out: re-announcing the same
/// reload error every tick is not a change worth a redraw.
impl PartialEq for Status {
    fn eq(&self, other: &Self) -> bool {
        self.level == other.level && self.text == other.text
    }
}

impl PartialEq<&str> for Status {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_and_success_expire_after_the_ttl() {
        let t0 = Instant::now();
        for level in [Level::Info, Level::Success] {
            let mut s = Status::default();
            s.set_at_time(level, "saved", t0);
            assert_eq!(s.deadline(), Some(t0 + TTL));
            assert!(!s.expire(t0 + TTL - Duration::from_millis(1)));
            assert_eq!(s, "saved");
            assert!(s.expire(t0 + TTL), "{level:?}");
            assert!(s.is_empty());
            assert!(!s.expire(t0 + TTL * 2), "expiry reports a change once");
        }
    }

    #[test]
    fn warnings_and_errors_wait_for_a_key() {
        let t0 = Instant::now();
        for level in [Level::Warn, Level::Error] {
            let mut s = Status::default();
            s.set_at_time(level, "reload failed", t0);
            assert_eq!(s.deadline(), None);
            assert!(!s.expire(t0 + TTL * 10));
            s.on_key(t0 + Duration::from_secs(1));
            assert!(s.is_empty(), "{level:?}");
        }
    }

    #[test]
    fn a_key_in_flight_does_not_dismiss_a_fresh_error() {
        let t0 = Instant::now();
        let mut s = Status::default();
        s.set_at_time(Level::Error, "ps timed out", t0);
        s.on_key(t0 + Duration::from_millis(100));
        assert_eq!(s, "ps timed out", "too soon to have been read");
        s.on_key(t0 + MIN_DWELL);
        assert!(s.is_empty());

        // Info has no dwell: the key that follows it is the user moving on.
        s.set_at_time(Level::Info, "moving", t0);
        s.on_key(t0);
        assert!(s.is_empty());
    }

    #[test]
    fn an_empty_status_has_no_deadline() {
        assert_eq!(Status::default().deadline(), None);
    }
}
