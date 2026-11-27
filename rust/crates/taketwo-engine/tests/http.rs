//! The engine's HTTP API in-process (tower `oneshot`, no port bound), on a temporary library seeded from
//! contract/fixture.json. `parity_with_python` replays rust/golden/http.json, recorded from the Python
//! engine by rust/golden/record_http.py. The job phases (g1 to g4) run the real worker with the stub renderer
//! behind its gate folder, as the recorder runs Python's worker with its gated fake.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use taketwo_core::library::{self, Library};
use taketwo_engine::api::{AppState, router};
use taketwo_engine::render::{Renderer, StubRenderer};
use taketwo_engine::seed;
use taketwo_engine::worker::Ids;
use tower::ServiceExt;

const HEADERS: [&str; 5] = ["content-type", "content-disposition", "cache-control", "x-preview-kind", "content-range"];
/// Answers that can't match the Python app; only their shape (top-level keys) is compared.
const SHAPE_ONLY: [(&str, &str); 3] = [
    ("/api/engines", "Python lists its MLX engines; TakeTwo 1.0 has the one Qwen3 engine (keys of the first item compared, not the count)"),
    ("/api/integrations", "paths of this computer and the Python script vs the Rust taketwo-mcp program"),
    ("/api/diagnostics", "process id, start time and Python source hash (phases a to f; the job phases compare it masked)"),
];
/// Masked in the job phases, on both sides, by position; null stays null, so null against not-null still fails.
/// Each is a `time.time()` of worker.py or main.py (or the process), never pinned by the recorder.
const MASKED: [(&str, &str); 6] = [
    ("job objects (keys kind and phase): created, started, finished, phase_started_at, last_progress_at, \
      cancel_requested_at, cancel_acknowledged_at, phase_elapsed_seconds", "worker.py's wall and monotonic clock"),
    ("events[].at", "worker.py `_event` time"),
    ("duplicate notice (keys count and last): last", "main.py stamps the refused create with time.time()"),
    ("runtime.pid", "the process id"),
    ("runtime.started_at", "the worker's start time"),
    ("runtime.source_sha256", "Python's source hash vs the Rust engine's build stamp"),
];
const JOB_TIMES: [&str; 8] = ["created", "started", "finished", "phase_started_at", "last_progress_at", "cancel_requested_at",
    "cancel_acknowledged_at", "phase_elapsed_seconds"];

/// record_http.py `mask`.
fn mask(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(mask).collect()),
        Value::Object(o) => {
            let has = |k: &str| o.contains_key(k);
            let (job, notice, event, runtime) = (has("kind") && has("phase"), has("count") && has("last"), has("event") && has("at"), has("source_sha256"));
            Value::Object(o.iter().map(|(k, x)| {
                let k2 = k.as_str();
                let hide = (job && JOB_TIMES.contains(&k2)) || (notice && k2 == "last") || (event && k2 == "at")
                    || (runtime && ["pid", "started_at", "source_sha256"].contains(&k2));
                (k.clone(), if hide { if x.is_null() { Value::Null } else { Value::from("<masked>") } } else { mask(x) })
            }).collect())
        }
        other => other.clone(),
    }
}

/// record_http.py `pin_uuid`: the n-th id is `{n:010x}{n:022x}`.
fn counter_ids() -> Ids {
    let n = std::sync::atomic::AtomicU64::new(0);
    Arc::new(move || {
        let n = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        format!("{n:010x}{n:022x}")
    })
}

fn wt() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

struct Engine {
    _tmp: tempfile::TempDir,
    /// The stub renderer's gate folder (`go`, `fail`).
    gate: PathBuf,
    /// What record_http.py does through db.py in its own process (seeding, setup steps), with its own clock.
    setup: Library,
    state: Arc<AppState>,
    app: Router,
    revealed: Arc<Mutex<Vec<PathBuf>>>,
}

fn stepping(lib: Library, clock: Option<(f64, f64)>) -> Library {
    let Some((start, step)) = clock else { return lib };
    let t = Mutex::new(start);
    lib.with_clock(move || {
        let mut t = t.lock().unwrap();
        *t += step;
        *t
    })
}

