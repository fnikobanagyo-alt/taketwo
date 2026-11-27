//! Whisper parity run (step S13), ignored by default; rust/scripts/whisper_parity.sh runs it:
//!     WHISPER_PARITY_CLIPS=<dir> cargo test -p taketwo-voice -- --ignored whisper_parity
//! Reads clip1..clip4.wav (16 kHz mono, made with `say`) and rust/out/whisper/python.json, runs
//! whisper.cpp with both head choices, writes rust/out/whisper/parity.json. Numbers only, no verdict.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use taketwo_core::text::normalize_for_wer;
use taketwo_voice::whisper::{Heads, Transcriber, Transcript, WhisperCpp};

const CLIPS: [&str; 4] = ["clip1", "clip2", "clip3", "clip4"];

fn read_wav(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let spec = r.spec();
    assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (1, 16000, 16), "{}", path.display());
    // the same scaling as the Python side: int16 / 32768
    r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}

fn peak_rss_bytes() -> Option<u64> {
    #[cfg(unix)]
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        // bytes on macOS, kilobytes on Linux
        Some(if cfg!(target_os = "macos") { u.ru_maxrss as u64 } else { u.ru_maxrss as u64 * 1024 })
    }
    #[cfg(not(unix))]
    None
}

fn words_of(t: &Value) -> Vec<(String, f64, f64)> {
    t["segments"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["words"].as_array().unwrap().iter())
        .map(|w| (w["word"].as_str().unwrap().to_string(), w["start"].as_f64().unwrap(), w["end"].as_f64().unwrap()))
        .collect()
}

