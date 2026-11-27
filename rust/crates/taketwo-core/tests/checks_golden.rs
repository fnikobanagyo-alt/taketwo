//! Parity with backend/app/pipeline.py `evaluate`, `_internal_sentence_cut`, `HARD_ISSUES`, `noise_limit` and with
//! backend/app/ending_guard.py (step S11a), from rust/golden/record_checks.py's checks.json and checks/.
//! Inputs are read from the recorded float32 files, never regenerated. Every case with word times runs three
//! times (as given, -0.16 s, +0.16 s), and Rust must equal Python on each.
//! Tolerances (lead decision 14): issues and their order, cut and trim positions in samples, lengths, raised or
//! not and the message: exact. Rounded numbers: equal. Audio: 1e-6 (the shifted runs: bit-equal, stored as a
//! SHA-256). Seconds: 1e-9. dB (`last_level`, `threshold_db`): 1e-3.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use taketwo_core::checks::{self, Limits};
use taketwo_core::ending_guard;
use taketwo_core::transcript::{Transcript, Word};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden")
}

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join("checks.json")).unwrap()).unwrap()
}

fn f32s(file: &Value) -> Vec<f32> {
    let b = std::fs::read(root().join(file["dir"].as_str().unwrap()).join(file["file"].as_str().unwrap())).unwrap();
    let v: Vec<f32> = b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    assert_eq!(v.len() as u64, file["len"].as_u64().unwrap(), "{file}");
    v
}

fn cases<'a>(v: &'a Value, n: usize, what: &str) -> &'a Vec<Value> {
    let c = v.as_array().unwrap();
    assert_eq!(c.len(), n, "number of golden cases for {what}");
    c
}

fn num(v: &Value) -> f64 {
    match v {
        Value::String(s) => match s.as_str() {
            "-inf" => f64::NEG_INFINITY,
            "inf" => f64::INFINITY,
            "nan" => f64::NAN,
            _ => panic!("{s}"),
        },
        _ => v.as_f64().unwrap(),
    }
}

fn close(what: &str, got: f64, want: f64, tol: f64, max: &mut f64) {
    let d = if got == want { 0.0 } else { (got - want).abs() };
    assert!(d <= tol, "{what}: {got} against {want}");
    *max = max.max(d);
}

fn audio_matches(what: &str, got: &[f32], want: &Value, max: &mut f64) {
    if let Some(h) = want.get("sha256") {
        assert_eq!(got.len() as u64, want["len"].as_u64().unwrap(), "{what}: length");
        let mut s = Sha256::new();
        for x in got {
            s.update(x.to_le_bytes());
        }
        let hex: String = s.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, h.as_str().unwrap(), "{what}: samples (bit-equal)");
    } else {
        let w = f32s(want);
        assert_eq!(got.len(), w.len(), "{what}: length");
        for (a, b) in got.iter().zip(&w) {
            close(what, *a as f64, *b as f64, 1e-6, max);
        }
    }
}

fn words(v: &Value) -> Vec<Word> {
    serde_json::from_value(v.clone()).unwrap()
}

