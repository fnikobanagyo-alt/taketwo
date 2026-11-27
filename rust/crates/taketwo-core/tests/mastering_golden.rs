//! Parity with delivery.ltas / mic_match (with scipy firwin2), studio.expander / de_ess / master_chain and
//! pipeline._processed_edge_issues (step S11b), from the vectors rust/golden/record_mastering.py writes to
//! rust/golden/mastering.json and rust/golden/mastering/. Inputs are read from the recorded float32 files.
//! Tolerances are the lead's (decision 18); each test prints its measured maximum. Decisions are exact: early
//! exits, errors, curves, protected masks, issue sets, lengths. Plus the three backend tests on these stages.

use serde_json::Value;
use std::path::PathBuf;
use taketwo_core::audio::{self, PyError};
use taketwo_core::{delivery, mastering, pipeline};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden")
}

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join("mastering.json")).unwrap()).unwrap()
}

fn read(file: &Value) -> Vec<u8> {
    std::fs::read(root().join("mastering").join(file["file"].as_str().unwrap())).unwrap()
}

fn f32s(file: &Value) -> Vec<f32> {
    let v: Vec<f32> = read(file).chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    assert_eq!(v.len() as u64, file["len"].as_u64().unwrap(), "{file}");
    v
}

fn f64s(file: &Value) -> Vec<f64> {
    let v: Vec<f64> = read(file).chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(v.len() as u64, file["len"].as_u64().unwrap(), "{file}");
    v
}

/// A case's input: a named signal or its own array, with the rate.
fn input(g: &Value, c: &Value) -> (Vec<f32>, u32) {
    match c.get("signal") {
        Some(name) => {
            let s = &g["signals"][name.as_str().unwrap()];
            (f32s(s), s["sr"].as_u64().unwrap() as u32)
        }
        None => (f32s(&c["input"]), c["sr"].as_u64().unwrap() as u32),
    }
}

fn signal(g: &Value, name: &str) -> (Vec<f32>, u32) {
    let s = &g["signals"][name];
    (f32s(s), s["sr"].as_u64().unwrap() as u32)
}

fn cases<'a>(g: &'a Value, key: &str, n: usize) -> &'a Vec<Value> {
    let c = g[key].as_array().unwrap();
    assert_eq!(c.len(), n, "number of golden cases for {key}");
    c
}

/// A band list: null is NaN.
fn bands(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()
}

/// |a - b|: 0 when equal or both NaN, infinity when only one is NaN.
fn diff(a: f64, b: f64) -> f64 {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => 0.0,
        (true, false) | (false, true) => f64::INFINITY,
        _ if a == b => 0.0,
        _ => (a - b).abs(),
    }
}

fn max_diff<T: Copy + Into<f64>>(what: &str, got: &[T], want: &[T]) -> f64 {
    assert_eq!(got.len(), want.len(), "{what}: length");
    got.iter().zip(want).map(|(a, b)| diff((*a).into(), (*b).into())).fold(0.0, f64::max)
}

fn report(name: &str, max: f64, tol: f64) {
    eprintln!("mastering parity {name}: max {max:.3e} (tolerance {tol:.0e})");
    assert!(max <= tol, "{name}: {max} over {tol}");
}

/// A recorded outcome: the output array, or the Python exception's type name.
fn check_result(what: &str, got: Result<Vec<f32>, PyError>, c: &Value) -> Option<(Vec<f32>, Vec<f32>)> {
    match (got, c.get("result")) {
        (Err(e), Some(r)) => {
            assert_eq!(e.kind, r["error"].as_str().unwrap(), "{what} {}: {} / {}", c["case"], e.message, r["message"]);
            None
        }
        (Ok(y), None) => Some((y, f32s(&c["out"]))),
        (got, want) => panic!("{what} {}: got {got:?}, Python {want:?}", c["case"]),
    }
}

#[test]
fn mastering_bands_constant() {
    let g = golden();
    let want = bands(&g["bands"]);
    assert_eq!(want.len(), 24);
    report("BANDS Hz", max_diff("bands", &delivery::bands(), &want), 0.0);
}

#[test]
fn mastering_ltas() {
    let g = golden();
    let mut worst = 0.0f64;
    let mut nan_bands = 0;
    for c in cases(&g, "ltas", 11) {
        let (a, sr) = input(&g, c);
        let want = bands(&c["bands"]);
        let got = delivery::ltas(&a, sr);
        // NaN bands (no bin, or at or above 0.92 x Nyquist) are a decision: same places exactly
        assert_eq!(got.iter().map(|v| v.is_nan()).collect::<Vec<_>>(), want.iter().map(|v| v.is_nan()).collect::<Vec<_>>(), "{c}");
        nan_bands += want.iter().filter(|v| v.is_nan()).count();
        worst = worst.max(max_diff(&c.to_string(), &got, &want));
    }
    assert!(nan_bands >= 10, "the 24 kHz cases have NaN top bands");
    report("ltas dB", worst, 1e-3);
}