fn max_median(v: &mut [f64]) -> (Option<f64>, Option<f64>) {
    if v.is_empty() {
        return (None, None);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let med = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    (Some(v[n - 1]), Some(med))
}

fn compare(rust: &Transcript, py: &Value, duration: f64) -> Value {
    let r = serde_json::to_value(rust).unwrap();
    let rw = words_of(&r);
    let pw = words_of(&py["transcript"]);
    let r_norm: Vec<String> = rw.iter().map(|w| normalize_for_wer(&w.0)).collect();
    let p_norm: Vec<String> = py["normalized_words"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_string()).collect();
    let words_equal = r_norm == p_norm;
    let r_text = normalize_for_wer(&rw.iter().map(|w| w.0.as_str()).collect::<String>());
    let text_equal = r_text == py["normalized_text"].as_str().unwrap();
    let monotonic = rw.iter().all(|w| w.1 <= w.2) && rw.windows(2).all(|p| p[1].1 >= p[0].1 && p[1].2 >= p[0].2);
    // times are rounded to 0.01 s, so the clip end is allowed that rounding
    let inside_clip = rw.iter().all(|w| w.1 >= 0.0 && w.2 <= duration + 0.005);
    let (mut ds, mut de): (Vec<f64>, Vec<f64>) = if rw.len() == pw.len() {
        rw.iter().zip(&pw).map(|(a, b)| ((a.1 - b.1).abs(), (a.2 - b.2).abs())).unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    let (start_max, start_median) = max_median(&mut ds);
    let (end_max, end_median) = max_median(&mut de);
    let seg = |t: &Value, k: &str| -> Vec<Value> { t["segments"].as_array().unwrap().iter().map(|s| s[k].clone()).collect() };
    let mut out = json!({
        "words_equal": words_equal,
        "text_equal": text_equal,
        "word_count": {"rust": rw.len(), "python": pw.len()},
        "monotonic": monotonic,
        "inside_clip": inside_clip,
        "start_abs_diff_max": start_max,
        "start_abs_diff_median": start_median,
        "end_abs_diff_max": end_max,
        "end_abs_diff_median": end_median,
        "segments": {"rust": r["segments"].as_array().unwrap().len(), "python": py["transcript"]["segments"].as_array().unwrap().len()},
        "avg_logprob": {"rust": seg(&r, "avg_logprob"), "python": seg(&py["transcript"], "avg_logprob")},
        "no_speech_prob": {"rust": seg(&r, "no_speech_prob"), "python": seg(&py["transcript"], "no_speech_prob")},
        "rust_text": r["text"],
        "python_text": py["transcript"]["text"],
    });
    if !words_equal {
        out["rust_words"] = json!(rw.iter().map(|w| &w.0).collect::<Vec<_>>());
        out["python_words"] = json!(pw.iter().map(|w| &w.0).collect::<Vec<_>>());
    }
    out
}

#[test]
#[ignore = "needs the converted ggml model and the clips from rust/scripts/whisper_parity.sh"]
fn whisper_parity() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let clips = PathBuf::from(std::env::var("WHISPER_PARITY_CLIPS").expect("WHISPER_PARITY_CLIPS"));
    let models = std::env::var("TAKETWO_MODELS").map(PathBuf::from).unwrap_or_else(|_| root.join(".models"));
    let model = models.join("ggml-large-v3-turbo.bin");
    let use_gpu = std::env::var("WHISPER_PARITY_GPU").as_deref() == Ok("1");
    let py: Value = serde_json::from_str(&std::fs::read_to_string(root.join("out/whisper/python.json")).unwrap()).unwrap();
    let audio: Vec<Vec<f32>> = CLIPS.iter().map(|c| read_wav(&clips.join(format!("{c}.wav")))).collect();
    let never = AtomicBool::new(false);

    let mut report = json!({"model_loaded": false, "use_gpu": use_gpu, "metal_feature": cfg!(feature = "metal"), "heads": {}});
    for (key, heads) in [("top_two_layers", Heads::TopTwoLayers), ("large_v3_turbo", Heads::LargeV3Turbo)] {
        let t = Instant::now();
        let mut w = WhisperCpp::open(&model, use_gpu, heads).expect("model loads");
        let load_s = t.elapsed().as_secs_f64();
        report["model_loaded"] = json!(true);
        let mut per = json!({"load_seconds": load_s, "clips": {}});
        for (i, c) in CLIPS.iter().enumerate() {
            let t = Instant::now();
            let r = w.transcribe(&audio[i], Some("en"), true, &never).expect("transcribes");
            let secs = t.elapsed().as_secs_f64();
            let mut cmp = compare(&r, &py["clips"][*c], audio[i].len() as f64 / 16000.0);
            cmp["seconds"] = json!(secs);
            cmp["python_seconds"] = py["clips"][*c]["seconds"].clone();
            eprintln!("{key} {c}: {secs:.2} s, words_equal {}", cmp["words_equal"]);
            per["clips"][*c] = cmp;
        }
        if heads == Heads::TopTwoLayers {
            let t = Instant::now();
            let r = w.transcribe(&audio[0], None, true, &never).expect("transcribes");
            let mut cmp = compare(&r, &py["clips"]["clip1_auto"], audio[0].len() as f64 / 16000.0);
            cmp["seconds"] = json!(t.elapsed().as_secs_f64());
            report["detected_language"] = json!({"rust": r.language, "python": py["clips"]["clip1_auto"]["transcript"]["language"]});
            per["clips"]["clip1_auto"] = cmp;
            let cancel = AtomicBool::new(true);
            let t = Instant::now();
            let res = w.transcribe(&audio[0], Some("en"), true, &cancel);
            report["cancel"] = json!({"error": res.err(), "seconds": t.elapsed().as_secs_f64()});
        }
        report["heads"][key] = per;
        drop(w); // one model in memory at a time
    }
    report["peak_rss_bytes"] = json!(peak_rss_bytes());
    report["python"] = json!({"load_seconds": py["load_seconds"], "peak_rss_bytes": py["peak_rss_bytes"], "alignment_heads": py["alignment_heads"].as_array().map(|a| a.len())});
    let out = root.join("out/whisper/parity.json");
    std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!("wrote {}", out.display());
}
