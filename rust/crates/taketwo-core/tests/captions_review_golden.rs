//! Parity with the Python app for captions, timings, subtitles and review rules (step S05):
//! contract/golden/narrations.json and the vectors rust/golden/record.py writes to rust/golden/review.json.
//! Every value must equal Python exactly: floats bit-equal, ints stay ints, key order the same.

use serde_json::Value;
use std::path::{Path, PathBuf};
use taketwo_core::captions;
use taketwo_core::library::Doc;
use taketwo_core::review;

fn load(rel: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join(rel);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn golden() -> Value {
    load("rust/golden/review.json")
}

fn cases(v: &Value, k: &str, want: usize) -> Vec<Value> {
    let c = v[k].as_array().unwrap_or_else(|| panic!("no section {k}")).clone();
    assert_eq!(c.len(), want, "section {k}: case count");
    c
}

/// Bit-equal floats, int and float kept apart, object key order compared too.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64(), x.is_f64(), y.is_f64()) {
            (_, _, true, true) => x.as_f64().unwrap().to_bits() == y.as_f64().unwrap().to_bits(),
            (Some(p), Some(q), false, false) => p == q,
            _ => x.is_u64() && y.is_u64() && x.as_u64() == y.as_u64(),
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|((kp, p), (kq, q))| kp == kq && same(p, q))
        }
        _ => a == b,
    }
}

fn doc(v: &Value) -> Doc {
    v.as_object().unwrap().clone()
}

/// A recorded `{"result": ...}` or `{"error": "<Python exception>"}` against a Rust result.
fn check_run(bad: &mut Vec<String>, what: &str, want: &Value, got: Result<Value, String>) {
    match (want.get("result"), got) {
        (Some(w), Ok(g)) if same(w, &g) => {}
        (None, Err(_)) if want["error"].is_string() => {} // Python raised; the port returns Err
        (_, got) => bad.push(format!("{what}\n  python {want}\n  rust   {got:?}")),
    }
}

fn finish(bad: Vec<String>) {
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}

fn caps_value(c: &[Doc]) -> Value {
    Value::Array(c.iter().cloned().map(Value::Object).collect())
}

#[test]
fn captions_review_contract_narrations() {
    let fixture = load("contract/fixture.json");
    let golden = load("contract/golden/narrations.json");
    let gens = fixture["generations"].as_array().unwrap();
    assert_eq!(gens.len(), 5, "fixture narrations");
    assert_eq!(golden.as_object().unwrap().len(), 5, "golden narrations");
    let (mut bad, mut with_timeline) = (Vec::new(), 0);
    for g in gens {
        let g = doc(g);
        let id = g["id"].as_str().unwrap();
        let want = &golden[id];
        let outcome = review::outcome(&g).unwrap();
        let mut got = serde_json::Map::new();
        got.insert("outcome".into(), outcome.map(Value::from).unwrap_or(Value::Null));
        let items = if outcome.is_some() { review::review(&g, None).unwrap() } else { Vec::new() };
        let keys = ["part", "text", "issues", "reasons", "start", "end", "checked_by"];
        let items: Vec<Value> = items.iter().map(|r| keys.iter().map(|k| (k.to_string(), r[*k].clone())).collect::<Doc>().into()).collect();
        got.insert("review".into(), items.into());
        if g.get("timeline").is_some_and(|t| !t.is_null() && t != &Value::Array(vec![])) {
            with_timeline += 1;
            let caps = captions::generation_captions(&g).unwrap();
            got.insert("timings".into(), captions::generation_timings(&g).unwrap().into());
            got.insert("captions".into(), caps_value(&caps));
            got.insert("srt".into(), captions::to_srt(&caps).unwrap().into());
            got.insert("vtt".into(), captions::to_vtt(&caps).unwrap().into());
            got.insert("txt".into(), captions::to_txt(&caps).unwrap().into());
        }
        let got = Value::Object(got);
        if !same(want, &got) {
            bad.push(format!("{id}\n  python {want}\n  rust   {got}"));
        }
    }
    assert_eq!(with_timeline, 4, "narrations with a timeline");
    finish(bad);
}

#[test]
fn captions_round_and_ts() {
    let mut bad = Vec::new();
    for c in cases(&golden(), "round", 280) {
        let x = c["x"].as_f64().unwrap();
        let r = captions::round3(x);
        if r.to_bits() != c["round3"].as_f64().unwrap().to_bits() {
            bad.push(format!("round({x:?}, 3): python {} rust {r:?}", c["round3"]));
        }
        if let Some(want) = c["ts"].as_str() {
            let got = captions::ts(x, ',');
            if got != want {
                bad.push(format!("_ts({x:?}): python {want} rust {got}"));
            }
        }
    }
    finish(bad);
}

