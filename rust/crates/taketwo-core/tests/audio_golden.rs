//! Parity with backend/app/audio.py (step S10), from the vectors rust/golden/record_audio.py writes to
//! rust/golden/audio.json and rust/golden/audio/. Inputs are read from the recorded float32 files, never
//! regenerated. Tolerances are the lead's (decision 13); each test prints its measured maximum.
//! Decisions (frame indexes, cut positions, lengths, bools, counts, gap lists, None or not) are exact;
//! every decision case was recorded with a margin of at least 0.01 dB from its threshold.

use serde_json::Value;
use std::path::{Path, PathBuf};
use taketwo_core::audio::{self, signal};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden")
}

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join("audio.json")).unwrap()).unwrap()
}

fn f32s(file: &Value) -> Vec<f32> {
    let b = std::fs::read(root().join("audio").join(file["file"].as_str().unwrap())).unwrap();
    let v: Vec<f32> = b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    assert_eq!(v.len() as u64, file["len"].as_u64().unwrap(), "{file}");
    v
}

struct Sig {
    g: Value,
}

impl Sig {
    fn get(&self, name: &str) -> (Vec<f32>, u32) {
        let s = &self.g["signals"][name];
        (f32s(s), s["sr"].as_u64().unwrap() as u32)
    }
}

fn cases<'a>(g: &'a Value, key: &str, n: usize) -> &'a Vec<Value> {
    let c = g[key].as_array().unwrap();
    assert_eq!(c.len(), n, "number of golden cases for {key}");
    c
}

/// A recorded number: JSON number or "-inf" / "inf" / "nan".
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

/// |a - b|: 0 when equal (same infinity included) or both NaN, infinity when only one is NaN, so a
/// NaN can never hide inside a `max`.
fn diff(a: f64, b: f64) -> f64 {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => 0.0,
        (true, false) | (false, true) => f64::INFINITY,
        _ if a == b => 0.0,
        _ => (a - b).abs(),
    }
}

/// Same length exactly, then the largest sample difference.
fn max_diff(what: &str, got: &[f32], want: &[f32]) -> f64 {
    assert_eq!(got.len(), want.len(), "{what}: length");
    got.iter().zip(want).map(|(a, b)| diff(*a as f64, *b as f64)).fold(0.0, f64::max)
}

fn report(name: &str, max: f64, tol: f64) {
    eprintln!("audio parity {name}: max {max:.3e} (tolerance {tol:.0e})");
    assert!(max <= tol, "{name}: {max} over {tol}");
}

#[test]
fn audio_frame_db_and_thresholds() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst = 0.0f64;
    for c in cases(&g, "frame_db", 30) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let got = audio::frame_db(&a, sr, c["frame_ms"].as_u64().unwrap() as u32);
        worst = worst.max(max_diff(&c.to_string(), &got, &f32s(&c["out"])));
    }
    report("frame_db dB", worst, 1e-3); // returned dB values: 1e-3 dB
    let mut worst = 0.0f64;
    for c in cases(&g, "thresholds", 31) {
        let db = f32s(&c["db"]); // Python's own levels, so only the threshold maths is compared
        for (key, got) in [("speech_threshold_db", audio::speech_threshold_db(&db)), ("floor_threshold", audio::floor_threshold(&db))] {
            match got {
                Ok(v) => worst = worst.max(diff(v, num(&c[key]))),
                Err(e) => assert_eq!(c[key]["error"], e.kind, "{c}"),
            }
        }
    }
    report("speech_threshold_db/_floor_threshold dB", worst, 1e-3); // thresholds: 1e-3 dB
}

