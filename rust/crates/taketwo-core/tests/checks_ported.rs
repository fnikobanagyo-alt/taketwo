//! Cases of backend/tests (test_audit_fixes.py, test_repeat_cutoff.py, test_regressions.py, test_ending_guard.py)
//! that test `evaluate`, `_internal_sentence_cut` and the ending guard on synthetic input, ported with their
//! own fixtures (a test fixture, not a golden vector: these assert the behaviour the Python test asserts).
//! Python's tests patch the Whisper normalizer with a simple one; these use the real port, which gives the
//! same verdict on their texts. Speaker similarity is 1.0, as the tests patch it.

use taketwo_core::checks::{self, Limits};
use taketwo_core::ending_guard::{self, CONTINUATION};
use taketwo_core::transcript::{Segment, Transcript, Word};

const SR: u32 = 24000;
const QC: Limits = Limits { max_wer: 0.12, min_speaker_similarity: 0.85, max_internal_silence: 0.9 };

fn sine(seconds: f64, hz: f64, amp: f64, round: bool) -> Vec<f32> {
    let n = if round { (seconds * SR as f64).round() } else { (seconds * SR as f64).trunc() } as usize;
    (0..n).map(|i| (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / SR as f64).sin()) as f32).collect()
}

fn geomspace(n: usize) -> Vec<f64> {
    (0..n).map(|i| if n == 1 { 1.0 } else { 10f64.powf(-5.0 * i as f64 / (n - 1) as f64) }).collect()
}

/// test_audit_fixes.tone: a 200 Hz tone that fades over `decay` seconds.
fn tone(seconds: f64, decay: f64, amp: f64) -> Vec<f32> {
    let mut a = sine(seconds, 200.0, amp, false);
    if decay > 0.0 {
        let n = (decay * SR as f64) as usize;
        let k = a.len() - n;
        for (x, g) in a[k..].iter_mut().zip(geomspace(n)) {
            *x *= g as f32;
        }
    }
    a
}

fn zeros(seconds: f64) -> Vec<f32> {
    vec![0.0; (seconds * SR as f64) as usize]
}

fn cat(parts: &[Vec<f32>]) -> Vec<f32> {
    parts.concat()
}

fn w(word: &str, start: f64, end: f64) -> Word {
    Word { word: word.into(), start, end, p: 1.0 }
}

fn tr(text: &str, words: Vec<Word>) -> Transcript {
    Transcript {
        text: text.into(),
        segments: vec![Segment { start: 0.0, end: 0.0, text: text.into(), avg_logprob: 0.0, no_speech_prob: 0.0, words }],
        language: "en".into(),
    }
}

fn eval(a: &[f32], text: &str, wps: f64, t: &Transcript) -> checks::Evaluation {
    checks::evaluate(a, SR, text, 1.0, wps, &QC, 0.09, t, |_, _| 1.0)
}

// ---- test_audit_fixes.py

#[test]
fn checks_a_short_last_word_alone_is_not_a_cut() {
    let take = cat(&[tone(1.75, 0.0, 0.3), tone(0.35, 0.0, 0.3), tone(0.03, 0.0, 0.3 * 10f64.powf(-27.0 / 20.0)), zeros(0.3)]);
    let mut words: Vec<Word> = (0..5).map(|k| w("words", k as f64 * 0.35, k as f64 * 0.35 + 0.3)).collect();
    words.push(w("Between.", 1.75, 2.13));
    let text = "words words words words words Between.";
    let ev = eval(&take, text, 2.5, &tr(text, words));
    assert!(!ev.issues.contains(&"cut_off"), "{:?}", ev.issues);
}

#[test]
fn checks_a_title_like_mr_is_not_a_sentence_end_inside_a_take() {
    let a = cat(&[tone(2.0, 0.0, 0.3), tone(1.5, 0.15, 0.3), zeros(0.3)]);
    let mut words = vec![w("Later,", 0.0, 0.6), w("Mr.", 0.7, 1.0), w("Bell", 1.2, 1.6), w("rode", 1.7, 2.2), w("up.", 2.3, 3.4)];
    assert!(!checks::internal_sentence_cut(&a, SR, &words));
    words[1].word = "hand.".into(); // the same sound after a real sentence end is still a cut
    assert!(checks::internal_sentence_cut(&a, SR, &words));
}

#[test]
fn checks_the_next_sentence_starting_early_is_not_a_cut() {
    let a = cat(&[tone(1.0, 0.15, 0.3), zeros(0.15), tone(1.0, 0.0, 0.3), zeros(0.3)]);
    let mut words: Vec<Word> = ["one", "two", "three", "four"].iter().enumerate().map(|(i, t)| w(t, i as f64 * 0.2, i as f64 * 0.2 + 0.15)).collect();
    words.extend([w("said.", 0.8, 1.0), w("But", 1.3, 1.6), w("more.", 1.7, 2.1)]);
    assert!(!checks::internal_sentence_cut(&a, SR, &words));
}

