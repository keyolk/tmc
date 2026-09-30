//! Physical-position key normalization for CJK input sources.
//!
//! With the OS input source set to Korean, pressing the `q` key emits `ㅂ`, so
//! every single-letter shortcut in the TUI silently stops working until the
//! user switches back to English. This module maps each jamo back to the Latin
//! key at the same physical position on a US QWERTY keyboard — the same idea as
//! Vim's `langmap`.

use crossterm::event::{KeyCode, KeyModifiers};

/// Latin key at the same physical position as `ch` on the 2-set (두벌식) Korean
/// layout, or `None` when `ch` is not a jamo we map.
///
/// Shifted jamo (double consonants, ㅒ/ㅖ) map to the uppercase Latin letter,
/// which is what the same physical chord would have produced in English.
pub fn hangul_to_latin(ch: char) -> Option<char> {
    let latin = match ch {
        // unshifted row
        'ㅂ' => 'q',
        'ㅈ' => 'w',
        'ㄷ' => 'e',
        'ㄱ' => 'r',
        'ㅅ' => 't',
        'ㅛ' => 'y',
        'ㅕ' => 'u',
        'ㅑ' => 'i',
        'ㅐ' => 'o',
        'ㅔ' => 'p',
        'ㅁ' => 'a',
        'ㄴ' => 's',
        'ㅇ' => 'd',
        'ㄹ' => 'f',
        'ㅎ' => 'g',
        'ㅗ' => 'h',
        'ㅓ' => 'j',
        'ㅏ' => 'k',
        'ㅣ' => 'l',
        'ㅋ' => 'z',
        'ㅌ' => 'x',
        'ㅊ' => 'c',
        'ㅍ' => 'v',
        'ㅠ' => 'b',
        'ㅜ' => 'n',
        'ㅡ' => 'm',
        // shifted row
        'ㅃ' => 'Q',
        'ㅉ' => 'W',
        'ㄸ' => 'E',
        'ㄲ' => 'R',
        'ㅆ' => 'T',
        'ㅒ' => 'O',
        'ㅖ' => 'P',
        _ => return None,
    };
    Some(latin)
}

/// Rewrite a jamo keypress to the Latin key at the same physical position.
///
/// Takes the code and modifiers separately because tmc's handlers do -- see
/// `ui::app::handle_key`. Chords carrying CTRL or ALT are returned untouched:
/// they already arrive as Latin regardless of the input source, and rewriting
/// them would break ctrl+c / ctrl+n style bindings.
pub fn normalize(code: KeyCode, mods: KeyModifiers) -> KeyCode {
    if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return code;
    }
    let KeyCode::Char(ch) = code else {
        return code;
    };
    match hangul_to_latin(ch) {
        // Most keys produce the same jamo shifted or not -- shift+ㅡ is still
        // `ㅡ` -- so the table can only map them to the unshifted letter and
        // the shift has to be put back here, or `J`, `M`, `G` and `P` arrive
        // as their lowercase twins and do the wrong thing. The seven keys that
        // do have a distinct shifted jamo are already uppercase in the table,
        // where this is a no-op.
        Some(latin) if mods.contains(KeyModifiers::SHIFT) => {
            KeyCode::Char(latin.to_ascii_uppercase())
        }
        Some(latin) => KeyCode::Char(latin),
        None => code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unshifted_jamo_map_to_lowercase_latin() {
        for (jamo, latin) in [('ㅂ', 'q'), ('ㅁ', 'a'), ('ㅋ', 'z'), ('ㅓ', 'j')] {
            assert_eq!(hangul_to_latin(jamo), Some(latin));
        }
    }

    #[test]
    fn shifted_jamo_map_to_uppercase_latin() {
        assert_eq!(hangul_to_latin('ㅃ'), Some('Q'));
        assert_eq!(hangul_to_latin('ㄲ'), Some('R'));
    }

    #[test]
    fn latin_and_composed_syllables_are_left_alone() {
        assert_eq!(hangul_to_latin('q'), None);
        assert_eq!(hangul_to_latin('가'), None);
    }

    #[test]
    fn normalize_rewrites_a_plain_jamo_press() {
        assert_eq!(
            normalize(KeyCode::Char('ㅂ'), KeyModifiers::NONE),
            KeyCode::Char('q')
        );
    }

    #[test]
    fn normalize_maps_a_shifted_jamo_to_the_uppercase_letter() {
        // The uppercase char already carries the shift, so SHIFT staying set on
        // the event is harmless -- only the code is rewritten.
        assert_eq!(
            normalize(KeyCode::Char('ㅃ'), KeyModifiers::SHIFT),
            KeyCode::Char('Q')
        );
    }

    #[test]
    fn shift_survives_a_key_with_no_distinct_shifted_jamo() {
        // `ㅡ` is what the physical `m` key sends shifted or not, so the shift
        // is only in the modifiers. Without putting it back, `M` (merge) would
        // arrive as `m` (move to the other session) -- two very different
        // things to press by accident.
        assert_eq!(
            normalize(KeyCode::Char('ㅡ'), KeyModifiers::SHIFT),
            KeyCode::Char('M')
        );
        assert_eq!(
            normalize(KeyCode::Char('ㅡ'), KeyModifiers::NONE),
            KeyCode::Char('m')
        );
        assert_eq!(
            normalize(KeyCode::Char('ㅓ'), KeyModifiers::SHIFT),
            KeyCode::Char('J')
        );
    }

    #[test]
    fn normalize_leaves_control_chords_untouched() {
        assert_eq!(
            normalize(KeyCode::Char('ㅁ'), KeyModifiers::CONTROL),
            KeyCode::Char('ㅁ')
        );
    }

    #[test]
    fn normalize_leaves_non_char_keys_untouched() {
        assert_eq!(
            normalize(KeyCode::Enter, KeyModifiers::NONE),
            KeyCode::Enter
        );
    }
}