/// A seeded temporary library. `clocks` pin the two clocks as record_http.py pins them: the recorder's
/// (seeding and setup steps) and the engine child's (every request), each `(start, step)`.
/// The opener only records what would be shown: no test opens the file manager.
fn engine(clocks: Option<((f64, f64), (f64, f64))>, ui: Option<PathBuf>) -> Engine {
    build(clocks, ui, false)
}

/// `stub`: the stub renderer behind a gate folder and pinned ids, as record_http.py's engine child.
fn build(clocks: Option<((f64, f64), (f64, f64))>, ui: Option<PathBuf>, stub: bool) -> Engine {
    let tmp = tempfile::tempdir().unwrap();
    let gate = tmp.path().join("gate");
    std::fs::create_dir(&gate).unwrap();
    let dir = tmp.path().join("library");
    let setup = stepping(Library::open(&dir).unwrap(), clocks.map(|c| c.0));
    seed::seed_into(&setup, &wt().join("contract/fixture.json")).unwrap();
    let lib = stepping(Library::open(&dir).unwrap(), clocks.map(|c| c.1));
    library::startup_repairs(&lib).unwrap(); // as serve does, and as main.py does on import
    let revealed = Arc::new(Mutex::new(Vec::new()));
    let seen = revealed.clone();
    let reveal = Box::new(move |p: &Path| seen.lock().unwrap().push(p.to_path_buf()));
    let (renderer, ids): (Option<Arc<dyn Renderer>>, Ids) = if stub {
        (Some(Arc::new(StubRenderer { gate: Some(gate.clone()) })), counter_ids())
    } else {
        (None, taketwo_engine::worker::random_ids())
    };
    let state = Arc::new(AppState::new(lib, ui, 1.0, reveal, renderer, ids));
    Engine { _tmp: tmp, gate, setup, app: router(state.clone()), state, revealed }
}

struct Answer {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

async fn send(app: &Router, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&str>) -> Answer {
    let mut r = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    if body.is_some() {
        r = r.header("content-type", "application/json");
    }
    let req = r.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, b) = res.into_parts();
    Answer { status: parts.status.as_u16(), headers: parts.headers, body: b.collect().await.unwrap().to_bytes().to_vec() }
}

const LOCAL: [(&str, &str); 1] = [("host", "127.0.0.1:8790")];

fn header<'a>(a: &'a Answer, k: &str) -> Option<&'a str> {
    a.headers.get(k).and_then(|v| v.to_str().ok())
}

fn json(a: &Answer) -> Value {
    serde_json::from_slice(&a.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&a.body)))
}

/// Compare one recorded answer; key order counts (bodies compared as re-serialised strings).
fn compare(case: &Value, got: &Answer) -> Vec<String> {
    let want = &case["response"];
    let mut bad = Vec::new();
    if want["status"].as_u64() != Some(got.status as u64) {
        bad.push(format!("status {} (python {})", got.status, want["status"]));
    }
    for h in HEADERS {
        let w = want["headers"][h].as_str();
        if header(got, h) != w {
            bad.push(format!("{h}: {:?} (python {w:?})", header(got, h)));
        }
    }
    if let Some(w) = want.get("json") {
        match serde_json::from_slice::<Value>(&got.body) {
            Ok(g) if g.to_string() == w.to_string() => {}
            Ok(g) => bad.push(format!("body\n  rust   {g}\n  python {w}")),
            Err(_) => bad.push(format!("body not JSON: {}", String::from_utf8_lossy(&got.body))),
        }
    } else if let Some(w) = want.get("text") {
        if String::from_utf8_lossy(&got.body) != w.as_str().unwrap() {
            bad.push(format!("text\n  rust   {:?}\n  python {w}", String::from_utf8_lossy(&got.body)));
        }
    } else if let Some(w) = want.get("bytes") {
        if w.as_u64() != Some(got.body.len() as u64) {
            bad.push(format!("{} bytes (python {w})", got.body.len()));
        }
    } else if let Some(w) = want.get("shape") {
        let g = serde_json::from_slice::<Value>(&got.body).unwrap_or(Value::Null);
        let first = if let Value::Array(a) = &g { a.first().cloned().unwrap_or(Value::Null) } else { g };
        let keys: Vec<Value> = first.as_object().map(|o| o.keys().map(|k| Value::from(k.as_str())).collect()).unwrap_or_default();
        if &Value::Array(keys.clone()) != w {
            bad.push(format!("shape {keys:?} (python {w})"));
        }
    } else {
        bad.push("recorded case has no body".into());
    }
    bad
}

