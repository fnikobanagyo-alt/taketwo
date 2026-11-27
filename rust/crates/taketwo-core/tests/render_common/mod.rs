//! Shared by render_golden.rs and render_ported.rs: replay one case of rust/golden/render.json through
//! `taketwo_core::render::render_chunk` with the scripted fake, in a temporary library folder.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use taketwo_core::library::Doc;
use taketwo_core::render::models::fake::{Call, Fake, Kind};
use taketwo_core::render::models::Take;
use taketwo_core::render::{self, Render, RenderError};
use taketwo_core::transcript::Transcript;

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden")
}

pub fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join("render.json")).unwrap()).unwrap()
}

pub fn bytes(file: &Value) -> Vec<u8> {
    std::fs::read(root().join("render").join(file["file"].as_str().unwrap())).unwrap()
}

pub fn f32s(file: &Value) -> Vec<f32> {
    let v: Vec<f32> = bytes(file).chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    assert_eq!(v.len() as u64, file["len"].as_u64().unwrap(), "{file}");
    v
}

pub fn i16s(file: &Value) -> Vec<i16> {
    let v: Vec<i16> = bytes(file).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    assert_eq!(v.len() as u64, file["len"].as_u64().unwrap(), "{file}");
    v
}

pub fn obj(v: &Value) -> Doc {
    v.as_object().unwrap().clone()
}

pub struct Outcome {
    pub result: Result<Doc, RenderError>,
    pub fake: Fake,
    pub gdir: PathBuf,
    pub profile: Doc,
    pub puts: Vec<Doc>,
    pub narration: Doc,
    pub tmp: TempDir,
}

/// A folder removed when dropped.
pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn tempdir(tag: &str) -> TempDir {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("taketwo-render-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TempDir(d)
}

pub fn kind(k: &str) -> Kind {
    match k {
        "synthesize" => Kind::Synthesize,
        "transcribe" => Kind::Transcribe,
        "embedding" => Kind::Embedding,
        "clock" => Kind::Clock,
        _ => panic!("{k}"),
    }
}

/// Replay `case` (its inputs and script). `edit` may change the library folder before the call.
pub fn replay(case: &Value, pitch_cache: &mut HashMap<String, f64>, edit: impl FnOnce(&Path)) -> Outcome {
    replay_with(case, pitch_cache, None, None, edit)
}

/// `replay` with the narration (its folder kept in `dir`) and the nonce replaced: renders the part again.
pub fn replay_with(case: &Value, pitch_cache: &mut HashMap<String, f64>, again: Option<(&Doc, &Path)>, nonce: Option<u32>, edit: impl FnOnce(&Path)) -> Outcome {
    let tmp = tempdir(case["name"].as_str().unwrap());
    let mut inp = case["inputs"].clone();
    if let Some((gen_doc, _)) = again {
        inp["gen"] = Value::Object(gen_doc.clone());
    }
    if let Some(n) = nonce {
        inp["nonce"] = n.into();
    }
    let inp = &inp;
    let profiles = tmp.0.join("profiles");
    let generations = tmp.0.join("generations");
    let pdir = profiles.join(inp["profile"]["id"].as_str().unwrap());
    std::fs::create_dir_all(&pdir).unwrap();
    for (name, f) in case["voice"].as_object().unwrap() {
        std::fs::write(pdir.join(name), bytes(f)).unwrap();
    }
    let gdir = generations.join(inp["gen"]["id"].as_str().unwrap());
    std::fs::create_dir_all(&gdir).unwrap();
    if let Some((_, old)) = again {
        for e in std::fs::read_dir(old).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), gdir.join(e.file_name())).unwrap();
        }
    }
    for (name, hex) in inp["pre_files"].as_object().unwrap() {
        std::fs::write(gdir.join(name), unhex(hex.as_str().unwrap())).unwrap();
    }
    edit(&tmp.0);
    let sc = &case["script"];
    let takes = sc["takes"].as_array().unwrap().iter().map(|t| Take { audio: f32s(&t["audio"]), sr: t["sr"].as_u64().unwrap() as u32, report: obj(&t["report"]) }).collect();
    let trs: Vec<Transcript> = serde_json::from_value(sc["transcripts"].clone()).unwrap();
    let embs: Vec<Vec<f32>> = sc["embeddings"].as_array().unwrap().iter().map(|e| e.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect()).collect();
    let clock = inp["clock"].as_array().unwrap();
    let fake = Fake::new(takes, trs, embs, clock[0].as_f64().unwrap(), clock[1].as_f64().unwrap());
    if let Some(ca) = inp["cancel_after"].as_array() {
        fake.cancel_after(kind(ca[0].as_str().unwrap()), ca[1].as_u64().unwrap() as usize);
    }
    let flag = fake.flag();
    let (mut v, mut t, mut e, c) = (fake.clone(), fake.clone(), fake.clone(), fake.clone());
    let mut puts = Vec::new();
    let mut put = |p: &mut Doc| {
        puts.push(p.clone());
        Ok(())
    };
    let mut clock = || c.now();
    let mut narration = obj(&inp["gen"]);
    let mut profile = obj(&inp["profile"]);
    let result = {
        let mut r = Render {
            profiles_dir: &profiles,
            generations_dir: &generations,
            voice: &mut v,
            transcriber: &mut t,
            encoder: &mut e,
            put_profile: &mut put,
            clock: &mut clock,
            cancel: &flag,
            pitch_cache,
        };
        render::render_chunk(&mut r, &mut narration, inp["ci"].as_u64().unwrap() as usize, &mut profile, &obj(&inp["settings"]), inp["regen_round"].as_i64().unwrap(), inp["nonce"].as_u64().unwrap() as u32)
    };
    if let Ok(part) = &result {
        assert_eq!(&narration["chunks"][inp["ci"].as_u64().unwrap() as usize], &Value::Object(part.clone()), "the part in the narration is the returned part");
    } else {
        assert_eq!(narration, obj(&inp["gen"]), "nothing is written to the narration on an error");
    }
    Outcome { result, fake, gdir, profile, puts, narration, tmp }
}

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
}

