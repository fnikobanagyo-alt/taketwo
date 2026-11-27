//! Parity with backend/app/delivery.py `prosody` (librosa 0.11.0 `yin`), `expressiveness`, `DEFAULT_VOICE_SETTINGS`,
//! `resolve` and `pick_variant` (step S11a), from rust/golden/record_checks.py's checks.json and checks/.
//! Tolerances (lead decision 14): the voiced mask and the frames kept after the octave filter: exact. f0 on voiced
//! frames: 1e-3 Hz. pitch_st, pitch_range, dyn_db, pitch_hz: 1e-4. round(pitch_hz, 1) and round(pitch_st / r, 2)
//! for r in 0.5, 1.0, 2.0: exact. resolve, pick_variant, expressiveness: exact. Each test prints its measured maximum.

use serde_json::Value;
use std::path::PathBuf;
use taketwo_core::audio;
use taketwo_core::checks::py_round;
use taketwo_core::delivery::{self, Prosody};
use taketwo_core::library::Doc;

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

fn doc(v: &Value) -> Option<Doc> {
    v.as_object().cloned()
}

#[test]
fn delivery_default_settings_and_expressiveness() {
    let g = golden();
    let d = &g["delivery"];
    assert_eq!(serde_json::to_string(&delivery::default_voice_settings()).unwrap(), serde_json::to_string(&d["default_voice_settings"]).unwrap());
    for c in cases(&d["expressiveness"], 4, "expressiveness") {
        let i = &c["in"];
        let p = Prosody { pitch_st: i["pitch_st"].as_f64().unwrap(), pitch_range: i["pitch_range"].as_f64().unwrap(), dyn_db: i["dyn_db"].as_f64().unwrap(), pitch_hz: 0.0 };
        assert_eq!(delivery::expressiveness(&p), c["out"].as_f64().unwrap(), "{c}");
    }
}

#[test]
fn delivery_resolve_golden() {
    let g = golden();
    let d = &g["delivery"];
    let qwen: Vec<&str> = d["qwen_param_keys"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(qwen, ["temperature", "top_p", "top_k", "repetition_penalty"]);
    let c = cases(&d["resolve"], 7 * 10 * 4 * 2, "resolve");
    for case in c {
        let keys: Vec<&str> = case["keys"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let (p, q) = delivery::resolve(doc(&case["vs"]).as_ref(), &keys, doc(&case["base"]).as_ref(), &doc(&case["qc"]).unwrap()).unwrap();
        // serialized, so key order and int/float are compared too
        assert_eq!(serde_json::to_string(&p).unwrap(), serde_json::to_string(&case["params"]).unwrap(), "{case}");
        assert_eq!(serde_json::to_string(&q).unwrap(), serde_json::to_string(&case["qc_out"]).unwrap(), "{case}");
    }
}

#[test]
fn delivery_pick_variant_golden() {
    let g = golden();
    for case in cases(&g["delivery"]["pick_variant"], 64, "pick_variant") {
        let got = delivery::pick_variant(&doc(&case["profile"]).unwrap(), case["style"].as_f64().unwrap()).unwrap();
        assert_eq!(got, case["out"], "{case}");
    }
}

fn report(name: &str, max: f64, tol: f64) {
    eprintln!("delivery parity {name}: max {max:.3e} (tolerance {tol:.0e})");
    assert!(max <= tol, "{name}: {max} over {tol}");
}

#[test]
fn delivery_prosody_golden() {
    let g = golden();
    assert_eq!(g["delivery"]["librosa"].as_str().unwrap(), "0.11.0");
    let c = cases(&g["prosody"], 12, "prosody");
    let (mut x_max, mut f0_max, mut num_max) = (0.0f64, 0.0f64, 0.0f64);
    let mut frames = 0;
    for case in c {
        let name = case["name"].as_str().unwrap();
        let a = f32s(&case["signal"]);
        let sr = case["sr"].as_u64().unwrap() as u32;
        let x_py = f32s(&case["x"]);
        let x = audio::resample(&a, sr, 16000);
        assert_eq!(x.len(), x_py.len(), "{name}: resampled length");
        for (p, q) in x.iter().zip(&x_py) {
            x_max = x_max.max((p - q).abs() as f64);
        }
        if let Some(f0_py) = case.get("f0") {
            let f0_py: Vec<f64> = f0_py.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
            // yin on Python's own 16 kHz input, so this measures yin alone
            let f0 = delivery::yin(&x_py, 60.0, 420.0, 16000.0, 1024, 320);
            let db = audio::frame_db(&x_py, 16000, audio::FRAME_MS);
            let n = f0.len().min(db.len());
            assert_eq!(n as u64, case["n_frames"].as_u64().unwrap(), "{name}: frames");
            let line = audio::np::percentile_f32(&db[..n], 90.0).unwrap() - 20.0f32;
            let voiced: Vec<bool> = (0..n).map(|i| db[i] > line && f0[i] > 62.0 && f0[i] < 410.0).collect();
            let want: Vec<bool> = case["voiced"].as_array().unwrap().iter().map(|v| v.as_bool().unwrap()).collect();
            assert_eq!(voiced, want, "{name}: voiced mask");
            for i in 0..n {
                if voiced[i] {
                    f0_max = f0_max.max((f0[i] - f0_py[i]).abs());
                    frames += 1;
                }
            }
            if let Some(kept) = case.get("kept") {
                let fv: Vec<f64> = (0..n).filter(|&i| voiced[i]).map(|i| f0[i]).collect();
                let mut s = fv.clone();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let m = if s.len() % 2 == 1 { s[s.len() / 2] } else { (s[s.len() / 2 - 1] + s[s.len() / 2]) / 2.0 };
                let k = fv.iter().filter(|f| (12.0 * (*f / m).log2()).abs() < 12.0).count();
                assert_eq!(k as u64, kept.as_u64().unwrap(), "{name}: frames kept after the octave filter");
            }
        }
        // end to end, from the recorded signal
        let p = delivery::prosody(&a, sr);
        let o = &case["out"];
        for (k, v) in [("pitch_st", p.pitch_st), ("pitch_range", p.pitch_range), ("dyn_db", p.dyn_db), ("pitch_hz", p.pitch_hz)] {
            let w = o[k].as_f64().unwrap();
            num_max = num_max.max((v - w).abs());
            if w == 0.0 {
                assert_eq!(v, 0.0, "{name}: {k}");
            }
        }
        if let Some(dec) = case.get("decisions") {
            assert_eq!(py_round(p.pitch_hz, 1), dec["pitch_hz_1"].as_f64().unwrap(), "{name}: round(pitch_hz, 1)");
            for r in [0.5, 1.0, 2.0] {
                assert_eq!(py_round(p.pitch_st / r, 2), dec[format!("st_over_{r:?}")].as_f64().unwrap(), "{name}: round(pitch_st / {r}, 2)");
            }
        }
    }
    eprintln!("delivery prosody: {frames} voiced frames compared");
    eprintln!("delivery parity resample to 16 kHz (S10 tolerance 1e-4): max {x_max:.3e}");
    report("yin f0 on voiced frames (Hz)", f0_max, 1e-3);
    report("pitch_st, pitch_range, dyn_db, pitch_hz", num_max, 1e-4);
}