#[test]
fn audio_silence_report_and_ending_level() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst = 0.0f64;
    for c in cases(&g, "silence_report", 38) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let r = audio::silence_report(&a, sr, c["min_gap"].as_f64().unwrap());
        let gaps: Vec<(f64, f64)> =
            c["internal_gaps"].as_array().unwrap().iter().map(|g| (g[0].as_f64().unwrap(), g[1].as_f64().unwrap())).collect();
        assert_eq!(r.internal_gaps, gaps, "{c}"); // gap list: exact
        for (got, key) in [(r.lead, "lead"), (r.tail, "tail"), (r.max_internal, "max_internal"), (r.speech_ratio, "speech_ratio")] {
            worst = worst.max(diff(got, num(&c[key])));
        }
    }
    report("silence_report seconds", worst, 1e-9); // returned seconds: 1e-9
    let mut worst = 0.0f64;
    for c in cases(&g, "ending_level", 14) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        worst = worst.max(diff(audio::ending_level(&a, sr), num(&c["ending_level"])));
    }
    report("ending_level dB", worst, 1e-3);
}

#[test]
fn audio_trim_edges_and_speech_start() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    for c in cases(&g, "trim_edges", 42) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let out = audio::trim_edges(&a, sr, c["keep_ms"].as_u64().unwrap() as u32, c["tail_ms"].as_u64().unwrap() as u32);
        // Python returns a slice of its input; the slice bounds are recorded and compared exactly,
        // which is stricter than the lead's "same length, samples within 1e-6".
        let start = if out.is_empty() { 0 } else { (out.as_ptr() as usize - a.as_ptr() as usize) / 4 };
        assert_eq!((start as u64, out.len() as u64), (c["start"].as_u64().unwrap(), c["len"].as_u64().unwrap()), "{c}");
    }
    let mut worst = 0.0f64;
    for c in cases(&g, "speech_start", 36) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        worst = worst.max(diff(audio::speech_start(&a, sr, c["keep_ms"].as_u64().unwrap() as u32), num(&c["speech_start"])));
    }
    report("speech_start seconds", worst, 1e-9);
}

#[test]
fn audio_fade_limit_gaps_concat() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst = 0.0f64;
    let u = |v: &Value| v.as_u64().unwrap() as u32;
    for c in cases(&g, "fade", 15) {
        let (a, _) = sig.get(c["signal"].as_str().unwrap());
        let got = audio::fade(&a, u(&c["sr"]), u(&c["fade_in_ms"]), u(&c["fade_out_ms"]));
        worst = worst.max(max_diff(&c.to_string(), &got, &f32s(&c["out"])));
    }
    report("fade samples", worst, 1e-6); // same length exactly, samples within 1e-6
    let mut worst = 0.0f64;
    for c in cases(&g, "soft_limit", 6) {
        let (a, _) = sig.get(c["signal"].as_str().unwrap());
        worst = worst.max(max_diff(&c.to_string(), &audio::soft_limit(&a, c["ceiling"].as_f64().unwrap()), &f32s(&c["out"])));
    }
    report("soft_limit samples", worst, 1e-6);
    for c in cases(&g, "clipping_ratio", 6) {
        let (a, _) = sig.get(c["signal"].as_str().unwrap());
        assert_eq!(audio::clipping_ratio(&a), c["clipping_ratio"].as_f64().unwrap(), "{c}"); // a count over a length: exact
    }
    let mut worst = 0.0f64;
    for c in cases(&g, "shorten_gaps", 8) {
        let (a, _) = sig.get(c["signal"].as_str().unwrap());
        let gaps: Vec<(f64, f64)> = c["gaps"].as_array().unwrap().iter().map(|g| (g[0].as_f64().unwrap(), g[1].as_f64().unwrap())).collect();
        let got = audio::shorten_gaps(&a, u(&c["sr"]), &gaps, c["max_gap"].as_f64().unwrap());
        worst = worst.max(max_diff(&c.to_string(), &got, &f32s(&c["out"])));
    }
    report("shorten_gaps samples", worst, 1e-6);
    let mut worst = 0.0f64;
    for c in cases(&g, "concat_with_pauses", 7) {
        let chunks: Vec<Vec<f32>> = c["chunks"].as_array().unwrap().iter().map(f32s).collect();
        let refs: Vec<&[f32]> = chunks.iter().map(|v| v.as_slice()).collect();
        let pauses: Vec<i64> = c["pauses_ms"].as_array().unwrap().iter().map(|p| p.as_i64().unwrap()).collect();
        let got = audio::concat_with_pauses(&refs, &pauses, u(&c["sr"]));
        worst = worst.max(max_diff(&c.to_string(), &got, &f32s(&c["out"])));
    }
    report("concat_with_pauses samples", worst, 1e-6);
}

