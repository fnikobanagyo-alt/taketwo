//! Python's `html.unescape` (3.11), with its HTML5 entity table embedded as data.

use regex::{Captures, Regex};
use std::collections::HashMap;
use std::sync::LazyLock;

static HTML5: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../../data/html5_entities.json")).expect("entity table"));

static CHARREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\x0C <&#;]{1,32};?)").unwrap());

/// Python's `html._invalid_charrefs`: Windows-1252 and a few others.
fn invalid_charref(num: u64) -> Option<&'static str> {
    Some(match num {
        0x00 => "\u{fffd}",
        0x0d => "\r",
        0x80 => "\u{20ac}",
        0x81 => "\u{81}",
        0x82 => "\u{201a}",
        0x83 => "\u{0192}",
        0x84 => "\u{201e}",
        0x85 => "\u{2026}",
        0x86 => "\u{2020}",
        0x87 => "\u{2021}",
        0x88 => "\u{02c6}",
        0x89 => "\u{2030}",
        0x8a => "\u{0160}",
        0x8b => "\u{2039}",
        0x8c => "\u{0152}",
        0x8d => "\u{8d}",
        0x8e => "\u{017d}",
        0x8f => "\u{8f}",
        0x90 => "\u{90}",
        0x91 => "\u{2018}",
        0x92 => "\u{2019}",
        0x93 => "\u{201c}",
        0x94 => "\u{201d}",
        0x95 => "\u{2022}",
        0x96 => "\u{2013}",
        0x97 => "\u{2014}",
        0x98 => "\u{02dc}",
        0x99 => "\u{2122}",
        0x9a => "\u{0161}",
        0x9b => "\u{203a}",
        0x9c => "\u{0153}",
        0x9d => "\u{9d}",
        0x9e => "\u{017e}",
        0x9f => "\u{0178}",
        _ => return None,
    })
}

/// Python's `html._invalid_codepoints`.
fn invalid_codepoint(n: u64) -> bool {
    matches!(n, 0x1..=0x8 | 0xb | 0xe..=0x1f | 0x7f..=0x9f | 0xfdd0..=0xfdef)
        || (n <= 0x10ffff && (n & 0xfffe) == 0xfffe)
}

fn replace_charref(s: &str) -> String {
    if let Some(num) = s.strip_prefix('#') {
        let num = num.trim_end_matches(';');
        // Saturate: anything past u64 is past U+10FFFF anyway.
        let n = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u64::from_str_radix(hex, 16).unwrap_or(u64::MAX),
            None => num.parse::<u64>().unwrap_or(u64::MAX),
        };
        if let Some(r) = invalid_charref(n) {
            return r.to_string();
        }
        if (0xd800..=0xdfff).contains(&n) || n > 0x10ffff {
            return "\u{fffd}".to_string();
        }
        if invalid_codepoint(n) {
            return String::new();
        }
        return char::from_u32(n as u32).map(String::from).unwrap_or_default();
    }
    if let Some(v) = HTML5.get(s) {
        return v.clone();
    }
    // The longest matching name, as defined by the standard (prefixes of 2 code points or more).
    let chars: Vec<char> = s.chars().collect();
    for x in (2..chars.len()).rev() {
        let head: String = chars[..x].iter().collect();
        if let Some(v) = HTML5.get(&head) {
            return format!("{v}{}", chars[x..].iter().collect::<String>());
        }
    }
    format!("&{s}")
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    CHARREF.replace_all(s, |c: &Captures| replace_charref(&c[1])).into_owned()
}
