//! Step S17: reference.py (`taketwo_core::reference`) over the scripted fake models, and the noisereduce port
//! against rust/golden/mastering.json (`noisereduce` cases).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use taketwo_core::audio::{self, ffmpeg, noisereduce};
use taketwo_core::library::Doc;
use taketwo_core::reference::{self as rf, Models, Utterance};
use taketwo_core::render::files::read_npy;
use taketwo_core::render::models::fake::Fake;
use taketwo_core::render::models::Restorer;
use taketwo_core::render::RenderError;
use taketwo_core::transcript::{Segment, Transcript, Word};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden")
}

fn f32_file(name: &str) -> Vec<f32> {
    std::fs::read(golden_dir().join("mastering").join(name)).unwrap().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[test]
fn noise_reduction_matches_python() {
    let g: Value = serde_json::from_str(&std::fs::read_to_string(golden_dir().join("mastering.json")).unwrap()).unwrap();
    let sig = &g["signals"]["nr_tone"];
    let y = f32_file(sig["file"].as_str().unwrap());
    let sr = sig["sr"].as_u64().unwrap() as u32;
    for c in g["noisereduce"].as_array().unwrap() {
        let want = f32_file(c["out"]["file"].as_str().unwrap());
        let got = match c.get("chunk_size") {
            Some(n) => noisereduce::reduce_noise_chunked(&y, sr, n.as_u64().unwrap() as usize, c["padding"].as_u64().unwrap() as usize),
            None => noisereduce::reduce_noise(&y, sr),
        }
        .unwrap();
        assert_eq!(got.len(), want.len());
        let max = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        println!("noisereduce {}: max abs difference {max:e}", c["name"]);
        // measured 0 here (bit-equal float32); 1e-4 leaves room for a mask bin at the threshold flipping when the
        // FFT library sums in another order, which moves the output by a fraction of the 0.02 noise level
        assert!(max < 1e-4, "{}: {max}", c["name"]);
    }
}

const SR: u32 = 24000;

fn have_ffmpeg(test: &str) -> bool {
    let ok = std::process::Command::new(ffmpeg::program()).arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        println!("{test}: ffmpeg is not on PATH (or TAKETWO_FFMPEG); analyze decodes with it, so this test checks nothing here");
    }
    ok
}

/// A synthetic sample: 0.5 s of near silence, then `n` utterances of three 0.4 s "words" (harmonic tones, 0.1 s
/// apart), 0.6 s between utterances. Returns the audio and the word times.
fn speech(n: usize) -> (Vec<f32>, Vec<Vec<(f64, f64)>>) {
    let (word, gap, pause) = (0.4, 0.1, 0.6);
    let mut times = Vec::new();
    let mut t = 0.5;
    for _ in 0..n {
        let mut u = Vec::new();
        for k in 0..3 {
            u.push((t, t + word));
            t += word + if k < 2 { gap } else { 0.0 };
        }
        times.push(u);
        t += pause;
    }
    let total = ((t + 0.4) * SR as f64) as usize;
    let mut seed = 12345u32;
    let mut a: Vec<f32> = (0..total)
        .map(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed as f32 / u32::MAX as f32 - 0.5) * 2e-4
        })
        .collect();
    for (ui, u) in times.iter().enumerate() {
        for &(s, e) in u {
            let (i0, i1) = ((s * SR as f64) as usize, (e * SR as f64) as usize);
            for i in i0..i1 {
                let x = (i - i0) as f64 / SR as f64;
                let env = (x / 0.02).min(1.0).min((e - s - x) / 0.02);
                let f0 = 140.0 + 15.0 * ui as f64;
                let v = (1..5).map(|h| (2.0 * std::f64::consts::PI * f0 * h as f64 * x).sin() / h as f64).sum::<f64>();
                a[i] += (0.25 * env * v) as f32;
            }
        }
    }
    (a, times)
}