#[test]
fn audio_resample_matches_resample_poly() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst = 0.0f64;
    for c in cases(&g, "resample", 12) {
        let (a, _) = sig.get(c["signal"].as_str().unwrap());
        let got = audio::resample(&a, c["sr_from"].as_u64().unwrap() as u32, c["sr_to"].as_u64().unwrap() as u32);
        let m = max_diff(&c.to_string(), &got, &f32s(&c["out"])); // same length exactly
        eprintln!("audio parity resample {} {}->{}: max {m:.3e}", c["signal"], c["sr_from"], c["sr_to"]);
        worst = worst.max(m);
    }
    report("resample samples (edges included)", worst, 1e-4);
}

#[test]
fn audio_butter_and_sosfiltfilt() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst = 0.0f64;
    for c in cases(&g, "butter", 15) {
        let hz: Vec<f64> = c["hz"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let bt = if c["kind"] == "band" { signal::BType::Bandpass } else { signal::BType::Highpass };
        let sos = signal::butter(4, &hz, bt, c["sr"].as_f64().unwrap()).unwrap();
        let want = c["sos"].as_array().unwrap();
        assert_eq!(sos.len(), want.len(), "{c}");
        for (s, w) in sos.iter().zip(want) {
            for (x, y) in s.iter().zip(w.as_array().unwrap()) {
                worst = worst.max(diff(*x, y.as_f64().unwrap()));
            }
        }
    }
    report("butter SOS coefficients", worst, 1e-9);
    let mut worst = 0.0f64;
    for c in cases(&g, "highpass", 9) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        match audio::highpass(&a, sr, c["hz"].as_f64().unwrap()) {
            Ok(got) => worst = worst.max(max_diff(&c.to_string(), &got, &f32s(&c["out"]))),
            Err(e) => assert_eq!(c["out"]["error"], e.kind, "{c}"),
        }
    }
    for c in cases(&g, "band_sosfiltfilt", 4) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let sos = signal::butter(4, &[4000.0, (sr as f64 / 2.0 * 0.95).min(10000.0)], signal::BType::Bandpass, sr as f64).unwrap();
        match signal::sosfiltfilt(&sos, &a) {
            Ok(got) => {
                let got: Vec<f32> = got.into_iter().map(|v| v as f32).collect(); // recorded as float32
                worst = worst.max(max_diff(&c.to_string(), &got, &f32s(&c["out"])))
            }
            Err(e) => assert_eq!(c["out"]["error"], e.kind, "{c}"),
        }
    }
    report("highpass/band-pass sosfiltfilt samples", worst, 1e-4);
}

#[test]
fn audio_loudness_matches_pyloudnorm() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst = 0.0f64;
    for c in cases(&g, "loudness", 16) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let (got, want) = (audio::loudness(&a, sr), num(&c["loudness"]));
        assert_eq!(got.is_finite(), want.is_finite(), "{c}: {got}");
        worst = worst.max(diff(got, want));
    }
    report("loudness LU", worst, 0.05); // within 0.05 LU of pyloudnorm
    let (mut fed, mut e2e) = (0.0f64, 0.0f64);
    for c in cases(&g, "normalize_lufs", 8) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let want = f32s(&c["out"]);
        let got = audio::normalize_lufs_with(&a, num(&c["python_loudness"]), c["target"].as_f64().unwrap(), c["peak_db"].as_f64().unwrap());
        fed = fed.max(max_diff(&c.to_string(), &got, &want));
        let got = audio::normalize_lufs(&a, sr, c["target"].as_f64().unwrap(), c["peak_db"].as_f64().unwrap());
        e2e = e2e.max(max_diff(&c.to_string(), &got, &want));
    }
    report("normalize_lufs samples, fed Python's loudness", fed, 1e-3);
    eprintln!("audio parity normalize_lufs end to end (Rust loudness): max {e2e:.3e} (reported, no tolerance set)");
}

