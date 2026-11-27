//! Voice creation over HTTP (step S17), in-process with the stub renderer's models (a transcript made from the
//! sound regions, one fixed embedding, tones as the voice; no model loads). Clips come from macOS `say`; a test
//! prints its reason and checks nothing where `say` or ffmpeg is missing.
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use taketwo_core::library::Library;
use taketwo_core::reference::STUDIO_FALLBACK_TIP;
use taketwo_engine::api::{AppState, router};
use taketwo_engine::render::{Renderer, StubRenderer};
use tower::ServiceExt;

const LOCAL: [(&str, &str); 1] = [("host", "127.0.0.1:8790")];
const SAY_TEXT: &str = "The morning was quiet and the river ran slowly past the old mill. \
    We walked along the bank and talked about the long winter ahead. \
    Later the clouds came in from the west, and a light rain began to fall. \
    By evening the fields were green again, and the birds were singing in the trees.";

/// One models folder for this test binary with an (empty) Qwen folder, so the stub makes previews.
/// Set once, before any engine starts; every test calls it first.
fn models() {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("taketwo-profiles-models-{}", std::process::id()));
        std::fs::create_dir_all(d.join(taketwo_voice::qwen::MODEL_DIR)).unwrap();
        // SAFETY: set once under the OnceLock before this binary's tests start an engine or read the variable
        unsafe { std::env::set_var("TAKETWO_MODELS", &d) };
        d
    });
}

fn tool(name: &str, arg: &str) -> bool {
    std::process::Command::new(name).arg(arg).output().is_ok()
}

/// A `say` clip of `text` (AIFF), or None with the reason printed.
fn say(dir: &Path, test: &str, text: &str) -> Option<PathBuf> {
    if !tool("ffmpeg", "-version") || !tool("ffprobe", "-version") {
        println!("{test}: ffmpeg or ffprobe is not on PATH; voice creation decodes with them, so this test checks nothing here");
        return None;
    }
    let out = dir.join("clip.aiff");
    match std::process::Command::new("say").arg("-o").arg(&out).arg(text).status() {
        Ok(s) if s.success() && out.exists() => Some(out),
        _ => {
            println!("{test}: macOS `say` is not available; no clip to upload, so this test checks nothing here");
            None
        }
    }
}

struct Engine {
    _tmp: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn engine() -> Engine {
    models();
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open(&tmp.path().join("library")).unwrap();
    let r: Arc<dyn Renderer> = Arc::new(StubRenderer { gate: None });
    let state = Arc::new(AppState::new(lib, None, 1.0, Box::new(|_: &Path| {}), Some(r), taketwo_engine::worker::random_ids()));
    Engine { _tmp: tmp, app: router(state.clone()), state }
}

struct Answer {
    status: u16,
    body: Vec<u8>,
}

fn json_of(a: &Answer) -> Value {
    serde_json::from_slice(&a.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&a.body)))
}

async fn send(app: &Router, method: &str, path: &str, body: Option<Value>) -> Answer {
    let mut r = Request::builder().method(method).uri(path).header("host", LOCAL[0].1);
    if body.is_some() {
        r = r.header("content-type", "application/json");
    }
    let req = r.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, b) = res.into_parts();
    Answer { status: parts.status.as_u16(), body: b.collect().await.unwrap().to_bytes().to_vec() }
}

