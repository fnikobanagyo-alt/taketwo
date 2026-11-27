//! Sidon parity run (step S18), ignored by default (it loads the model twice, one process after the other):
//!     cd rust && source scripts/env.sh && cargo test -p taketwo-voice --test sidon_parity -- --ignored
//! Makes a `say` clip of about 6 s with added white noise under rust/target/sidon-parity, runs
//! rust/scripts/sidon_python_ref.py (the app's SeamlessM4TFeatureExtractor and studio._restore_segment), then the
//! Rust port on the same samples. Gate: features within 1e-3 (both frame-count parities) and equal output length.
//! Reported only, never judged: waveform difference, loudness, load and restore seconds, peak memory
//! (written to rust/out/sidon/parity.json).

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use taketwo_core::render::models::Restorer;
use taketwo_voice::sidon::{self, Sidon};

const TEXT: &str = "The harbour was quiet before sunrise, and a single boat moved slowly across the grey water.";

fn rust_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn read_f32(p: &Path) -> Vec<f32> {
    std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())).chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn write_f32(p: &Path, v: &[f32]) {
    std::fs::write(p, v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
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

fn run(cmd: &mut Command) {
    let out = cmd.output().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(out.status.success(), "{cmd:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs() as f64).fold(0.0, f64::max)
}

#[test]
#[ignore]
fn sidon_parity() {
    // a model run: Franz's app must be idle
    let s = Command::new("curl").args(["-s", "-m", "5", "http://127.0.0.1:8766/api/status"]).output().unwrap();
    let st: Value = serde_json::from_slice(&s.stdout).expect("8766 answers /api/status");
    assert!(st["running"].is_null() && st["queued"] == 0, "Franz's app is busy: {st}");

    let rust = rust_dir();
    let dir = rust.join("target/sidon-parity");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    run(Command::new("say").arg("-o").arg(dir.join("say.aiff")).arg(TEXT));
    run(Command::new("ffmpeg").args(["-v", "error", "-y", "-i"]).arg(dir.join("say.aiff")).args(["-ar", "16000", "-ac", "1", "-f", "f32le"]).arg(dir.join("clean.f32")));
    // white noise at about -40 dBFS RMS from a fixed LCG (the sum of 12 uniforms minus 6 is near Gaussian)
    let mut seed: u64 = 12345;
    let mut uni = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let noisy: Vec<f32> = read_f32(&dir.join("clean.f32")).iter().map(|v| v + (0.01 * ((0..12).map(|_| uni()).sum::<f64>() - 6.0)) as f32).collect();
    write_f32(&dir.join("noisy.f32"), &noisy);
    let seconds = noisy.len() as f64 / 16000.0;

    // Python first, to the end (one model process at a time)
    let wt = rust.parent().unwrap();
    run(Command::new(wt.join("backend/.venv/bin/python"))
        .arg("-B")
        .arg(rust.join("scripts/sidon_python_ref.py"))
        .arg(&dir)
        .env("TAKETWO_DATA", dir.join("data"))
        .env("VOICEFORGE_DATA", dir.join("data"))
        .env("HF_HUB_OFFLINE", "1"));
    let py: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("python.json")).unwrap()).unwrap();

    // the gate: features
    let pad = vec![0.0f32; 8000];
    let mut feat = json!({});
    for (name, clip) in [("feats", &noisy[..]), ("feats_short", &noisy[..noisy.len() - 160])] {
        let x: Vec<f32> = clip.iter().chain(&pad).copied().collect();
        let (f, rows) = sidon::features(&x);
        let want = read_f32(&dir.join(format!("{name}.f32")));
        assert_eq!(py["shapes"][name], json!([1, rows, 160]), "{name}: frame rows");
        let d = max_abs_diff(&f, &want);
        println!("{name}: rows {rows}, largest absolute difference {d:e}");
        assert!(d <= 1e-3, "{name}: largest absolute feature difference {d} is over 1e-3");
        feat[name] = json!({"rows": rows, "max_abs_diff": d});
    }

    // the Rust model run
    let models = std::env::var_os("TAKETWO_MODELS").map(PathBuf::from).unwrap_or_else(|| rust.join(".models"));
    let mut s = Sidon::new(&models.join(sidon::DIR));
    assert!(s.available(), "the Sidon files are not in {}", models.join(sidon::DIR).display());
    let t = Instant::now();
    s.load().unwrap();
    let load_s = t.elapsed().as_secs_f64();
    let tensors = s.describe().unwrap();
    println!("{tensors}");
    let t = Instant::now();
    let y = s.restore_segment(&noisy).unwrap();
    let restore_s = t.elapsed().as_secs_f64();
    let want = read_f32(&dir.join("out.f32"));
    assert_eq!(y.len(), want.len(), "output length");
    let rms = (y.iter().zip(&want).map(|(a, b)| ((a - b) as f64).powi(2)).sum::<f64>() / y.len() as f64).sqrt();
    let lufs = |v: &[f32]| taketwo_core::audio::loudness(v, 48000);
    let r = json!({
        "clip_seconds": (seconds * 100.0).round() / 100.0,
        "features": feat,
        "tensors": tensors.lines().collect::<Vec<_>>(),
        "output_samples": y.len(),
        "waveform_max_abs_diff": max_abs_diff(&y, &want),
        "waveform_rms_diff": rms,
        "lufs_rust": lufs(&y),
        "lufs_python": lufs(&want),
        "rust": {"load_s": (load_s * 100.0).round() / 100.0, "restore_s": (restore_s * 100.0).round() / 100.0, "peak_rss_bytes": peak_rss_bytes()},
        "python": {"load_s": py["load_s"], "restore_s": py["restore_s"], "peak_rss_bytes": py["peak_rss_bytes"]},
        "note": "measurements only; no quality claim",
    });
    let out = rust.join("out/sidon");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("parity.json"), serde_json::to_string_pretty(&r).unwrap() + "\n").unwrap();
    println!("{}", serde_json::to_string_pretty(&r).unwrap());
    drop(s);
    let _ = std::fs::remove_dir_all(dir.join("data"));
}
