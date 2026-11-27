//! Parity with the Python app: contract goldens and vectors recorded by rust/golden/record.py.
//! Every case must equal Python exactly (floats within 1e-12).

use serde_json::Value;
use std::path::PathBuf;
use taketwo_core::text::{self, Pronunciation};

fn load(rel: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join(rel);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn section(rel: &str, key: Option<&str>, min: usize) -> Vec<Value> {
    let v = load(rel);
    let cases = match key {
        Some(k) => v[k].as_array().cloned().unwrap_or_else(|| panic!("{rel}: no section {k}")),
        None => v.as_array().cloned().unwrap(),
    };
    assert!(cases.len() >= min, "{rel} {key:?}: {} cases, want at least {min}", cases.len());
    cases
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn pronunciations(v: &Value) -> Vec<Pronunciation> {
    let field = |p: &Value, k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    v.as_array()
        .map(|a| a.iter().map(|p| Pronunciation { from: field(p, "from"), to: field(p, "to") }).collect())
        .unwrap_or_default()
}

fn check_clean(cases: &[Value]) {
    let mut bad = Vec::new();
    for c in cases {
        let strip = c.get("strip_citations").and_then(Value::as_bool).unwrap_or(true);
        let got = text::clean_text(s(&c["text"]), &pronunciations(&c["pronunciations"]), strip);
        if got != s(&c["output"]) {
            bad.push(format!("{:?}\n  python {:?}\n  rust   {:?}", s(&c["text"]), s(&c["output"]), got));
        }
    }
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}

fn check_chunks(cases: &[Value]) {
    let mut bad = Vec::new();
    for c in cases {
        let max = c["max_chars"].as_u64().unwrap() as usize;
        let got: Vec<(String, &str)> =
            text::chunk_text(s(&c["text"]), max).into_iter().map(|k| (k.text, k.pause_after)).collect();
        let want: Vec<(String, &str)> = c["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| (s(&k["text"]).to_string(), s(&k["pause_after"])))
            .collect();
        if got != want {
            bad.push(format!("{:?} max {max}\n  python {want:?}\n  rust   {got:?}", s(&c["text"])));
        }
    }
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn text_contract_clean_text() {
    let cases = section("contract/golden/clean_text.json", None, 0);
    assert_eq!(cases.len(), 18);
    check_clean(&cases);
}

#[test]
fn text_contract_chunk_text() {
    let cases = section("contract/golden/chunk_text.json", None, 0);
    assert_eq!(cases.len(), 8);
    check_chunks(&cases);
}

#[test]
fn text_recorded_clean_text() {
    check_clean(&section("rust/golden/text.json", Some("clean_text"), 20));
}

#[test]
fn text_recorded_chunk_text() {
    check_chunks(&section("rust/golden/text.json", Some("chunk_text"), 10));
}

#[test]
fn text_recorded_word_count() {
    for c in section("rust/golden/text.json", Some("word_count"), 10) {
        assert_eq!(text::word_count(s(&c["text"])) as u64, c["count"].as_u64().unwrap(), "{:?}", c["text"]);
    }
}

#[test]
fn text_normalize_for_wer() {
    let cases = section("rust/golden/text.json", Some("normalize"), 60);
    let mut bad = Vec::new();
    for c in &cases {
        let got = text::normalize_for_wer(s(&c["text"]));
        if got != s(&c["output"]) {
            bad.push(format!("{:?}\n  python {:?}\n  rust   {:?}", s(&c["text"]), s(&c["output"]), got));
        }
    }
    assert!(bad.is_empty(), "{} of {} mismatches:\n{}", bad.len(), cases.len(), bad.join("\n"));
}

#[test]
fn text_wer() {
    for c in section("rust/golden/text.json", Some("wer"), 30) {
        let got = text::wer(s(&c["reference"]), s(&c["hypothesis"]));
        let want = c["wer"].as_f64().unwrap();
        assert!((got - want).abs() < 1e-12, "{c}: rust {got}");
    }
}

#[test]
fn text_sequence_matcher_ratio() {
    for c in section("rust/golden/text.json", Some("ratio"), 30) {
        let got = text::ratio(s(&c["a"]), s(&c["b"]));
        let want = c["ratio"].as_f64().unwrap();
        assert!((got - want).abs() < 1e-12, "{c}: rust {got}");
    }
}

#[test]
fn text_word_coverage() {
    for c in section("rust/golden/text.json", Some("coverage"), 30) {
        let (t, h) = (s(&c["text"]), s(&c["heard"]));
        let want: Vec<bool> = c["covered"].as_array().unwrap().iter().map(|b| b.as_bool().unwrap()).collect();
        assert_eq!(text::word_coverage(t, h), want, "{t:?} / {h:?}");
        assert_eq!(text::final_word_missing(t, h), c["final_missing"].as_bool().unwrap(), "{t:?} / {h:?}");
        assert_eq!(text::first_word_missing(t, h), c["first_missing"].as_bool().unwrap(), "{t:?} / {h:?}");
    }
}

#[test]
fn text_long_input_does_not_panic() {
    // The look-around regexes (fancy-regex) must not hit a backtrack limit on a long script.
    let para = "word ".repeat(500_000);
    let rule = [Pronunciation { from: "never".into(), to: "x".into() }];
    assert!(!text::clean_text(&para, &rule, true).is_empty());
    assert!(!text::split_sentences(&para).is_empty());
    let prose = "One sentence here. ".repeat(100_000);
    assert_eq!(text::split_sentences(&prose).len(), 100_000);
}
