//! Port of backend/app/ending_guard.py (step S11a): the experimental spoken continuation, off by default.
//! ASR word coverage cannot prove a complete final consonant; keeping the requested ending away from the
//! model's end-of-sequence is preventive, not a phoneme score. Only English is enabled.
//! Parity vectors: rust/golden/record_checks.py -> rust/golden/checks.json, tests/checks_golden.rs.

use crate::audio::{self, np::percentile_f32, np::py_slice};
use crate::text::{self, py};
use crate::transcript::Word;

pub const CONTINUATION: &str = "Meanwhile, another chapter begins.";

/// Python's `UnsafeBoundary(ValueError)`: the message is Python's, word for word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsafeBoundary(pub &'static str);

impl std::fmt::Display for UnsafeBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for UnsafeBoundary {}

/// `synthesis_text(text, family, language, enabled=...)`: the text sent to the model and whether it is guarded.
pub fn synthesis_text(text: &str, family: &str, language: &str, enabled: bool) -> (String, bool) {
    let lang = language.to_lowercase();
    if !(enabled && family == "qwen3" && (lang == "en" || lang == "english")) {
        return (text.to_string(), false);
    }
    let body = py::rstrip(text);
    // A period also separates chunks ending at a comma/semicolon or without punctuation.
    let terminal = if body.ends_with(['.', '!', '?']) { "" } else { "." };
    (format!("{body}{terminal}\n\n{CONTINUATION}"), true)
}

/// Python's `str.casefold()` for one character.
/// shortcut: the full-folding exceptions of Latin, Greek and ligatures plus `to_lowercase`; scripts with
/// other CaseFolding.txt entries (Cherokee, some Armenian ligatures) would differ. Upgrade if a voice needs them.
fn casefold_char(c: char, out: &mut String) {
    match c {
        'ß' | 'ẞ' => out.push_str("ss"),
        'ſ' => out.push('s'),
        'ς' => out.push('σ'),
        'ϐ' => out.push('β'),
        'ϑ' => out.push('θ'),
        'ϕ' => out.push('φ'),
        'ϖ' => out.push('π'),
        'ϰ' => out.push('κ'),
        'ϱ' => out.push('ρ'),
        'ϵ' => out.push('ε'),
        'ẛ' => out.push('ṡ'),
        '\u{345}' => out.push('ι'),
        'ﬀ' => out.push_str("ff"),
        'ﬁ' => out.push_str("fi"),
        'ﬂ' => out.push_str("fl"),
        'ﬃ' => out.push_str("ffi"),
        'ﬄ' => out.push_str("ffl"),
        'ﬅ' | 'ﬆ' => out.push_str("st"),
        // per character, so a final capital sigma folds to σ as Python does (no word-final ς)
        _ => out.extend(c.to_lowercase()),
    }
}

/// `_tokens(text)`: `re.findall(r"[^\W_]+", text.casefold())`.
pub fn tokens(text: &str) -> Vec<String> {
    let mut folded = String::new();
    for c in text.chars() {
        casefold_char(c, &mut folded);
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in folded.chars() {
        if py::is_word(c) && c != '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `remove_context`'s info dict.
#[derive(Clone, Debug, PartialEq)]
pub struct Removed {
    pub audio: Vec<f32>,
    /// The returned transcript `{"text": ..., "segments": [{"words": ...}]}` as its text and words.
    pub text: String,
    pub words: Vec<Word>,
    pub version: i64,
    pub full_duration: f64,
    pub cut_seconds: f64,
    pub pause_start: f64,
    pub pause_end: f64,
    pub context_start: f64,
    pub last_word_end: f64,
    pub threshold_db: f64,
}

/// `remove_context(a, sr, text, transcript)` on the transcript's words: a sample-exact prefix and its words,
/// or the reason the boundary is unsafe. The cut must sit inside >= 60 ms of acoustic silence before the context.
pub fn remove_context(a: &[f32], sr: u32, text: &str, words: &[Word]) -> Result<Removed, UnsafeBoundary> {
    let srf = sr as f64;
    let (mut toks, mut owners) = (Vec::new(), Vec::new());
    for (wi, w) in words.iter().enumerate() {
        for t in tokens(&w.word) {
            toks.push(t);
            owners.push(wi);
        }
    }
    let suffix = tokens(CONTINUATION);
    if toks.len() <= suffix.len() || toks[toks.len() - suffix.len()..] != suffix[..] {
        return Err(UnsafeBoundary("Continuation was not fully recognized"));
    }
    let first_context = owners[toks.len() - suffix.len()];
    if first_context == 0 || owners[toks.len() - suffix.len() - 1] == first_context {
        return Err(UnsafeBoundary("Requested ending and continuation share a word timestamp"));
    }
    let expected = tokens(text);
    let last_target = &words[first_context - 1];
    let actual = tokens(&last_target.word);
    if expected.is_empty() || actual.is_empty() || text::ratio(expected.last().unwrap(), &actual.concat()) < 0.6 {
        return Err(UnsafeBoundary("Requested final word was not recognized before the continuation"));
    }
    let context_start = words[first_context].start;
    let last_end = last_target.end;
    let lo = (last_target.start + 0.06).max(last_end - 0.15);
    let hi = context_start - 0.02;
    let db = audio::frame_db(a, sr, 10);
    if db.is_empty() || !(0.0 <= lo && lo < hi && hi <= a.len() as f64 / srf) {
        return Err(UnsafeBoundary("No room to separate the requested ending"));
    }
    // Compare to the target voice, not an unusually loud continuation.
    let target_db = &db[py_slice(db.len(), 0, ((lo / 0.01) as i64).max(1))];
    let (left, right) = ((lo / 0.01).ceil() as i64, (hi / 0.01).floor() as i64);
    if right - left < 6 {
        return Err(UnsafeBoundary("Word timestamps leave no measurable pause"));
    }
    let win = &db[py_slice(db.len(), left, right)];
    // Codec silence has a noise floor: require a valley near it and at least 42 dB below speech.
    let floor = percentile_f32(win, 10.0).expect("a window of six frames or more") as f64;
    let speech = percentile_f32(target_db, 90.0).expect("at least one frame") as f64;
    let threshold = (-105.0f64).max((speech - 42.0).min(floor + 6.0));
    let thr32 = threshold as f32; // a float32 array against a Python float compares in float32 (numpy 2)
    let mut runs = Vec::new();
    let mut run_start = None;
    for (i, &d) in win.iter().chain(std::iter::once(&f32::INFINITY)).enumerate() {
        let quiet = i < win.len() && d < thr32;
        match (quiet, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                if i - s >= 6 {
                    runs.push((left + s as i64, left + i as i64));
                }
                run_start = None;
            }
            _ => {}
        }
    }
    let Some(&(start, end)) = runs.last() else {
        return Err(UnsafeBoundary("No verified quiet pause before the continuation"));
    };
    let cut = ((start + end) as f64 * 0.005 * srf).round_ties_even() as usize;
    let retained = words[..first_context].to_vec();
    Ok(Removed {
        audio: a[..cut.min(a.len())].to_vec(),
        text: retained.iter().map(|w| py::strip(&w.word)).collect::<Vec<_>>().join(" "),
        words: retained,
        version: 2,
        full_duration: a.len() as f64 / srf,
        cut_seconds: cut as f64 / srf,
        pause_start: start as f64 * 0.01,
        pause_end: end as f64 * 0.01,
        context_start,
        last_word_end: last_end,
        threshold_db: threshold,
    })
}
