//! UTC formatting, without a date dependency.
//!
//! Only two shapes are ever needed and both come from one civil-time
//! conversion, so a crate would be more surface than the problem.

use std::time::{SystemTime, UNIX_EPOCH};

/// The current instant in the two forms the layout format uses.
pub struct Now {
    /// `2026-08-30T00:00:00Z` — the `saved_at` format tmux.sh writes.
    pub timestamp: String,
    /// `20260830T000000Z` — used for directory and file names.
    pub compact: String,
}

pub fn now() -> Now {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    Now {
        timestamp: format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"),
        compact: format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z"),
    }
}

/// Days-from-civil, inverted. Howard Hinnant's algorithm — exact for every
/// date the epoch can express.
pub fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };

    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// How long ago `stamp` (in `compact` form) was, as "3h" / "2d" / "just now".
///
/// The restore-point picker shows this instead of a raw timestamp: what the
/// reader is deciding is how much work a point predates, and an absolute UTC
/// string makes them do that subtraction in their head.
pub fn age_of(stamp: &str, now_secs: u64) -> String {
    let Some(then) = unix_from_compact(stamp) else {
        return String::new();
    };
    let secs = now_secs.saturating_sub(then);
    match secs {
        0..=90 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// Parse `20260830T000000Z` back to epoch seconds.
fn unix_from_compact(stamp: &str) -> Option<u64> {
    let b = stamp.as_bytes();
    if b.len() < 16 || b[8] != b'T' {
        return None;
    }
    let num =
        |from: usize, len: usize| -> Option<i64> { stamp.get(from..from + len)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(4, 2)?, num(6, 2)?);
    let (h, mi, s) = (num(9, 2)?, num(11, 2)?, num(13, 2)?);

    // days_from_civil, the forward direction of the algorithm above.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_known_instant() {
        // Cross-checked with `date -u -r`.
        assert_eq!(civil_from_unix(1_788_048_000), (2026, 8, 30, 0, 0, 0));
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        // A leap day, which a naive 365-day conversion gets wrong.
        assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn compact_parsing_inverts_the_conversion() {
        for secs in [0u64, 1_709_164_800, 1_788_048_061] {
            let (y, mo, d, h, mi, s) = civil_from_unix(secs);
            let compact = format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z");
            assert_eq!(unix_from_compact(&compact), Some(secs), "{compact}");
        }
    }

    #[test]
    fn describes_an_age_in_the_largest_useful_unit() {
        let now = 1_788_048_000;
        let stamp = |offset: u64| {
            let (y, mo, d, h, mi, s) = civil_from_unix(now - offset);
            format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
        };
        assert_eq!(age_of(&stamp(30), now), "just now");
        assert_eq!(age_of(&stamp(600), now), "10m ago");
        assert_eq!(age_of(&stamp(7200), now), "2h ago");
        assert_eq!(age_of(&stamp(3 * 86_400), now), "3d ago");
    }

    #[test]
    fn an_unparseable_stamp_yields_no_age_rather_than_a_wrong_one() {
        assert_eq!(age_of("not-a-stamp", 1_788_048_000), "");
        assert_eq!(age_of("", 1_788_048_000), "");
    }
}