fn transcript(times: &[Vec<(f64, f64)>]) -> Transcript {
    let names = ["one", "two", "three"];
    let segments: Vec<Segment> = times
        .iter()
        .map(|u| {
            let words: Vec<Word> = u
                .iter()
                .enumerate()
                .map(|(k, &(s, e))| Word { word: format!(" {}{}", names[k], if k == 2 { "." } else { "" }), start: s, end: e, p: 0.95 })
                .collect();
            Segment { start: u[0].0, end: u[2].1, text: " one two three.".into(), avg_logprob: -0.2, no_speech_prob: 0.01, words }
        })
        .collect();
    let text = segments.iter().map(|s| s.text.as_str()).collect::<String>();
    Transcript { text, segments, language: "en".into() }
}

fn emb(x: f32, y: f32) -> Vec<f32> {
    let n = (x * x + y * y).sqrt();
    vec![x / n, y / n, 0.0]
}

fn upload(dir: &Path, a: &[f32]) -> PathBuf {
    let p = dir.join("original.wav");
    audio::save(&p, a, SR).unwrap();
    p
}

/// analyze over the fake: returns (analysis, out dir, the fake).
fn run_analyze(dir: &Path, a: &[f32], tr: Transcript, embs: Vec<Vec<f32>>, studio: bool) -> (Result<Doc, RenderError>, PathBuf, Vec<(f64, String)>) {
    run_analyze_with(dir, a, tr, embs, studio, None)
}

fn run_analyze_with(dir: &Path, a: &[f32], tr: Transcript, embs: Vec<Vec<f32>>, studio: bool, restorer: Option<&mut dyn Restorer>)
                    -> (Result<Doc, RenderError>, PathBuf, Vec<(f64, String)>) {
    let src = upload(dir, a);
    let out = dir.join("voice");
    let fake = Fake::new(vec![], vec![tr], embs, 0.0, 0.0);
    let (mut t, mut e) = (fake.clone(), fake.clone());
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    let mut progress = |p: f64, m: &str| seen.push((p, m.to_string()));
    let mut m = Models { transcriber: &mut t, encoder: &mut e, restorer: restorer.map(|r| &mut *r as &mut dyn Restorer), cancel: &cancel };
    let r = rf::analyze(&src, &out, Some("en"), false, 12.0, &mut progress, studio, &mut m);
    (r, out, seen)
}

fn words(spec: &[(&str, f64, f64)]) -> Vec<Word> {
    spec.iter().map(|&(w, s, e)| Word { word: w.into(), start: s, end: e, p: 0.9 }).collect()
}

#[test]
fn utterances_split_at_pauses_sentence_ends_and_14_seconds() {
    let seg = |ws: Vec<Word>, nsp: f64| Segment { start: 0.0, end: 0.0, text: String::new(), avg_logprob: -0.3, no_speech_prob: nsp, words: ws };
    let segs = vec![
        seg(words(&[(" Hello", 0.0, 0.3), (" there.", 0.32, 0.6), (" Next", 0.75, 1.0), (" bit", 1.1, 1.3)]), 0.1),
        // a 0.34 s gap does not split; 0.35 does
        seg(words(&[(" long", 1.64, 1.9), (" gap", 2.25, 2.5)]), 0.7),
    ];
    let u = rf::utterances(&segs, 2.4);
    let got: Vec<(f64, f64, &str, usize)> = u.iter().map(|u| (u.start, u.end, u.text.as_str(), u.n_words)).collect();
    // "there." then a 0.15 s gap: a sentence end splits from 0.12 s
    assert_eq!(got, vec![(0.0, 0.6, "Hello there.", 2), (0.75, 1.9, "Next bit long", 3), (2.25, 2.4, "gap", 1)]);
    assert_eq!((u[1].no_speech_prob, u[2].no_speech_prob), (0.1, 0.7), "the first word's segment gives the probabilities");
    assert!((u[0].word_p - 0.9).abs() < 1e-12);
    // more than 14 s in one utterance splits at the next word
    let long: Vec<Word> = (0..20).map(|k| Word { word: " w".into(), start: k as f64 * 0.8, end: k as f64 * 0.8 + 0.7, p: 0.9 }).collect();
    let u = rf::utterances(&[seg(long, 0.0)], 30.0);
    assert_eq!(u.iter().map(|u| u.n_words).collect::<Vec<_>>(), vec![18, 2], "Python gives [18, 2]");
    assert!(rf::utterances(&[], 3.0).is_empty());
}

