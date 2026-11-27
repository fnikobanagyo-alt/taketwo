//! Python `str` and `re` semantics the ported text code relies on.
//! shortcut: the Unicode tables are the Rust crates' (newer than Python 3.11's Unicode 14.0);
//! only characters added after Unicode 14 can differ, upgrade if that ever matters.

use regex::Regex;
use std::sync::LazyLock;

/// Python's `\s` inside a character class: Rust's White_Space plus `\x1c`-`\x1f`.
pub const SPACE: &str = r"\s\x1C-\x1F";
/// Python's `\w` inside a character class (str.isalnum() or `_`); Rust's `\w` also takes marks.
pub const WORD: &str = r"\p{L}\p{N}_";

pub fn re(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("{SPACE}", SPACE).replace("{WORD}", WORD)).expect("valid regex")
}

/// `str.isspace()` for one character.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// A character matched by Python's `\w`.
pub fn is_word(c: char) -> bool {
    static W: LazyLock<Regex> = LazyLock::new(|| re(r"\A[{WORD}]\z"));
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    let mut buf = [0u8; 4];
    W.is_match(c.encode_utf8(&mut buf))
}

/// Python's `\b` at char index `i` of `chars`.
pub fn boundary(chars: &[char], i: usize) -> bool {
    let before = i > 0 && is_word(chars[i - 1]);
    let after = i < chars.len() && is_word(chars[i]);
    before != after
}

pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `str.split()` with no separator.
pub fn split(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// Length in code points, as Python's `len(str)`.
pub fn len(s: &str) -> usize {
    s.chars().count()
}

/// The first code point of each Unicode 14 run of decimal digits (`Nd`), generated from
/// Python's `unicodedata`; every run is ten digits 0-9 in order.
const ND_ZEROS: [u32; 66] = [
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66, 0xde6, 0xe50,
    0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40,
    0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30, 0x11066,
    0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950,
    0x11c50, 0x11d50, 0x11da0, 0x16a60, 0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6,
    0x1e140, 0x1e2f0, 0x1e950, 0x1fbf0,
];

/// The value of a `\d` character, as Python's `int()` reads it.
pub fn digit_value(c: char) -> u32 {
    let cp = c as u32;
    let zero = ND_ZEROS.iter().rev().find(|&&z| z <= cp).copied().unwrap_or(0x30);
    (cp - zero).min(9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_py_helpers() {
        assert!(is_space('\x1c') && is_space('\u{a0}') && !is_space('\u{200b}'));
        assert!(is_word('é') && is_word('²') && !is_word('\u{301}') && !is_word('-'));
        assert_eq!(split(" a\u{1f}b  c "), vec!["a", "b", "c"]);
        assert_eq!(digit_value('٣'), 3);
        assert_eq!(digit_value('7'), 7);
        let c: Vec<char> = "ab c".chars().collect();
        assert!(boundary(&c, 0) && !boundary(&c, 1) && boundary(&c, 2) && boundary(&c, 4));
    }
}
