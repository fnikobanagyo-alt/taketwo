//! Parity with pipeline.create_generation and pipeline.clean_replacement (step S07a), from the vectors
//! rust/golden/record.py writes to rust/golden/pipeline.json. Key order is compared too.

use serde_json::Value;
use std::path::PathBuf;
use taketwo_core::library::Doc;
use taketwo_core::pipeline::{self, Fail, NewNarration};

fn golden() -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../rust/golden/pipeline.json");
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

fn doc(v: &Value) -> Doc {
    v.as_object().cloned().unwrap_or_default()
}

/// The Rust result against `{"result": ...}` or `{"error": "ValueError", "message": ...}`.
fn check(bad: &mut Vec<String>, what: &str, want: &Value, got: Result<Value, Fail>) {
    let ok = match (&got, want.get("result")) {
        (Ok(g), Some(w)) => g.to_string() == w.to_string(),
        (Err(Fail::Value(m)), None) => want["error"] == "ValueError" && want["message"] == m.as_str(),
        _ => false,
    };
    if !ok {
        bad.push(format!("{what}\n  python {want}\n  rust   {got:?}"));
    }
}

#[test]
fn create_generation_matches_python() {
    let g = golden();
    let (start, step) = (g["create_clock_start"].as_f64().unwrap(), g["clock_step"].as_f64().unwrap());
    let cases = g["create"].as_array().unwrap();
    assert_eq!(cases.len(), 22, "case count");
    let mut bad = Vec::new();
    let mut ticks = 0.0;
    for (n, c) in cases.iter().enumerate() {
        let (profile, s, vs) = (doc(&c["profile"]), doc(&c["s"]), c["voice_settings"].as_object().cloned());
        let req = NewNarration {
            profile: &profile,
            text: c["text"].as_str().unwrap(),
            engine: c["engine"].as_str().unwrap(),
            language: c["language"].clone(),
            params: c["params"].clone(),
            speed: c["speed"].as_f64(),
            voice_settings: vs.as_ref(),
            title: c["title"].as_str(),
            source: c["source"].as_str().unwrap(),
        };
        let mut want = c["out"].clone();
        // db.put_generation stamps the saved document (setdefault created, then updated: two clock readings);
        // the port leaves the save, and so the stamps, to its caller.
        if let Some(w) = want.get_mut("result").and_then(Value::as_object_mut) {
            for k in ["created", "updated"] {
                ticks += 1.0;
                assert_eq!(w.shift_remove(k).and_then(|v| v.as_f64()), Some(start + ticks * step), "case {n}: {k} stamp");
            }
        }
        let got = pipeline::create_generation("GID", &req, &s).map(Value::Object);
        check(&mut bad, &format!("create case {n}"), &want, got);
    }
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn clean_replacement_matches_python() {
    let g = golden();
    let cases = g["replacement"].as_array().unwrap();
    assert_eq!(cases.len(), 11, "case count");
    let mut bad = Vec::new();
    for (n, c) in cases.iter().enumerate() {
        let got = pipeline::clean_replacement(c["text"].as_str().unwrap(), &doc(&c["gen"]), &doc(&c["s"])).map(Value::from);
        check(&mut bad, &format!("replacement case {n}"), &c["out"], got);
    }
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}