#[test]
fn pause_threshold_valleys_and_end_pauses() {
    // 100 frames: speech at -10 dB with dips at -70 dB
    let mut db = vec![-10.0f32; 100];
    for i in (10..20).chain(30..34).chain(50..60).chain(62..80) {
        db[i] = -70.0;
    }
    let thr = rf::pause_threshold(&db).unwrap();
    // p10 = -70 -> -60; p90 = -10 -> -30: the lower wins
    assert_eq!(thr, -60.0);
    assert_eq!(rf::valleys(&db, thr, 6), vec![(10, 20), (50, 60), (62, 80)], "the 4-frame dip is too short");
    // each dip is at most 20 frames and followed by at most 30 frames of audible sound: stop closures, not endings
    // (Python gives [] here too)
    assert_eq!(rf::end_pauses(&db, thr, None).unwrap(), vec![]);
    // with the next word starting at 0.22 s, every dip from there on is a pause
    let p = rf::end_pauses(&db, thr, Some(0.22)).unwrap();
    assert_eq!(p, vec![(10, 20), (50, 60), (62, 80)], "a next word at 0.22 s makes every later valley a pause");
}

#[test]
fn snap_window_moves_cuts_into_pauses() {
    let mut db = vec![-10.0f32; 600];
    for i in (0..40).chain(90..110).chain(450..480).chain(520..600) {
        db[i] = -70.0;
    }
    // speech 0.4-0.9 s and 1.1-4.5 s; the clip asked is 1.1 to 4.5
    let w = rf::snap_window(&db, 1.1, 4.5, None, 6.0).unwrap().unwrap();
    // end: valley (450, 480) -> 4.5 + min(0.22, 0.65 * 0.3); start: valley (90, 110) -> 1.1 - min(0.12, 0.1)
    assert_eq!(w, (1.1 - 0.1, 4.5 + 0.65 * 30.0 * 0.01));
    // a start at or before 0.3 s starts at 0
    assert_eq!(rf::snap_window(&db, 0.2, 4.5, None, 6.0).unwrap().unwrap().0, 0.0);
    // the next word starts before any pause: no clean end
    assert_eq!(rf::snap_window(&db, 1.1, 4.5, Some(4.4), 6.0).unwrap(), None);
    // next_start 0.0 is "none" for the limit (Python tests its truth)
    assert!(rf::snap_window(&db, 1.1, 4.5, Some(0.0), 6.0).unwrap().is_some());
    // shorter than 3 s: not used
    assert_eq!(rf::snap_window(&db, 1.1, 3.0, None, 6.0).unwrap(), None);
}

fn utt(start: f64, end: f64, text: &str, score: f64) -> Utterance {
    Utterance {
        start, end, text: text.into(), word_p: 0.9, avg_logprob: -0.2, no_speech_prob: 0.0, n_words: 3, dur: end - start,
        snr: 30.0, clip: 0.0, emb: None, sim: None, score,
    }
}

#[test]
fn candidate_windows_rank_and_never_overlap() {
    // five 2.5 s utterances 0.5 s apart; target 6 -> lo 5, hi 9
    let us: Vec<Utterance> = (0..5).map(|k| utt(k as f64 * 3.0, k as f64 * 3.0 + 2.5, if k == 1 { "ends." } else { "goes on" }, [0.5, 0.9, 0.9, 0.2, 0.8][k])).collect();
    let c = rf::candidate_windows(&us, 6.0, 4);
    let spans: Vec<(usize, usize)> = c.iter().map(|c| (c.0, c.1)).collect();
    assert_eq!(spans, vec![(1, 2), (3, 4)], "best pair first, then the best one not overlapping it");
    let s = 0.9 + 0.3 * 0.9 + 0.0 - 0.02 * (5.5f64 - 6.0).abs();
    assert_eq!(c[0].2, s);
    // a gap over 1.2 s inside a span rules it out; nothing long enough: everything, scored by the mean
    let us = vec![utt(0.0, 1.0, "a.", 0.4), utt(1.5, 2.5, "b.", 0.6)];
    assert_eq!(rf::candidate_windows(&us, 12.0, 4), vec![(0, 1, 0.5)]);
    let us = vec![utt(0.0, 3.0, "a.", 0.4), utt(4.5, 7.0, "b.", 0.6)];
    assert_eq!(rf::candidate_windows(&us, 6.0, 4), vec![(0, 1, 0.5)], "the 1.5 s gap leaves only the fallback");
}