#[test]
fn audio_noise_bursts_and_snr() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    for c in cases(&g, "noise_bursts", 18) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        assert_eq!(audio::noise_bursts(&a, sr), c["noise_bursts"].as_f64().unwrap(), "{c}"); // run length: exact
    }
    let mut worst = 0.0f64;
    for c in cases(&g, "estimate_snr", 11) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        worst = worst.max(diff(audio::estimate_snr(&a, sr), c["estimate_snr"].as_f64().unwrap()));
    }
    report("estimate_snr dB", worst, 1e-3);
}

#[test]
fn audio_sound_regions_end_analysis_edges() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst_db = 0.0f64;
    for c in cases(&g, "sound_regions", 63) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let (regions, db, sp) = audio::sound_regions(&a, sr, c["bridge_ms"].as_u64().unwrap() as u32);
        let want: Vec<(usize, usize)> =
            c["regions"].as_array().unwrap().iter().map(|r| (r[0].as_u64().unwrap() as usize, r[1].as_u64().unwrap() as usize)).collect();
        assert_eq!(regions, want, "{c}");
        assert_eq!(db.len() as u64, c["n_db"].as_u64().unwrap());
        worst_db = worst_db.max(diff(sp, c["speech_level"].as_f64().unwrap()));
    }
    let mut worst_s = 0.0f64;
    for c in cases(&g, "edge_silence", 21) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let (lead, tail) = audio::edge_silence(&a, sr);
        worst_s = worst_s.max(diff(lead, num(&c["lead"]))).max(diff(tail, num(&c["tail"])));
    }
    for c in cases(&g, "end_analysis", 417) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let r = audio::end_analysis(&a, sr, c["last_word_end"].as_f64().unwrap(), c["tail_s"].as_f64().unwrap());
        assert_eq!((r.natural, r.junk), (c["natural"].as_bool().unwrap(), c["junk"].as_bool().unwrap()), "{c}");
        // start and cut are frame counts times 0.01 (or the file length): compare as samples, exactly, and as seconds
        worst_s = worst_s.max(diff(r.start, num(&c["start"]))).max(diff(r.cut, num(&c["cut"])));
        assert_eq!((r.cut * sr as f64).round(), (num(&c["cut"]) * sr as f64).round(), "{c}");
        worst_db = worst_db.max(diff(r.last_level, num(&c["last_level"])));
    }
    for c in cases(&g, "terminal_end", 25) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        let db = audio::frame_db(&a, sr, 10);
        let got = audio::terminal_end(&db, c["scan0"].as_i64().unwrap(), c["speech_level"].as_f64().unwrap());
        assert_eq!(got.map(|v| v as u64), c["end"].as_u64(), "{c}");
    }
    report("sound_regions speech level / end_analysis last_level dB", worst_db, 1e-3);
    report("edge_silence / end_analysis seconds", worst_s, 1e-9);
}

#[test]
fn audio_lead_burst() {
    let g = golden();
    let sig = Sig { g: g.clone() };
    let mut worst = 0.0f64;
    let mut found = 0;
    for c in cases(&g, "lead_burst", 14) {
        let (a, sr) = sig.get(c["signal"].as_str().unwrap());
        match (audio::lead_burst(&a, sr), c["result"].as_array()) {
            (None, None) => {}
            (Some((s, l)), Some(w)) => {
                found += 1;
                worst = worst.max(diff(s, w[0].as_f64().unwrap())).max(diff(l, w[1].as_f64().unwrap()));
            }
            (got, _) => panic!("{c}: rust {got:?}"),
        }
    }
    assert!(found >= 1);
    report("lead_burst seconds", worst, 1e-9);
}

