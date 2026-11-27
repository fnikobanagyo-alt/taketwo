//! Settings: DEFAULT_SETTINGS (config.py), get_settings, SETTING_RULES, _checked and save_settings (db.py).

use super::Doc;
use crate::text::py;
use regex::Regex;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::LazyLock;

/// Same keys, order and values as config.py DEFAULT_SETTINGS.
pub fn default_settings() -> Doc {
    let v = json!({
        "default_engine": "qwen3-tts-1.7b",
        "max_chunk_chars": 220,
        "max_attempts": 4,
        "compare_takes": 1,
        "experimental_qwen_ending_guard": false,
        "max_wer": 0.12,
        "min_speaker_similarity": 0.85,
        "max_internal_silence": 0.9,
        "max_pitch_offset_st": 2.0,
        "sentence_pause_ms": 300,
        "inner_sentence_pause_ms": 0,
        "paragraph_pause_ms": 700,
        "target_lufs": -16.0,
        "seed": 1234,
        "pronunciations": [],
        "strip_citations": true,
        "custom_engines": [],
        "check_updates": true,
        "update_dismissed": "",
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// (key, lo, hi, is_int): ranges for numeric settings, as db.py SETTING_RULES.
pub const SETTING_RULES: [(&str, f64, f64, bool); 12] = [
    ("max_chunk_chars", 80.0, 600.0, true),
    ("max_attempts", 1.0, 8.0, true),
    ("compare_takes", 1.0, 3.0, true),
    ("max_wer", 0.01, 0.5, false),
    ("min_speaker_similarity", 0.5, 0.99, false),
    ("max_internal_silence", 0.2, 3.0, false),
    ("max_pitch_offset_st", 0.5, 12.0, false),
    ("sentence_pause_ms", 0.0, 2000.0, true),
    ("inner_sentence_pause_ms", 0.0, 2000.0, true),
    ("paragraph_pause_ms", 0.0, 4000.0, true),
    ("target_lufs", -30.0, -10.0, false),
    ("seed", 0.0, 2147483647.0, true),
];

fn custom_engines_ok(v: &Value) -> bool {
    v.as_array().is_some_and(|a| {
        a.iter().all(|c| c.as_object().is_some_and(|c| ["id", "repo"].iter().all(|k| c.get(*k).is_some_and(Value::is_string))))
    })
}

/// Python's str-to-number preparation: strip, Unicode digits to ASCII, underscores only between digits.
/// None when the string can't be a Python number.
fn ascii_number(s: &str) -> Option<String> {
    let chars: Vec<char> = py::strip(s).chars().map(|c| if py::is_decimal(c) { char::from(b'0' + py::digit_value(c) as u8) } else { c }).collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let digit = |j: Option<&char>| j.is_some_and(char::is_ascii_digit);
            if !(i > 0 && digit(chars.get(i - 1)) && digit(chars.get(i + 1))) {
                return None;
            }
        } else if !c.is_ascii() {
            return None;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `int(str)`: optional sign and decimal digits. Huge values saturate, so they fail the range check as in Python.
fn py_int(s: &str) -> Option<i128> {
    let t = ascii_number(s)?;
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, &t[..]),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = digits.bytes().fold(0i128, |n, b| n.saturating_mul(10).saturating_add((b - b'0') as i128));
    Some(if neg { -n } else { n })
}

/// `float(str)`; Rust's grammar (sign, digits with optional point, exponent, inf/infinity/nan) matches Python's
/// once underscores and Unicode digits are handled.
fn py_float(s: &str) -> Option<f64> {
    ascii_number(s)?.parse().ok()
}

fn fmt_bound(x: f64, is_int: bool) -> String {
    if is_int { format!("{}", x as i64) } else { format!("{x:?}") } // `{:?}` prints like Python's repr: 0.01, -30.0
}

/// db.py `_checked`: validate a settings patch; numeric values come back converted (\"5\" -> 5, 1 -> 1.0 for floats).
pub fn checked(patch: &Doc) -> Result<Doc, String> {
    static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A\d{1,4}\.\d{1,4}\.\d{1,4}\z").expect("valid regex"));
    let mut out = Doc::new();
    for (k, v) in patch {
        let mut v = v.clone();
        if k == "check_updates" && !v.is_boolean() {
            return Err("check_updates must be true or false".into());
        }
        if k == "update_dismissed" && !v.as_str().is_some_and(|s| s.is_empty() || VERSION.is_match(s)) {
            return Err("update_dismissed must be a version like 2.1.0".into());
        }
        if k == "custom_engines" && !custom_engines_ok(&v) {
            return Err("custom_engines must be a list of models".into());
        }
        if let Some(&(_, lo, hi, is_int)) = SETTING_RULES.iter().find(|r| r.0 == k) {
            let nan = || format!("{k} must be a number");
            let range = || format!("{k} must be between {} and {}", fmt_bound(lo, is_int), fmt_bound(hi, is_int));
            if is_int {
                let num = match &v {
                    Value::Number(n) => match (n.as_i64(), n.as_u64()) {
                        (Some(i), _) => i as i128,
                        (_, Some(u)) => u as i128,
                        _ => n.as_f64().filter(|f| f.is_finite()).ok_or_else(nan)?.trunc() as i128, // int(3.7) == 3
                    },
                    Value::String(s) => py_int(s).ok_or_else(nan)?,
                    _ => return Err(nan()), // bool, None, list, dict
                };
                if !(lo as i128 <= num && num <= hi as i128) {
                    return Err(range());
                }
                v = (num as i64).into();
            } else {
                let num = match &v {
                    Value::Number(n) => n.as_f64().ok_or_else(nan)?,
                    Value::String(s) => py_float(s).ok_or_else(nan)?,
                    _ => return Err(nan()),
                };
                if !num.is_finite() {
                    return Err(nan());
                }
                if !(lo <= num && num <= hi) {
                    return Err(range());
                }
                v = num.into();
            }
        }
        out.insert(k.clone(), v);
    }
    Ok(out)
}

/// Defaults plus saved settings. A damaged file never stops the app: it is moved to settings.json.broken
/// and the defaults are used. A saved `custom_engines` of the wrong shape is dropped.
pub fn get_settings(path: &Path) -> Result<Doc, String> {
    let mut s = default_settings();
    if !path.exists() {
        return Ok(s);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // shortcut: Python's json also reads NaN/Infinity, so such a file is kept there and moved aside here.
    let saved = std::str::from_utf8(&bytes).ok().and_then(|t| serde_json::from_str::<Value>(t).ok());
    let Some(Value::Object(mut saved)) = saved else {
        std::fs::rename(path, path.with_file_name("settings.json.broken")).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(s);
    };
    if saved.get("custom_engines").is_some_and(|v| !custom_engines_ok(v)) {
        saved.shift_remove("custom_engines");
    }
    s.extend(saved);
    Ok(s)
}

/// Validate, merge known keys into the saved settings, and write them through a temp file and a rename.
pub fn save_settings(path: &Path, patch: &Doc) -> Result<Doc, String> {
    let mut s = get_settings(path)?;
    let defaults = default_settings();
    for (k, v) in checked(patch)? {
        if defaults.contains_key(&k) {
            s.insert(k, v);
        }
    }
    let mut text = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut text, serde_json::ser::PrettyFormatter::with_indent(b" "));
    serde::Serialize::serialize(&s, &mut ser).map_err(|e| e.to_string())?;
    let tmp = path.with_file_name("settings.json.tmp");
    std::fs::write(&tmp, &text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?; // never a half-written file
    Ok(s)
}
