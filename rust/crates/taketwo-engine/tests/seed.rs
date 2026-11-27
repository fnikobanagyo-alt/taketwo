//! `taketwo-engine seed` against the golden of contract/seed_python.py `seed()` recorded by rust/golden/record.py.
//! Each run gets temporary data, HOME and working folders; nothing else is reached.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use taketwo_core::library::Library;

fn wt() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn engine(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_taketwo-engine"));
    c.env_remove("TAKETWO_DATA").env_remove("VOICEFORGE_DATA").env("HOME", home).current_dir(home);
    c
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Value>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        if p.is_dir() {
            out.push(json!({"path": format!("{rel}/")}));
            walk(root, &p, out);
        } else if rel.ends_with(".wav") {
            let r = hound::WavReader::open(&p).unwrap();
            let s = r.spec();
            out.push(json!({"path": rel, "channels": s.channels, "sample_width": s.bits_per_sample / 8,
                            "rate": s.sample_rate, "frames": r.duration(), "bytes": std::fs::metadata(&p).unwrap().len()}));
        } else {
            out.push(json!({"path": rel}));
        }
    }
}

fn sorted(mut v: Vec<Value>) -> Vec<Value> {
    v.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    v
}

fn without_updated(docs: &[Value]) -> String {
    let v: Vec<Value> = docs.iter().map(|d| { let mut d = d.as_object().unwrap().clone(); d.shift_remove("updated"); Value::Object(d) }).collect();
    Value::Array(v).to_string() // a string, so key order counts too
}

#[test]
fn seed_matches_python_golden() {
    let p = wt().join("rust/golden/library.json");
    let g: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let golden = &g["seed"][0];
    let (files, profiles, gens) = (golden["files"].as_array().unwrap(), golden["profiles"].as_array().unwrap(), golden["generations"].as_array().unwrap());
    assert_eq!((files.len(), profiles.len(), gens.len()), (24, 1, 5), "seed golden case counts");

    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("library");
    let out = engine(tmp.path()).env("TAKETWO_DATA", &data).args(["seed", "--fixture"]).arg(wt().join("contract/fixture.json")).output().unwrap();
    assert!(out.status.success(), "seed failed: {}", String::from_utf8_lossy(&out.stderr));

    let mut got = Vec::new();
    walk(&data, &data, &mut got);
    assert_eq!(sorted(got), sorted(files.clone()), "file list and WAV headers");
    let lib = Library::open(&data).unwrap();
    let docs = |v: Vec<taketwo_core::library::Doc>| v.into_iter().map(Value::Object).collect::<Vec<_>>();
    assert_eq!(without_updated(&docs(lib.list_profiles().unwrap())), without_updated(profiles));
    assert_eq!(without_updated(&docs(lib.list_generations().unwrap())), without_updated(gens));
    assert!(!data.join("generations/ctpending01/final.wav").exists());
}

#[test]
fn seed_refuses_without_taketwo_data() {
    let fixture = wt().join("contract/fixture.json");
    for args in [vec!["seed", "--fixture"], vec!["seed", "--live", "--fixture"]] {
        let tmp = tempfile::tempdir().unwrap();
        let out = engine(tmp.path()).args(&args).arg(&fixture).output().unwrap();
        assert!(!out.status.success(), "{args:?} must refuse");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "{args:?} wrote something");
    }
    // TAKETWO_DATA together with --live is refused too, and the folder stays empty.
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("library");
    let out = engine(tmp.path()).env("TAKETWO_DATA", &data).args(["seed", "--live", "--fixture"]).arg(&fixture).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}