#[test]
fn audio_wav_save_and_load() {
    let g = golden();
    for c in cases(&g, "save", 3) {
        let a = f32s(&c["input"]);
        let want = std::fs::read(root().join("audio").join(c["expected"].as_str().unwrap())).unwrap();
        assert!(audio::wav::pcm16_wav_bytes(&a, c["sr"].as_u64().unwrap() as u32) == want, "{c}: PCM16 bytes differ"); // bytes equal
    }
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("x.wav");
    audio::save(&p, &[0.5, -0.25], 24000).unwrap();
    assert_eq!(audio::load(&p, None).unwrap(), (vec![0.5, -0.25], 24000));
    let mut worst = 0.0f64;
    let mut worst_rs = 0.0f64;
    for c in cases(&g, "load", 15) {
        let path = root().join("audio").join(c["file"].as_str().unwrap());
        let (a, sr) = audio::load(Path::new(&path), c["sr"].as_u64().map(|v| v as u32)).unwrap();
        assert_eq!(sr as u64, c["out_sr"].as_u64().unwrap(), "{c}");
        let m = max_diff(&c.to_string(), &a, &f32s(&c["out"]));
        let header_sr = hound::WavReader::open(&path).unwrap().spec().sample_rate;
        if header_sr == sr { worst = worst.max(m) } else { worst_rs = worst_rs.max(m) }
    }
    report("load samples before any rate change", worst, 1e-7);
    report("load samples with a rate change (resample tolerance)", worst_rs, 1e-4);
}

#[test]
fn audio_ffmpeg_arguments_and_round_trip() {
    let (s, d) = (Path::new("in.m4a"), Path::new("out.wav"));
    let strs = |v: Vec<std::ffi::OsString>| v.into_iter().map(|x| x.into_string().unwrap()).collect::<Vec<_>>();
    assert_eq!(
        strs(audio::ffmpeg::decode_args(s, d, 24000, Some(300.0))),
        ["-y", "-hide_banner", "-loglevel", "error", "-i", "in.m4a", "-vn", "-ac", "1", "-ar", "24000", "-t", "300.0", "-c:a", "pcm_f32le", "out.wav"]
    );
    assert_eq!(
        strs(audio::ffmpeg::decode_args(s, d, 16000, Some(0.0))),
        ["-y", "-hide_banner", "-loglevel", "error", "-i", "in.m4a", "-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_f32le", "out.wav"]
    );
    assert_eq!(strs(audio::ffmpeg::decode_args(s, d, 16000, Some(2.5)))[11..13], ["-t", "2.5"]);
    assert_eq!(
        strs(audio::ffmpeg::export_args(d, Path::new("o.mp3"))),
        ["-y", "-hide_banner", "-loglevel", "error", "-i", "out.wav", "-b:a", "192k", "o.mp3"]
    );

    // Round trip through the real program: WAV -> MP3 -> WAV at another rate, cut by max_seconds.
    let tmp = tempfile::tempdir().unwrap();
    let wav = tmp.path().join("tone.wav");
    let tone: Vec<f32> = (0..48000).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect(); // 2 s at 24 kHz
    audio::save(&wav, &tone, 24000).unwrap();
    let mp3 = tmp.path().join("tone.mp3");
    if let Err(e) = audio::ffmpeg::export_mp3(&wav, &mp3) {
        panic!("ffmpeg is needed for this test (TAKETWO_FFMPEG or ffmpeg on PATH): {e}");
    }
    let back = tmp.path().join("back.wav");
    audio::ffmpeg::decode_to_wav(&mp3, &back, 16000, Some(1.5)).unwrap();
    let spec = hound::WavReader::open(&back).unwrap().spec();
    assert_eq!((spec.sample_rate, spec.channels, spec.sample_format), (16000, 1, hound::SampleFormat::Float));
    let (a, sr) = audio::load(&back, None).unwrap();
    assert_eq!((sr, a.len()), (16000, 24000), "1.5 s at 16 kHz");
    // a failing run (here: no such input) is an error value, not a panic; a missing program is the unit test in ffmpeg.rs
    let missing = audio::ffmpeg::export_mp3(Path::new("/nonexistent/x.wav"), &tmp.path().join("y.mp3"));
    assert!(missing.is_err());
}