/// `POST /api/profiles` as a browser sends it (multipart form).
async fn upload(app: &Router, fields: &[(&str, &str)], file: Option<(&str, &[u8])>) -> Answer {
    let b = "taketwoboundary7";
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend(format!("--{b}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    if let Some((name, bytes)) = file {
        body.extend(format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend(bytes);
        body.extend(b"\r\n");
    }
    body.extend(format!("--{b}--\r\n").as_bytes());
    let req = Request::builder()
        .method("POST")
        .uri("/api/profiles")
        .header("host", LOCAL[0].1)
        .header("content-type", format!("multipart/form-data; boundary={b}"))
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, b) = res.into_parts();
    Answer { status: parts.status.as_u16(), body: b.collect().await.unwrap().to_bytes().to_vec() }
}

fn wait_idle(e: &Engine) {
    let t = Instant::now();
    while !e.state.worker.idle() {
        assert!(t.elapsed() < Duration::from_secs(120), "the worker did not finish: {:?}", e.state.worker.active_jobs());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Hold the worker with a job that waits for the returned sender (or its drop).
fn block(e: &Engine) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    let rx = std::sync::Mutex::new(rx);
    e.state.worker.submit("hold", Box::new(move |_| {
        let _ = rx.lock().unwrap().recv();
        Ok(())
    }), Some("test-hold"), "Holding", None, "unknown");
    tx
}

fn profile(e: &Engine, pid: &str) -> Value {
    Value::Object(e.state.lib.get_profile(pid).unwrap().unwrap())
}

/// A voice made from a `say` clip, run to the end (analysis and preview).
async fn ready_voice(e: &Engine, test: &str, extra: &[(&str, &str)]) -> Option<String> {
    let clip = say(e._tmp.path(), test, SAY_TEXT)?;
    let mut fields = vec![("name", "  Morning voice  ")];
    fields.extend_from_slice(extra);
    let a = upload(&e.app, &fields, Some(("Morning Take.AIFF", &std::fs::read(clip).unwrap()))).await;
    assert_eq!(a.status, 200, "{}", String::from_utf8_lossy(&a.body));
    let pid = json_of(&a)["id"].as_str().unwrap().to_string();
    wait_idle(e);
    Some(pid)
}

#[tokio::test(flavor = "multi_thread")]
async fn create_runs_analysis_then_a_preview() {
    let e = engine();
    let Some(clip) = say(e._tmp.path(), "create_runs_analysis_then_a_preview", SAY_TEXT) else { return };
    let tx = block(&e);
    let a = upload(&e.app, &[("name", "  Morning voice  ")], Some(("Morning Take.AIFF", &std::fs::read(clip).unwrap()))).await;
    assert_eq!(a.status, 200, "{}", String::from_utf8_lossy(&a.body));
    let p = json_of(&a);
    let pid = p["id"].as_str().unwrap().to_string();
    assert!(pid.len() == 12 && pid.bytes().all(|b| b.is_ascii_hexdigit()), "{pid}");
    // the library adds its own time stamps (`created`, `updated`)
    let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).filter(|k| !["created", "updated"].contains(k)).collect();
    assert_eq!(&keys[..8], ["id", "name", "language", "status", "original_file", "original_duration", "truncated", "studio"]);
    assert_eq!(keys.last(), Some(&"job"));
    assert_eq!((p["name"].as_str(), p["language"].as_str(), p["status"].as_str()), (Some("Morning voice"), Some("en"), Some("queued")));
    assert_eq!((p["original_file"].as_str(), p["truncated"].as_bool(), p["studio"].as_bool()), (Some("original.aiff"), Some(false), Some(true)));
    assert!(p["original_duration"].as_f64().unwrap() > 10.0);
    assert_eq!((p["job"]["kind"].as_str(), p["job"]["ref"].as_str(), p["job"]["label"].as_str()), (Some("analyze"), Some(pid.as_str()), Some("Analyzing voice")));
    // the preview is queued right behind the analysis
    let jobs = e.state.worker.active_jobs();
    let pv = jobs.iter().find(|j| j["kind"] == "preview").expect("a preview job is queued");
    assert_eq!((pv["ref"].as_str(), pv["label"].as_str()), (Some(format!("{pid}:preview").as_str()), Some("Making voice preview")));
    let g = send(&e.app, "GET", &format!("/api/profiles/{pid}"), None).await;
    assert_eq!(json_of(&g)["job"]["kind"], "analyze", "GET shows the voice's active job");
    drop(tx);
    wait_idle(&e);
    let p = profile(&e, &pid);
    assert_eq!(p["status"], "ready", "{p}");
    for k in ["duration", "snr_db", "denoised", "self_similarity", "usable_speech_seconds", "speech_rate_wps", "variants", "active_variant",
              "ref_text_override", "tips", "transcript", "studio", "room_echo_db", "room_echo_after_db", "pipeline_version"] {
        assert!(p.get(k).is_some(), "{k} missing: {p}");
    }
    assert_eq!((p["error"].clone(), p["studio"].clone(), p["room_echo_after_db"].clone()), (Value::Null, json!(false), Value::Null));
    assert_eq!((p["active_variant"].clone(), p["ref_text_override"].clone(), p["pipeline_version"].clone()), (json!(0), json!({}), json!(4)));
    assert_eq!(p["tips"][0], STUDIO_FALLBACK_TIP, "studio defaults to true and falls back with a tip");
    let pdir = e.state.lib.profiles_dir().join(&pid);
    for f in ["original.aiff", "clean.wav", "ref_0.wav", "fingerprint.npy", "mic.json", "analysis.json", "preview.wav"] {
        assert!(pdir.join(f).exists(), "{f}");
    }
    assert_eq!(p["preview"]["file"], "preview.wav");
    let an = send(&e.app, "GET", &format!("/api/profiles/{pid}/analysis"), None).await;
    assert_eq!(json_of(&an)["studio"], json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_too_short_upload_ends_in_the_error_state() {
    let e = engine();
    let Some(clip) = say(e._tmp.path(), "a_too_short_upload_ends_in_the_error_state", "Hi.") else { return };
    let a = upload(&e.app, &[("name", "Short")], Some(("hi.aiff", &std::fs::read(clip).unwrap()))).await;
    let pid = json_of(&a)["id"].as_str().unwrap().to_string();
    wait_idle(&e);
    let p = profile(&e, &pid);
    assert_eq!((p["status"].as_str(), p["error"].as_str()), (Some("error"), Some("Sample is too short. Upload at least 3 seconds (30 s - 5 min recommended).")));
    // no preview for a voice that is not ready, and refresh refuses it
    assert!(!e.state.lib.profiles_dir().join(&pid).join("preview.wav").exists());
    let r = send(&e.app, "POST", &format!("/api/profiles/{pid}/refresh"), None).await;
    assert_eq!((r.status, json_of(&r)), (400, json!({"detail": "This voice isn't ready to update."})));
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_form_fields_are_422_and_nothing_is_written() {
    let e = engine();
    let a = upload(&e.app, &[("language", "en"), ("denoise", "maybe")], None).await;
    assert_eq!(a.status, 422);
    assert_eq!(json_of(&a), json!({"detail": [
        {"type": "missing", "loc": ["body", "name"], "msg": "Field required", "input": null},
        {"type": "bool_parsing", "loc": ["body", "denoise"], "msg": "Input should be a valid boolean, unable to interpret input", "input": "maybe"},
        {"type": "missing", "loc": ["body", "file"], "msg": "Field required", "input": null},
    ]}));
    let a = send(&e.app, "POST", "/api/profiles", Some(json!({"name": "x"}))).await;
    assert_eq!(a.status, 422, "a JSON body has no form fields");
    assert!(e.state.lib.list_profiles().unwrap().is_empty());
    let left: Vec<_> = std::fs::read_dir(e.state.lib.profiles_dir()).map(|d| d.flatten().collect()).unwrap_or_default();
    assert!(left.is_empty(), "no upload part file is left");
}

#[tokio::test(flavor = "multi_thread")]
async fn patch_branches_and_a_stale_preview() {
    let e = engine();
    let Some(pid) = ready_voice(&e, "patch_branches_and_a_stale_preview", &[]).await else { return };
    let path = format!("/api/profiles/{pid}");
    let pdir = e.state.lib.profiles_dir().join(&pid);
    assert!(pdir.join("preview.wav").exists());
    let r = send(&e.app, "PATCH", &path, Some(json!({"name": "   "}))).await;
    assert_eq!((r.status, json_of(&r)), (400, json!({"detail": "Name cannot be empty"})));
    let r = send(&e.app, "PATCH", &path, Some(json!({"active_variant": 99}))).await;
    assert_eq!((r.status, json_of(&r)), (400, json!({"detail": "Bad variant"})));
    let r = send(&e.app, "PATCH", &path, Some(json!({"active_variant": "x"}))).await;
    assert_eq!(r.status, 422);
    let r = send(&e.app, "PATCH", "/api/profiles/nosuchvoice", Some(json!({"name": "a"}))).await;
    assert_eq!(r.status, 404);
    // name and description only: cut to 60 / 300, the preview stays
    let r = send(&e.app, "PATCH", &path, Some(json!({"name": format!(" {} ", "n".repeat(80)), "description": "d".repeat(400)}))).await;
    let p = json_of(&r);
    assert_eq!((p["name"].as_str().unwrap().len(), p["description"].as_str().unwrap().len()), (60, 300));
    assert!(p.get("preview").is_some() && pdir.join("preview.wav").exists());
    assert!(e.state.worker.active_for(&format!("{pid}:preview")).is_none());
    // a new transcript for the clip: the preview is dropped and a new one queued
    let tx = block(&e);
    let r = send(&e.app, "PATCH", &path, Some(json!({"ref_text": "  New words.  "}))).await;
    let p = json_of(&r);
    assert_eq!(p["ref_text_override"], json!({"0": "New words."}));
    assert!(p.get("preview").is_none() && !pdir.join("preview.wav").exists());
    assert!(e.state.worker.active_for(&format!("{pid}:preview")).is_some(), "re-queued");
    // the same engine again is not a change; a new one is
    let r = send(&e.app, "PATCH", &path, Some(json!({"default_engine": "qwen3-tts-1.7b"}))).await;
    assert_eq!(json_of(&r)["default_engine"], "qwen3-tts-1.7b");
    drop(tx);
    wait_idle(&e);
    assert!(pdir.join("preview.wav").exists(), "the queued preview was made");
    let r = send(&e.app, "PATCH", &path, Some(json!({"default_engine": "qwen3-tts-1.7b", "active_variant": 0}))).await;
    assert!(json_of(&r).get("preview").is_some(), "nothing changed: the preview stays");
}

#[tokio::test(flavor = "multi_thread")]
async fn reanalyze_clamps_the_window() {
    let e = engine();
    let Some(pid) = ready_voice(&e, "reanalyze_clamps_the_window", &[("studio", "false")]).await else { return };
    let r = send(&e.app, "POST", "/api/profiles/nosuch/reanalyze", Some(json!({}))).await;
    assert_eq!(r.status, 404);
    // a 1 s target is raised to 6 s: clips from 5 to 9 s of speech (plus the pause-snapping margin)
    let r = send(&e.app, "POST", &format!("/api/profiles/{pid}/reanalyze"), Some(json!({"target_window": 1}))).await;
    assert_eq!((r.status, json_of(&r)["kind"].as_str()), (200, Some("analyze")));
    assert_eq!(profile(&e, &pid)["status"], "queued");
    wait_idle(&e);
    let p = profile(&e, &pid);
    assert_eq!(p["status"], "ready");
    for v in p["variants"].as_array().unwrap() {
        assert!(v["duration"].as_f64().unwrap() <= 9.0 + 0.6, "{v}");
    }
    // the stored studio choice (false) is used: no fallback tip
    assert!(!p["tips"].as_array().unwrap().iter().any(|t| t == STUDIO_FALLBACK_TIP));
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_answers_and_jobs() {
    let e = engine();
    let Some(pid) = ready_voice(&e, "refresh_answers_and_jobs", &[]).await else { return };
    let path = format!("/api/profiles/{pid}/refresh");
    let r = send(&e.app, "POST", "/api/profiles/nosuch/refresh", None).await;
    assert_eq!(r.status, 404);
    let tx = block(&e);
    let r = send(&e.app, "POST", &path, None).await;
    let j = json_of(&r);
    assert_eq!((r.status, j["kind"].as_str(), j["label"].as_str()), (200, Some("refresh"), Some("Updating voice")));
    let r = send(&e.app, "POST", &path, None).await;
    assert_eq!((r.status, json_of(&r)), (409, json!({"detail": "This voice is busy."})));
    drop(tx);
    wait_idle(&e);
    let p = profile(&e, &pid);
    assert_eq!(p["pipeline_version"], json!(4));
    let rep = &p["refresh_report"];
    assert!(rep["updated"].is_array() && rep["kept"].is_array(), "{rep}");
    assert!(e.state.lib.profiles_dir().join(&pid).join("preview.wav").exists(), "a fresh preview follows");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_while_queued_cancels_the_voices_jobs() {
    let e = engine();
    let Some(clip) = say(e._tmp.path(), "delete_while_queued_cancels_the_voices_jobs", SAY_TEXT) else { return };
    let tx = block(&e);
    let a = upload(&e.app, &[("name", "Gone")], Some(("x.aiff", &std::fs::read(clip).unwrap()))).await;
    let p = json_of(&a);
    let pid = p["id"].as_str().unwrap().to_string();
    let jid = p["job"]["id"].as_str().unwrap().to_string();
    let r = send(&e.app, "DELETE", &format!("/api/profiles/{pid}"), None).await;
    assert_eq!((r.status, json_of(&r)), (200, json!({"ok": true})));
    assert_eq!(e.state.worker.get(&jid).unwrap()["status"], "cancelled");
    assert!(e.state.worker.active_for(&format!("{pid}:preview")).is_none());
    assert!(e.state.lib.get_profile(&pid).unwrap().is_none() && !e.state.lib.profiles_dir().join(&pid).exists());
    drop(tx);
    wait_idle(&e);
    assert!(!e.state.lib.profiles_dir().join(&pid).exists(), "no job recreated the folder");
    let r = send(&e.app, "DELETE", "/api/profiles/bad.id", None).await;
    assert_eq!((r.status, json_of(&r)), (400, json!({"detail": "Invalid id"})));
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_all_refresh_and_full() {
    let e = engine();
    let Some(pid) = ready_voice(&e, "upgrade_all_refresh_and_full", &[]).await else { return };
    let r = send(&e.app, "POST", "/api/profiles/upgrade-all", Some(json!({}))).await;
    assert_eq!(json_of(&r), json!({"queued": []}), "an up-to-date voice is skipped");
    let mut p = e.state.lib.get_profile(&pid).unwrap().unwrap();
    p.insert("pipeline_version".into(), 3.into());
    e.state.lib.put_profile(&mut p).unwrap();
    let tx = block(&e);
    let r = send(&e.app, "POST", "/api/profiles/upgrade-all", Some(json!({}))).await;
    assert_eq!(json_of(&r), json!({"queued": ["Morning voice"]}));
    assert_eq!(e.state.worker.active_for(&pid).unwrap()["kind"], "refresh");
    drop(tx);
    wait_idle(&e);
    let tx = block(&e);
    let r = send(&e.app, "POST", "/api/profiles/upgrade-all", Some(json!({"mode": "full", "only_outdated": false, "studio": false}))).await;
    assert_eq!(json_of(&r), json!({"queued": ["Morning voice"]}));
    assert_eq!(e.state.worker.active_for(&pid).unwrap()["kind"], "analyze");
    assert_eq!((profile(&e, &pid)["status"].as_str(), profile(&e, &pid)["studio"].as_bool()), (Some("queued"), Some(false)));
    drop(tx);
    wait_idle(&e);
    assert_eq!(profile(&e, &pid)["status"], "ready");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_studio_request_is_stored_false_with_the_tip_and_denoise_is_kept() {
    let e = engine();
    let Some(pid) = ready_voice(&e, "a_studio_request_is_stored_false_with_the_tip_and_denoise_is_kept", &[("studio", "true"), ("denoise", "1")]).await else { return };
    let p = profile(&e, &pid);
    assert_eq!((p["studio"].clone(), p["denoised"].clone(), p["room_echo_after_db"].clone()), (json!(false), json!(true), Value::Null));
    assert_eq!(p["tips"][0], STUDIO_FALLBACK_TIP);
}
