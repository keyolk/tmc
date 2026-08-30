//! Fuzzy matching, for every list in this tool.
//!
//! Subsequence matching with a score, not substring: typing `bnp` should find
//! `binpack`, and `cohm` should find `cohome`. A picker that demands the exact
//! letters in order is a filter, not a search, and with 27 windows and 100
//! paste buffers the difference is the whole point.

/// How well a candidate matches, higher is better. `None` means no match.
///
/// The score exists so results can be ranked rather than merely filtered:
/// with a two-letter query half the list still matches, and the ordering is
/// what makes the answer reachable.
pub fn score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }

    // Case-insensitive unless the query has an uppercase letter — the
    // smart-case convention every tool here already follows.
    let sensitive = needle.chars().any(char::is_uppercase);
    let hay: Vec<char> = if sensitive {
        haystack.chars().collect()
    } else {
        haystack.chars().flat_map(char::to_lowercase).collect()
    };
    let query: Vec<char> = if sensitive {
        needle.chars().collect()
    } else {
        needle.chars().flat_map(char::to_lowercase).collect()
    };

    let mut total = 0;
    let mut at = 0usize;
    let mut previous: Option<usize> = None;

    for &want in &query {
        let found = hay[at..].iter().position(|&c| c == want)? + at;

        let mut points = 1;
        let adjacent = previous == Some(found.wrapping_sub(1)) && found > 0;

        if adjacent {
            // A run of letters is a strong signal on its own.
            points += 8;
        } else {
            if let Some(p) = previous {
                // Any gap at all costs, before the boundary bonus is
                // considered. Without this floor, `b-i-n-ary` — where every
                // letter follows a hyphen and so collects the boundary bonus —
                // outscores `binpack`, which is backwards. The extra per-
                // character cost is capped so a long tail of misses cannot
                // push a real match below a spurious one.
                let gap = found - p - 1;
                points -= 7 + (gap as i32).min(4);
            }
            // Jumping to a word boundary is what typing initials looks like,
            // and it is worth more than mere adjacency: `rs` means
            // right+sizing, not the r and s sitting together in `airships`.
            //
            // Only when the jump is real. Rewarding a boundary that is also
            // adjacent lets `b-i-n-ary` collect the bonus three times and beat
            // `binpack`, which is backwards.
            if found == 0 || is_boundary(&hay, found) {
                points += 10;
            }
        }

        total += points;
        previous = Some(found);
        at = found + 1;
    }

    // Shorter candidates win ties: with `cc` matching both `ccx` and
    // `ccproxy monitor`, the first is what was meant.
    total -= (hay.len() as i32) / 8;
    Some(total)
}

/// Whether the character at `i` starts a word.
fn is_boundary(hay: &[char], i: usize) -> bool {
    let Some(&prev) = hay.get(i.wrapping_sub(1)) else {
        return true;
    };
    !prev.is_alphanumeric()
}

/// Rank `items` by how well they match, dropping what does not.
///
/// Stable within a score so an unfiltered list keeps its original order —
/// windows stay in index order until the query actually distinguishes them.
pub fn rank<T>(needle: &str, items: &[T], text: impl Fn(&T) -> &str) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| score(needle, text(item)).map(|s| (i, s)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_subsequence_not_just_a_substring() {
        // The reason this module exists: `bnp` is how you actually type
        // `binpack` when you are not looking.
        assert!(score("bnp", "binpack").is_some());
        assert!(score("chm", "cohome").is_some());
        assert!(score("rsz", "right-sizing").is_some());
    }

    #[test]
    fn rejects_letters_that_are_out_of_order() {
        assert_eq!(score("pnb", "binpack"), None);
        assert_eq!(score("xyz", "binpack"), None);
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn adjacent_letters_beat_scattered_ones() {
        let tight = score("bin", "binpack").unwrap();
        let loose = score("bin", "b-i-n-ary").unwrap();
        assert!(tight > loose, "tight {tight} should beat loose {loose}");
    }

    #[test]
    fn a_word_boundary_outranks_a_letter_mid_word() {
        // `rs` should mean right + sizing, not the `r` and `s` inside one word.
        let boundary = score("rs", "right-sizing").unwrap();
        let inside = score("rs", "airships").unwrap();
        assert!(boundary > inside, "boundary {boundary} vs inside {inside}");
    }

    #[test]
    fn shorter_candidates_win_a_tie() {
        let short = score("cc", "ccx").unwrap();
        let long = score("cc", "ccproxy monitor  --intercept").unwrap();
        assert!(short > long, "short {short} should beat long {long}");
    }

    #[test]
    fn matching_is_case_insensitive_until_you_type_a_capital() {
        assert!(score("ccx", "CCX").is_some());
        assert!(score("CCX", "ccx").is_none(), "an uppercase query is exact");
        assert!(score("CCX", "CCX").is_some());
    }

    #[test]
    fn ranking_puts_the_best_match_first() {
        let windows = ["cohome", "mitm", "right-sizing", "cco", "cage"];
        let ranked = rank("co", &windows, |w| w);
        // `cohome` starts with the query; `cco` matches from its second
        // character. A prefix is the stronger signal.
        assert_eq!(windows[ranked[0]], "cohome", "ranked: {ranked:?}");
        assert!(!ranked.iter().any(|&i| windows[i] == "mitm"));
    }

    #[test]
    fn typing_initials_finds_the_hyphenated_name() {
        // How these window names are actually reached: `rs` for right-sizing.
        let windows = ["airships", "right-sizing", "narwhal"];
        let ranked = rank("rs", &windows, |w| w);
        assert_eq!(windows[ranked[0]], "right-sizing", "ranked: {ranked:?}");
    }

    #[test]
    fn an_unfiltered_list_keeps_its_original_order() {
        // Windows stay in index order until the query distinguishes them;
        // re-sorting a list nobody has filtered is disorienting.
        let windows = ["alpha", "beta", "gamma"];
        assert_eq!(rank("", &windows, |w| w), vec![0, 1, 2]);
    }

    /// The window names actually open on the reference machine.
    const WINDOWS: [&str; 11] = [
        "cohome",
        "mitm",
        "right-sizing",
        "kite",
        "cco",
        "firewall",
        "istio",
        "civiz",
        "cage",
        "binpack",
        "nolb",
    ];

    #[test]
    fn short_queries_reach_the_real_window_names() {
        for (query, want) in [
            ("bnp", "binpack"),
            ("rs", "right-sizing"),
            ("chm", "cohome"),
            ("fw", "firewall"),
            ("ist", "istio"),
        ] {
            let ranked = rank(query, &WINDOWS, |w| w);
            assert_eq!(
                WINDOWS[ranked[0]],
                want,
                "{query} should reach {want}; got {:?}",
                ranked.iter().map(|&i| WINDOWS[i]).collect::<Vec<_>>(),
            );
        }
    }

    #[test]
    fn handles_multibyte_text_without_splitting_a_character() {
        // Window names and paste buffers here are often Korean.
        assert!(score("한글", "한글이름").is_some());
        assert_eq!(score("한자", "한글이름"), None);
    }
}
