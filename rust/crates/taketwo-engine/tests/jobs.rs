//! The worker and the stub renderer in-process (tower `oneshot`, no port bound), on a temporary library
//! seeded from contract/fixture.json. The parity of job answers with Python is in http.rs (phases g1-g4,
//! and its mutation check `parity_catches_a_job_change`).

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use taketwo_core::library::{self, Library};
use taketwo_engine::api::{AppState, router};
use taketwo_engine::render::StubRenderer;
use taketwo_engine::{seed, worker};
use tower::ServiceExt;

struct Engine {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    gate: PathBuf,
    app: Router,
}

fn wt() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn engine() -> Engine {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, gate) = (tmp.path().join("library"), tmp.path().join("gate"));
    std::fs::create_dir(&gate).unwrap();
    let lib = Library::open(&dir).unwrap();
    seed::seed_into(&lib, &wt().join("contract/fixture.json")).unwrap();
    library::startup_repairs(&lib).unwrap();
    let stub = Arc::new(StubRenderer { gate: Some(gate.clone()) });
    let state = Arc::new(AppState::new(lib, None, 1.0, Box::new(|_: &Path| {}), Some(stub), worker::random_ids()));
    Engine { _tmp: tmp, dir, gate, app: router(state) }
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (u16, Vec<u8>) {
    let mut r = Request::builder().method(method).uri(path).header("host", "127.0.0.1:8790");
    if body.is_some() {
        r = r.header("content-type", "application/json");
    }
    let req = r.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    (status, res.into_body().collect().await.unwrap().to_bytes().to_vec())
}

async fn get(app: &Router, path: &str) -> Value {
    let (s, b) = call(app, "GET", path, None).await;
    assert_eq!(s, 200, "{path}: {}", String::from_utf8_lossy(&b));
    serde_json::from_slice(&b).unwrap()
}

async fn create(app: &Router, text: &str) -> (String, String) {
    let (s, b) = call(app, "POST", "/api/generations", Some(json!({"profile_id": "ctvoice01", "text": text}))).await;
    assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
    let g: Value = serde_json::from_slice(&b).unwrap();
    (g["id"].as_str().unwrap().to_string(), g["job"]["id"].as_str().unwrap().to_string())
}