fn audit_evaluate(text: &str, heard: &str, a: &[f32]) -> checks::Evaluation {
    let last = heard.split_whitespace().last().unwrap();
    eval(a, text, 2.5, &tr(heard, vec![w(last, 0.0, a.len() as f64 / SR as f64)]))
}

#[test]
fn checks_dropped_last_word_is_rejected() {
    let a = cat(&[tone(4.0, 0.15, 0.3), zeros(0.2)]);
    let ev = audit_evaluate("We walked through the old village before returning home.", "We walked through the old village before returning.", &a);
    assert!(ev.issues.contains(&"end_word_missing"), "{:?}", ev.issues);
}

#[test]
fn checks_complete_last_word_is_accepted() {
    let a = cat(&[tone(4.0, 0.15, 0.3), zeros(0.2)]);
    let t = "We walked through the old village before returning home.";
    let ev = audit_evaluate(t, t, &a);
    assert!(!ev.issues.contains(&"end_word_missing"), "{:?}", ev.issues);
}

#[test]
fn checks_hard_stop_after_first_sentence_is_rejected() {
    let raw = cat(&[tone(0.85, 0.0, 0.3), zeros(0.3), tone(1.0, 0.2, 0.3)]);
    assert!(checks::internal_sentence_cut(&raw, SR, &[w("First.", 0.0, 0.85), w("Second", 1.15, 1.6)]));
}

#[test]
fn checks_natural_sentence_ends_pass() {
    let a = cat(&[tone(0.9, 0.3, 0.3), zeros(0.3), tone(1.0, 0.2, 0.3)]);
    assert!(!checks::internal_sentence_cut(&a, SR, &[w("First.", 0.0, 0.9), w("Second", 1.2, 1.9)]));
}

#[test]
fn checks_lead_burst_is_a_hard_issue() {
    assert!(checks::HARD_ISSUES.contains(&"lead_burst"));
    assert!(checks::HARD_ISSUES.contains(&"start_word_missing"));
}

// ---- test_repeat_cutoff.py (its tone has no decay)

fn rc_tone(d: f64, amp: f64) -> Vec<f32> {
    sine(d, 200.0, amp, false)
}

fn rc_natural() -> Vec<f32> {
    let mut a = rc_tone(2.0, 0.3);
    let n = SR as usize / 4;
    let k = a.len() - n;
    for (x, g) in a[k..].iter_mut().zip(geomspace(n)) {
        *x *= g as f32;
    }
    a.extend(vec![0.0; SR as usize / 3]);
    a
}

fn pad_of(ev: &checks::Evaluation) -> usize {
    (ev.details.as_ref().unwrap().ending.head_pad_s.unwrap() * SR as f64).round() as usize
}

#[test]
fn checks_quiet_first_sound_is_not_trimmed_or_faded() {
    let a = cat(&[rc_tone(0.3, 0.3 * 10f64.powf(-45.0 / 20.0)), rc_tone(1.7, 0.3), vec![0.001; SR as usize / 20]]);
    let ev = eval(&a, "hello", 0.5, &tr("hello", vec![w("hello", 0.0, 2.0)]));
    let pad = pad_of(&ev);
    let n = (0.3 * SR as f64) as usize;
    for (x, y) in ev.audio[pad..pad + n].iter().zip(&a[..n]) {
        assert!((x - y).abs() <= 1e-8, "{x} {y}");
    }
}

#[test]
fn checks_evaluate_keeps_a_short_release_after_a_long_closure() {
    let mut release = rc_tone(0.06, 0.3);
    let env = geomspace(release.len());
    for (x, g) in release.iter_mut().zip(env) {
        *x *= g as f32;
    }
    let a = cat(&[rc_natural()[..2 * SR as usize].to_vec(), zeros(0.3), release.clone(), vec![0.0; SR as usize / 3]]);
    let ev = eval(&a, "hello", 0.5, &tr("hello", vec![w("hello", 0.0, 2.0)]));
    assert!(!ev.details.as_ref().unwrap().ending.junk);
    assert!(!ev.issues.contains(&"cut_off"), "{:?}", ev.issues);
    let pad = pad_of(&ev);
    let lo = (2.3 * SR as f64).round() as usize;
    assert_eq!(ev.audio[pad + lo..pad + lo + release.len()], a[lo..lo + release.len()]);
}

#[test]
fn checks_missing_first_word_is_a_hard_issue_in_real_evaluate() {
    let a = rc_natural();
    let text = "When the long day was finally over they asked me to come home.";
    let heard = text.strip_prefix("When ").unwrap();
    let words = heard.split(' ').enumerate().map(|(i, t)| w(t, i as f64 * 0.12, (i + 1) as f64 * 0.12)).collect();
    let ev = eval(&a, text, 6.0, &tr(heard, words));
    assert!(ev.issues.contains(&"start_word_missing"), "{:?}", ev.issues);
    assert!(checks::HARD_ISSUES.contains(&"start_word_missing"));
}

// ---- test_regressions.py

