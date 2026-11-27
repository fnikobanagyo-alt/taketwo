//! The engine's HTTP API in-process (tower `oneshot`, no port bound), on a temporary library seeded from
//! contract/fixture.json. `parity_with_python` replays rust/golden/http.json, recorded from the Python
//! engine by rust/golden/record_http.py.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use taketwo_core::library::Library;
use taketwo_engine::api::{AppState, router};
use taketwo_engine::seed;
use tower::ServiceExt;

const HEADERS: [&str; 5] = ["content-type", "content-disposition", "cache-control", "x-preview-kind", "content-range"];
/// Answers that can't match the Python app; only their shape (top-level keys) is compared.
const SHAPE_ONLY: [(&str, &str); 3] = [
    ("/api/engines", "Python lists its MLX engines; TakeTwo 1.0 has the one Qwen3 engine (keys of the first item compared, not the count)"),
    ("/api/integrations", "paths of this computer and the Python script vs the Rust taketwo-mcp program"),
    ("/api/diagnostics", "process id, start time and Python source hash"),
];

fn wt() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

struct Engine {
    _tmp: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

/// A seeded temporary library. `clock` pins the library clock as record_http.py pins db.py's.
fn engine(clock: Option<(f64, f64)>, ui: Option<PathBuf>) -> Engine {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = Library::open(&tmp.path().join("library")).unwrap();
    if let Some((start, step)) = clock {
        let t = Mutex::new(start);
        lib = lib.with_clock(move || {
            let mut t = t.lock().unwrap();
            *t += step;
            *t
        });
    }
    seed::seed_into(&lib, &wt().join("contract/fixture.json")).unwrap();
    let state = Arc::new(AppState { lib, ui, started: 1.0 });
    Engine { _tmp: tmp, app: router(state.clone()), state }
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
    let e = engine(Some((golden["clock_start"].as_f64().unwrap(), golden["clock_step"].as_f64().unwrap())), None);
    let mut failures = Vec::new();
    let mut n = 0;
    for phase in golden["phases"].as_array().unwrap() {
        for step in phase["setup"].as_array().unwrap() {
            let gid = step["soft_delete"].as_str().expect("known setup step");
            assert!(e.state.lib.set_generation_deleted(gid, true).unwrap().is_some());
        }
        for case in phase["cases"].as_array().unwrap() {
            let path = case["path"].as_str().unwrap();
            let shape_only = SHAPE_ONLY.iter().any(|(p, _)| *p == path.split('?').next().unwrap());
            assert_eq!(shape_only, case["response"].get("shape").is_some(), "{path}: recorder and test disagree on shape-only");
            let headers: Vec<(String, String)> =
                case["headers"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
            let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let body = case["body"].as_object().map(|_| case["body"].to_string());
            let got = send(&e.app, case["method"].as_str().unwrap(), path, &hs, body.as_deref()).await;
            for b in compare(case, &got) {
                failures.push(format!("{} {} {:?}: {b}", case["method"], path, case["headers"]));
            }
            n += 1;
        }
    }
    (n, failures)
}

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(wt().join("rust/golden/http.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn parity_with_python() {
    let g = golden();
    let (n, failures) = replay(&g).await;
    assert_eq!(n, 152, "number of recorded cases");
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
async fn write_routes_answer_501_after_their_id_check() {
    let e = engine(None, None);
    for (m, p) in [("POST", "/api/generations"), ("DELETE", "/api/generations/ctclean01"), ("PATCH", "/api/generations/ctclean01"),
                   ("POST", "/api/generations/ctaccept01/chunks/1/accept"), ("POST", "/api/generations/reveal"),
                   ("POST", "/api/profiles/upgrade-all"), ("GET", "/api/transcripts"), ("POST", "/api/designs")] {
        let a = send(&e.app, m, p, &LOCAL, None).await;
        assert_eq!((a.status, json(&a)), (501, serde_json::json!({"detail": "Not in TakeTwo 1.0 yet."})), "{m} {p}");
    }
    let a = send(&e.app, "DELETE", "/api/generations/bad.id", &LOCAL, None).await;
    assert_eq!(a.status, 400);
    assert_eq!(e.state.lib.list_generations().unwrap().len(), 5, "nothing deleted");
    let a = send(&e.app, "GET", "/api/catalog/library", &LOCAL, None).await;
    assert_eq!(a.status, 200);
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
