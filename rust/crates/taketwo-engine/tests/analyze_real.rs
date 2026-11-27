//! One real load for voice creation (step S17), ignored by default, release build, macOS only:
//!     source rust/scripts/env.sh && cargo test --release -p taketwo-engine --features metal --test analyze_real -- --ignored --nocapture
//! Checks Franz's app on 8766 is idle (GET /api/status only), else prints "busy" and writes nothing. `reference::analyze`
//! runs once on a macOS `say` clip through the real renderer's Whisper and speaker encoder (no voice model, no take),
//! in a temporary folder. Writes rust/out/analyze_real.json: seconds, peak memory, utterance and clip counts.
//! Numbers only: no claim about sound or transcription quality.
#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use taketwo_core::reference;
use taketwo_engine::real::{self, RealRenderer};
use taketwo_engine::render::Renderer;

const TEXT: &str = "The morning was quiet and the river ran slowly past the old mill. \
    We walked along the bank and talked about the long winter ahead. \
    Later the clouds came in from the west, and a light rain began to fall. \
    By evening the fields were green again, and the birds were singing in the trees.";

fn peak_rss_bytes() -> u64 {
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        u.ru_maxrss as u64 // bytes on macOS
    }
}

/// None when Franz's app is idle; Some(reason) when busy or unknown.
fn busy() -> Option<String> {
    let out = Command::new("curl").args(["-s", "-m", "5", "http://127.0.0.1:8766/api/status"]).output().ok()?;
    let st: Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return Some(format!("status unreadable: {}", String::from_utf8_lossy(&out.stdout))),
    };
    if st["running"].is_null() && st["queued"] == json!(0) { None } else { Some(format!("app busy: running={} queued={}", st["running"], st["queued"])) }
}

#[test]
#[ignore]
fn analyze_real() {
    if let Some(why) = busy() {
        println!("busy: {why}");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let clip = tmp.path().join("clip.aiff");
    assert!(Command::new("say").arg("-o").arg(&clip).arg(TEXT).status().unwrap().success(), "say failed");
    let r = RealRenderer::new(&real::models_dir(), Some(0), Duration::from_secs(3600));
    let out = tmp.path().join("voice");
    let mut analysis = None;
    let mut steps = Vec::new();
    let never = || false;
    let t0 = Instant::now();
    r.voice_tools(&never, &mut |m| {
        let mut progress = |p: f64, msg: &str| steps.push(json!([(p * 1000.0).round() / 1000.0, msg, t0.elapsed().as_secs_f64()]));
        analysis = Some(reference::analyze(&clip, &out, Some("en"), false, 12.0, &mut progress, true, m)?);
        Ok(())
    })
    .unwrap();
    let seconds = t0.elapsed().as_secs_f64();
    let a = analysis.expect("analyze ran");
    let files: serde_json::Map<String, Value> =
        ["clean.wav", "ref_0.wav", "fingerprint.npy", "mic.json", "analysis.json"].iter().map(|f| (f.to_string(), out.join(f).exists().into())).collect();
    let report = json!({
        "note": "S17 analyze_real: reference::analyze on a macOS say clip with the real Whisper (whisper.cpp) and speaker encoder; no voice model, no take. Numbers only.",
        "seconds": seconds,
        "peak_rss_bytes": peak_rss_bytes(),
        "sample_seconds": a["duration"],
        "utterances": a["utterances"].as_array().map_or(0, Vec::len),
        "variants": a["variants"].as_array().map_or(0, Vec::len),
        "variant_durations": a["variants"].as_array().map(|v| v.iter().map(|x| x["duration"].clone()).collect::<Vec<_>>()),
        "words_heard": a["utterances"].as_array().map(|u| u.iter().map(|x| x["n_words"].as_u64().unwrap_or(0)).sum::<u64>()),
        "snr_db": a["snr_db"], "denoised": a["denoised"], "studio": a["studio"], "self_similarity": a["self_similarity"],
        "files": files,
        "progress": steps,
    });
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("analyze_real.json"), serde_json::to_string_pretty(&report).unwrap() + "\n").unwrap();
    println!("{report:#}");
}