#[test]
fn checks_evaluate_keeps_untouched_evidence_and_does_not_fade_speech_tail() {
    let a = sine(1.0, 200.0, 0.4, false);
    let ev = eval(&a, "hello", 1.0, &tr("hello", vec![w("hello.", 0.0, 1.0)]));
    assert!(ev.issues.contains(&"cut_off"), "{:?}", ev.issues);
    let d = ev.details.as_ref().unwrap();
    assert_eq!(d.raw_audio, a);
    assert!(!d.ending.natural);
    let pad = pad_of(&ev);
    let sr = SR as usize;
    assert_eq!(ev.audio[pad + sr - 600..pad + sr], a[sr - 600..sr]);
    assert!(ev.audio.len() >= a.len() + (0.12 * SR as f64) as usize);
}

// ---- test_ending_guard.py (its tone is 210 Hz, lengths rounded)

fn eg_fixture() -> (Vec<f32>, Vec<Word>) {
    let a = cat(&[sine(1.0, 210.0, 0.3, true), sine(0.25, 210.0, 0.0015, true), vec![0.0; (0.35 * SR as f64).round() as usize], sine(1.0, 210.0, 0.3, true)]);
    let words = vec![w(" village.", 0.5, 1.0), w(" Meanwhile,", 1.6, 1.85), w(" another", 1.85, 2.1), w(" chapter", 2.1, 2.35), w(" begins.", 2.35, 2.6)];
    (a, words)
}

fn r(s: f64) -> usize {
    (s * SR as f64).round() as usize
}

#[test]
fn ending_guard_english_qwen_only_gets_continuation_when_explicitly_enabled() {
    let (speak, on) = ending_guard::synthesis_text("A village.", "qwen3", "en", true);
    assert!(on);
    assert_eq!(speak, format!("A village.\n\n{CONTINUATION}"));
}

#[test]
fn ending_guard_unsupported_engines_and_languages_are_unchanged() {
    for (family, language) in [("voxcpm2", "en"), ("qwen3", "ja"), ("generic", "en")] {
        assert_eq!(ending_guard::synthesis_text("A village", family, language, true), ("A village".to_string(), false));
    }
}

#[test]
fn ending_guard_default_qwen_text_is_unchanged() {
    assert_eq!(ending_guard::synthesis_text("A village.", "qwen3", "en", false), ("A village.".to_string(), false));
}

#[test]
fn ending_guard_unpunctuated_chunk_has_sentence_separator() {
    assert!(ending_guard::synthesis_text("A village", "qwen3", "en", true).0.starts_with("A village.\n"));
}

#[test]
fn ending_guard_quiet_release_preserved_and_context_not_exported() {
    let (a, words) = eg_fixture();
    let out = ending_guard::remove_context(&a, SR, "village.", &words).unwrap();
    assert!(out.audio.len() > r(1.25) && out.audio.len() < r(1.6), "{}", out.audio.len());
    assert_eq!(out.audio[..], a[..out.audio.len()]);
    assert_eq!(out.text, "village.");
    assert_eq!(out.words.len(), 1);
    assert!(out.pause_end - out.pause_start >= 0.06);
}

#[test]
fn ending_guard_whisper_end_time_alone_does_not_cut_the_audio() {
    let (a, words) = eg_fixture();
    let out = ending_guard::remove_context(&a, SR, "village.", &words).unwrap();
    assert_eq!(out.audio[SR as usize..r(1.25)], a[SR as usize..r(1.25)]);
}

#[test]
fn ending_guard_closure_inside_last_word_is_not_selected() {
    let (mut a, words) = eg_fixture();
    a[r(0.99)..r(1.06)].fill(0.0);
    let out = ending_guard::remove_context(&a, SR, "village.", &words).unwrap();
    assert!(out.pause_start > 1.1, "{}", out.pause_start);
    assert_eq!(out.audio[..r(1.25)], a[..r(1.25)]);
}

#[test]
fn ending_guard_no_pause_is_rejected_without_saving_dummy_words() {
    let (_, words) = eg_fixture();
    assert!(ending_guard::remove_context(&sine(2.6, 210.0, 0.3, true), SR, "village.", &words).is_err());
}

#[test]
fn ending_guard_missing_context_word_is_rejected() {
    let (a, mut words) = eg_fixture();
    words.pop();
    assert!(ending_guard::remove_context(&a, SR, "village.", &words).is_err());
}

#[test]
fn ending_guard_missing_target_word_is_rejected() {
    let (a, mut words) = eg_fixture();
    words[0].word = "butchered".into();
    assert!(ending_guard::remove_context(&a, SR, "village.", &words).is_err());
}

#[test]
fn ending_guard_identical_context_in_script_uses_final_occurrence() {
    let (a, words) = eg_fixture();
    let mut all = vec![w("Meanwhile, another chapter begins.", 0.0, 0.4)];
    all.extend(words);
    let out = ending_guard::remove_context(&a, SR, &format!("{CONTINUATION} village."), &all).unwrap();
    assert_eq!(out.text, format!("{CONTINUATION} village."));
    assert!(out.audio.len() < (1.6 * SR as f64) as usize);
}