#[test]
fn checks_hard_issues_and_noise_limit() {
    let g = golden();
    let mut want: Vec<&str> = g["hard_issues"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let mut got = checks::HARD_ISSUES.to_vec();
    want.sort();
    got.sort();
    assert_eq!(got, want);
    let c = cases(&g["noise_limit"], 13, "noise_limit");
    for case in c {
        if let Some(p) = case.get("profile") {
            assert_eq!(checks::noise_limit(num(&p["noise_baseline"])), num(&case["limit"]), "{case}");
        } else {
            let vals: Vec<f64> = case["vals"].as_array().unwrap().iter().map(num).collect();
            let b = checks::noise_baseline(&vals);
            assert_eq!(b, num(&case["baseline"]), "{case}");
            assert_eq!(checks::noise_limit(b), num(&case["limit"]), "{case}");
        }
    }
}

#[test]
fn checks_evaluate_golden() {
    let g = golden();
    let c = cases(&g["evaluate"], 39, "evaluate");
    let (mut audio_max, mut sec_max, mut db_max) = (0.0f64, 0.0f64, 0.0f64);
    let mut runs_n = 0;
    for case in c {
        let name = case["name"].as_str().unwrap();
        let a = f32s(&case["signal"]);
        let sr = case["sr"].as_u64().unwrap() as u32;
        let l = &case["limits"];
        let limits = Limits {
            max_wer: num(&l["max_wer"]),
            min_speaker_similarity: num(&l["min_speaker_similarity"]),
            max_internal_silence: num(&l["max_internal_silence"]),
        };
        let sim = num(&case["similarity"]);
        for run in case["runs"].as_array().unwrap() {
            runs_n += 1;
            let what = format!("{name} {}", run["timing"]);
            let tr: Transcript = serde_json::from_value(run["transcript"].clone()).unwrap();
            let mut handed: Vec<(Vec<f32>, u32)> = Vec::new();
            let ev = checks::evaluate(
                &a, sr, case["text"].as_str().unwrap(), num(&case["self_sim"]), num(&case["wps"]), &limits, num(&case["noise_max"]), &tr,
                |x, r| {
                    handed.push((x.to_vec(), r));
                    sim
                },
            );
            let want_issues: Vec<&str> = run["issues"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            assert_eq!(ev.issues, want_issues, "{what}: issues");
            for (k, got) in [("score", ev.score), ("wer", ev.wer), ("sim", ev.sim), ("max_gap", ev.max_gap), ("dur", ev.dur)] {
                assert_eq!(got, num(&run[k]), "{what}: {k}");
            }
            assert_eq!(ev.sr as u64, run["sr"].as_u64().unwrap());
            audio_matches(&what, &ev.audio, &run["audio"], &mut audio_max);
            assert_eq!(!handed.is_empty(), run["similarity_called"].as_bool().unwrap(), "{what}: similarity called");
            assert!(handed.len() <= 1);
            if let Some((x, r)) = handed.first() {
                assert_eq!(x.len() as u64, run["similarity_len"].as_u64().unwrap(), "{what}: similarity input length");
                assert_eq!(run["similarity_input_is_audio"].as_bool(), Some(true));
                assert!(x == &ev.audio && *r == sr, "{what}: similarity gets the returned audio");
            }
            match (&ev.details, run.get("gaps")) {
                (None, None) => {}
                (Some(d), Some(gaps)) => {
                    let gaps = gaps.as_array().unwrap();
                    assert_eq!(d.gaps.len(), gaps.len(), "{what}: gaps");
                    for ((s, e), w) in d.gaps.iter().zip(gaps) {
                        close(&what, *s, num(&w[0]), 1e-9, &mut sec_max);
                        close(&what, *e, num(&w[1]), 1e-9, &mut sec_max);
                    }
                    assert_eq!(d.heard, run["heard"].as_str().unwrap());
                    assert_eq!(d.words, words(&run["words"]), "{what}: words");
                    assert_eq!(d.raw_audio == a, run["raw_is_input"].as_bool().unwrap());
                    let e = &run["ending"];
                    let en = &d.ending;
                    for (k, got) in [("start", en.start), ("cut", en.cut), ("raw_duration", en.raw_duration), ("last_word_end", en.last_word_end)] {
                        close(&format!("{what} ending {k}"), got, num(&e[k]), 1e-9, &mut sec_max);
                    }
                    close(&format!("{what} last_level"), en.last_level, num(&e["last_level"]), 1e-3, &mut db_max);
                    assert_eq!(en.natural, e["natural"].as_bool().unwrap(), "{what}: natural");
                    assert_eq!(en.junk, e["junk"].as_bool().unwrap(), "{what}: junk");
                    assert_eq!(en.trim_start.is_some(), e.get("trim_start").is_some());
                    if let Some(t) = en.trim_start {
                        close(&format!("{what} trim_start"), t, num(&e["trim_start"]), 1e-9, &mut sec_max);
                        assert_eq!((t * sr as f64) as i64, (num(&e["trim_start"]) * sr as f64) as i64, "{what}: trim start in samples");
                        assert_eq!(en.head_pad_s, Some(num(&e["head_pad_s"])));
                    }
                    assert_eq!((en.cut * sr as f64) as i64, run["cut_sample"].as_i64().unwrap(), "{what}: cut in samples");
                }
                _ => panic!("{what}: keys differ from Python's"),
            }
        }
    }
    eprintln!("checks evaluate: {runs_n} runs; audio max {audio_max:.3e} (tolerance 1e-6), seconds max {sec_max:.3e} (1e-9), last_level max {db_max:.3e} dB (1e-3)");
}

#[test]
fn checks_internal_sentence_cut_golden() {
    let g = golden();
    let c = cases(&g["internal_sentence_cut"], 6, "internal_sentence_cut");
    for case in c {
        let a = f32s(&case["signal"]);
        let sr = case["sr"].as_u64().unwrap() as u32;
        let runs = case["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 3);
        for run in runs {
            let got = checks::internal_sentence_cut(&a, sr, &words(&run["words"]));
            assert_eq!(got, run["cut"].as_bool().unwrap(), "{} {}", case["name"], run["timing"]);
        }
    }
}

#[test]
fn ending_guard_synthesis_text_and_tokens() {
    let g = golden();
    let eg = &g["ending_guard"];
    assert_eq!(eg["continuation"].as_str().unwrap(), ending_guard::CONTINUATION);
    for c in cases(&eg["synthesis_text"], 10, "synthesis_text") {
        let (t, on) = ending_guard::synthesis_text(
            c["text"].as_str().unwrap(), c["family"].as_str().unwrap(), c["language"].as_str().unwrap(), c["enabled"].as_bool().unwrap(),
        );
        assert_eq!(t, c["out"].as_str().unwrap(), "{c}");
        assert_eq!(on, c["guarded"].as_bool().unwrap(), "{c}");
    }
    for c in cases(&eg["tokens"], 13, "tokens") {
        let want: Vec<String> = serde_json::from_value(c["out"].clone()).unwrap();
        assert_eq!(ending_guard::tokens(c["text"].as_str().unwrap()), want, "{c}");
    }
}

#[test]
fn ending_guard_remove_context_golden() {
    let g = golden();
    let c = cases(&g["ending_guard"]["remove_context"], 9, "remove_context");
    let (mut sec_max, mut db_max) = (0.0f64, 0.0f64);
    for case in c {
        let a = f32s(&case["signal"]);
        let sr = case["sr"].as_u64().unwrap() as u32;
        let text = case["text"].as_str().unwrap();
        for run in case["runs"].as_array().unwrap() {
            let what = format!("{} {}", case["name"], run["timing"]);
            let got = ending_guard::remove_context(&a, sr, text, &words(&run["words"]));
            if !run["ok"].as_bool().unwrap() {
                assert_eq!(got.map(|_| ()).unwrap_err().to_string(), run["error"].as_str().unwrap(), "{what}");
                continue;
            }
            let r = got.unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(r.audio.len() as u64, run["len"].as_u64().unwrap(), "{what}: cut in samples");
            assert_eq!(r.audio[..], a[..r.audio.len()]);
            assert_eq!(r.text, run["text"].as_str().unwrap());
            assert_eq!(r.words, words(&run["words_kept"]));
            let i = &run["info"];
            assert_eq!(r.version as f64, num(&i["version"])); // recorded through num(); Python returns the int 2
            assert_eq!(i["continuation"].as_str().unwrap(), ending_guard::CONTINUATION);
            for (k, v) in [("full_duration", r.full_duration), ("cut_seconds", r.cut_seconds), ("pause_start", r.pause_start),
                           ("pause_end", r.pause_end), ("context_start", r.context_start), ("last_word_end", r.last_word_end)] {
                close(&format!("{what} {k}"), v, num(&i[k]), 1e-9, &mut sec_max);
            }
            close(&format!("{what} threshold_db"), r.threshold_db, num(&i["threshold_db"]), 1e-3, &mut db_max);
        }
    }
    eprintln!("ending_guard remove_context: seconds max {sec_max:.3e} (1e-9), threshold_db max {db_max:.3e} dB (1e-3)");
}
