//! The take checks of backend/app/pipeline.py (step S11a): `evaluate`, `_internal_sentence_cut`, `HARD_ISSUES`
//! and the arithmetic of `noise_limit` (its file reads and `put_profile` are S15).
//! The transcript is always given (Whisper runs in the caller); `language` only fed that call and is dropped.
//! The speaker similarity is a callback, called where Python calls `ml.cosine(ml.speaker_embedding(a, sr), fp)`.
//! Parity vectors: rust/golden/record_checks.py -> rust/golden/checks.json, tests/checks_golden.rs.

use crate::audio::{self, np::py_slice, np::percentile_f32};
use crate::text::{self, py, ABBREVIATIONS};
use crate::transcript::{Transcript, Word};

/// pipeline.HARD_ISSUES: a take with one of these is never accepted as clean.
pub const HARD_ISSUES: [&str; 18] = [
    "cut_off", "junk_tail", "lead_burst", "ending_unverified", "start_word_missing", "end_word_missing", "flat", "off_pitch",
    "odd_pause", "empty", "long_silence", "too_long", "too_short", "voice_drift", "noise", "wrong_words", "mispronounced", "clipping",
];

/// Python `round(x, n)` on a float: the exact binary value rounded half-to-even, read back
/// (Rust's `{:.n}` formats the exact value with ties to even, as CPython's dtoa rounds).
pub fn py_round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().expect("formatted float parses")
}

/// The three settings `evaluate` reads from the QC dict.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub max_wer: f64,
    pub min_speaker_similarity: f64,
    pub max_internal_silence: f64,
}

/// `result["ending"]`: end_analysis's five keys, then (words branch only) `trim_start` and `head_pad_s`,
/// then `raw_duration` and `last_word_end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ending {
    pub start: f64,
    pub cut: f64,
    pub natural: bool,
    pub junk: bool,
    pub last_level: f64,
    pub trim_start: Option<f64>,
    pub head_pad_s: Option<f64>,
    pub raw_duration: f64,
    pub last_word_end: f64,
}

/// The keys `evaluate` adds after the first eight (absent on the `empty` early return).
#[derive(Clone, Debug, PartialEq)]
pub struct Details {
    pub gaps: Vec<(f64, f64)>,
    pub heard: String,
    pub raw_audio: Vec<f32>,
    pub words: Vec<Word>,
    pub ending: Ending,
}

/// `evaluate`'s dict. `issues` keeps Python's order (a second `cut_off` included).
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluation {
    pub audio: Vec<f32>,
    pub sr: u32,
    pub issues: Vec<&'static str>,
    pub score: f64,
    pub wer: f64,
    pub sim: f64,
    pub max_gap: f64,
    pub dur: f64,
    pub details: Option<Details>,
}

/// `noise_limit` once `noise_baseline` is known: max(0.09, baseline * 1.3 + 0.05).
pub fn noise_limit(baseline: f64) -> f64 {
    let x = baseline * 1.3 + 0.05;
    if x > 0.09 { x } else { 0.09 }
}

/// `noise_baseline` from the noise_bursts of the voice's files: round(max(vals, default=0.0), 3).
pub fn noise_baseline(vals: &[f64]) -> f64 {
    // Python's max keeps the first of equal values; for numbers that is the same value
    py_round(vals.iter().copied().reduce(f64::max).unwrap_or(0.0), 3)
}

fn ends_with_any(s: &str, marks: &[char]) -> bool {
    s.chars().last().is_some_and(|c| marks.contains(&c))
}

/// `int(x)` of a Python float: truncation toward zero.
fn int(x: f64) -> i64 {
    x as i64
}