/// The fake's calls in render.json's shape.
pub fn calls_json(calls: &[Call]) -> Vec<Value> {
    calls
        .iter()
        .map(|c| match c {
            Call::Synthesize { engine, text, ref_file, ref_text, language, params, seed, expected_seconds } => serde_json::json!({"synthesize": {
                "engine": engine, "text": text, "ref_file": ref_file, "ref_text": ref_text, "language": language,
                "params": params, "seed": seed, "expected_seconds": expected_seconds}}),
            Call::Transcribe { len, language, words } => serde_json::json!({"transcribe": {"len_16k": len, "language": language, "words": words}}),
            Call::Embedding { len, sr } => serde_json::json!({"embedding": {"len": len, "sr": sr}}),
        })
        .collect()
}

/// Python's recorded call without the fields the Rust trait does not see (the transcribe input at its own rate).
pub fn python_call(c: &Value) -> Value {
    let mut c = c.clone();
    if let Some(t) = c.get_mut("transcribe").and_then(Value::as_object_mut) {
        t.shift_remove("len");
        t.shift_remove("sr");
    }
    c
}

/// Compare two JSON documents: key order, int against float, null, strings exact; numbers within
/// `decision_tol` of the key path (`parent` is the object holding the key). `max` gets the largest difference.
pub fn same(path: &str, got: &Value, want: &Value, parent: &Value, max: &mut f64) {
    match (got, want) {
        (Value::Object(g), Value::Object(w)) => {
            let gk: Vec<&String> = g.keys().collect();
            let wk: Vec<&String> = w.keys().collect();
            assert_eq!(gk, wk, "{path}: keys");
            for (k, wv) in w {
                same(&format!("{path}.{k}"), &g[k], wv, want, max);
            }
        }
        (Value::Array(g), Value::Array(w)) => {
            assert_eq!(g.len(), w.len(), "{path}: length");
            for (i, (a, b)) in g.iter().zip(w).enumerate() {
                same(&format!("{path}[{i}]"), a, b, parent, max);
            }
        }
        (Value::Number(a), Value::Number(b)) => {
            assert_eq!(a.is_f64(), b.is_f64(), "{path}: int against float ({a} vs {b})");
            if a.is_f64() {
                let (x, y) = (a.as_f64().unwrap(), b.as_f64().unwrap());
                let d = (x - y).abs();
                assert!(d <= decision_tol(path, parent), "{path}: {x} against {y}");
                *max = max.max(d);
            } else {
                assert_eq!(a, b, "{path}");
            }
        }
        _ => assert_eq!(got, want, "{path}"),
    }
}

/// The tolerances of lead decision 15: `expected_seconds` 1e-12, the unrounded fallback `score` 1e-9, else exact.
/// `parent` is the object holding the key.
pub fn decision_tol(path: &str, parent: &Value) -> f64 {
    if path.ends_with(".expected_seconds") {
        1e-12
    } else if path.ends_with(".score") && parent.get("ending_guard_fallback").is_some() {
        1e-9
    } else {
        0.0
    }
}