#[test]
fn tips_follow_python() {
    assert!(rf::tips(60.0, 40.0, 0.8, 30.0, -50.0, false).is_empty());
    let t = rf::tips(10.0, 20.0, 0.5, 5.0, -20.0, false);
    assert_eq!(t.len(), 5);
    assert!(t[0].starts_with("This recording has noticeable room echo") && t[4].starts_with("Very little clean speech"));
    let t = rf::tips(10.0, 20.0, 0.5, 5.0, -20.0, true);
    assert_eq!(t.len(), 3, "studio mode drops the echo and noise tips");
}

#[test]
fn analyze_writes_clips_fingerprint_mic_and_analysis() {
    if !have_ffmpeg("analyze_writes_clips_fingerprint_mic_and_analysis") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (a, times) = speech(8);
    // seven utterances from one speaker, one outlier
    let embs: Vec<Vec<f32>> = (0..8).map(|k| if k == 5 { emb(0.0, 1.0) } else { emb(1.0, 0.05 * k as f32) }).collect();
    let (r, out, seen) = run_analyze(tmp.path(), &a, transcript(&times), embs.clone(), false);
    let an = r.unwrap();
    let keys: Vec<&str> = an.keys().map(String::as_str).collect();
    assert_eq!(keys, ["duration", "language", "snr_db", "denoised", "studio", "room_echo_db", "room_echo_after_db", "self_similarity",
                      "usable_speech_seconds", "transcript", "utterances", "variants", "tips"]);
    assert_eq!((an["denoised"].clone(), an["studio"].clone(), an["room_echo_after_db"].clone()), (json!(false), json!(false), Value::Null));
    let utts = an["utterances"].as_array().unwrap();
    assert_eq!(utts.len(), 8);
    let ukeys: Vec<&str> = utts[0].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(ukeys, ["start", "end", "text", "word_p", "avg_logprob", "no_speech_prob", "n_words", "dur", "snr", "clip", "sim", "score"]);
    // the outlier was dropped from the centroid: its similarity is low, the others' high
    assert!(utts[5]["sim"].as_f64().unwrap() < 0.2, "{}", utts[5]);
    assert!(utts.iter().enumerate().all(|(k, u)| k == 5 || u["sim"].as_f64().unwrap() > 0.95));
    let fp = read_npy(&out.join("fingerprint.npy")).unwrap();
    let mut want = [0.0f32; 3];
    for (k, e) in embs.iter().enumerate().filter(|(k, _)| *k != 5) {
        let _ = k;
        for i in 0..3 {
            want[i] += e[i] / 7.0;
        }
    }
    let n = (want.iter().map(|x| x * x).sum::<f32>()).sqrt();
    assert!(fp.iter().zip(want).all(|(a, b)| (a - b / n).abs() < 1e-6), "{fp:?}");
    // variants: best score first, files renamed to their place, each a clip in real pauses
    let vs = an["variants"].as_array().unwrap();
    assert!(!vs.is_empty() && vs.len() <= 4);
    for (k, v) in vs.iter().enumerate() {
        assert_eq!((v["index"].as_u64(), v["file"].as_str()), (Some(k as u64), Some(format!("ref_{k}.wav").as_str())));
        assert!(out.join(format!("ref_{k}.wav")).exists());
        assert!(v["duration"].as_f64().unwrap() >= 3.0);
        assert!(k == 0 || vs[k - 1]["score"].as_f64() >= v["score"].as_f64());
    }
    assert!(!out.join("tmp_0.wav").exists());
    let mic = std::fs::read_to_string(out.join("mic.json")).unwrap();
    assert!(mic.starts_with("{\"sr\": 24000, \"bands\": [") && mic.contains("NaN"), "{mic}");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(out.join("analysis.json")).unwrap()).unwrap();
    assert_eq!(file, Value::Object(an.clone()));
    assert!(std::fs::read_to_string(out.join("analysis.json")).unwrap().starts_with("{\n \"duration\""), "indent=1");
    let msgs: Vec<&str> = seen.iter().map(|s| s.1.as_str()).collect();
    assert_eq!(msgs.first(), Some(&"Decoding audio"));
    assert_eq!(msgs.last(), Some(&"Done"));
    assert!(msgs.contains(&"Transcribing the full sample (Whisper large-v3-turbo)") && msgs.contains(&"Measuring delivery and choosing reference clips"));
    assert!(!msgs.contains(&"Reducing background noise"), "a quiet sample is not denoised");
}