/// `_internal_sentence_cut(raw, sr, words)`: true when a sentence that ends inside a multi-sentence take
/// stops while still loud. Judged on the untouched take.
pub fn internal_sentence_cut(raw: &[f32], sr: u32, words: &[Word]) -> bool {
    let db = audio::frame_db(raw, sr, 10);
    // float32 percentile minus a Python int stays float32; the comparison is in float32
    let thr = percentile_f32(&db, 90.0).expect("a take has frames") - 40.0f32;
    let quiet: Vec<bool> = db.iter().map(|&d| d < thr).collect();
    for k in 0..words.len().saturating_sub(1) {
        let w = &words[k];
        let token = py::strip(&w.word);
        let bare = token.trim_end_matches('.').to_lowercase();
        if !ends_with_any(token, &['.', '!', '?']) || ABBREVIATIONS.contains(&bare.as_str()) {
            continue; // Whisper writes "Mister" as "Mr.": not a sentence end
        }
        let gap = words[k + 1].start - w.end;
        if gap < 0.15 {
            continue;
        }
        let mut stop = int((w.end + gap * 0.8) / 0.01);
        let mut i = int(w.end / 0.01);
        let limit = (int(words[k + 1].start / 0.01) + 10).min(quiet.len() as i64);
        while i < limit {
            let mut j = i;
            while j < quiet.len() as i64 && quiet[j as usize] {
                j += 1;
            }
            if j - i >= 8 {
                stop = i + 4;
                break;
            }
            i = j + 1;
        }
        let seg = &raw[py_slice(raw.len(), 0, int(stop as f64 * 0.01 * sr as f64))];
        if (seg.len() as f64) < sr as f64 * 0.2 {
            continue;
        }
        if !audio::end_analysis(seg, sr, w.end, 0.12).natural {
            return true;
        }
    }
    false
}