#[test]
fn captions_wrap_pieces_and_header() {
    let g = golden();
    assert_eq!(g["max_caption_chars"], captions::MAX_CAPTION_CHARS);
    let mut bad = Vec::new();
    for c in cases(&g, "wrap", 17) {
        let got = captions::wrap(c["text"].as_str().unwrap());
        if got != c["out"].as_str().unwrap() {
            bad.push(format!("_wrap {:?}: python {:?} rust {got:?}", c["text"], c["out"]));
        }
    }
    for c in cases(&g, "pieces", 18) {
        let got = Value::from(captions::caption_pieces(c["text"].as_str().unwrap()));
        if got != c["pieces"] {
            bad.push(format!("_caption_pieces {:?}: python {} rust {got}", c["text"], c["pieces"]));
        }
    }
    for c in cases(&g, "header", 14) {
        let got = captions::attachment_header(c["name"].as_str().unwrap(), c["ext"].as_str().unwrap());
        if got != c["out"].as_str().unwrap() {
            bad.push(format!("attachment_header {:?}: python {:?} rust {got:?}", c["name"], c["out"]));
        }
    }
    finish(bad);
}

#[test]
fn captions_timings_and_subtitles() {
    let (mut bad, mut errors, mut estimated) = (Vec::new(), 0, 0);
    for c in cases(&golden(), "timings", 26) {
        let g = doc(&c["gen"]);
        let t = captions::generation_timings(&g);
        estimated += t.as_ref().is_ok_and(|t| t["estimated"] == true) as usize;
        check_run(&mut bad, &format!("timings {}", c["gen"]), &c["timings"], t.map(Value::Object));
        let caps = captions::generation_captions(&g);
        if let Ok(caps) = &caps {
            for (k, got) in [("srt", captions::to_srt(caps)), ("vtt", captions::to_vtt(caps)), ("txt", captions::to_txt(caps))] {
                if got.as_deref().ok() != c[k].as_str() {
                    bad.push(format!("{k} {}\n  python {:?}\n  rust   {got:?}", c["gen"], c[k]));
                }
            }
        } else {
            errors += 1;
        }
        check_run(&mut bad, &format!("captions {}", c["gen"]), &c["captions"], caps.map(|c| caps_value(&c)));
    }
    assert_eq!((errors, estimated), (2, 9), "error cases and estimated cases");
    finish(bad);
}

#[test]
fn review_issue_label_and_constants() {
    let g = golden();
    let labels = cases(&g, "issue_label", 25);
    for (want, (code, label)) in labels.iter().zip(review::ISSUE_LABEL) {
        assert_eq!((want[0].as_str().unwrap(), want[1].as_str().unwrap()), (code, label));
    }
    let fields = cases(&g, "take_fields", 15);
    let fields: Vec<&str> = fields.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(fields, review::TAKE_FIELDS);
    assert_eq!(g["max_old_takes"], review::MAX_OLD_TAKES);
}

#[test]
fn review_rules_golden() {
    let mut bad = Vec::new();
    let mut needs = 0;
    for c in cases(&golden(), "rules", 26) {
        let g = doc(&c["gen"]);
        let id = &c["gen"];
        check_run(&mut bad, &format!("fix_targets {id}"), &c["fix_targets"], review::fix_targets(&g).map(Value::from));
        let o = review::outcome(&g).map(|o| o.map(Value::from).unwrap_or(Value::Null));
        needs += o.as_ref().is_ok_and(|o| o == "needs_listen") as usize;
        check_run(&mut bad, &format!("outcome {id}"), &c["outcome"], o);
        check_run(&mut bad, &format!("review {id}"), &c["review"], review::review(&g, None).map(Value::from));
        check_run(&mut bad, &format!("review(pitch) {id}"), &c["review_pitch"], review::review(&g, Some(181.25)).map(Value::from));
        let mut s = g.clone();
        check_run(&mut bad, &format!("summarize {id}"), &c["summarize"], review::summarize(&mut s).map(|_| Value::Object(s)));
    }
    assert_eq!(needs, 19, "needs_listen cases");
    finish(bad);
}

#[test]
fn review_set_accepted_golden() {
    let mut bad = Vec::new();
    for c in cases(&golden(), "accept", 10) {
        let mut g = doc(&c["gen"]);
        let now = c["now"].as_f64().unwrap();
        let r = review::set_accepted(&mut g, c["ci"].as_i64().unwrap(), c["on"].as_bool().unwrap(), &|| now);
        check_run(&mut bad, &format!("set_accepted {} {} {}", c["ci"], c["on"], c["gen"]), &c["after"], r.map(|_| Value::Object(g)));
    }
    finish(bad);
}

/// Every file and folder under `root`, as record.py's `_tree` lists them.
fn tree(root: &Path) -> Value {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Value)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if p.is_dir() {
                out.push((rel + "/", Value::Null));
                walk(root, &p, out);
            } else {
                out.push((rel, std::fs::read_to_string(&p).unwrap().into()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|a, b| a.0.trim_end_matches('/').cmp(b.0.trim_end_matches('/')));
    Value::Object(out.into_iter().collect())
}

fn write_files(gdir: &Path, files: &Value) {
    for (rel, content) in files.as_object().unwrap() {
        let p = gdir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content.as_str().unwrap()).unwrap();
    }
}