#[test]
fn a_studio_request_runs_the_plain_path_with_a_tip() {
    if !have_ffmpeg("a_studio_request_runs_the_plain_path_with_a_tip") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (a, times) = speech(8);
    let (r, _, _) = run_analyze(tmp.path(), &a, transcript(&times), vec![emb(1.0, 0.0); 8], true);
    let an = r.unwrap();
    assert_eq!((an["studio"].clone(), an["room_echo_after_db"].clone()), (json!(false), Value::Null));
    assert_eq!(an["tips"][0], rf::STUDIO_FALLBACK_TIP);
    // a restorer whose files are missing: the same plain path, and it is never called
    let tmp = tempfile::tempdir().unwrap();
    let mut gone = Sidonish { calls: 0, available: false, fail: false };
    let (r, _, _) = run_analyze_with(tmp.path(), &a, transcript(&times), vec![emb(1.0, 0.0); 8], true, Some(&mut gone));
    let an = r.unwrap();
    assert_eq!((an["studio"].clone(), gone.calls), (json!(false), 0));
    assert_eq!(an["tips"][0], rf::STUDIO_FALLBACK_TIP);
}

/// A stand-in for Sidon: 16 kHz in, the same audio at 48 kHz out (each sample three times).
struct Sidonish {
    calls: usize,
    available: bool,
    fail: bool,
}

impl Restorer for Sidonish {
    fn restore_segment(&mut self, x16: &[f32]) -> Result<Vec<f32>, String> {
        self.calls += 1;
        if self.fail {
            return Err("RuntimeError: the predictor failed".into());
        }
        Ok(x16.iter().flat_map(|v| [*v; 3]).collect())
    }
    fn available(&self) -> bool {
        self.available
    }
}