async fn replay(golden: &Value) -> (usize, Vec<String>) {
    let step = golden["clock_step"].as_f64().unwrap();
    let e = build(Some(((golden["clock_start"].as_f64().unwrap(), step), (golden["engine_clock_start"].as_f64().unwrap(), step))), None, true);
    let mut failures = Vec::new();
    let mut n = 0;
    for phase in golden["phases"].as_array().unwrap() {
        let masked = phase["masked"] == true;
        for step in phase["setup"].as_array().unwrap() {
            run_setup(&e, step);
        }
        for case in phase["cases"].as_array().unwrap() {
            let path = case["path"].as_str().unwrap();
            if let Some(w) = case.get("wait_before") {
                wait_for(&e.app, w).await;
            }
            let shape_only = !masked && SHAPE_ONLY.iter().any(|(p, _)| *p == path.split('?').next().unwrap());
            assert_eq!(shape_only, case["response"].get("shape").is_some(), "{path}: recorder and test disagree on shape-only");
            let headers: Vec<(String, String)> =
                case["headers"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
            let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let body = case["body"].as_object().map(|_| case["body"].to_string());
            let mut got = send(&e.app, case["method"].as_str().unwrap(), path, &hs, body.as_deref()).await;
            if masked && let Ok(v) = serde_json::from_slice::<Value>(&got.body) {
                got.body = serde_json::to_vec(&mask(&v)).unwrap();
            }
            for b in compare(case, &got) {
                failures.push(format!("{} {} {:?}: {b}", case["method"], path, case["headers"]));
            }
            n += 1;
        }
    }
    (n, failures)
}

/// record_http.py `wait_for`: poll a GET until a key has the value (`"<set>"`: any but null).
async fn wait_for(app: &Router, w: &Value) {
    for _ in 0..1000 {
        let a = send(app, "GET", w["path"].as_str().unwrap(), &LOCAL, None).await;
        let mut v = serde_json::from_slice::<Value>(&a.body).unwrap_or(Value::Null);
        for k in w["key"].as_str().unwrap().split('.') {
            v = v.get(k).cloned().unwrap_or(Value::Null);
        }
        if (w["value"] == "<set>" && !v.is_null()) || v == w["value"] {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {w}");
}

/// record_http.py `run_setup`, through the library and the gate folder.
fn run_setup(e: &Engine, step: &Value) {
    let lib = &e.setup;
    let flip = |name: &str, on: bool| if on { std::fs::write(e.gate.join(name), "").unwrap() } else { std::fs::remove_file(e.gate.join(name)).unwrap() };
    if let Some(g) = step["gate"].as_str() {
        return flip("go", g == "open");
    }
    if let Some(f) = step["fail"].as_bool() {
        return flip("fail", f);
    }
    if let Some(t) = step["add_take"].as_array() {
        let (gid, ci, take) = (t[0].as_str().unwrap(), t[1].as_u64().unwrap() as usize, t[2].clone());
        let mut d = lib.get_generation(gid).unwrap().unwrap();
        let text = d["chunks"][ci]["text"].clone();
        d["chunks"][ci].as_object_mut().unwrap().insert("takes".into(), serde_json::json!([{"take": take, "text": text}]));
        lib.put_generation(&mut d).unwrap();
        return;
    }
    if let Some(gid) = step["soft_delete"].as_str() {
        assert!(lib.set_generation_deleted(gid, true).unwrap().is_some());
        return;
    }
    let s = step["set_status"].as_array().expect("known setup step");
    let (table, id, status) = (s[0].as_str().unwrap(), s[1].as_str().unwrap(), s[2].clone());
    if table == "generations" {
        let mut d = lib.get_generation(id).unwrap().unwrap();
        d.insert("status".into(), status);
        lib.put_generation(&mut d).unwrap();
    } else {
        let mut d = lib.get_profile(id).unwrap().unwrap();
        d.insert("status".into(), status);
        lib.put_profile(&mut d).unwrap();
    }
}

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(wt().join("rust/golden/http.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn parity_with_python() {
    let g = golden();
    let (n, failures) = replay(&g).await;
    assert_eq!(n, 275, "number of recorded cases");
    assert_eq!(MASKED.len(), 6);
    assert!(failures.is_empty(), "{} of {n} cases differ from Python:\n{}", failures.len(), failures.join("\n"));
}

/// The comparison is live: a recorded body with two keys swapped must fail.
#[tokio::test]
async fn parity_catches_key_order() {
    let mut g = golden();
    let case = g["phases"][0]["cases"].as_array_mut().unwrap().iter_mut().find(|c| c["path"] == "/api/health").unwrap();
    let body = case["response"]["json"].as_object_mut().unwrap();
    let ok = body.shift_remove("ok").unwrap();
    body.insert("ok".into(), ok);
    let (_, failures) = replay(&g).await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("/api/health"));
}

/// The job comparison is live too: a recorded job answer without `phase` must fail, and only there.
#[tokio::test]
async fn parity_catches_a_job_change() {
    let mut g = golden();
    let phase = g["phases"].as_array_mut().unwrap().iter_mut().find(|p| p["name"] == "g3").unwrap();
    let case = phase["cases"].as_array_mut().unwrap().iter_mut().find(|c| c["path"].as_str().unwrap().starts_with("/api/jobs/")).unwrap();
    assert!(case["response"]["json"].as_object_mut().unwrap().shift_remove("phase").is_some());
    let (_, failures) = replay(&g).await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("/api/jobs/"), "{failures:?}");
}

#[tokio::test]
async fn guards_run_before_routing() {
    let e = engine(None, None);
    let a = send(&e.app, "GET", "/api/no/such/route", &[("host", "evil.example")], None).await;
    assert_eq!((a.status, json(&a)["detail"].as_str().unwrap()), (403, "TakeTwo only answers this computer."));
    let a = send(&e.app, "DELETE", "/api/no/such/%2e%2e", &LOCAL, None).await;
    assert_eq!((a.status, json(&a)["detail"].as_str().unwrap()), (400, "Invalid path."));
    let a = send(&e.app, "POST", "/api/no/such/route", &[("host", "localhost:8790"), ("origin", "https://evil.example")], None).await;
    assert_eq!(a.status, 403);
    let a = send(&e.app, "GET", "/api/no/such/route", &LOCAL, None).await;
    assert_eq!((a.status, json(&a)["detail"].as_str().unwrap()), (404, "Not Found"));
    // CORS for the dev UI, as Python's CORSMiddleware.
    let a = send(&e.app, "GET", "/api/health", &[("host", "127.0.0.1:8790"), ("origin", "http://localhost:5173")], None).await;
    assert_eq!(header(&a, "access-control-allow-origin"), Some("http://localhost:5173"));
}

#[tokio::test]
async fn origin_is_checked_on_put_settings() {
    let e = engine(None, None);
    let body = r#"{"seed": 7}"#;
    for (origin, want) in [("https://evil.example", 403), ("http://localhost:8790", 403), ("http://127.0.0.1:8790", 200)] {
        let a = send(&e.app, "PUT", "/api/settings", &[("host", "127.0.0.1:8790"), ("origin", origin)], Some(body)).await;
        assert_eq!(a.status, want, "{origin}");
        let saved = e.state.lib.settings_path().exists();
        assert_eq!(saved, want == 200, "{origin}: settings written only when allowed");
    }
    let a = send(&e.app, "PUT", "/api/settings", &LOCAL, Some("[1]")).await;
    assert_eq!(a.status, 422);
}

#[tokio::test]
async fn range_request_on_a_wav() {
    let e = engine(None, None);
    let file = std::fs::read(e.state.lib.generations_dir().join("ctclean01/final.wav")).unwrap();
    let a = send(&e.app, "GET", "/api/generations/ctclean01/audio", &[("host", "127.0.0.1:8790"), ("range", "bytes=40-49")], None).await;
    assert_eq!(a.status, 206);
    assert_eq!(a.body, file[40..50]);
    assert_eq!(header(&a, "content-range"), Some(format!("bytes 40-49/{}", file.len()).as_str()));
    let a = send(&e.app, "GET", "/api/generations/ctclean01/audio", &LOCAL, None).await;
    assert_eq!((a.status, a.body.len()), (200, file.len()));
    assert_eq!(header(&a, "accept-ranges"), Some("bytes"));
    assert!(header(&a, "etag").is_some() && header(&a, "last-modified").is_some());
}

fn ui_folder(root: &Path) -> PathBuf {
    let ui = root.join("ui");
    std::fs::create_dir_all(ui.join("assets")).unwrap();
    std::fs::write(ui.join("index.html"), "<!doctype html><title>TakeTwo</title>").unwrap();
    std::fs::write(ui.join("assets/app.js"), "console.log(1)").unwrap();
    std::fs::write(ui.join("favicon.svg"), "<svg/>").unwrap();
    std::fs::write(root.join("secret.txt"), "outside the UI folder").unwrap();
    ui
}

#[tokio::test]
async fn spa_serves_only_files_inside_the_ui_folder() {
    let outside = tempfile::tempdir().unwrap();
    let e = engine(None, Some(ui_folder(outside.path())));
    let index = |a: &Answer| a.status == 200 && a.body.starts_with(b"<!doctype html>") && header(a, "cache-control") == Some("no-cache");
    let a = send(&e.app, "GET", "/", &LOCAL, None).await;
    assert!(index(&a));
    assert_eq!(header(&a, "content-type"), Some("text/html; charset=utf-8"));
    let a = send(&e.app, "GET", "/voices/ctvoice01", &LOCAL, None).await;
    assert!(index(&a), "client-side route");
    let a = send(&e.app, "GET", "/assets/app.js", &LOCAL, None).await;
    assert_eq!((a.status, a.body.as_slice()), (200, b"console.log(1)".as_slice()));
    assert_eq!(header(&a, "content-type"), Some("text/javascript; charset=utf-8"));
    let a = send(&e.app, "GET", "/favicon.svg", &LOCAL, None).await;
    assert_eq!((a.status, header(&a, "content-type")), (200, Some("image/svg+xml")));
    for escape in ["/%2e%2e/secret.txt", "/../secret.txt", "/%2e%2e%2fsecret.txt"] {
        let a = send(&e.app, "GET", escape, &LOCAL, None).await;
        assert!(index(&a), "{escape} must answer index.html, not the file outside");
    }
    let a = send(&e.app, "GET", "/assets/%2e%2e/index.html", &LOCAL, None).await;
    assert_eq!(a.status, 404, "assets mount stays inside assets/");
    let a = send(&e.app, "GET", "/api/health", &LOCAL, None).await;
    assert_eq!(json(&a)["name"], "TakeTwo", "API routes win over the UI");
    let a = send(&e.app, "POST", "/somewhere", &LOCAL, None).await;
    assert_eq!(a.status, 405);
}

#[tokio::test]
async fn routes_outside_1_0_answer_501_after_their_id_check() {
    let e = engine(None, None);
    for (m, p) in [("POST", "/api/profiles/upgrade-all"), ("GET", "/api/transcripts"), ("POST", "/api/designs"), ("DELETE", "/api/profiles/ctvoice01")] {
        let a = send(&e.app, m, p, &LOCAL, None).await;
        assert_eq!((a.status, json(&a)), (501, serde_json::json!({"detail": "Not in TakeTwo 1.0 yet."})), "{m} {p}");
    }
    let a = send(&e.app, "DELETE", "/api/transcripts/bad.id", &LOCAL, None).await;
    assert_eq!(a.status, 400);
    let a = send(&e.app, "GET", "/api/catalog/library", &LOCAL, None).await;
    assert_eq!(a.status, 200);
}

/// Every file under the library (path and bytes) and every stored document.
fn snapshot(e: &Engine) -> (Vec<(String, Vec<u8>)>, Vec<Value>, Vec<Value>) {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|d| d.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if p.is_dir() {
                out.push((rel + "/", Vec::new()));
                walk(root, &p, out);
            } else if !rel.starts_with("voiceforge.db") {
                out.push((rel, std::fs::read(&p).unwrap())); // the database is compared through its documents
            } else {
                out.push((rel, Vec::new()));
            }
        }
    }
    let mut files = Vec::new();
    walk(e.state.lib.dir(), e.state.lib.dir(), &mut files);
    let docs = |v: Vec<library::Doc>| v.into_iter().map(Value::Object).collect::<Vec<_>>();
    (files, docs(e.state.lib.list_generations().unwrap()), docs(e.state.lib.list_profiles().unwrap()))
}

