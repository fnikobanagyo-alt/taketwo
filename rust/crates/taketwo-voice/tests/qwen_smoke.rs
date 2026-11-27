//! Qwen smoke run (step S12), ignored by default, release build, macOS only:
//!     source rust/scripts/env.sh && cargo test --release -p taketwo-voice --features metal -- --ignored qwen_smoke --nocapture
//! Checks Franz's app on 8766 is idle (GET only) and memory is at least 50% free, else prints "busy" and writes nothing.
//! The reference is made with macOS `say` in a temporary folder. Three takes at most, one process, through the
//! Voice trait. Writes rust/out/qwen_smoke.json. Numbers only, no claim about sound; no WAV is kept.
#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use taketwo_core::library::Doc;
use taketwo_core::render::models::Voice;
use taketwo_voice::qwen::QwenVoice;

const REF_TEXT: &str = "The quick brown fox jumps over the lazy dog near the river bank.";
const LINE: &str = "This is a short test line.";

fn peak_rss_bytes() -> u64 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        u.ru_maxrss as u64 // bytes on macOS
    }
}

/// None when idle; Some(reason) when busy or unknown.
fn busy() -> Option<String> {
    let out = Command::new("curl").args(["-s", "-m", "5", "http://127.0.0.1:8766/api/status"]).output().ok()?;
    let st: Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return Some(format!("status unreadable: {}", String::from_utf8_lossy(&out.stdout))),
    };
    if !(st["running"].is_null() && st["queued"] == json!(0) && st["loaded_engine"].is_null()) {
        return Some(format!("app busy: running={} queued={} loaded_engine={}", st["running"], st["queued"], st["loaded_engine"]));
    }
    let mp = Command::new("memory_pressure").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let free = mp
        .lines()
        .find_map(|l| l.strip_prefix("System-wide memory free percentage: "))
        .and_then(|p| p.trim().trim_end_matches('%').parse::<u32>().ok());
    match free {
        Some(f) if f >= 50 => None,
        other => Some(format!("memory free {other:?}% (< 50)")),
    }
}

#[test]
#[ignore]
fn qwen_smoke() {
    if let Some(why) = busy() {
        println!("busy: {why}");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("taketwo-qwen-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let ref_wav = tmp.join("ref.wav");
    let ok = Command::new("say")
        .args(["-o", ref_wav.to_str().unwrap(), "--file-format=WAVE", "--data-format=LEI16@24000", REF_TEXT])
        .status()
        .unwrap()
        .success();
    assert!(ok, "say failed");

    let mut voice = QwenVoice::from_env().unwrap();
    let t0 = Instant::now();
    voice.load().unwrap();
    let load_seconds = t0.elapsed().as_secs_f64();

    // the real decoder's raw length for 10 frames, before decode_unpadded cuts it (a measurement)
    let m = voice.model().unwrap();
    let codes = candle_core::Tensor::ones((1, 10, 16), candle_core::DType::U32, m.device()).unwrap();
    let decoded_10 = m.decode_codes(&codes).unwrap().dims().last().copied().unwrap();

    let cancel = AtomicBool::new(false);
    let mut takes = Vec::new();
    let mut audios = Vec::new();
    for seed in [1u32, 1, 2] {
        let t = Instant::now();
        let take = voice.synthesize("qwen3-tts-1.7b", LINE, &ref_wav, REF_TEXT, "en", &Doc::new(), seed, 2.5, &cancel).unwrap();
        let seconds = t.elapsed().as_secs_f64();
        let r = &take.report;
        for k in ["tokens", "token_limit", "raw_samples", "sample_rate", "stop_reason", "sampler", "generation_route", "decoder"] {
            assert!(r.contains_key(k), "report key {k} missing: {r:?}");
        }
        let raw = r["raw_samples"].as_u64().unwrap() as usize;
        assert_eq!(raw, take.audio.len());
        assert_eq!(raw % 1920, 0, "raw_samples {raw}");
        assert!(raw > 0, "empty take");
        assert!(take.audio.iter().any(|&x| x != 0.0), "all-zero take");
        takes.push(json!({
            "seed": seed, "tokens": r["tokens"], "token_limit": r["token_limit"], "raw_samples": raw, "raw_mod_1920": raw % 1920,
            "stop_reason": r["stop_reason"], "seconds": seconds, "audio_seconds": raw as f64 / take.sr as f64,
            "decoder": r["decoder"], "sampler": r["sampler"],
        }));
        audios.push(take.audio);
    }
    let commit = Command::new("git").args(["rev-parse", "HEAD"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let doc = json!({
        "takes": takes,
        "same_seed_equal": audios[0] == audios[1],
        "load_seconds": load_seconds,
        "peak_memory_bytes": peak_rss_bytes(),
        "decoder_raw_samples_for_10_frames": decoded_10,
        "commit": commit,
        "line": LINE,
        "reference": "macOS say, 24 kHz PCM16, temporary folder",
    });
    voice.unload();
    let _ = std::fs::remove_dir_all(&tmp);
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("qwen_smoke.json"), serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
