//! Backend tests of render_chunk, _guarded_take, the pitch gate and widen_sentence_pauses (step S15), ported.
//! Python patches `evaluate` and `prosody`; Rust cannot, so each test runs the real code on a golden case of
//! rust/golden/render.json (real synthetic takes, scripted model calls) or a widen vector, and asserts what the
//! Python test asserts. The test-by-test list is in rust/PROGRESS.md (S15 entry).

mod render_common;

use render_common::*;
use serde_json::Value;
use std::collections::HashMap;
use taketwo_core::audio;
use taketwo_core::render::models::fake::Call;
use taketwo_core::render::{self, widen_sentence_pauses, PartWord, RenderError};

fn case<'a>(g: &'a Value, name: &str) -> &'a Value {
    g["render"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap_or_else(|| panic!("no golden case {name}"))
}

fn run(g: &Value, name: &str) -> Outcome {
    replay(case(g, name), &mut HashMap::new(), |_| {})
}

fn part(o: &Outcome) -> &taketwo_core::library::Doc {
    o.result.as_ref().unwrap()
}

fn issues(o: &Outcome) -> Vec<Vec<String>> {
    part(o)["attempts"].as_array().unwrap().iter().map(|a| a["issues"].as_array().unwrap().iter().map(|i| i.as_str().unwrap().to_string()).collect()).collect()
}

fn synth(o: &Outcome) -> Vec<(String, u32, String)> {
    o.fake.calls().into_iter().filter_map(|c| match c {
        Call::Synthesize { text, seed, ref_file, .. } => Some((text, seed, ref_file)),
        _ => None,
    }).collect()
}

fn gap_after_first(a: &[f32], sr: u32) -> f64 {
    audio::silence_report(a, sr, 0.02).internal_gaps.iter().map(|(s, e)| e - s).fold(0.0, f64::max)
}

fn pcm(p: &std::path::Path) -> (Vec<f32>, u32) {
    audio::load(p, None).unwrap()
}

// ------------------------------------------------------------------ test_narration_quality.py: pitch level

#[test]
fn take_far_from_the_voice_median_is_retaken() {
    let g = golden();
    let o = run(&g, "off_pitch_median");
    assert_eq!(issues(&o), [vec!["off_pitch".to_string()], vec![]]);
    assert_eq!(part(&o)["status"], "ok");
    assert_eq!(part(&o)["pitch_hz"], part(&o)["attempts"][1]["pitch_hz"]);
}

#[test]
fn take_far_from_its_neighbour_part_is_retaken() {
    let g = golden();
    // every take is within 2 semitones of the voice's 150 Hz but far from the next part's 118 Hz
    let o = run(&g, "off_pitch_neighbours");
    for (a, i) in part(&o)["attempts"].as_array().unwrap().iter().zip(issues(&o)) {
        let hz = a["pitch_hz"].as_f64().unwrap();
        assert!((12.0 * (hz / 150.0).log2()).abs() <= 2.0 && i.contains(&"off_pitch".to_string()), "{a}");
    }
}

#[test]
fn closest_pitch_is_kept_when_every_take_is_off() {
    let g = golden();
    let o = run(&g, "off_pitch_all_closest");
    let p = part(&o);
    assert_eq!(p["status"], "flagged");
    assert_eq!(p["issues"], serde_json::json!(["off_pitch"]));
    let closest = p["attempts"].as_array().unwrap().iter().map(|a| (a["pitch_hz"].as_f64().unwrap() / 110.0).log2().abs()).fold(f64::MAX, f64::min);
    assert_eq!((p["pitch_hz"].as_f64().unwrap() / 110.0).log2().abs(), closest);
}

#[test]
fn voice_without_a_stored_median_is_not_gated() {
    let g = golden();
    let o = run(&g, "no_stored_median");
    assert_eq!(part(&o)["status"], "ok");
    assert!(part(&o)["pitch_hz"].is_null());
}

// ------------------------------------------------------------------ test_narration_quality.py: sentence pauses