#[test]
fn review_takes_golden() {
    let gens = tempfile::tempdir().unwrap();
    let mut bad = Vec::new();
    let mut archived = 0;
    for c in cases(&golden(), "takes", 9) {
        let gid = c["gen_id"].as_str().unwrap();
        let gdir = gens.path().join(gid);
        write_files(&gdir, &c["files"]);
        let ch = doc(&c["chunk"]);
        let now = c["now"].as_f64().unwrap();
        let a = review::archive_take(gens.path(), gid, &ch, &|| now).unwrap();
        archived += a.is_some() as usize;
        let a = a.map(Value::Object).unwrap_or(Value::Null);
        if !same(&a, &c["archived"]) {
            bad.push(format!("{} archive\n  python {}\n  rust   {a}", c["name"], c["archived"]));
        }
        let mut trimmed = ch.clone();
        review::trim_takes(gens.path(), gid, &mut trimmed).unwrap();
        if !same(&Value::Object(trimmed.clone()), &c["trimmed"]) {
            bad.push(format!("{} trim\n  python {}\n  rust   {trimmed:?}", c["name"], c["trimmed"]));
        }
        let t = tree(&gdir);
        if t != c["tree"] {
            bad.push(format!("{} files\n  python {}\n  rust   {t}", c["name"], c["tree"]));
        }
        let ci = c["take_audio_ci"].as_i64().unwrap();
        let mut chunks = vec![Value::Object(trimmed)];
        if ci == 1 {
            chunks.insert(0, serde_json::json!({"i": 0, "take": 1}));
        }
        let g = doc(&serde_json::json!({"id": gid, "chunks": chunks}));
        for (k, want) in c["take_audio"].as_object().unwrap() {
            let got = review::take_audio(gens.path(), &g, ci, k.parse().unwrap()).unwrap();
            let got = got.map(|p| p.strip_prefix(gens.path()).unwrap().to_string_lossy().replace('\\', "/"));
            if got.as_deref() != want.as_str() {
                bad.push(format!("{} take_audio({ci}, {k}): python {want} rust {got:?}", c["name"]));
            }
        }
        std::fs::remove_dir_all(&gdir).unwrap();
    }
    assert_eq!(archived, 7, "takes archived");
    finish(bad);
}

#[test]
fn review_takes_temp_folder() {
    let gens = tempfile::tempdir().unwrap();
    let g = gens.path();
    let gdir = g.join("g1");
    std::fs::create_dir_all(&gdir).unwrap();
    for (f, s) in [("chunk_002.wav", "now"), ("chunk_002.raw.wav", "raw")] {
        std::fs::write(gdir.join(f), s).unwrap();
    }
    let ch = doc(&serde_json::json!({"i": 2, "file": "chunk_002.wav", "raw_file": "chunk_002.raw.wav",
        "context_file": "chunk_002.ctx.wav", "take": 4, "accepted": {"by": "person"}}));
    let take = review::archive_take(g, "g1", &ch, &|| 12.5).unwrap().unwrap();
    let keys: Vec<&str> = take.keys().map(String::as_str).collect();
    assert_eq!(&keys[..15], review::TAKE_FIELDS);
    assert_eq!(&keys[15..], ["take", "archived"]);
    assert_eq!((take["take"].clone(), take["archived"].clone()), (Value::from(4), Value::from(12.5)));
    let d = review::take_dir(g, "g1", 2, "4");
    assert!(d.ends_with("g1/takes/002-4"));
    assert_eq!(std::fs::read_to_string(d.join("chunk_002.wav")).unwrap(), "now");
    assert_eq!(std::fs::read_to_string(d.join("chunk_002.raw.wav")).unwrap(), "raw");
    assert!(!d.join("chunk_002.ctx.wav").exists(), "a context file that does not exist is not made");
    let mut missing = ch.clone();
    missing.insert("file".into(), "nope.wav".into());
    assert!(review::archive_take(g, "g1", &missing, &|| 0.0).unwrap().is_none());

    let mut many = ch.clone();
    let olds: Vec<Value> = (1..=5).map(|k| serde_json::json!({"take": k})).collect();
    for k in 1..=5 {
        std::fs::create_dir_all(review::take_dir(g, "g1", 2, &k.to_string())).unwrap();
    }
    many.insert("takes".into(), olds.into());
    review::trim_takes(g, "g1", &mut many).unwrap();
    assert_eq!(many["takes"], serde_json::json!([{"take": 3}, {"take": 4}, {"take": 5}]));
    for k in 1..=5 {
        assert_eq!(review::take_dir(g, "g1", 2, &k.to_string()).exists(), k >= 3, "take {k}");
    }

    let nar = doc(&serde_json::json!({"id": "g1", "chunks": [{"i": 0}, {"i": 1}, ch]}));
    assert_eq!(review::take_audio(g, &nar, 2, 4).unwrap(), Some(gdir.join("chunk_002.wav")));
    std::fs::write(review::take_dir(g, "g1", 2, "3").join("chunk_002.wav"), "old").unwrap();
    assert_eq!(review::take_audio(g, &nar, 2, 3).unwrap(), Some(review::take_dir(g, "g1", 2, "3").join("chunk_002.wav")));
    assert_eq!(review::take_audio(g, &nar, 2, 1).unwrap(), None);
}