/// Poll until `f` holds (10 s at most).
async fn until(app: &Router, path: &str, f: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..500 {
        let v = get(app, path).await;
        if f(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{path} never reached the state: {}", get(app, path).await);
}

fn finished(j: &Value) -> bool {
    j.get("finished").is_some_and(|f| !f.is_null())
}

fn open_gate(e: &Engine) {
    std::fs::write(e.gate.join("go"), "").unwrap();
}

#[tokio::test]
async fn two_jobs_run_in_order_never_together() {
    let e = engine();
    let (g1, j1) = create(&e.app, "The first narration of two.").await;
    let (g2, j2) = create(&e.app, "The second narration of two.").await;
    until(&e.app, &format!("/api/generations/{g1}"), |g| g["status"] == "running").await;
    let st = get(&e.app, "/api/status").await;
    assert_eq!((st["running"]["id"].as_str(), st["queued"].as_i64()), (Some(j1.as_str()), Some(1)));
    assert_eq!(get(&e.app, &format!("/api/jobs/{j2}")).await["status"], "queued", "the second waits");
    assert_eq!(get(&e.app, &format!("/api/generations/{g2}")).await["queue"], json!({"ahead": 1, "waiting_for": ["The first narration of two."]}));
    open_gate(&e);
    let a = until(&e.app, &format!("/api/jobs/{j1}"), finished).await;
    let b = until(&e.app, &format!("/api/jobs/{j2}"), finished).await;
    assert_eq!((a["status"].as_str(), b["status"].as_str()), (Some("done"), Some("done")));
    assert!(b["started"].as_f64().unwrap() >= a["finished"].as_f64().unwrap(), "the second started after the first finished");
    assert_eq!(get(&e.app, &format!("/api/generations/{g2}")).await["status"], "done");
}

#[tokio::test]
async fn cancel_while_queued_and_while_running() {
    let e = engine();
    let (g1, j1) = create(&e.app, "This one runs and is cancelled.").await;
    let (g2, j2) = create(&e.app, "This one waits and is cancelled.").await;
    until(&e.app, &format!("/api/generations/{g1}"), |g| g["status"] == "running").await;
    assert_eq!(call(&e.app, "POST", &format!("/api/generations/{g2}/cancel"), None).await.0, 200);
    let g = get(&e.app, &format!("/api/generations/{g2}")).await;
    assert_eq!((g["status"].as_str(), g["message"].as_str(), &g["job"]), (Some("cancelled"), Some("Cancelled before it started"), &Value::Null));
    assert_eq!(get(&e.app, &format!("/api/jobs/{j2}")).await["status"], "cancelled", "at once, before the loop reaches it");
    assert_eq!(call(&e.app, "POST", &format!("/api/generations/{g1}/cancel"), None).await.0, 200);
    let a = until(&e.app, &format!("/api/jobs/{j1}"), finished).await;
    assert_eq!(a["status"], "cancelled");
    let g = get(&e.app, &format!("/api/generations/{g1}")).await;
    assert_eq!((g["status"].as_str(), g["chunks"][0]["status"].as_str()), (Some("cancelled"), Some("pending")));
    let b = until(&e.app, &format!("/api/jobs/{j2}"), finished).await;
    assert!(b["started"].is_null() && !b["cancel_acknowledged_at"].is_null(), "{b}");
    assert!(!e.dir.join("generations").join(&g1).join("final.wav").exists(), "nothing rendered");
}

#[tokio::test]
async fn the_fail_switch_marks_job_and_narration() {
    let e = engine();
    std::fs::write(e.gate.join("fail"), "").unwrap();
    let (g, j) = create(&e.app, "This one fails.").await;
    let job = until(&e.app, &format!("/api/jobs/{j}"), finished).await;
    assert_eq!((job["status"].as_str(), job["error_type"].as_str(), job["error"].as_str()), (Some("error"), Some("RuntimeError"), Some("Stub render failed.")));
    let n = get(&e.app, &format!("/api/generations/{g}")).await;
    assert_eq!((n["status"].as_str(), n["message"].as_str(), n["chunks"][0]["status"].as_str()), (Some("error"), Some("Stub render failed."), Some("pending")));
}

#[tokio::test]
async fn a_finished_stub_render_answers_timings_subtitles_and_audio() {
    let e = engine();
    open_gate(&e);
    let (g, j) = create(&e.app, "One sentence here. And a second sentence there.").await;
    until(&e.app, &format!("/api/jobs/{j}"), finished).await;
    let n = get(&e.app, &format!("/api/generations/{g}")).await;
    assert_eq!((n["status"].as_str(), n["outcome"].is_null()), (Some("done"), false), "{n}");
    assert!(get(&e.app, &format!("/api/generations/{g}/timings")).await.is_object());
    let (s, srt) = call(&e.app, "GET", &format!("/api/generations/{g}/subtitles?fmt=srt"), None).await;
    assert_eq!(s, 200);
    assert!(String::from_utf8_lossy(&srt).starts_with("1\n00:00:00,000 --> "), "{}", String::from_utf8_lossy(&srt));
    let (s, wav) = call(&e.app, "GET", &format!("/api/generations/{g}/audio"), None).await;
    assert_eq!((s, &wav[..4]), (200, b"RIFF".as_slice()));
    assert_eq!(call(&e.app, "GET", &format!("/api/generations/{g}/chunks/0/audio"), None).await.0, 200);
}

#[tokio::test]
async fn delete_cancels_the_running_job() {
    let e = engine();
    let (g, j) = create(&e.app, "This one is deleted while it runs.").await;
    until(&e.app, &format!("/api/generations/{g}"), |n| n["status"] == "running").await;
    assert_eq!(call(&e.app, "DELETE", &format!("/api/generations/{g}"), None).await.0, 200);
    let job = until(&e.app, &format!("/api/jobs/{j}"), finished).await;
    assert_eq!(job["status"], "cancelled");
    assert_eq!(call(&e.app, "GET", &format!("/api/generations/{g}"), None).await.0, 404, "the deleted mark stays");
}

#[tokio::test]
async fn restart_repair_opens_a_running_narration_as_interrupted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("library");
    {
        let lib = Library::open(&dir).unwrap();
        seed::seed_into(&lib, &wt().join("contract/fixture.json")).unwrap();
        let mut g = lib.get_generation("ctpending01").unwrap().unwrap();
        g.insert("status".into(), "running".into());
        g["chunks"][0]["status"] = "running".into();
        lib.put_generation(&mut g).unwrap(); // as an engine that stopped mid-render leaves it
    }
    let lib = Library::open(&dir).unwrap();
    library::startup_repairs(&lib).unwrap();
    let state = Arc::new(AppState::new(lib, None, 1.0, Box::new(|_: &Path| {}), None, worker::random_ids()));
    let app = router(state);
    let g = get(&app, "/api/generations/ctpending01").await;
    assert_eq!((g["status"].as_str(), g["message"].as_str()), (Some("interrupted"), Some(library::INTERRUPTED_MESSAGE)));
    assert_eq!(g["chunks"][0]["status"], "pending");
    assert_eq!(g["job"], Value::Null);
}