fn widen_case(g: &Value, name: &str) -> (Vec<Vec<f32>>, u32, String, Vec<PartWord>, f64) {
    let c = g["widen"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap();
    let pieces = c["pieces"].as_array().unwrap().iter().map(f32s).collect();
    let words = c["words"].as_array().unwrap().iter().map(|w| PartWord { word: w["word"].as_str().unwrap().into(), start: w["start"].as_f64().unwrap(), end: w["end"].as_f64().unwrap() }).collect();
    (pieces, c["sr"].as_u64().unwrap() as u32, c["text"].as_str().unwrap().into(), words, c["target_s"].as_f64().unwrap())
}

#[test]
fn short_full_stop_is_widened_to_the_target_without_touching_speech() {
    let g = golden();
    let (p, sr, text, words, target) = widen_case(&g, "short_full_stop");
    let (s1, s2) = (&p[0], &p[2]);
    let out = widen_sentence_pauses(&p.concat(), sr, &text, &words, target).0;
    assert!((gap_after_first(&out, sr) - 0.30).abs() <= 0.03);
    assert_eq!(&out[..s1.len()], &s1[..]);
    assert_eq!(&out[out.len() - s2.len()..], &s2[..]);
    assert!(out[s1.len()..out.len() - s2.len()].iter().any(|x| *x != 0.0), "room tone, not digital silence");
}

#[test]
fn full_stop_misheard_as_a_comma_is_still_widened() {
    let g = golden();
    let (p, sr, text, words, target) = widen_case(&g, "misheard_comma");
    assert_eq!(words[1].word, " half,");
    let out = widen_sentence_pauses(&p.concat(), sr, &text, &words, target).0;
    assert!((gap_after_first(&out, sr) - 0.30).abs() <= 0.03);
}

#[test]
fn pause_goes_after_the_word_not_into_a_dip_inside_it() {
    let g = golden();
    let (p, sr, text, words, target) = widen_case(&g, "dip_inside_word");
    let out = widen_sentence_pauses(&p.concat(), sr, &text, &words, target).0;
    assert_eq!(&out[..p[0].len()], &p[0][..]);
    assert!((gap_after_first(&out, sr) - 0.30).abs() <= 0.03);
}

#[test]
fn long_enough_pause_and_comma_are_left_alone() {
    let g = golden();
    for name in ["long_enough", "comma_in_script"] {
        let (p, sr, text, words, target) = widen_case(&g, name);
        let a = p.concat();
        assert_eq!(widen_sentence_pauses(&a, sr, &text, &words, target).0, a, "{name}");
    }
}

#[test]
fn full_stop_inside_a_part_gets_the_setting_when_turned_on() {
    let g = golden();
    let o = run(&g, "inner_pause_300");
    let (a, sr) = pcm(&o.gdir.join(part(&o)["file"].as_str().unwrap()));
    assert!((gap_after_first(&a, sr) - 0.30).abs() <= 0.03, "{}", gap_after_first(&a, sr));
}

#[test]
fn kept_word_times_follow_the_widened_audio() {
    let g = golden();
    let (on, off) = (run(&g, "inner_pause_300"), run(&g, "inner_pause_0"));
    let start = |o: &Outcome, i: usize| part(o)["words"][i]["start"].as_f64().unwrap();
    assert_eq!(start(&on, 0), start(&off, 0), "before the full stop: not moved");
    let (a, sr) = pcm(&off.gdir.join("chunk_000.wav"));
    let added = 0.30 - gap_after_first(&a, sr);
    assert!((start(&on, 2) - start(&off, 2) - added).abs() <= 0.03, "after the full stop: moved by the added quiet");
}

#[test]
fn off_by_default_until_a_length_is_chosen_by_ear() {
    assert_eq!(taketwo_core::library::default_settings()["inner_sentence_pause_ms"], 0);
    let g = golden();
    let o = run(&g, "inner_pause_0");
    let (a, sr) = pcm(&o.gdir.join("chunk_000.wav"));
    assert!((gap_after_first(&a, sr) - 0.08).abs() <= 0.03, "{}", gap_after_first(&a, sr));
}

// ------------------------------------------------------------------ test_ending_guard.py

#[test]
fn default_render_does_not_synthesize_or_remove_dummy_context() {
    let g = golden();
    let o = run(&g, "clean_first");
    assert_eq!(synth(&o).iter().map(|s| s.0.clone()).collect::<Vec<_>>(), ["The river ran past the old mill."]);
    assert!(part(&o)["ending_guard"].is_null() && part(&o)["context_file"].is_null());
    assert_eq!(taketwo_core::library::default_settings()["experimental_qwen_ending_guard"], false);
}

#[test]
fn false_string_cannot_accidentally_enable_experimental_context() {
    let g = golden();
    for name in ["guard_false_string", "guard_one_int"] {
        let o = run(&g, name);
        assert_eq!(synth(&o)[0].0, "The river ran past the old mill.", "{name}");
        assert!(part(&o)["context_file"].is_null(), "{name}");
    }
}

#[test]
fn real_render_path_saves_separate_context_evidence() {
    let g = golden();
    let c = case(&g, "guard_one_sentence");
    let o = replay(c, &mut HashMap::new(), |_| {});
    let p = part(&o);
    assert_eq!(p["status"], "ok");
    assert!(p["ending_guard"].is_object());
    let (saved, sr) = pcm(&o.gdir.join(p["context_file"].as_str().unwrap()));
    assert_eq!(saved, f32s(&c["script"]["takes"][0]["audio"]), "the whole take with its continuation");
    assert_eq!(sr, 16000);
    let plain = (7.0f64 / 2.6).max(1.0);
    assert!(p["attempts"][0]["expected_seconds"].as_f64().unwrap() > plain + 0.4 - 1e-9, "room for the continuation");
}

#[test]
fn all_unsafe_takes_leave_old_audio_untouched() {
    let g = golden();
    let o = run(&g, "guard_all_unsafe_old_file");
    match &o.result {
        Err(RenderError::Value(m)) => assert!(m.contains("safely separate"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(o.gdir.join("chunk_000.wav")).unwrap(), b"previous clip");
    assert!(!o.gdir.join("raw_chunk_000.wav").exists());
}

#[test]
fn each_sentence_inside_one_part_is_guarded() {
    let g = golden();
    let o = run(&g, "guard_two_sentences");
    let s = synth(&o);
    assert_eq!(s[0].0, format!("Pull half.\n\n{}", taketwo_core::ending_guard::CONTINUATION));
    assert_eq!(s[1].0, format!("One nudge more.\n\n{}", taketwo_core::ending_guard::CONTINUATION));
    assert_ne!(s[0].1, s[1].1);
    let p = part(&o);
    let ctx = p["attempts"][1]["ending_guard"]["sentences"].as_array().unwrap();
    assert_eq!(ctx.len(), 2);
    assert!(ctx[1]["offset"].as_f64().unwrap() >= ctx[0]["cut_seconds"].as_f64().unwrap(), "the second sentence comes after the first");
    let w = p["words"].as_array().unwrap();
    assert_eq!(w.iter().map(|w| w["word"].as_str().unwrap().trim()).collect::<Vec<_>>(), ["Pull", "half.", "One", "nudge", "more."]);
    assert!(w[2]["start"].as_f64().unwrap() > w[1]["end"].as_f64().unwrap(), "word times moved by the offset");
    let (full, _) = pcm(&o.gdir.join("context_chunk_000.wav"));
    let takes = &case(&g, "guard_two_sentences")["script"]["takes"];
    assert_eq!(full.len(), f32s(&takes[2]["audio"]).len() + f32s(&takes[3]["audio"]).len(), "both whole takes kept as evidence");
}

#[test]
fn new_narration_can_continue_with_flagged_plain_fallback() {
    let g = golden();
    let o = run(&g, "guard_all_unsafe_fallback");
    let p = part(&o);
    assert_eq!(p["status"], "flagged");
    assert!(p["issues"].as_array().unwrap().contains(&"ending_unverified".into()));
    assert!(p["context_file"].is_null());
    let at = p["attempts"].as_array().unwrap();
    assert_eq!(at.last().unwrap()["ending_guard_fallback"], true);
    assert_ne!(at[0]["seed"], at.last().unwrap()["seed"]);
}

#[test]
fn bad_second_boundary_does_not_regenerate_first_sentence() {
    let g = golden();
    let o = run(&g, "guard_local_retry");
    let s = synth(&o);
    assert_eq!(s.len(), 3);
    let ctx = &part(&o)["ending_guard"]["sentences"];
    assert_eq!(ctx[0]["boundary_attempts"].as_array().unwrap().len(), 1);
    assert_eq!(ctx[1]["boundary_attempts"].as_array().unwrap().len(), 2);
    let seeds: std::collections::BTreeSet<u32> = s.iter().map(|x| x.1).collect();
    assert_eq!(seeds.len(), 3);
}

// ------------------------------------------------------------------ test_repeat_cutoff.py

#[test]
fn when_every_take_fails_the_last_one_uses_another_clip() {
    let g = golden();
    let o = run(&g, "last_take_other_clip");
    let refs: Vec<i64> = part(&o)["attempts"].as_array().unwrap().iter().map(|a| a["ref"].as_i64().unwrap()).collect();
    assert_eq!(refs, [0, 0, 2]); // the next-best clip by score (Python's test has two clips: [0, 0, 1])
    assert_eq!(synth(&o).last().unwrap().2, "ref_2.wav");
}

/// Render `name`'s part once, then again from the result with `second`'s script.
fn render_twice(g: &Value, name: &str, second: &str, nonces: (u32, u32)) -> (Outcome, Outcome) {
    let mut cache = HashMap::new();
    let a = replay_with(case(g, name), &mut cache, None, Some(nonces.0), |_| {});
    let b = replay_with(case(g, second), &mut cache, Some((&a.narration, &a.gdir)), Some(nonces.1), |_| {});
    (a, b)
}

#[test]
fn two_generations_do_not_repeat_the_same_take_seed() {
    let g = golden();
    // two narrations draw two nonces (the caller's `secrets.randbits(32)`)
    let mut cache = HashMap::new();
    let a = replay_with(case(&g, "clean_first"), &mut cache, None, Some(1), |_| {});
    let b = replay_with(case(&g, "budget_stored_low"), &mut cache, None, Some(2), |_| {});
    assert_ne!(synth(&a)[0].1, synth(&b)[0].1);
}

#[test]
fn repeated_render_even_without_regen_counter_has_new_seed() {
    let g = golden();
    let (a, b) = render_twice(&g, "budget_stored_low", "clean_first", (100, 200));
    assert_ne!(synth(&a)[0].1, synth(&b)[0].1);
}

#[test]
fn render_avoids_previous_seed_even_if_random_nonce_repeats() {
    let g = golden();
    let (a, b) = render_twice(&g, "budget_stored_low", "clean_first", (100, 100));
    assert_ne!(synth(&a)[0].1, synth(&b)[0].1);
    assert_eq!(synth(&b)[0].1, synth(&a)[0].1 + 1, "the used seed is skipped");
}

#[test]
fn learned_duration_budget_survives_a_later_render() {
    let g = golden();
    let (a, b) = render_twice(&g, "length_limit", "clean_first", (7, 7));
    let initial = part(&a)["attempts"][0]["expected_seconds"].as_f64().unwrap();
    assert!(part(&a)["duration_budget"].as_f64().unwrap() > 1.0);
    assert!(part(&b)["attempts"][0]["expected_seconds"].as_f64().unwrap() > initial);
}

// ------------------------------------------------------------------ test_regressions.py

#[test]
fn render_saves_raw_float_and_keeps_cutoff_flagged() {
    let g = golden();
    let (a, b) = render_twice(&g, "one_take_no_switch", "one_take_no_switch", (1, 2));
    assert_eq!(part(&a)["audio_revision"], 1);
    assert_eq!(part(&b)["audio_revision"], 2, "the revision changes only with a new saved take");
    assert_eq!(part(&b)["status"], "flagged");
    // the take as synthesized, float32, before any trim
    let c = case(&g, "one_take_no_switch");
    let mut r = hound::WavReader::open(b.gdir.join(part(&b)["raw_file"].as_str().unwrap())).unwrap();
    assert_eq!((r.spec().sample_format, r.spec().bits_per_sample), (hound::SampleFormat::Float, 32));
    let raw: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(raw, f32s(&c["script"]["takes"][0]["audio"]));
    // a cut-off take stays flagged
    let o = run(&g, "length_limit");
    assert_eq!(issues(&o)[0], ["cut_off"]);
}

#[test]
fn clean_take_outranks_lower_scoring_failed_take() {
    let g = golden();
    // take 1 is mispronounced with score 0.64; take 2 is clean
    let o = run(&g, "seed_schedule");
    let p = part(&o);
    assert_eq!(p["status"], "ok");
    assert_eq!(p["issues"], serde_json::json!([]));
    assert_eq!(p["chosen_seed"], p["attempts"][2]["seed"]);
}

#[test]
fn token_cap_is_never_a_clean_take() {
    let g = golden();
    // budget_cap's first take is clean audio stopped at the token cap
    let o = run(&g, "budget_cap");
    assert_eq!(issues(&o)[0], ["cut_off"]);
    assert_eq!(part(&o)["attempts"][0]["synthesis"]["stop_reason"], "length_limit_reached");
}

// ------------------------------------------------------------------ test_audit_fixes.py

#[test]
fn pitch_is_remeasured_after_the_reference_is_replaced() {
    let dir = tempdir("pitch");
    let p = dir.0.join("pitch_ref.wav");
    let tone = |sec: f64, hz: f64, wobble: f64| -> Vec<f32> {
        (0..(sec * 16000.0) as usize).map(|i| { let t = i as f64 / 16000.0; (0.3 * (2.0 * std::f64::consts::PI * hz * (t + wobble * (2.0 * std::f64::consts::PI * 3.0 * t).sin())).sin()) as f32 }).collect()
    };
    audio::save(&p, &tone(1.0, 150.0, 0.002), 16000).unwrap();
    let mut cache = HashMap::new();
    let first = render::ref_pitch(&mut cache, &p).unwrap();
    assert_eq!(render::ref_pitch(&mut cache, &p).unwrap(), first, "cached");
    audio::save(&p, &tone(1.5, 150.0, 0.006), 16000).unwrap(); // replaced in place
    let second = render::ref_pitch(&mut cache, &p).unwrap();
    assert_ne!(second, first, "the replaced clip is measured again");
    assert_eq!(cache.len(), 2);
}
