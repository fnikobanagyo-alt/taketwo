//! Parity with backend/app/pipeline.py `render_chunk`, `_guarded_take` and `widen_sentence_pauses` (step S15),
//! from rust/golden/record_render.py's render.json and render/. Every case replays Python's scripted model
//! outputs through the Rust code and must give the same part document (key order, int against float, null),
//! the same model calls in the same order, the same files, the same profile and `put_profile` calls, the same
//! clock readings and the same raised error.
//! Tolerances (lead decision 15): exact everywhere except `expected_seconds` (1e-12), the unrounded fallback
//! `score` (1e-9) and `chunk_NNN.wav` (S10's `normalize_lufs` tolerance, 1e-3 of full scale on the saved samples).
//! `raw_chunk`, `context_chunk` and `rejected_context` WAVs: the float32 samples bit-equal (SHA-256).

mod render_common;

use render_common::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use taketwo_core::render::models::fake::Kind;
use taketwo_core::render::{widen_sentence_pauses, PartWord};

fn sha_f32(a: &[f32]) -> String {
    let mut s = Sha256::new();
    for x in a {
        s.update(x.to_le_bytes());
    }
    format!("{:x}", s.finalize())
}

fn float_wav(p: &std::path::Path) -> (Vec<f32>, u32) {
    let mut r = hound::WavReader::open(p).unwrap();
    assert_eq!(r.spec().sample_format, hound::SampleFormat::Float, "{}", p.display());
    let sr = r.spec().sample_rate;
    (r.samples::<f32>().map(Result::unwrap).collect(), sr)
}

#[test]
fn render_golden_cases() {
    let g = golden();
    let cases = g["render"].as_array().unwrap();
    assert_eq!(cases.len(), 47, "number of golden render cases");
    let (mut num_max, mut pcm_max) = (0.0f64, 0i32);
    let mut cache = HashMap::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let want = &case["result"];
        let out = replay(case, &mut cache, |_| {});
        // the part, or the error
        match (&out.result, want.get("part")) {
            (Ok(part), Some(p)) => same(name, &Value::Object(part.clone()), p, &Value::Null, &mut num_max),
            (Err(e), None) => {
                assert_eq!(e.kind(), want["error"]["type"], "{name}: error type ({e:?})");
                assert_eq!(e.message(), want["error"]["message"], "{name}: error message");
            }
            (got, _) => panic!("{name}: Rust gave {got:?}, Python {want}"),
        }
        // the model calls, in order, with their arguments
        let got_calls = calls_json(&out.fake.calls());
        let want_calls: Vec<Value> = want["calls"].as_array().unwrap().iter().map(python_call).collect();
        same(&format!("{name}.calls"), &Value::Array(got_calls), &Value::Array(want_calls), &Value::Null, &mut num_max);
        assert_eq!(out.fake.left(), (0, 0, 0), "{name}: script used up");
        assert_eq!(out.fake.count(Kind::Clock) as u64, want["clock_reads"].as_u64().unwrap(), "{name}: clock readings");
        // the profile afterwards and the put_profile calls
        same(&format!("{name}.profile"), &Value::Object(out.profile.clone()), &want["profile_after"], &Value::Null, &mut num_max);
        let puts: Vec<Value> = out.puts.iter().cloned().map(Value::Object).collect();
        same(&format!("{name}.put_profile"), &Value::Array(puts), &want["put_profile"], &Value::Null, &mut num_max);
        // the files in the narration folder
        let mut names: Vec<String> = std::fs::read_dir(&out.gdir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        let wf = want["files"].as_object().unwrap();
        assert_eq!(names, wf.keys().cloned().collect::<Vec<_>>(), "{name}: files");
        let pre = case["inputs"]["pre_files"].as_object().unwrap();
        for (f, w) in wf {
            let p = out.gdir.join(f);
            if w.get("unchanged").is_some() {
                let hex: String = std::fs::read(&p).unwrap().iter().map(|b| format!("{b:02x}")).collect();
                assert_eq!(Some(&Value::from(hex)), pre.get(f), "{name}: {f} untouched");
            } else if let Some(j) = w.get("json") {
                let got: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
                same(&format!("{name}.{f}"), &got, j, &Value::Null, &mut num_max);
            } else if let Some(pcm) = w.get("pcm16") {
                let mut r = hound::WavReader::open(&p).unwrap();
                assert_eq!((r.spec().bits_per_sample, r.spec().sample_format), (16, hound::SampleFormat::Int), "{name}: {f}");
                assert_eq!(r.spec().sample_rate as u64, w["sr"].as_u64().unwrap());
                let got: Vec<i16> = r.samples::<i16>().map(Result::unwrap).collect();
                let wantv = i16s(pcm);
                assert_eq!(got.len(), wantv.len(), "{name}: {f} length");
                for (a, b) in got.iter().zip(&wantv) {
                    let d = (*a as i32 - *b as i32).abs();
                    assert!(d as f64 / 32768.0 <= 1e-3, "{name}: {f} samples {a} against {b}");
                    pcm_max = pcm_max.max(d);
                }
            } else {
                let (a, sr) = float_wav(&p);
                assert_eq!(sr as u64, w["sr"].as_u64().unwrap(), "{name}: {f} rate");
                assert_eq!(a.len() as u64, w["float"]["len"].as_u64().unwrap(), "{name}: {f} length");
                assert_eq!(sha_f32(&a), w["float"]["sha256"].as_str().unwrap(), "{name}: {f} samples bit-equal");
            }
        }
    }
    eprintln!("render parity: largest numeric difference {num_max:.3e} (expected_seconds 1e-12, fallback score 1e-9, else 0); chunk_NNN.wav largest sample difference {pcm_max} LSB of int16 (limit 32)");
}

#[test]
fn render_golden_widen_sentence_pauses() {
    let g = golden();
    let cases = g["widen"].as_array().unwrap();
    assert_eq!(cases.len(), 6, "number of golden widen cases");
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let a: Vec<f32> = c["pieces"].as_array().unwrap().iter().flat_map(f32s).collect();
        let words: Vec<PartWord> = c["words"].as_array().unwrap().iter().map(|w| PartWord {
            word: w["word"].as_str().unwrap().into(), start: w["start"].as_f64().unwrap(), end: w["end"].as_f64().unwrap() }).collect();
        let (out, w) = widen_sentence_pauses(&a, c["sr"].as_u64().unwrap() as u32, c["text"].as_str().unwrap(), &words, c["target_s"].as_f64().unwrap());
        assert_eq!(out.len() as u64, c["out"]["len"].as_u64().unwrap(), "{name}: length");
        assert_eq!(sha_f32(&out), c["out"]["sha256"].as_str().unwrap(), "{name}: samples bit-equal");
        assert_eq!(out != a, c["changed"].as_bool().unwrap(), "{name}: widened or not");
        let got: Vec<Value> = w.iter().map(|w| serde_json::json!({"word": w.word, "start": w.start, "end": w.end})).collect();
        let mut m = 0.0;
        same(name, &Value::Array(got), &c["words_out"], &Value::Null, &mut m);
    }
}

#[test]
fn render_golden_reads_a_libsndfile_float_wav() {
    let g = golden();
    let w = &g["sndfile_float_wav"];
    let dir = tempdir("probe");
    let p = dir.0.join("probe.wav");
    std::fs::write(&p, bytes(w)).unwrap();
    let (a, sr) = taketwo_core::audio::load(&p, None).unwrap();
    let want: Vec<f32> = w["samples"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
    assert_eq!((a, sr as u64), (want, w["sr"].as_u64().unwrap()));
}