#[test]
fn a_studio_request_with_a_restorer_runs_the_studio_branch() {
    if !have_ffmpeg("a_studio_request_with_a_restorer_runs_the_studio_branch") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (a, times) = speech(8);
    let mut r = Sidonish { calls: 0, available: true, fail: false };
    let (res, out, seen) = run_analyze_with(tmp.path(), &a, transcript(&times), vec![emb(1.0, 0.0); 8], true, Some(&mut r));
    let an = res.unwrap();
    assert_eq!(r.calls, 1, "a short sample is one segment");
    assert_eq!((an["studio"].clone(), an["denoised"].clone()), (json!(true), json!(false)));
    assert!(an["room_echo_after_db"].is_f64(), "{:?}", an["room_echo_after_db"]);
    assert!(!an["tips"].as_array().unwrap().iter().any(|t| t == rf::STUDIO_FALLBACK_TIP));
    let (y, sr) = audio::load(&out.join("studio.wav"), None).unwrap();
    assert_eq!(sr, 48000);
    assert!(!y.is_empty());
    let msg = "Restoring to studio quality: removing echo, noise and mic colour";
    let steps: Vec<f64> = seen.iter().filter(|(_, m)| m == msg).map(|(p, _)| *p).collect();
    assert_eq!(steps, vec![0.08], "one segment reports no window progress");
    let at = seen.iter().position(|(_, m)| m == msg).unwrap();
    assert_eq!(seen[at - 1].1, "Decoding audio");
    assert!(!seen.iter().any(|(_, m)| m == "Reducing background noise"));
    // a restorer that fails on present files is an error, not a silent fallback
    let tmp = tempfile::tempdir().unwrap();
    let mut bad = Sidonish { calls: 0, available: true, fail: true };
    let (res, _, _) = run_analyze_with(tmp.path(), &a, transcript(&times), vec![emb(1.0, 0.0); 8], true, Some(&mut bad));
    match res {
        Err(RenderError::Other(m)) => assert!(m.contains("the predictor failed"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn too_short_no_speech_and_no_long_utterance() {
    if !have_ffmpeg("too_short_no_speech_and_no_long_utterance") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (a, _) = speech(2);
    let short = &a[..(2.5 * SR as f64) as usize];
    let (r, _, _) = run_analyze(tmp.path(), short, transcript(&[]), vec![], false);
    assert_eq!(r.unwrap_err(), RenderError::Value("Sample is too short. Upload at least 3 seconds (30 s - 5 min recommended).".into()));
    let (r, _, _) = run_analyze(tmp.path(), &a, Transcript { text: String::new(), segments: vec![], language: "en".into() }, vec![], false);
    assert_eq!(r.unwrap_err(), RenderError::Value("No speech found in the sample.".into()));
    // words heard as 0.1 s blips 0.4 s apart: one-word utterances under 1.2 s get no embedding, so the whole
    // sample gets one and self_sim is 0.75
    let (a, times) = speech(8);
    let mut tr = transcript(&times);
    for s in tr.segments.iter_mut() {
        for w in s.words.iter_mut() {
            w.end = w.start + 0.1;
        }
    }
    let (r, _, _) = run_analyze(tmp.path(), &a, tr, vec![emb(1.0, 0.0)], false);
    let an = r.unwrap();
    assert_eq!(an["self_similarity"], json!(0.75));
    assert!(an["utterances"].as_array().unwrap().iter().all(|u| u["sim"].is_null()));
}

#[test]
fn refresh_clips_recuts_or_keeps_each_clip() {
    if !have_ffmpeg("refresh_clips_recuts_or_keeps_each_clip") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (a, times) = speech(16);
    let (r, out, _) = run_analyze(tmp.path(), &a, transcript(&times), vec![emb(1.0, 0.0); 16], false);
    let an = r.unwrap();
    let mut profile: Doc = json!({"id": "v", "language": "en", "variants": an["variants"], "ref_text_override": {"1": "edited words"}})
        .as_object().unwrap().clone();
    let n = an["variants"].as_array().unwrap().len();
    assert!(n >= 2, "two clips to refresh");
    // clip 0: Whisper hears its text (re-cut); clip 1: the edited text is not heard and it may not be trimmed (kept)
    let heard: Vec<Transcript> = (0..n)
        .map(|k| Transcript { text: if k == 0 { an["variants"][0]["text"].as_str().unwrap().into() } else { "something else".into() }, segments: vec![], language: "en".into() })
        .collect();
    let fake = Fake::new(vec![], heard, vec![], 0.0, 0.0);
    let (mut t, mut e) = (fake.clone(), fake.clone());
    let cancel = AtomicBool::new(false);
    let mut m = Models { transcriber: &mut t, encoder: &mut e, restorer: None, cancel: &cancel };
    let rep = rf::refresh_clips(&out, &mut profile, &mut m).unwrap();
    assert_eq!(rep["updated"], json!([0]));
    assert!(rep["kept"].as_array().unwrap().contains(&json!(1)));
    assert!(rep.get("trimmed").is_none());
    assert_eq!(profile["variants"][0]["clip_version"], json!(4));
    assert!(profile["variants"][1].get("clip_version").is_none());
    assert!(out.join("ref_0.v3.wav").exists() && !out.join("ref_1.v3.wav").exists(), "only a re-cut clip keeps a backup");
}
