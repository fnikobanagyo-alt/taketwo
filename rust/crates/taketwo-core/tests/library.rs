//! Parity of `taketwo_core::library` with backend/app/db.py, config.py and main.py's startup repairs.
//! Vectors: rust/golden/library.sqlite3 (written by db.py) and rust/golden/library.json, both from
//! rust/golden/record.py. Every test works on temporary folders and asserts how many golden cases it read.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use taketwo_core::library::{self, Doc, Library};

const CLOCK0: f64 = 1800000000.0;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden")
}

fn golden() -> Value {
    let p = golden_dir().join("library.json");
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn cases(g: &Value, key: &str, want: usize) -> Vec<Value> {
    let c = g[key].as_array().unwrap_or_else(|| panic!("library.json: no {key}")).clone();
    assert_eq!(c.len(), want, "library.json {key}: case count");
    c
}

fn obj(v: &Value) -> Doc {
    v.as_object().cloned().unwrap_or_else(|| panic!("not an object: {v}"))
}

/// Value-equal and key-order-equal (serde_json's `==` ignores key order).
fn same(a: &Value, b: &Value) -> bool {
    a == b && a.to_string() == b.to_string()
}

/// A clock that returns `start`, then whatever `set` stores.
fn fixed_clock(start: f64) -> (Arc<AtomicU64>, impl Fn() -> f64 + Send + Sync + 'static) {
    let t = Arc::new(AtomicU64::new(start.to_bits()));
    let r = t.clone();
    (t, move || f64::from_bits(r.load(Ordering::SeqCst)))
}

/// A temp copy of the Python database, so the committed file is never opened read-write.
fn python_library(tmp: &Path) -> PathBuf {
    let dir = tmp.join("lib");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(golden_dir().join("library.sqlite3"), dir.join(library::DB_FILE)).unwrap();
    dir
}

fn docs(v: Vec<Doc>) -> Value {
    Value::Array(v.into_iter().map(Value::Object).collect())
}

#[test]
fn library_python_database_reads_unchanged() {
    let g = golden();
    let db = &g["database"];
    let profiles = cases(db, "profiles", 5);
    let generations = cases(db, "generations", 8);
    let tmp = tempfile::tempdir().unwrap();
    let dir = python_library(tmp.path());
    let (_, clock) = fixed_clock(CLOCK0 + 100.0);
    let lib = Library::open(&dir).unwrap().with_clock(clock);

    assert!(same(&docs(lib.list_profiles().unwrap()), &Value::Array(profiles.clone())), "list_profiles");
    assert!(same(&docs(lib.list_generations().unwrap()), &Value::Array(generations.clone())), "list_generations");
    for p in &profiles {
        let got = lib.get_profile(p["id"].as_str().unwrap()).unwrap().unwrap();
        assert!(same(&Value::Object(got), p), "get_profile {}", p["id"]);
    }
    for gen_ in &generations {
        let got = lib.get_generation(gen_["id"].as_str().unwrap()).unwrap().unwrap();
        assert!(same(&Value::Object(got), gen_), "get_generation {}", gen_["id"]);
    }
    assert!(lib.get_generation("missing01").unwrap().is_none());

    // The id/created/profile_id columns as db.py wrote them.
    let conn = rusqlite::Connection::open(dir.join(library::DB_FILE)).unwrap();
    let cols = |sql: &str, n: usize| -> Value {
        let mut st = conn.prepare(sql).unwrap();
        let rows = st
            .query_map([], |r| {
                let mut row = vec![json!(r.get::<_, String>(0)?), json!(r.get::<_, f64>(1)?)];
                if n == 3 {
                    row.push(json!(r.get::<_, Option<String>>(2)?));
                }
                Ok(Value::Array(row))
            })
            .unwrap();
        Value::Array(rows.map(Result::unwrap).collect())
    };
    let want = &db["columns"];
    assert_eq!(cols("SELECT id, created FROM profiles ORDER BY id", 2), want["profiles"]);
    assert_eq!(cols("SELECT id, created, profile_id FROM generations ORDER BY id", 3), want["generations"]);

    // Write one back through Rust: unchanged apart from `updated`, columns unchanged.
    let before = lib.get_generation("glib03").unwrap().unwrap();
    let mut doc = before.clone();
    lib.put_generation(&mut doc).unwrap();
    let after = lib.get_generation("glib03").unwrap().unwrap();
    let mut expect = before.clone();
    expect.insert("updated".into(), json!(CLOCK0 + 100.0));
    assert!(same(&Value::Object(after), &Value::Object(expect)), "glib03 written back");
    assert_eq!(cols("SELECT id, created, profile_id FROM generations ORDER BY id", 3), want["generations"]);
    let mut p = lib.get_profile("vlib01").unwrap().unwrap();
    let mut expect = p.clone();
    lib.put_profile(&mut p).unwrap();
    expect.insert("updated".into(), json!(CLOCK0 + 100.0));
    assert!(same(&Value::Object(lib.get_profile("vlib01").unwrap().unwrap()), &Value::Object(expect)), "vlib01");
}

#[test]
fn library_store_cases() {
    let tmp = tempfile::tempdir().unwrap();
    let (t, clock) = fixed_clock(100.0);
    let set = |x: f64| t.store(x.to_bits(), Ordering::SeqCst);
    let lib = Library::open(&tmp.path().join("lib")).unwrap().with_clock(clock);
    for d in ["profiles", "generations", "designs", "transcripts"] {
        assert!(tmp.path().join("lib").join(d).is_dir(), "{d}");
    }
    assert!(tmp.path().join("lib/voiceforge.db").is_file());

    // _put: `created` only when missing, `updated` always (appended at the end when new).
    let mut a = obj(&json!({"id": "a", "status": "done", "profile_id": "v1"}));
    lib.put_generation(&mut a).unwrap();
    assert_eq!(Value::Object(a.clone()).to_string(), r#"{"id":"a","status":"done","profile_id":"v1","created":100.0,"updated":100.0}"#);
    set(200.0);
    let mut b = obj(&json!({"id": "b", "created": 50.0, "updated": 1.0}));
    lib.put_generation(&mut b).unwrap();
    assert_eq!(Value::Object(b).to_string(), r#"{"id":"b","created":50.0,"updated":200.0}"#);
    let ids = |v: Vec<Doc>| v.iter().map(|d| d["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(ids(lib.list_generations().unwrap()), ["a", "b"], "ORDER BY created DESC");

    // put_generation_live: a missing row stays missing; title, favorite, deleted_at survive from the store.
    let mut ghost = obj(&json!({"id": "ghost", "status": "done"}));
    assert!(!lib.put_generation_live(&mut ghost).unwrap());
    assert!(lib.get_generation("ghost").unwrap().is_none());
    assert!(lib.patch_generation_meta("a", &obj(&json!({"title": "Mine", "favorite": true, "status": "x"}))).unwrap().is_some());
    assert_eq!(lib.get_generation("a").unwrap().unwrap()["status"], "done", "patch touches title and favorite only");
    set(300.0);
    lib.set_generation_deleted("a", true).unwrap();
    let mut stale = obj(&json!({"id": "a", "status": "running", "title": "Old", "progress": 0.5}));
    set(310.0);
    assert!(lib.put_generation_live(&mut stale).unwrap());
    let got = lib.get_generation("a").unwrap().unwrap();
    assert_eq!(Value::Object(got).to_string(), r#"{"id":"a","status":"running","title":"Mine","progress":0.5,"favorite":true,"deleted_at":300.0,"updated":310.0}"#);
    let mut plain = obj(&json!({"id": "b", "title": "Worker title"}));
    assert!(lib.put_generation_live(&mut plain).unwrap());
    assert_eq!(lib.get_generation("b").unwrap().unwrap()["title"], "Worker title", "no stored title to keep");

    // Soft delete and restore; restore keeps the other keys in order and leaves `updated` alone.
    let restored = lib.set_generation_deleted("a", false).unwrap().unwrap();
    assert_eq!(Value::Object(restored).to_string(), r#"{"id":"a","status":"running","title":"Mine","progress":0.5,"favorite":true,"updated":310.0}"#);
    assert!(lib.set_generation_deleted("ghost", true).unwrap().is_none());
    assert!(lib.patch_generation_meta("ghost", &Map::new()).unwrap().is_none());

    // Purge: strictly older than the cut, and a deleted_at of 0 is never purged.
    for (id, at) in [("old", json!(10.0)), ("edge", json!(1000.0 - 500.0)), ("zero", json!(0)), ("new", json!(900.0))] {
        let mut d = obj(&json!({"id": id, "created": 1.0, "deleted_at": at}));
        lib.put_generation(&mut d).unwrap();
    }
    set(1000.0);
    assert_eq!(lib.purge_deleted_generations(500.0).unwrap(), ["old"]);
    assert!(lib.get_generation("edge").unwrap().is_some() && lib.get_generation("zero").unwrap().is_some());

    lib.delete_generation("b").unwrap();
    assert!(lib.get_generation("b").unwrap().is_none());
    let mut p = obj(&json!({"id": "p1"}));
    lib.put_profile(&mut p).unwrap();
    lib.delete_profile("p1").unwrap();
    assert!(lib.list_profiles().unwrap().is_empty());
    assert!(library::is_plain_name("glib01") && !library::is_plain_name("..") && !library::is_plain_name("a/b"));
    assert!(!library::is_plain_name("/abs") && !library::is_plain_name("a\\b") && !library::is_plain_name(""));
}

#[test]
fn library_startup_repairs() {
    let g = golden();
    let want_gens = cases(&g["database"], "generations", 8);
    let tmp = tempfile::tempdir().unwrap();
    let dir = python_library(tmp.path());
    let (_, clock) = fixed_clock(CLOCK0 + 10.0);
    let lib = Library::open(&dir).unwrap().with_clock(clock);
    for id in ["glib04", "glib05", "glib07"] {
        std::fs::create_dir_all(lib.generations_dir().join(id)).unwrap();
        std::fs::write(lib.generations_dir().join(id).join("final.wav"), b"x").unwrap();
    }
    assert_eq!(library::startup_repairs(&lib).unwrap(), ["glib04"]);
    assert!(!lib.generations_dir().join("glib04").exists(), "purged folder removed");
    assert!(lib.generations_dir().join("glib05/final.wav").exists() && lib.generations_dir().join("glib07").exists());

    let gen_ = |id: &str| lib.get_generation(id).unwrap().unwrap();
    let chunk_status = |d: &Doc| d["chunks"].as_array().unwrap().iter().map(|c| c["status"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    for id in ["glib01", "glib02"] {
        let d = gen_(id);
        assert_eq!(d["status"], "interrupted", "{id}");
        assert_eq!(d["message"], library::INTERRUPTED_MESSAGE, "{id}");
        assert_eq!(d["updated"], json!(CLOCK0 + 10.0), "{id}");
    }
    assert_eq!(chunk_status(&gen_("glib01")), ["done", "pending", "pending", "pending"]);
    assert_eq!(chunk_status(&gen_("glib08")), ["running"], "only queued/running narrations are touched");
    // Everything else is unchanged.
    for w in &want_gens {
        let id = w["id"].as_str().unwrap();
        if !["glib01", "glib02", "glib04"].contains(&id) {
            assert!(same(&Value::Object(gen_(id)), w), "{id} unchanged");
        }
    }
    assert!(lib.get_generation("glib04").unwrap().is_none());

    let prof = |id: &str| lib.get_profile(id).unwrap().unwrap();
    for id in ["vlib02", "vlib03"] {
        assert_eq!(prof(id)["status"], "error", "{id}");
        assert_eq!(prof(id)["error"], library::ANALYSIS_INTERRUPTED, "{id}");
    }
    assert_eq!(prof("vlib04")["error"], "Old error");
    assert!(prof("vlib05").get("status").is_none());
    let golden_v1 = &cases(&g["database"], "profiles", 5).into_iter().find(|p| p["id"] == "vlib01").unwrap();
    assert!(same(&Value::Object(prof("vlib01")), golden_v1));
}

fn outcome(r: Result<Doc, String>) -> Value {
    match r {
        Ok(d) => json!({"ok": d}),
        Err(e) => json!({"error_type": "ValueError", "error": e}),
    }
}

fn want_outcome(c: &Value) -> Value {
    match c.get("ok") {
        Some(ok) => json!({"ok": ok}),
        None => json!({"error_type": c["error_type"], "error": c["error"]}),
    }
}

fn write_settings_file(path: &Path, c: &Value) {
    for n in ["settings.json", "settings.json.broken", "settings.json.tmp"] {
        let _ = std::fs::remove_file(path.with_file_name(n));
    }
    if let Some(t) = c.get("file").and_then(Value::as_str) {
        std::fs::write(path, t).unwrap();
    } else if let Some(h) = c.get("file_hex").and_then(Value::as_str) {
        let bytes: Vec<u8> = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect();
        std::fs::write(path, bytes).unwrap();
    }
}

fn settings_after(path: &Path) -> Value {
    let exists = |n: &str| path.with_file_name(n).exists();
    let file_after = std::fs::read_to_string(path).ok().map(|t| serde_json::from_str::<Value>(&t).unwrap()).unwrap_or(Value::Null);
    json!({"settings_exists": exists("settings.json"), "broken_exists": exists("settings.json.broken"),
           "tmp_exists": exists("settings.json.tmp"), "file_after": file_after})
}

#[test]
fn library_settings_vectors() {
    let g = golden();
    let defaults = cases(&g, "defaults", 1);
    assert!(same(&Value::Object(library::default_settings()), &defaults[0]), "DEFAULT_SETTINGS");
    let rules = cases(&g, "rules", 12);
    for (r, (k, lo, hi, is_int)) in rules.iter().zip(library::SETTING_RULES) {
        assert_eq!((r["key"].as_str().unwrap(), r["lo"].as_f64().unwrap(), r["hi"].as_f64().unwrap(), r["type"] == "int"), (k, lo, hi, is_int));
    }
    assert_eq!(cases(&g, "gen_meta_keys", 1)[0], json!(library::GEN_META_KEYS));

    let mut bad = Vec::new();
    let checked = cases(&g, "checked", 330);
    for c in &checked {
        assert_eq!(c.get("error_type").map_or("ValueError", |t| t.as_str().unwrap()), "ValueError", "a non-ValueError case: {c}");
        let got = outcome(library::checked(&obj(&c["patch"])));
        if !same(&got, &want_outcome(c)) {
            bad.push(format!("{}\n  python {}\n  rust   {got}", c["patch"], want_outcome(c)));
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    for c in &cases(&g, "get_settings", 19) {
        write_settings_file(&path, c);
        let got = json!({"result": outcome(library::get_settings(&path)), "after": settings_after(&path)});
        let want = json!({"result": want_outcome(c), "after": {"settings_exists": c["settings_exists"],
            "broken_exists": c["broken_exists"], "tmp_exists": c["tmp_exists"], "file_after": c["file_after"]}});
        if !same(&got, &want) {
            bad.push(format!("get_settings {:?}\n  python {want}\n  rust   {got}", c.get("file").or(c.get("file_hex"))));
        }
    }
    for c in &cases(&g, "save_settings", 11) {
        write_settings_file(&path, c);
        let got = json!({"result": outcome(library::save_settings(&path, &obj(&c["patch"]))), "after": settings_after(&path)});
        let want = json!({"result": want_outcome(c), "after": {"settings_exists": c["settings_exists"],
            "broken_exists": c["broken_exists"], "tmp_exists": c["tmp_exists"], "file_after": c["file_after"]}});
        if !same(&got, &want) {
            bad.push(format!("save_settings {:?} {}\n  python {want}\n  rust   {got}", c.get("file"), c["patch"]));
        }
    }
    assert!(bad.is_empty(), "{} settings cases differ:\n{}", bad.len(), bad.join("\n"));
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::metadata(&p).unwrap();
            if m.is_dir() {
                out.push((p.strip_prefix(root).unwrap().to_path_buf(), Vec::new(), m.modified().unwrap()));
                stack.push(p);
            } else {
                out.push((p.strip_prefix(root).unwrap().to_path_buf(), std::fs::read(&p).unwrap(), m.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn library_one_time_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("voiceforge-data");
    {
        let lib = Library::open(&src).unwrap();
        let mut p = obj(&json!({"id": "v1", "name": "Voice", "status": "ready"}));
        lib.put_profile(&mut p).unwrap();
        let mut g = obj(&json!({"id": "g1", "profile_id": "v1", "status": "done", "chunks": []}));
        lib.put_generation(&mut g).unwrap();
    }
    for (rel, bytes) in [
        ("profiles/v1/reference.wav", &b"RIFF ref"[..]),
        ("profiles/v1/preview.wav", b"RIFF preview"),
        ("generations/g1/final.wav", b"RIFF final"),
        ("generations/g1/takes/chunk_000_take1.wav", b"take"),
        ("designs/d1/doc.json", b"{}"),
        ("settings.json", b"{\"seed\": 7}"),
    ] {
        let p = src.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(20)); // a write after this point would move an mtime
    let before = snapshot(&src);
    assert_eq!(before.len(), 15, "synthetic source entries");

    let dst = tmp.path().join("TakeTwo/library");
    assert!(library::copy_library_once(&src, &dst).unwrap());
    let after = snapshot(&src);
    assert!(before == after, "the source changed (bytes, mtime or file set)");
    let copied: Vec<_> = snapshot(&dst).into_iter().map(|e| (e.0, e.1)).collect();
    let want: Vec<_> = before.iter().map(|e| (e.0.clone(), e.1.clone())).collect();
    assert_eq!(copied, want, "destination holds the same files and bytes");
    let lib = Library::open(&dst).unwrap();
    assert_eq!(lib.get_generation("g1").unwrap().unwrap()["profile_id"], "v1");
    drop(lib);

    // A destination that already has a library is left alone.
    std::fs::write(dst.join("settings.json"), b"{\"seed\": 9}").unwrap();
    std::fs::write(src.join("generations/g1/final.wav"), b"changed").unwrap();
    assert!(!library::copy_library_once(&src, &dst).unwrap());
    assert_eq!(std::fs::read(dst.join("settings.json")).unwrap(), b"{\"seed\": 9}");
    assert_eq!(std::fs::read(dst.join("generations/g1/final.wav")).unwrap(), b"RIFF final");
    // A source without a database is refused, and nothing is written.
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(library::copy_library_once(&empty, &tmp.path().join("dst2")).is_err());
    assert!(!tmp.path().join("dst2").exists());
}