/// Without a renderer, the paths that would start a job answer 501 after their checks and change nothing:
/// no folder, no file, no document. (A speed change re-times without a renderer: `speed_change_retimes_over_http`.)
#[tokio::test]
async fn job_paths_answer_501_and_change_nothing() {
    let e = engine(None, None);
    // A kept earlier take, so the take switch passes its checks (the fixture keeps none).
    let mut g = e.setup.get_generation("ctreview01").unwrap().unwrap();
    g["chunks"][1].as_object_mut().unwrap().insert("takes".into(), serde_json::json!([{"take": 1, "text": "Part two ends early."}]));
    e.setup.put_generation(&mut g).unwrap();
    let before = snapshot(&e);
    let cases: [(&str, &str, Option<&str>); 7] = [
        ("POST", "/api/generations", Some(r#"{"profile_id": "ctvoice01", "text": "Hello there. A new narration."}"#)),
        ("POST", "/api/generations", Some(r#"{"profile_id": "ctvoice01", "text": "Hi.", "engine": "qwen3-tts-1.7b", "speed": 2, "title": "T"}"#)),
        ("POST", "/api/generations/ctpending01/resume", None),
        ("POST", "/api/generations/ctreview01/chunks/1/regenerate", Some("{}")),
        ("POST", "/api/generations/ctreview01/chunks/1/regenerate", Some(r#"{"text": "Part two, said again."}"#)),
        ("POST", "/api/generations/ctreview01/chunks/1/select-take", Some(r#"{"take": 1}"#)),
        ("POST", "/api/generations/ctreview01/fix-remaining", None),
    ];
    for (m, p, body) in cases {
        let a = send(&e.app, m, p, &[("host", "127.0.0.1:8790"), ("origin", "http://127.0.0.1:8790")], body).await;
        assert_eq!((a.status, json(&a)), (501, serde_json::json!({"detail": "Not in TakeTwo 1.0 yet."})), "{m} {p} {body:?}");
        assert!(snapshot(&e) == before, "{m} {p} changed the library");
    }
    // An equal speed is ignored, so the edit goes through.
    let a = send(&e.app, "PATCH", "/api/generations/ctclean01", &LOCAL, Some(r#"{"speed": 1.2505, "title": "Saved"}"#)).await;
    assert_eq!((a.status, json(&a)["title"].as_str()), (200, Some("Saved")));
}

/// main.py `patch_generation` with a new speed: 400 without `master.wav` (nothing changed); otherwise the speed
/// (clamped to 0.7-1.3), timeline and duration change, final.wav and final.mp3 are re-made, and the MP3 downloads.
#[tokio::test]
async fn speed_change_retimes_over_http() {
    let e = engine(None, None);
    let before = snapshot(&e);
    let a = send(&e.app, "PATCH", "/api/generations/ctclean01", &LOCAL, Some(r#"{"speed": 1.2, "title": "Not saved"}"#)).await;
    assert_eq!((a.status, json(&a)), (400, serde_json::json!({"detail": "Re-render this generation to change its speed."})));
    assert!(snapshot(&e) == before, "a refused speed change changes nothing");
    let ok = std::process::Command::new(taketwo_core::audio::ffmpeg::program()).arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        println!("speed_change_retimes_over_http: ffmpeg is not on PATH; the re-timed export cannot be made here");
        return;
    }
    let gdir = e.state.lib.generations_dir().join("ctclean01");
    std::fs::copy(gdir.join("final.wav"), gdir.join("master.wav")).unwrap();
    let old = e.state.lib.get_generation("ctclean01").unwrap().unwrap();
    let a = send(&e.app, "PATCH", "/api/generations/ctclean01", &LOCAL, Some(r#"{"speed": 2, "title": "Faster"}"#)).await;
    assert_eq!(a.status, 200, "{}", String::from_utf8_lossy(&a.body));
    let g = json(&a);
    let k = old["speed"].as_f64().unwrap_or(1.0) / 1.3;
    assert_eq!((g["speed"].as_f64(), g["title"].as_str()), (Some(1.3), Some("Faster")), "clamped to 1.3");
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    assert_eq!(g["timeline"][0]["start"].as_f64(), Some(r3(old["timeline"][0]["start"].as_f64().unwrap() * k)));
    assert!(gdir.join("final.mp3").is_file());
    let a = send(&e.app, "GET", "/api/generations/ctclean01/audio?fmt=mp3", &LOCAL, None).await;
    assert_eq!(a.status, 200);
    assert_eq!(a.headers["content-type"], "audio/mpeg");
    assert_eq!(a.body, std::fs::read(gdir.join("final.mp3")).unwrap());
    let a = send(&e.app, "PATCH", "/api/generations/ctclean01", &LOCAL, Some(r#"{"speed": 1.3}"#)).await;
    assert_eq!(a.status, 200, "an equal speed is not a change");
}

#[tokio::test]
async fn reveal_calls_the_injected_opener() {
    let e = engine(None, None);
    let gens = e.state.lib.generations_dir();
    let reveal = |gid: &str| format!("/api/generations/{gid}/reveal");
    let a = send(&e.app, "POST", &reveal("ctclean01"), &LOCAL, None).await;
    assert_eq!((a.status, json(&a)["path"].as_str().map(PathBuf::from)), (200, Some(gens.join("ctclean01/final.wav"))));
    let a = send(&e.app, "POST", &reveal("ctpending01"), &LOCAL, None).await;
    assert_eq!((a.status, json(&a)["path"].as_str().map(PathBuf::from)), (200, Some(gens.join("ctpending01"))), "no audio: the folder");
    let a = send(&e.app, "POST", "/api/generations/reveal", &LOCAL, None).await;
    assert_eq!((a.status, json(&a)["path"].as_str().map(PathBuf::from)), (200, Some(gens.clone())));
    assert_eq!(*e.revealed.lock().unwrap(), vec![gens.join("ctclean01/final.wav"), gens.join("ctpending01"), gens.clone()]);
    assert_eq!(send(&e.app, "DELETE", "/api/generations/cttrash01", &LOCAL, None).await.status, 200);
    let a = send(&e.app, "POST", &reveal("cttrash01"), &LOCAL, None).await;
    assert_eq!((a.status, json(&a)["detail"].as_str()), (404, Some("Not found")), "deleted narration");
    std::fs::remove_dir_all(gens.join("ctpending01")).unwrap(); // a temporary library
    let a = send(&e.app, "POST", &reveal("ctpending01"), &LOCAL, None).await;
    assert_eq!((a.status, json(&a)["detail"].as_str()), (404, Some("This narration has no files yet")));
    assert_eq!(e.revealed.lock().unwrap().len(), 3, "refusals open nothing");
}

#[tokio::test]
async fn other_websites_cannot_delete_or_edit() {
    let e = engine(None, None);
    let before = snapshot(&e);
    for origin in ["https://evil.example", "null", "http://127.0.0.1:3000"] {
        let h = [("host", "127.0.0.1:8790"), ("origin", origin)];
        assert_eq!(send(&e.app, "DELETE", "/api/generations/cttrash01", &h, None).await.status, 403, "{origin}");
        assert_eq!(send(&e.app, "PATCH", "/api/generations/ctclean01", &h, Some(r#"{"title": "Hacked", "favorite": true}"#)).await.status, 403);
        assert_eq!(send(&e.app, "POST", "/api/generations/ctaccept01/chunks/1/accept", &h, None).await.status, 403);
    }
    assert!(snapshot(&e) == before, "a refused write changed the library");
}

fn engine_bin(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_taketwo-engine"));
    c.env_remove("TAKETWO_DATA").env_remove("VOICEFORGE_DATA").env("HOME", home).current_dir(home);
    c
}

#[test]
fn serve_refuses_without_taketwo_data_or_on_franz_port() {
    let tmp = tempfile::tempdir().unwrap();
    let out = engine_bin(tmp.path()).args(["serve"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to start"));
    let data = tmp.path().join("library");
    // The argument check refuses before the library is opened or a port is bound.
    let out = engine_bin(tmp.path()).env("TAKETWO_DATA", &data).args(["serve", "--port", "8766"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("reserved for the live app"));
    assert!(!data.exists());
}