/// `evaluate(a, sr, text, fp, self_sim, wps, s, language, noise_max, transcript)`: score one take.
/// `similarity(audio, sr)` stands for `ml.cosine(ml.speaker_embedding(audio, sr), fp)`; it receives the
/// trimmed, padded and faded take and is called only when the take is longer than 1.0 s.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    a: &[f32],
    sr: u32,
    text: &str,
    self_sim: f64,
    wps: f64,
    s: &Limits,
    noise_max: f64,
    transcript: &Transcript,
    mut similarity: impl FnMut(&[f32], u32) -> f64,
) -> Evaluation {
    let srf = sr as f64;
    let mut issues: Vec<&'static str> = Vec::new();
    if (a.len() as f64) < srf * 0.3 {
        return Evaluation { audio: a.to_vec(), sr, issues: vec!["empty"], score: 99.0, wer: 1.0, sim: 0.0, max_gap: 0.0, dur: 0.0, details: None };
    }
    let words: Vec<Word> = transcript.segments.iter().flat_map(|seg| seg.words.iter().cloned()).collect();
    let raw_dur = a.len() as f64 / srf;
    let last_word_end = words.last().map_or(raw_dur, |w| w.end);
    let end = audio::end_analysis(a, sr, last_word_end, 0.12);
    if !end.natural {
        issues.push("cut_off");
    }
    if audio::lead_burst(a, sr).is_some() {
        issues.push("lead_burst"); // the take opens with stray hiss before its first word
    }
    let (mut trim_start, mut head_pad_s) = (None, None);
    let out: Vec<f32>;
    if !words.is_empty() {
        if end.junk {
            issues.push("junk_tail");
        }
        let ss = audio::speech_start(a, sr, 100);
        let w0 = (words[0].start - 0.10).max(0.0);
        let start = if w0 < ss { w0 } else { ss };
        let body = &a[py_slice(a.len(), int(start * srf), int(end.cut * srf))];
        let head_pad = 0.02;
        let (head, tail) = (int(head_pad * srf) as usize, int(0.12 * srf) as usize);
        let mut padded = vec![0.0f32; head + body.len() + tail];
        padded[head..head + body.len()].copy_from_slice(body);
        trim_start = Some(start);
        head_pad_s = Some(head_pad);
        out = audio::fade(&padded, sr, 15, 25);
        // a long pause after a word with no punctuation is odd; the first one that is decides
        for (gs, ge) in audio::silence_report(&out, sr, 0.25).internal_gaps {
            if ge - gs < 0.5 {
                continue;
            }
            let t0 = gs + start - head_pad;
            let before = words.iter().rfind(|w| w.start <= t0 + 0.05);
            if before.is_some_and(|b| !ends_with_any(py::strip(&b.word), &[',', '.', '?', '!', ';', ':', '—', '-', '…'])) {
                issues.push("odd_pause");
                break;
            }
        }
        if internal_sentence_cut(a, sr, &words) {
            issues.push("cut_off"); // a sentence in the middle of the take stops while still loud
        }
    } else {
        let t = audio::trim_edges(a, sr, 60, 220);
        let mut padded = t.to_vec();
        padded.resize(t.len() + int(0.12 * srf) as usize, 0.0);
        out = padded;
    }
    let w = text::wer(text, &transcript.text);
    let coverage = text::word_coverage(text, &transcript.text);
    if coverage.last() == Some(&false) {
        issues.push("end_word_missing"); // WER alone accepts a dropped last word
    }
    if coverage.first() == Some(&false) {
        issues.push("start_word_missing");
    }

    let rep = audio::silence_report(&out, sr, 0.25);
    if rep.max_internal > s.max_internal_silence {
        issues.push("long_silence");
    }

    let dur = out.len() as f64 / srf;
    let per = text::word_count(text) as f64 / wps;
    let expected = if per > 0.8 { per } else { 0.8 };
    let ratio = dur / expected;
    if ratio > 1.75 {
        issues.push("too_long");
    } else if ratio < 0.5 {
        issues.push("too_short");
    }

    let sim = if dur > 1.0 { similarity(&out, sr) } else { self_sim };
    let rel_sim = sim / if self_sim > 1e-3 { self_sim } else { 1e-3 };
    if rel_sim < s.min_speaker_similarity {
        issues.push("voice_drift");
    }

    let nb = audio::noise_bursts(&out, sr);
    if nb >= noise_max {
        issues.push("noise");
    }
    if audio::clipping_ratio(&out) > 0.003 {
        issues.push("clipping");
    }
    if w > s.max_wer {
        issues.push(if w < 0.35 { "mispronounced" } else { "wrong_words" });
    }

    let has = |i: &str| issues.contains(&i);
    let flag = |on: bool, v: f64| if on { v } else { 0.0 };
    let score = 4.0 * w
        + 3.0 * (s.min_speaker_similarity - rel_sim).max(0.0)
        + flag(has("long_silence"), 1.5)
        + flag(has("too_long") || has("too_short"), 1.0)
        + 4.0 * (nb - noise_max + 0.09).max(0.0)
        + flag(has("clipping"), 0.5)
        + flag(has("cut_off"), 2.5)
        + flag(has("odd_pause"), 1.5)
        + flag(has("lead_burst"), 2.5)
        + flag(has("junk_tail"), 3.0)
        + flag(has("start_word_missing") || has("end_word_missing"), 2.5);
    let ending = Ending {
        start: end.start, cut: end.cut, natural: end.natural, junk: end.junk, last_level: end.last_level,
        trim_start, head_pad_s, raw_duration: raw_dur, last_word_end,
    };
    Evaluation {
        sr,
        issues,
        score: py_round(score, 4),
        wer: py_round(w, 3),
        sim: py_round(rel_sim, 3),
        max_gap: py_round(rep.max_internal, 2),
        dur: py_round(dur, 2),
        details: Some(Details { gaps: rep.internal_gaps, heard: transcript.text.clone(), raw_audio: a.to_vec(), words, ending }),
        audio: out,
    }
}

#[cfg(test)]
mod tests {
    use super::py_round;

    #[test]
    fn py_round_matches_python_on_half_cases() {
        // Python: round(2.625, 2) == 2.62, round(0.125, 2) == 0.12, round(0.375, 2) == 0.38 (exact halves, to even);
        // round(2.675, 2) == 2.67 and round(1.005, 2) == 1.0 (the binary value is under the half).
        for (x, n, want) in [(2.625, 2, 2.62), (0.125, 2, 0.12), (0.375, 2, 0.38), (2.675, 2, 2.67), (1.005, 2, 1.0), (0.5, 0, 0.0), (1.5, 0, 2.0), (-0.125, 2, -0.12)] {
            assert_eq!(py_round(x, n), want, "round({x}, {n})");
        }
    }
}