#[test]
fn mastering_firwin2_taps() {
    let g = golden();
    let mut worst = 0.0f64;
    let mut rates = std::collections::BTreeSet::new();
    for c in cases(&g, "firwin2", 7) {
        let f = |k: &str| -> Vec<f64> { c[k].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect() };
        let got = audio::signal::firwin2(1025, &f("fpts"), &f("gains"));
        worst = worst.max(max_diff(c["case"].as_str().unwrap(), &got, &f64s(&c["taps"])));
        rates.insert(c["sr"].as_u64().unwrap());
    }
    assert!(rates.len() >= 2, "{rates:?}");
    report("firwin2 taps", worst, 1e-9);
}

#[test]
fn mastering_mic_match() {
    let g = golden();
    let mut worst = 0.0f64;
    let (mut exits, mut capped) = (0, 0);
    for c in cases(&g, "mic_match", 10) {
        let (a, sr) = input(&g, c);
        let (out, curve) = delivery::mic_match(&a, sr, &bands(&c["ref_bands"]), 6.0);
        let want: Vec<f64> = c["curve"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        assert_eq!(curve, want, "{}", c["case"]); // the rounded curve: exact
        if want.is_empty() {
            assert_eq!(out, a, "{}: an early exit returns the input", c["case"]);
            exits += 1;
        } else {
            capped += curve.iter().filter(|v| v.abs() == 6.0).count();
            worst = worst.max(max_diff(c["case"].as_str().unwrap(), &out, &f32s(&c["out"])));
        }
    }
    assert_eq!(exits, 3);
    assert!(capped > 0, "some curve reaches the +/-6 dB cap");
    report("mic_match samples", worst, 1e-4);
}

#[test]
fn mastering_mic_json_reader_reads_bare_nan() {
    // Write each recorded band list as Python's json.dumps does (bare NaN), then read it back.
    let g = golden();
    let dir = tempfile::tempdir().unwrap();
    let mut with_nan = 0;
    for c in cases(&g, "ltas", 11) {
        let want = bands(&c["bands"]);
        let items: Vec<String> = want.iter().map(|v| if v.is_nan() { "NaN".into() } else { format!("{v:?}") }).collect();
        let path = dir.path().join("mic.json");
        std::fs::write(&path, format!("{{\"sr\": 24000, \"bands\": [{}]}}", items.join(", "))).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        if text.contains("NaN") {
            assert!(serde_json::from_str::<Value>(&text).is_err(), "plain serde_json must not be used on mic.json");
            with_nan += 1;
        }
        let got = delivery::read_mic_bands(&path).unwrap();
        assert_eq!(max_diff("mic.json", &got, &want), 0.0, "{text}");
    }
    assert_eq!(with_nan, 8);
    let path = dir.path().join("inf.json");
    std::fs::write(&path, "{\"bands\": [1.5, Infinity, -Infinity, NaN], \"sr\": 48000}").unwrap();
    let got = delivery::read_mic_bands(&path).unwrap();
    assert_eq!((got[0], got[1], got[2], got[3].is_nan()), (1.5, f64::INFINITY, f64::NEG_INFINITY, true));
}

#[test]
fn mastering_expander() {
    let g = golden();
    let mut worst = 0.0f64;
    let mut opened = 0;
    for c in cases(&g, "expander", 14) {
        let (a, sr) = input(&g, c);
        let mask = mastering::expander_protected(&a, sr, 300);
        let y = mastering::expander(&a, sr, 45.0, 8.0, 300);
        match c["protected"].as_str() {
            None => {
                assert!(c["unchanged"].as_bool().unwrap() && mask.is_none(), "{}", c["case"]);
                assert_eq!(y, a, "{}: under 20 frames the input comes back", c["case"]);
            }
            Some(want) => {
                let got: String = mask.unwrap().iter().map(|&p| if p { '1' } else { '0' }).collect();
                assert_eq!(got, want, "{}: protected mask", c["case"]);
                opened += want.matches('0').count();
                worst = worst.max(max_diff(c["case"].as_str().unwrap(), &y, &f32s(&c["out"])));
            }
        }
    }
    assert!(opened > 100, "some frames are expanded");
    report("expander samples", worst, 1e-6);
}

#[test]
fn mastering_de_ess() {
    let g = golden();
    let mut worst = 0.0f64;
    let mut errors = 0;
    for c in cases(&g, "de_ess", 12) {
        let (a, sr) = input(&g, c);
        match check_result("de_ess", mastering::de_ess(&a, sr, 1.0), c) {
            None => errors += 1,
            Some((y, want)) => {
                if c["unchanged"].as_bool().unwrap() {
                    assert_eq!(y, a, "{}: bypass under 20 kHz", c["case"]);
                }
                worst = worst.max(max_diff(c["case"].as_str().unwrap(), &y, &want));
            }
        }
    }
    assert_eq!(errors, 3);
    report("de_ess samples", worst, 1e-4);
}

#[test]
fn mastering_master_chain() {
    let g = golden();
    let mut worst = 0.0f64;
    let mut errors = 0;
    for c in cases(&g, "master_chain", 10) {
        let (a, sr) = input(&g, c);
        if let Some(st) = c.get("stages") {
            // the stages, so a miss is traced to one of them
            let hp = audio::highpass(&a, sr, 75.0).unwrap();
            report("master_chain stage highpass", max_diff("hp", &hp, &f32s(&st["highpass"])), 1e-4);
            let de = mastering::de_ess(&f32s(&st["highpass"]), sr, 1.0).unwrap();
            report("master_chain stage de_ess (on Python's high-pass)", max_diff("de", &de, &f32s(&st["de_ess"])), 1e-4);
            let y = mastering::expander(&f32s(&st["compressed"]), sr, 45.0, 8.0, 300);
            report("master_chain stage expander (on Python's compressed)", max_diff("exp", &y, &f32s(&c["out"])), 1e-6);
        }
        match check_result("master_chain", mastering::master_chain(&a, sr), c) {
            None => errors += 1,
            Some((y, want)) => worst = worst.max(max_diff(c["case"].as_str().unwrap(), &y, &want)),
        }
    }
    assert_eq!(errors, 3);
    report("master_chain samples", worst, 1e-4);
}

#[test]
fn mastering_processed_edge_issues() {
    let g = golden();
    let (r0, sr) = signal(&g, "edge24");
    let mut flagged = 0;
    for c in cases(&g, "edge", 20) {
        let op = &c["processed"];
        let n = |k: &str| op[k].as_u64().unwrap() as usize;
        let (r, p): (Vec<f32>, Vec<f32>) = match op["op"].as_str().unwrap() {
            "empty_reference" => (Vec::new(), r0.clone()),
            "silent_reference" => (vec![0.0; r0.len()], r0.clone()),
            "slice" => (r0.clone(), r0[n("lo")..n("hi")].to_vec()),
            "zero" => {
                let mut p = r0.clone();
                p[n("lo")..n("hi")].iter_mut().for_each(|v| *v = 0.0);
                (r0.clone(), p)
            }
            "shift" => {
                let mut p = vec![0.0; n("n")];
                p.extend_from_slice(&r0);
                p.truncate(r0.len());
                (r0.clone(), p)
            }
            "array" => (r0.clone(), f32s(&op["a"])),
            other => panic!("{other}"),
        };
        let got: Vec<&str> = pipeline::processed_edge_issues(&r, &p, sr).into_iter().collect();
        let want: Vec<&str> = c["issues"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(got, want, "{}", c["case"]);
        flagged += usize::from(!want.is_empty());
    }
    assert_eq!(flagged, 7);
}

/// backend/tests/test_audit_fixes.py `test_expander_keeps_quiet_word_tails` (audit R1).
#[test]
fn test_expander_keeps_quiet_word_tails() {
    let g = golden();
    let c = cases(&g, "expander", 14).iter().filter(|c| c["case"] == "audit_quiet_tail").collect::<Vec<_>>();
    assert_eq!(c.len(), 1);
    let (x, sr) = input(&g, c[0]);
    let y = mastering::expander(&x, sr, 45.0, 8.0, 300);
    let (lo, hi) = ((2.25 * sr as f64) as usize, (2.4 * sr as f64) as usize);
    let norm = |a: &[f32]| a.iter().map(|v| (*v as f64).powi(2)).sum::<f64>().sqrt();
    let gain = 20.0 * (norm(&y[lo..hi]) / norm(&x[lo..hi])).log10();
    assert!(gain > -1.0, "quiet tail lost {gain} dB");
}

/// backend/tests/test_audit_fixes.py `test_de_esser_keeps_soft_fricative_endings`.
#[test]
fn test_de_esser_keeps_soft_fricative_endings() {
    let g = golden();
    let c = cases(&g, "de_ess", 12).iter().filter(|c| c["case"] == "audit_fricative").collect::<Vec<_>>();
    assert_eq!(c.len(), 1);
    let (x, sr) = input(&g, c[0]);
    let y = mastering::de_ess(&x, sr, 1.0).unwrap();
    let lo = (0.8 * sr as f64) as usize;
    let norm = |a: &[f32]| a.iter().map(|v| (*v as f64).powi(2)).sum::<f64>().sqrt();
    let change = 20.0 * (norm(&y[lo..]) / norm(&x[lo..])).log10();
    assert!(change > -1.5, "fricative ending lost {change} dB");
}

/// backend/tests/test_repeat_cutoff.py `test_short_expander_keeps_shape_without_crashing`.
#[test]
fn test_short_expander_keeps_shape_without_crashing() {
    let g = golden();
    let short: Vec<&Value> =
        cases(&g, "expander", 14).iter().filter(|c| c["case"].as_str().unwrap().starts_with("audit_short_")).collect();
    assert_eq!(short.len(), 5); // 0.1, 0.3, 0.55, 0.6, 0.61 s
    for c in short {
        let (a, sr) = input(&g, c);
        assert_eq!(mastering::expander(&a, sr, 45.0, 8.0, 300).len(), a.len(), "{}", c["case"]);
    }
}
