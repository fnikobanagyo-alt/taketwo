//! The HTTP API, ported from backend/app/main.py (read side, step S06): the same routes, status codes,
//! `{"detail": ...}` texts and JSON key order. Step S07a adds the write routes that start no job; the paths
//! that would start one answer 501 until S07b (the worker), a speed change until S16.

use crate::files;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use taketwo_core::library::{self, Doc, Library, is_plain_name};
use taketwo_core::text::py;
use taketwo_core::pipeline::{self, Fail, NewNarration};
use taketwo_core::{captions, review};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

/// config.py APP_VERSION and PIPELINE_VERSION; main.py MAX_SCRIPT_WORDS.
pub const APP_VERSION: &str = "2.0.0";
pub const PIPELINE_VERSION: i64 = 4;
pub const MAX_SCRIPT_WORDS: i64 = 10_000;
pub const NOT_YET: &str = "Not in TakeTwo 1.0 yet.";
const LOCAL_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];
const DEV_UI_ORIGIN: &str = "http://localhost:5173";

pub struct AppState {
    pub lib: Library,
    /// The built React UI (`--ui`); None when not given or missing.
    pub ui: Option<PathBuf>,
    pub started: f64,
    /// Shows a file or folder in the file manager: `reveal_in_file_manager` when serving, a recorder in tests.
    pub reveal: Box<dyn Fn(&std::path::Path) + Send + Sync>,
}

/// main.py `_reveal`: a file selected, or a folder opened, in the system file manager.
pub fn reveal_in_file_manager(target: &std::path::Path) {
    use std::process::Command;
    let file = target.is_file();
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        if file {
            c.arg("-R");
        }
        c.arg(target);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("explorer");
        if file {
            c.arg(format!("/select,{}", target.display()));
        } else {
            c.arg(target);
        }
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(if file { target.parent().unwrap_or(target) } else { target });
        c
    };
    // Python waits up to 10 s and ignores the result; the opener returns at once, so a spawn is enough.
    if let Err(e) = cmd.spawn() {
        eprintln!("taketwo-engine: could not open the file manager: {e}");
    }
}

type St = State<Arc<AppState>>;

pub fn detail(status: u16, msg: &str) -> Response {
    (StatusCode::from_u16(status).expect("valid status"), axum::Json(json!({ "detail": msg }))).into_response()
}

fn not_yet() -> Response {
    detail(501, NOT_YET)
}

fn ok_json(v: impl Into<Value>) -> Response {
    axum::Json(v.into()).into_response()
}

/// Where Python raises: FastAPI's plain-text 500; the error goes to stderr only.
fn ise(e: String) -> Response {
    eprintln!("taketwo-engine: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "Internal Server Error").into_response()
}

/// FastAPI's 422 for one parameter that failed to parse.
fn invalid(errors: Vec<Value>) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({ "detail": errors }))).into_response()
}

fn int_error(loc: &str, name: &str, input: &str) -> Value {
    json!({"type": "int_parsing", "loc": [loc, name], "msg": "Input should be a valid integer, unable to parse string as an integer", "input": input})
}

/// pydantic's str -> int: optional sign and digits around spaces. Huge values saturate, which only ever
/// means "out of range" here.
fn parse_int(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, d) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = d.bytes().fold(0i64, |n, b| n.saturating_mul(10).saturating_add((b - b'0') as i64));
    Some(if neg { -n } else { n })
}

/// Path integers, all checked before the handler runs, as FastAPI does.
fn path_ints(params: &[(&str, &str)]) -> Result<Vec<i64>, Response> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (name, v) in params {
        match parse_int(v) {
            Some(n) => out.push(n),
            None => errors.push(int_error("path", name, v)),
        }
    }
    if errors.is_empty() { Ok(out) } else { Err(invalid(errors)) }
}

/// Query parameters; the last value wins, as Starlette's `QueryParams` does.
fn query(uri: &Uri) -> HashMap<String, String> {
    form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).into_owned().collect()
}

/// pydantic's str -> bool.
fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
        "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
        _ => None,
    }
}

/// main.py `_item_dir`: anything but `[A-Za-z0-9_-]{1,64}` is 400 `Invalid id`.
fn item_id_ok(id: &str) -> Result<(), Response> {
    let ok = (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok { Ok(()) } else { Err(detail(400, "Invalid id")) }
}

/// A folder under `root` for an id Python joins without `_item_dir`. A non-plain id can't name a saved
/// item (Python finds no file there), so it is 404 `Not found` before anything is joined.
fn plain_dir(root: PathBuf, id: &str) -> Result<PathBuf, Response> {
    if is_plain_name(id) { Ok(root.join(id)) } else { Err(detail(404, "Not found")) }
}

/// main.py `_live_gen`: missing or soft-deleted is 404.
fn live_gen(lib: &Library, gid: &str) -> Result<Doc, Response> {
    match lib.get_generation(gid).map_err(ise)? {
        Some(g) if !g.get("deleted_at").is_some_and(py::truthy) => Ok(g),
        _ => Err(detail(404, "Not found")),
    }
}

// ------------------------------------------------------------------ guards

/// `_host_name`: drop a trailing `:digits` only.
fn host_name(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    }
}

/// Characters re-encoded after decoding: everything but unreserved ones and `/`.
const PATH_KEEP: &AsciiSet = &NON_ALPHANUMERIC.remove(b'/').remove(b'-').remove(b'_').remove(b'.').remove(b'~');

/// main.py `only_this_computer`, before routing. It also routes on the decoded path, as Starlette does,
/// so an encoded `/` splits the path there too and never reaches a handler inside an id.
async fn only_this_computer(mut req: Request, next: Next) -> Response {
    let text = |h: &HeaderMap, n: &HeaderName| h.get(n).map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned()).unwrap_or_default();
    let (host, origin) = (text(req.headers(), &header::HOST), text(req.headers(), &header::ORIGIN));
    if !LOCAL_HOSTS.contains(&host_name(&host)) {
        return detail(403, "TakeTwo only answers this computer.");
    }
    let reads = [Method::GET, Method::HEAD, Method::OPTIONS];
    if !origin.is_empty() && !reads.contains(req.method()) && origin != format!("http://{host}") && origin != DEV_UI_ORIGIN {
        return detail(403, "Requests from other websites are not allowed.");
    }
    let decoded = percent_decode_str(req.uri().path()).decode_utf8_lossy().into_owned();
    if decoded.starts_with("/api/") && decoded.split(['/', '\\']).any(|p| p == "." || p == "..") {
        return detail(400, "Invalid path.");
    }
    let path = utf8_percent_encode(&decoded, PATH_KEEP).to_string();
    let pq = match req.uri().query() {
        Some(q) => format!("{path}?{q}"),
        None => path,
    };
    let mut parts = req.uri().clone().into_parts();
    if let Ok(pq) = pq.parse() {
        parts.path_and_query = Some(pq);
        if let Ok(u) = Uri::from_parts(parts) {
            *req.uri_mut() = u;
        }
    }
    next.run(req).await
}

// ------------------------------------------------------------------ health, status, stubs

async fn health() -> Response {
    ok_json(json!({"ok": true, "name": "TakeTwo", "formerly": "VoiceForge", "version": APP_VERSION, "loaded_engine": null}))
}

async fn pipeline_version() -> Response {
    ok_json(json!({ "version": PIPELINE_VERSION }))
}

async fn status() -> Response {
    ok_json(json!({"loaded_engine": null, "running": null, "queued": 0, "queue": [], "duplicates": [], "max_script_words": MAX_SCRIPT_WORDS}))
}

/// No update check in 1.0: today's shape with nothing checked.
async fn update_state() -> Response {
    ok_json(json!({"current": APP_VERSION, "check": null, "available": false, "dismissed": false, "installed": null, "job": null}))
}

async fn diagnostics(State(st): St) -> Response {
    ok_json(json!({"runtime": {"pid": std::process::id(), "started_at": st.started, "source_sha256": null},
                   "jobs": [], "job_limit": 20, "event_limit": 32}))
}

/// The one engine TakeTwo 1.0 has, in engines.py `EngineSpec.public()` shape.
async fn list_engines() -> Response {
    let downloaded = std::env::var_os("TAKETWO_MODELS")
        .is_some_and(|m| PathBuf::from(m).join("qwen3-tts-1.7b-base").join("config.json").is_file());
    let p = |key: &str, label: &str, default: Value, min: Value, max: Value, step: Value, help: &str| {
        json!({"key": key, "label": label, "default": default, "min": min, "max": max, "step": step, "help": help})
    };
    let params = vec![
        p("temperature", "Temperature", json!(0.7), json!(0.1), json!(1.2), json!(0.05),
          "Lower = steadier, more consistent delivery. Higher = more expressive but riskier."),
        p("top_p", "Top-p", json!(0.9), json!(0.5), json!(1.0), json!(0.01), ""),
        p("top_k", "Top-k", json!(40), json!(5), json!(100), json!(1), ""),
        p("repetition_penalty", "Repetition penalty", json!(1.05), json!(1.0), json!(2.0), json!(0.05),
          "Lower = less suppression of repeated sound codes. Higher = fewer loops. Changes can affect pronunciation."),
    ];
    ok_json(json!([{
        "id": "qwen3-tts-1.7b", "name": "Qwen3-TTS 1.7B", "repo": "Qwen/Qwen3-TTS-12Hz-1.7B-Base", "family": "qwen3",
        "description": "Best all-round cloning accuracy and natural narration. Uses reference audio + transcript (in-context cloning).",
        "license": "Apache-2.0", "size_gb": 4.5, "languages": ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"],
        "needs_ref_text": true, "recommended": true, "params": params, "custom": false, "kind": "clone",
        "downloaded": downloaded, "loaded": false, "download": null,
    }]))
}

/// mlx-audio is not used by the Rust engine.
async fn catalog_library() -> Response {
    ok_json(json!({"installed": "not used by TakeTwo 1.0", "latest": null, "update_available": false}))
}

/// `shlex.quote`.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// Is the TakeTwo MCP server registered in Claude Desktop and Claude Code? Read-only, as main.py does,
/// with the Rust `taketwo-mcp` program beside this one in place of the Python script.
async fn integrations() -> Response {
    let mcp = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(format!("taketwo-mcp{}", std::env::consts::EXE_SUFFIX))))
        .unwrap_or_else(|| PathBuf::from("taketwo-mcp"));
    let registered = |cfg: &std::path::Path| {
        std::fs::read_to_string(cfg)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("mcpServers").and_then(Value::as_object).map(|m| m.contains_key("taketwo") || m.contains_key("voiceforge")))
            .unwrap_or(false)
    };
    let desktop_cfg = dirs::config_dir().unwrap_or_default().join("Claude").join("claude_desktop_config.json");
    let code_cfg = dirs::home_dir().unwrap_or_default().join(".claude.json");
    let mcp_s = mcp.to_string_lossy().into_owned();
    let snippet = serde_json::to_string_pretty(&json!({"mcpServers": {"taketwo": {"command": mcp_s, "args": []}}})).unwrap_or_default();
    ok_json(json!({
        "script_found": mcp.is_file(),
        "apps": [
            {"id": "claude-desktop", "name": "Claude Desktop", "connected": registered(&desktop_cfg),
             "how": format!("Add this to {} (merge into an existing \"mcpServers\"), then quit and reopen Claude Desktop.", desktop_cfg.display()),
             "setup": snippet},
            {"id": "claude-code", "name": "Claude Code", "connected": registered(&code_cfg),
             "how": "Run this once in Terminal, then start a new Claude Code session.",
             "setup": format!("claude mcp add --scope user taketwo -- {}", shell_quote(&mcp_s))},
        ],
    }))
}

// ------------------------------------------------------------------ settings

async fn get_settings(State(st): St) -> Response {
    match library::get_settings(&st.lib.settings_path()) {
        Ok(s) => ok_json(Value::Object(s)),
        Err(e) => ise(e),
    }
}

async fn put_settings(State(st): St, body: Bytes) -> Response {
    let patch = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(m)) => m,
        Ok(other) => return invalid(vec![json!({"type": "dict_type", "loc": ["body"], "msg": "Input should be a valid dictionary", "input": other})]),
        Err(_) if body.is_empty() => return invalid(vec![json!({"type": "missing", "loc": ["body"], "msg": "Field required", "input": null})]),
        Err(e) => return invalid(vec![json!({"type": "json_invalid", "loc": ["body", 0], "msg": "JSON decode error", "input": {}, "ctx": {"error": e.to_string()}})]),
    };
    if let Err(e) = library::checked(&patch) {
        return detail(400, &e); // db.save_settings raises ValueError for these
    }
    match library::save_settings(&st.lib.settings_path(), &patch) {
        Ok(s) => ok_json(Value::Object(s)),
        Err(e) => ise(e),
    }
}

// ------------------------------------------------------------------ voices

async fn list_profiles(State(st): St) -> Response {
    match st.lib.list_profiles() {
        Ok(v) => ok_json(Value::Array(v.into_iter().map(Value::Object).collect())),
        Err(e) => ise(e),
    }
}

async fn get_profile(State(st): St, Path(pid): Path<String>) -> Response {
    match st.lib.get_profile(&pid) {
        Ok(Some(mut p)) => {
            p.insert("job".into(), Value::Null); // no jobs before S07
            ok_json(Value::Object(p))
        }
        Ok(None) => detail(404, "Not found"),
        Err(e) => ise(e),
    }
}

async fn get_analysis(State(st): St, Path(pid): Path<String>) -> Response {
    let f = match plain_dir(st.lib.profiles_dir(), &pid) {
        Ok(d) => d.join("analysis.json"),
        Err(r) => return r,
    };
    if !f.exists() {
        return detail(404, "Not found");
    }
    match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string())) {
        Ok(v) => ok_json(v),
        Err(e) => ise(format!("{}: {e}", f.display())),
    }
}

async fn profile_preview(State(st): St, Path(pid): Path<String>, method: Method, headers: HeaderMap) -> Response {
    let p = match st.lib.get_profile(&pid) {
        Ok(Some(p)) => p,
        Ok(None) => return detail(404, "Not found"),
        Err(e) => return ise(e),
    };
    let pdir = match plain_dir(st.lib.profiles_dir(), &pid) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let mut f = pdir.join("preview.wav");
    let mut kind = "generated";
    if !f.exists() {
        kind = "reference";
        let vs = p.get("variants").filter(|v| py::truthy(v));
        f = match vs {
            None => pdir.join("clean.wav"),
            Some(vs) => {
                let Some(vs) = vs.as_array() else { return ise("variants is not a list".into()) };
                let i = match p.get("active_variant") {
                    None => 0,
                    Some(v) => match v.as_i64().or(v.as_bool().map(i64::from)) {
                        Some(i) => i,
                        None => return ise("active_variant is not an int".into()),
                    },
                };
                let i = if i < 0 { i + vs.len() as i64 } else { i }; // Python's negative index
                match usize::try_from(i).ok().and_then(|i| vs.get(i)).and_then(|v| v.get("file")).and_then(Value::as_str) {
                    Some(name) if is_plain_name(name) => pdir.join(name),
                    Some(_) => return detail(404, "No audio for this voice yet"),
                    None => return ise("no such variant file".into()),
                }
            }
        };
    }
    if !f.is_file() {
        return detail(404, "No audio for this voice yet");
    }
    let extra = [(HeaderName::from_static("x-preview-kind"), kind.to_string()), (header::CACHE_CONTROL, "no-store".to_string())];
    files::serve(&f, &method, &headers, "audio/wav", &extra).await
}

async fn profile_audio(State(st): St, Path((pid, name)): Path<(String, String)>, method: Method, headers: HeaderMap) -> Response {
    if let Err(r) = item_id_ok(&pid) {
        return r;
    }
    let dir = st.lib.profiles_dir().join(&pid);
    if !is_plain_name(&name) {
        return detail(404, "Not found");
    }
    // Python resolves symlinks and wants the file directly in the voice's folder.
    let (Ok(f), Ok(d)) = (dir.join(&name).canonicalize(), dir.canonicalize()) else { return detail(404, "Not found") };
    if f.parent() != Some(d.as_path()) {
        return detail(404, "Not found");
    }
    if !f.is_file() {
        return ise(format!("{} is not a file", f.display()));
    }
    files::serve(&f, &method, &headers, &files::guess_type(&f.to_string_lossy()), &[]).await
}

// ------------------------------------------------------------------ narrations

async fn list_generations(State(st): St) -> Response {
    let run = || -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for mut g in st.lib.list_generations()? {
            if g.get("deleted_at").is_some_and(py::truthy) {
                continue;
            }
            let outcome = review::outcome(&g)?;
            let count = if g.get("status").and_then(Value::as_str) == Some("done") { review::fix_targets(&g)?.len() } else { 0 };
            g.shift_remove("chunks");
            g.shift_remove("cleaned_text");
            g.insert("outcome".into(), outcome.into());
            g.insert("review_count".into(), count.into());
            let preview: String = match g.shift_remove("text") {
                None => String::new(),
                Some(Value::String(t)) => t.chars().take(240).collect(),
                Some(other) => return Err(format!("text is not a str: {other}")),
            };
            g.insert("text_preview".into(), preview.into());
            out.push(Value::Object(g));
        }
        Ok(out)
    };
    match run() {
        Ok(v) => ok_json(Value::Array(v)),
        Err(e) => ise(e),
    }
}

fn lower_field(g: &Doc, k: &str) -> Result<String, String> {
    match g.get(k) {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.to_lowercase()),
        Some(other) => Err(format!("{k} is not a str: {other}")),
    }
}

async fn search_generations(st: &AppState, uri: &Uri) -> Response {
    let qs = query(uri);
    let arg = |k: &str| qs.get(k).cloned().unwrap_or_default();
    let limit = match qs.get("limit") {
        None => 20,
        Some(v) => match parse_int(v) {
            Some(n) => n,
            None => return invalid(vec![int_error("query", "limit", v)]),
        },
    };
    let (q, voice, status, same) = (arg("q"), arg("voice"), arg("status"), arg("same_script"));
    let run = || -> Result<Vec<Value>, String> {
        let s = library::get_settings(&st.lib.settings_path())?;
        let want = if py::strip(&same).is_empty() {
            String::new()
        } else {
            py::strip(&pipeline::clean_with_settings(&same, &s)).to_string()
        };
        let mut out = Vec::new();
        for g in st.lib.list_generations()? {
            if g.get("deleted_at").is_some_and(py::truthy) {
                continue;
            }
            if !voice.is_empty() {
                let v = voice.to_lowercase();
                if v != lower_field(&g, "profile_name")? && v != lower_field(&g, "profile_id")? {
                    continue;
                }
            }
            let oc = review::outcome(&g)?;
            let gst = g.get("status").cloned().unwrap_or(Value::Null);
            let st_v = if oc == Some("needs_listen") { Value::from("needs_review") } else { gst.clone() };
            if !status.is_empty() && st_v.as_str() != Some(status.as_str()) && !(status == "done" && gst.as_str() == Some("done")) {
                continue;
            }
            if !q.is_empty() {
                let text = g.get("text").map(|v| v.as_str().map(str::to_string).ok_or("text is not a str")).transpose()?.unwrap_or_default();
                let title = match g.get("title").filter(|v| py::truthy(v)) {
                    None => String::new(),
                    Some(Value::String(t)) => t.clone(),
                    Some(other) => return Err(format!("title is not a str: {other}")),
                };
                if !format!("{text} {title}").to_lowercase().contains(&q.to_lowercase()) {
                    continue;
                }
            }
            if !want.is_empty() {
                let cleaned = match g.get("cleaned_text").filter(|v| py::truthy(v)) {
                    None => "",
                    Some(v) => v.as_str().ok_or("cleaned_text is not a str")?,
                };
                if py::strip(cleaned) != want {
                    continue;
                }
            }
            let get = |k: &str| g.get(k).cloned().unwrap_or(Value::Null);
            let mut r = Doc::new();
            r.insert("id".into(), g.get("id").cloned().ok_or("narration without id")?);
            r.insert("title".into(), get("title"));
            r.insert("voice".into(), get("profile_name"));
            r.insert("profile_id".into(), get("profile_id"));
            r.insert("status".into(), st_v);
            r.insert("outcome".into(), oc.into());
            r.insert("created".into(), get("created"));
            r.insert("duration".into(), get("duration"));
            r.insert("source".into(), g.get("source").cloned().unwrap_or("api".into()));
            out.push(Value::Object(r));
            if out.len() as i64 >= limit.clamp(1, 100) {
                break;
            }
        }
        Ok(out)
    };
    match run() {
        Ok(v) => ok_json(Value::Array(v)),
        Err(e) => ise(e),
    }
}

/// GET /api/generations/{gid}; `search` is matched here as Starlette matches it before `{gid}`.
async fn get_generation(State(st): St, Path(gid): Path<String>, uri: Uri) -> Response {
    if gid == "search" {
        return search_generations(&st, &uri).await;
    }
    generation_answer(&st, &gid)
}

/// The `get_generation` answer, which the accept routes return too.
fn generation_answer(st: &AppState, gid: &str) -> Response {
    let mut g = match live_gen(&st.lib, gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    let run = |g: &mut Doc| -> Result<(), String> {
        let oc = review::outcome(g)?;
        g.insert("outcome".into(), oc.into());
        let pid = g.get("profile_id").and_then(Value::as_str).ok_or("profile_id is not a str")?.to_string();
        let pitch = st.lib.get_profile(&pid)?.and_then(|p| p.get("pitch_hz").and_then(Value::as_f64));
        let items = if oc.is_some() { review::review(g, pitch)? } else { Vec::new() };
        g.insert("review".into(), items.into());
        g.insert("job".into(), Value::Null);
        Ok(())
    };
    match run(&mut g) {
        Ok(()) => ok_json(Value::Object(g)),
        Err(e) => ise(e),
    }
}

/// No worker yet: Python answers the same for an idle or unknown narration.
async fn cancel_generation() -> Response {
    ok_json(json!({ "ok": true }))
}

async fn subtitles(State(st): St, Path(gid): Path<String>, uri: Uri) -> Response {
    let fmt = query(&uri).get("fmt").cloned().unwrap_or_else(|| "srt".into());
    let g = match live_gen(&st.lib, &gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    if !g.get("timeline").is_some_and(py::truthy) {
        return detail(404, "Subtitles are available once the audio is rendered");
    }
    let run = || -> Result<(String, String), String> {
        let caps = captions::generation_captions(&g)?;
        let body = match fmt.as_str() {
            "vtt" => captions::to_vtt(&caps)?,
            "txt" => captions::to_txt(&caps)?,
            _ => captions::to_srt(&caps)?,
        };
        let title = match g.get("title").filter(|v| py::truthy(v)) {
            Some(t) => py::display(t),
            None => py::display(g.get("id").ok_or("narration without id")?),
        };
        let name: String = title.replace('/', "-").replace('…', "").chars().take(60).collect();
        Ok((body, captions::attachment_header(py::strip(&name), &fmt)))
    };
    match run() {
        Ok((body, disp)) => {
            let mut r = (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response();
            if let Ok(v) = HeaderValue::from_str(&disp) {
                r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            r
        }
        Err(e) => ise(e),
    }
}

async fn generation_timings(State(st): St, Path(gid): Path<String>) -> Response {
    let g = match live_gen(&st.lib, &gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    if !g.get("timeline").is_some_and(py::truthy) {
        return detail(404, "Timings are available once the audio is rendered");
    }
    match captions::generation_timings(&g) {
        Ok(t) => ok_json(Value::Object(t)),
        Err(e) => ise(e),
    }
}

/// main.py's download name: `<voice> - <title>.<fmt>`, the title cut to Python's `[\w\s\-',.!?]`.
fn download_name(g: &Doc, fmt: &str) -> Result<String, String> {
    static UNSAFE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| py::re(r"[^{WORD}{SPACE}\-',.!?]"));
    let title = match g.get("title").filter(|v| py::truthy(v)) {
        None => "Narration".to_string(),
        Some(Value::String(t)) => t.clone(),
        Some(other) => return Err(format!("title is not a str: {other}")),
    };
    let cut = UNSAFE.replace_all(&title.replace('…', ""), "").into_owned();
    let cut: String = py::strip(&cut).chars().take(60).collect();
    let cut = cut.trim_end_matches([' ', '.', ',']);
    let voice = py::display(g.get("profile_name").ok_or("narration without profile_name")?);
    Ok(format!("{voice} - {}.{fmt}", if cut.is_empty() { "Narration" } else { cut }))
}

async fn generation_audio(State(st): St, Path(gid): Path<String>, uri: Uri, method: Method, headers: HeaderMap) -> Response {
    let qs = query(&uri);
    let fmt = qs.get("fmt").cloned().unwrap_or_else(|| "wav".into());
    let download = match qs.get("download") {
        None => false,
        Some(v) => match parse_bool(v) {
            Some(b) => b,
            None => {
                return invalid(vec![json!({"type": "bool_parsing", "loc": ["query", "download"],
                                           "msg": "Input should be a valid boolean, unable to interpret input", "input": v})]);
            }
        },
    };
    let g = match live_gen(&st.lib, &gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    let f = match plain_dir(st.lib.generations_dir(), &gid) {
        Ok(d) => d.join(if fmt == "mp3" { "final.mp3" } else { "final.wav" }),
        Err(r) => return r,
    };
    if !f.is_file() {
        return detail(404, "Not rendered yet");
    }
    let name = match download_name(&g, &fmt) {
        Ok(n) => n,
        Err(e) => return ise(e),
    };
    if download {
        let extra = [(header::CONTENT_DISPOSITION, files::content_disposition(&name))];
        files::serve(&f, &method, &headers, &files::guess_type(&name), &extra).await
    } else {
        files::serve(&f, &method, &headers, &files::guess_type(&f.to_string_lossy()), &[]).await
    }
}

async fn chunk_audio(State(st): St, Path((gid, ci)): Path<(String, String)>, method: Method, headers: HeaderMap) -> Response {
    let ci = match path_ints(&[("ci", &ci)]) {
        Ok(v) => v[0],
        Err(r) => return r,
    };
    let f = match plain_dir(st.lib.generations_dir(), &gid) {
        Ok(d) => d.join(format!("chunk_{ci:03}.wav")),
        Err(r) => return r,
    };
    if !f.is_file() {
        return detail(404, "Not found");
    }
    files::serve(&f, &method, &headers, "audio/x-wav", &[(header::CACHE_CONTROL, "no-store".into())]).await
}

async fn take_audio(State(st): St, Path((gid, ci, k)): Path<(String, String, String)>, method: Method, headers: HeaderMap) -> Response {
    let (ci, k) = match path_ints(&[("ci", &ci), ("k", &k)]) {
        Ok(v) => (v[0], v[1]),
        Err(r) => return r,
    };
    let g = match live_gen(&st.lib, &gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    let n = match g.get("chunks").and_then(Value::as_array) {
        Some(c) => c.len() as i64,
        None => return ise("chunks is not a list".into()),
    };
    if !(0 <= ci && ci < n) {
        return detail(404, "No such part");
    }
    if !is_plain_name(&gid) {
        return detail(404, "Not found");
    }
    match review::take_audio(&st.lib.generations_dir(), &g, ci, k) {
        Ok(Some(f)) => files::serve(&f, &method, &headers, &files::guess_type(&f.to_string_lossy()), &[(header::CACHE_CONTROL, "no-store".into())]).await,
        Ok(None) => detail(404, "That take is no longer kept"),
        Err(e) => ise(e),
    }
}

/// No jobs before the worker (S07).
async fn get_job() -> Response {
    detail(404, "Not found")
}

// ------------------------------------------------------------------ request bodies (pydantic, lax mode)

/// A JSON body for a pydantic model, field by field in the model's order; errors are collected as
/// FastAPI reports them (all of them, after any path errors).
struct Fields {
    body: Doc,
    errors: Vec<Value>,
}

fn field_error(kind: &str, name: &str, msg: &str, input: &Value) -> Value {
    json!({"type": kind, "loc": ["body", name], "msg": msg, "input": input})
}

impl Fields {
    /// The body itself: missing, not JSON, or not an object are one error at `["body"]`.
    fn parse(body: &Bytes) -> Result<Fields, Value> {
        match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(m)) => Ok(Fields { body: m, errors: Vec::new() }),
            Ok(other) => Err(json!({"type": "model_attributes_type", "loc": ["body"],
                                    "msg": "Input should be a valid dictionary or object to extract fields from", "input": other})),
            Err(_) if body.is_empty() => Err(json!({"type": "missing", "loc": ["body"], "msg": "Field required", "input": null})),
            Err(e) => Err(json!({"type": "json_invalid", "loc": ["body", 0], "msg": "JSON decode error", "input": {}, "ctx": {"error": e.to_string()}})),
        }
    }

    /// The value of `name`; a missing required field is an error. Null counts as given.
    fn get(&mut self, name: &str, required: bool) -> Option<Value> {
        match self.body.get(name) {
            Some(v) => Some(v.clone()),
            None => {
                if required {
                    let input = Value::Object(self.body.clone());
                    self.errors.push(json!({"type": "missing", "loc": ["body", name], "msg": "Field required", "input": input}));
                }
                None
            }
        }
    }

    fn fail(&mut self, kind: &str, name: &str, msg: &str, v: &Value) -> Option<Value> {
        self.errors.push(field_error(kind, name, msg, v));
        None
    }

    /// `str` (required) or `str | None`.
    fn text(&mut self, name: &str, required: bool) -> Option<String> {
        match self.get(name, required)? {
            Value::String(t) => Some(t),
            Value::Null if !required => None,
            v => self.fail("string_type", name, "Input should be a valid string", &v).and(None),
        }
    }

    /// `float` (null refused) or `float | None`.
    fn float(&mut self, name: &str, nullable: bool) -> Option<f64> {
        match self.get(name, false)? {
            Value::Number(n) => n.as_f64(),
            Value::Null if nullable => None,
            Value::String(t) => match py::strip(&t).parse::<f64>() {
                Ok(x) => Some(x),
                Err(_) => self.fail("float_parsing", name, "Input should be a valid number, unable to parse string as a number", &Value::String(t)).and(None),
            },
            v => self.fail("float_type", name, "Input should be a valid number", &v).and(None),
        }
    }

    /// `int`, required.
    fn int(&mut self, name: &str) -> Option<i64> {
        match self.get(name, true)? {
            Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => Some(i),
                (None, Some(f)) if f.fract() == 0.0 && f.abs() < 9.2e18 => Some(f as i64),
                _ => self.fail("int_from_float", name, "Input should be a valid integer, got a number with a fractional part", &Value::Number(n)).and(None),
            },
            Value::String(t) => match parse_int(&t) {
                Some(i) => Some(i),
                None => self.fail("int_parsing", name, "Input should be a valid integer, unable to parse string as an integer", &Value::String(t)).and(None),
            },
            v => self.fail("int_type", name, "Input should be a valid integer", &v).and(None),
        }
    }

    /// `bool | None`.
    fn flag(&mut self, name: &str) -> Option<bool> {
        match self.get(name, false)? {
            Value::Bool(b) => Some(b),
            Value::Null => None,
            Value::Number(n) if n.as_i64() == Some(0) || n.as_i64() == Some(1) => Some(n.as_i64() == Some(1)),
            Value::String(t) => match parse_bool(&t) {
                Some(b) => Some(b),
                None => self.fail("bool_parsing", name, "Input should be a valid boolean, unable to interpret input", &Value::String(t)).and(None),
            },
            v => self.fail("bool_type", name, "Input should be a valid boolean", &v).and(None),
        }
    }

    /// `dict = {}`.
    fn dict(&mut self, name: &str) -> Value {
        match self.get(name, false) {
            None => json!({}),
            Some(v @ Value::Object(_)) => v,
            Some(v) => {
                self.fail("dict_type", name, "Input should be a valid dictionary", &v);
                json!({})
            }
        }
    }
}


/// FastAPI's order: one part number in the path, then the body. The caller reads the body's fields and
/// then calls `finish_422`, so every error comes back in one 422.
fn part_and_body(ci: &str, body: &Bytes) -> (Option<i64>, Vec<Value>, Option<Fields>) {
    let mut errors = Vec::new();
    let n = parse_int(ci);
    if n.is_none() {
        errors.push(int_error("path", "ci", ci));
    }
    let fields = match Fields::parse(body) {
        Ok(f) => Some(f),
        Err(e) => {
            errors.push(e);
            None
        }
    };
    (n, errors, fields)
}

/// Body errors found while reading fields join the earlier ones.
fn finish_422(mut errors: Vec<Value>, fields: Option<&Fields>) -> Option<Response> {
    errors.extend(fields.map(|f| f.errors.clone()).unwrap_or_default());
    if errors.is_empty() { None } else { Some(invalid(errors)) }
}

// ------------------------------------------------------------------ narration writes (S07a)

const BUSY: &str = "This narration is busy. Try again when it finishes.";

fn chunk_count(g: &Doc) -> Result<i64, Response> {
    match g.get("chunks").and_then(Value::as_array) {
        Some(c) => Ok(c.len() as i64),
        None => Err(ise("chunks is not a list".into())),
    }
}

fn chunk(g: &Doc, ci: i64) -> &Value {
    &g["chunks"][ci as usize]
}

/// main.py `_idle_gen`. No worker yet, so the stored status is the only busy case.
fn idle_gen(lib: &Library, gid: &str, ci: i64) -> Result<Doc, Response> {
    let g = live_gen(lib, gid)?;
    if !(0 <= ci && ci < chunk_count(&g)?) {
        return Err(detail(404, "No such part"));
    }
    if matches!(g.get("status").and_then(Value::as_str), Some("queued" | "running")) {
        return Err(detail(409, BUSY));
    }
    Ok(g)
}

fn save_accepted(st: &AppState, gid: &str, mut g: Doc, ci: i64, on: bool) -> Response {
    let clock = || st.lib.now();
    if let Err(e) = review::set_accepted(&mut g, ci, on, &clock).and_then(|()| st.lib.put_generation(&mut g)) {
        return ise(e);
    }
    generation_answer(st, gid)
}

async fn accept_part(State(st): St, Path((gid, ci)): Path<(String, String)>) -> Response {
    let ci = match path_ints(&[("ci", &ci)]) {
        Ok(v) => v[0],
        Err(r) => return r,
    };
    let g = match idle_gen(&st.lib, &gid, ci) {
        Ok(g) => g,
        Err(r) => return r,
    };
    if !chunk(&g, ci).get("file").is_some_and(py::truthy) {
        return detail(400, "This part has no audio yet.");
    }
    let targets = match review::fix_targets(&g) {
        Ok(t) => t,
        Err(e) => return ise(e),
    };
    if !targets.contains(&ci) && !chunk(&g, ci).get("accepted").is_some_and(py::truthy) {
        return detail(400, "This part doesn't need review, so there is nothing to mark.");
    }
    save_accepted(&st, &gid, g, ci, true)
}

async fn unaccept_part(State(st): St, Path((gid, ci)): Path<(String, String)>) -> Response {
    let ci = match path_ints(&[("ci", &ci)]) {
        Ok(v) => v[0],
        Err(r) => return r,
    };
    match idle_gen(&st.lib, &gid, ci) {
        Ok(g) => save_accepted(&st, &gid, g, ci, false),
        Err(r) => r,
    }
}

/// Soft delete; an unknown id answers 200 as in Python.
async fn delete_generation(State(st): St, Path(gid): Path<String>) -> Response {
    if let Err(r) = item_id_ok(&gid) {
        return r;
    }
    match st.lib.set_generation_deleted(&gid, true) {
        Ok(_) => ok_json(json!({"ok": true, "undo": format!("/api/generations/{gid}/restore")})),
        Err(e) => ise(e),
    }
}

async fn restore_generation(State(st): St, Path(gid): Path<String>) -> Response {
    match st.lib.set_generation_deleted(&gid, false) {
        Ok(Some(g)) => ok_json(Value::Object(g)),
        Ok(None) => detail(404, "Not found"),
        Err(e) => ise(e),
    }
}

async fn patch_generation(State(st): St, Path(gid): Path<String>, body: Bytes) -> Response {
    let mut f = match Fields::parse(&body) {
        Ok(f) => f,
        Err(e) => return invalid(vec![e]),
    };
    let (title, favorite, speed) = (f.text("title", false), f.flag("favorite"), f.float("speed", true));
    if let Some(r) = finish_422(Vec::new(), Some(&f)) {
        return r;
    }
    let g = match live_gen(&st.lib, &gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    let mut fields = Doc::new();
    if let Some(t) = title.as_deref().map(py::strip).filter(|t| !t.is_empty()) {
        fields.insert("title".into(), t.chars().take(120).collect::<String>().into());
    }
    if let Some(b) = favorite {
        fields.insert("favorite".into(), b.into());
    }
    let stored = g.get("speed").filter(|v| py::truthy(v)).and_then(Value::as_f64).unwrap_or(1.0);
    if speed.is_some_and(|x| (x - stored).abs() > 1e-3) {
        return not_yet(); // S16: retime (Python re-times the finished audio here); nothing is changed
    }
    let doc = if fields.is_empty() { st.lib.get_generation(&gid) } else { st.lib.patch_generation_meta(&gid, &fields) };
    match doc {
        Ok(Some(d)) => ok_json(Value::Object(d)),
        Ok(None) => detail(404, "Not found"),
        Err(e) => ise(e),
    }
}

/// `POST /api/generations/reveal`: the folder narrations are kept in.
fn reveal_narrations_folder(st: &AppState) -> Response {
    let dir = st.lib.generations_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return ise(format!("{}: {e}", dir.display()));
    }
    (st.reveal)(&dir);
    ok_json(json!({"path": dir.to_string_lossy()}))
}

async fn reveal_narration(State(st): St, Path(gid): Path<String>) -> Response {
    if let Err(r) = live_gen(&st.lib, &gid).and_then(|_| item_id_ok(&gid)) {
        return r;
    }
    let gdir = st.lib.generations_dir().join(&gid);
    let target = [gdir.join("final.mp3"), gdir.join("final.wav")].into_iter().find(|f| f.exists()).unwrap_or(gdir);
    if !target.exists() {
        return detail(404, "This narration has no files yet");
    }
    (st.reveal)(&target);
    ok_json(json!({"path": target.to_string_lossy()}))
}

/// main.py `_client`: the TakeTwo window, the MCP server, or anything else.
fn client(headers: &HeaderMap) -> &'static str {
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty());
    match h("x-taketwo-client").or_else(|| h("x-voiceforge-client")) {
        Some("app") => "app",
        Some("mcp") => "mcp",
        _ => "api",
    }
}

/// The one engine TakeTwo 1.0 has (the `/api/engines` stub and the settings default).
const ENGINE_ID: &str = "qwen3-tts-1.7b";

async fn create_generation(State(st): St, headers: HeaderMap, body: Bytes) -> Response {
    let mut f = match Fields::parse(&body) {
        Ok(f) => f,
        Err(e) => return invalid(vec![e]),
    };
    let profile_id = f.text("profile_id", true);
    let text = f.text("text", true);
    let engine = f.text("engine", false);
    let language = f.text("language", false);
    let params = f.dict("params");
    let speed = f.float("speed", false);
    let voice_settings = f.dict("voice_settings");
    let title = f.text("title", false);
    if let Some(r) = finish_422(Vec::new(), Some(&f)) {
        return r;
    }
    let (Some(profile_id), Some(text)) = (profile_id, text) else { return ise("checked above".into()) };
    let run = || -> Result<Response, String> {
        let Some(p) = st.lib.get_profile(&profile_id)? else { return Ok(detail(404, "Voice not found")) };
        if p.get("status").and_then(Value::as_str) != Some("ready") {
            return Ok(detail(400, "This voice is still being analyzed."));
        }
        // updates.blocking_reason() is not ported: TakeTwo 1.0 has no updater.
        let s = library::get_settings(&st.lib.settings_path())?;
        let first = [engine.clone().map(Value::from), p.get("default_engine").cloned(), s.get("default_engine").cloned()]
            .into_iter()
            .flatten()
            .find(py::truthy);
        let engine = match first.as_ref().and_then(Value::as_str) {
            Some(ENGINE_ID) => ENGINE_ID,
            _ => return Ok(detail(404, "Unknown model")),
        };
        let words = py::split(&text).len() as i64;
        if words > MAX_SCRIPT_WORDS {
            return Ok(detail(400, &format!(
                "Script is {} words. The limit is {} per narration; split it into parts.",
                pipeline::thousands(words), pipeline::thousands(MAX_SCRIPT_WORDS)
            )));
        }
        // The duplicate check needs the queue: S07b.
        let language = [language.clone().map(Value::from), p.get("language").cloned()]
            .into_iter()
            .flatten()
            .find(py::truthy)
            .unwrap_or_else(|| "en".into());
        let req = NewNarration {
            profile: &p, text: &text, engine, language, params: params.clone(), speed, voice_settings: voice_settings.as_object(),
            title: title.as_deref(), source: client(&headers),
        };
        match pipeline::create_generation("unsaved", &req, &s) {
            Ok(_) => Ok(not_yet()), // S07b: save the document and queue the render; nothing is written yet
            Err(Fail::Value(m)) => Ok(detail(400, &m)),
            Err(Fail::Other(e)) => Err(e),
        }
    };
    run().unwrap_or_else(ise)
}

async fn resume_generation(State(st): St, Path(gid): Path<String>) -> Response {
    match live_gen(&st.lib, &gid) {
        Ok(_) => not_yet(), // S07b: queue the unfinished parts
        Err(r) => r,
    }
}

async fn regenerate(State(st): St, Path((gid, ci)): Path<(String, String)>, body: Bytes) -> Response {
    let (ci, errors, mut f) = part_and_body(&ci, &body);
    let new_text = f.as_mut().and_then(|f| f.text("text", false));
    if let Some(r) = finish_422(errors, f.as_ref()) {
        return r;
    }
    let Some(ci) = ci else { return ise("checked above".into()) };
    let g = match live_gen(&st.lib, &gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    match chunk_count(&g) {
        Ok(n) if 0 <= ci && ci < n => {}
        Ok(_) => return detail(404, "No such chunk"),
        Err(r) => return r,
    }
    if let Some(t) = new_text.as_deref().filter(|t| !py::strip(t).is_empty()) {
        let s = match library::get_settings(&st.lib.settings_path()) {
            Ok(s) => s,
            Err(e) => return ise(e),
        };
        match pipeline::clean_replacement(t, &g, &s) {
            Ok(_) => {}
            Err(Fail::Value(m)) => return detail(400, &m),
            Err(Fail::Other(e)) => return ise(e),
        }
    }
    not_yet() // S07b: re-speak the part
}

async fn select_take(State(st): St, Path((gid, ci)): Path<(String, String)>, body: Bytes) -> Response {
    let (ci, errors, mut f) = part_and_body(&ci, &body);
    let take = f.as_mut().and_then(|f| f.int("take"));
    if let Some(r) = finish_422(errors, f.as_ref()) {
        return r;
    }
    let (Some(ci), Some(take)) = (ci, take) else { return ise("checked above".into()) };
    let g = match idle_gen(&st.lib, &gid, ci) {
        Ok(g) => g,
        Err(r) => return r,
    };
    let kept = chunk(&g, ci).get("takes").and_then(Value::as_array).is_some_and(|ts| {
        ts.iter().any(|x| x.get("take").and_then(Value::as_f64).is_some_and(|k| k == take as f64))
    });
    if !kept {
        return detail(404, "That take is no longer kept.");
    }
    not_yet() // S07b: switch the take and re-mix
}

async fn fix_remaining(State(st): St, Path(gid): Path<String>) -> Response {
    let g = match live_gen(&st.lib, &gid) {
        Ok(g) => g,
        Err(r) => return r,
    };
    match review::fix_targets(&g) {
        Ok(t) if t.is_empty() => detail(400, "Nothing left to fix."),
        Ok(_) => not_yet(), // S07b: re-record the parts that need it
        Err(e) => ise(e),
    }
}

// ------------------------------------------------------------------ not in 1.0 yet

fn method_not_allowed() -> Response {
    detail(405, "Method Not Allowed")
}

/// POST on `/api/profiles/{pid}`: only `upgrade-all` exists there, as in Python.
async fn profile_post(Path(pid): Path<String>) -> Response {
    if pid == "upgrade-all" { not_yet() } else { method_not_allowed() }
}

/// POST on `/api/generations/{gid}`: only `reveal` exists there.
async fn generation_post(State(st): St, Path(gid): Path<String>) -> Response {
    if gid == "reveal" { reveal_narrations_folder(&st) } else { method_not_allowed() }
}

/// Write routes whose id Python checks with `_item_dir` first.
async fn delete_checked(Path(id): Path<String>) -> Response {
    item_id_ok(&id).err().unwrap_or_else(not_yet)
}

async fn design_audio(Path((did, _name)): Path<(String, String)>) -> Response {
    item_id_ok(&did).err().unwrap_or_else(not_yet)
}

// ------------------------------------------------------------------ UI

/// main.py `spa()`: a file inside the UI folder, anything else gets index.html with `Cache-Control: no-cache`.
async fn spa(root: &std::path::Path, rel: &str, method: &Method, headers: &HeaderMap) -> Response {
    let Ok(root) = root.canonicalize() else { return ise("UI folder is gone".into()) };
    if !rel.is_empty()
        && let Ok(f) = root.join(rel).canonicalize()
        && f.is_file()
        && f.starts_with(&root)
    {
        return files::serve(&f, method, headers, &files::guess_type(&f.to_string_lossy()), &[]).await;
    }
    let index = root.join("index.html");
    if !index.is_file() {
        return ise(format!("{} does not exist", index.display()));
    }
    files::serve(&index, method, headers, &files::guess_type("index.html"), &[(header::CACHE_CONTROL, "no-cache".into())]).await
}

/// The `/assets` mount: files inside `<ui>/assets` only.
async fn assets(State(st): St, method: Method, headers: HeaderMap, uri: Uri) -> Response {
    let Some(ui) = &st.ui else { return detail(404, "Not Found") };
    let rel = percent_decode_str(uri.path().trim_start_matches("/assets/")).decode_utf8_lossy().into_owned();
    let dir = ui.join("assets");
    if let (Ok(d), Ok(f)) = (dir.canonicalize(), dir.join(&rel).canonicalize())
        && f.is_file()
        && f.starts_with(&d)
    {
        return files::serve(&f, &method, &headers, &files::guess_type(&f.to_string_lossy()), &[]).await;
    }
    detail(404, "Not Found")
}

async fn fallback(State(st): St, method: Method, headers: HeaderMap, uri: Uri) -> Response {
    match &st.ui {
        None => detail(404, "Not Found"),
        Some(_) if method != Method::GET && method != Method::HEAD => method_not_allowed(),
        Some(ui) => {
            let rel = percent_decode_str(uri.path().trim_start_matches('/')).decode_utf8_lossy().into_owned();
            spa(ui, &rel, &method, &headers).await
        }
    }
}

// ------------------------------------------------------------------ the router

pub fn router(state: Arc<AppState>) -> Router {
    let ui = state.ui.is_some();
    let mut api = Router::new()
        .route("/api/health", get(health))
        .route("/api/pipeline-version", get(pipeline_version))
        .route("/api/status", get(status))
        .route("/api/update", get(update_state))
        .route("/api/update/{action}", post(|| async { not_yet() }))
        .route("/api/diagnostics", get(diagnostics))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/engines", get(list_engines))
        .route("/api/engines/custom", post(|| async { not_yet() }))
        .route("/api/engines/custom/{engine_id}", delete(|| async { not_yet() }))
        .route("/api/engines/{engine_id}/download", get(|| async { not_yet() }).post(|| async { not_yet() }))
        .route("/api/catalog/library", get(catalog_library))
        .route("/api/catalog/upgrade", get(|| async { not_yet() }).post(|| async { not_yet() }))
        .route("/api/catalog/discover", get(|| async { not_yet() }))
        .route("/api/integrations", get(integrations))
        .route("/api/profiles", get(list_profiles).post(|| async { not_yet() }))
        .route("/api/profiles/{pid}", get(get_profile).post(profile_post).patch(|| async { not_yet() }).delete(delete_checked))
        .route("/api/profiles/{pid}/analysis", get(get_analysis))
        .route("/api/profiles/{pid}/preview", get(profile_preview))
        .route("/api/profiles/{pid}/reanalyze", post(|| async { not_yet() }))
        .route("/api/profiles/{pid}/refresh", post(|| async { not_yet() }))
        .route("/api/profiles/{pid}/audio/{name}", get(profile_audio))
        .route("/api/generations", get(list_generations).post(create_generation))
        .route(
            "/api/generations/{gid}",
            get(get_generation).post(generation_post).patch(patch_generation).delete(delete_generation),
        )
        .route("/api/generations/{gid}/cancel", post(cancel_generation))
        .route("/api/generations/{gid}/resume", post(resume_generation))
        .route("/api/generations/{gid}/reveal", post(reveal_narration))
        .route("/api/generations/{gid}/restore", post(restore_generation))
        .route("/api/generations/{gid}/fix-remaining", post(fix_remaining))
        .route("/api/generations/{gid}/subtitles", get(subtitles))
        .route("/api/generations/{gid}/timings", get(generation_timings))
        .route("/api/generations/{gid}/audio", get(generation_audio))
        .route("/api/generations/{gid}/chunks/{ci}/audio", get(chunk_audio))
        .route("/api/generations/{gid}/chunks/{ci}/regenerate", post(regenerate))
        .route("/api/generations/{gid}/chunks/{ci}/accept", post(accept_part).delete(unaccept_part))
        .route("/api/generations/{gid}/chunks/{ci}/select-take", post(select_take))
        .route("/api/generations/{gid}/chunks/{ci}/takes/{k}/audio", get(take_audio))
        .route("/api/jobs/{jid}", get(get_job))
        .route("/api/transcripts", get(|| async { not_yet() }).post(|| async { not_yet() }))
        .route("/api/transcripts/{tid}", get(|| async { not_yet() }).delete(delete_checked))
        .route("/api/transcripts/{tid}/retry", post(|| async { not_yet() }))
        .route("/api/transcripts/{tid}/export", get(|| async { not_yet() }))
        .route("/api/transcripts/{tid}/audio", get(|| async { not_yet() }))
        .route("/api/designs", post(|| async { not_yet() }))
        .route("/api/designs/{did}", get(|| async { not_yet() }))
        .route("/api/designs/{did}/audio/{name}", get(design_audio))
        .route("/api/designs/{did}/save", post(|| async { not_yet() }))
        .method_not_allowed_fallback(|| async { method_not_allowed() })
        .fallback(fallback);
    if ui {
        api = api.route("/assets/{*rest}", get(assets));
    }
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::exact(HeaderValue::from_static(DEV_UI_ORIGIN)))
        .allow_methods(AllowMethods::mirror_request())
        .allow_headers(AllowHeaders::mirror_request());
    let inner = api.with_state(state).layer(cors);
    // Outermost, so it runs before routing, for routes that do not exist too.
    // The outer router has no routes, so its layer runs before the inner router routes anything.
    Router::new().fallback_service(inner).layer(middleware::from_fn(only_this_computer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_name_strips_only_a_trailing_port() {
        assert_eq!(host_name("127.0.0.1:8790"), "127.0.0.1");
        assert_eq!(host_name("[::1]:8790"), "[::1]");
        assert_eq!(host_name("[::1]"), "[::1]");
        assert_eq!(host_name("localhost:abc"), "localhost:abc");
        assert_eq!(host_name("localhost:"), "localhost:");
    }

    #[test]
    fn ids_and_ints() {
        assert!(item_id_ok("ab_C-9").is_ok());
        for bad in ["", "a.b", "a b", "é", &"a".repeat(65)] {
            assert!(item_id_ok(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_int(" -3 "), Some(-3));
        assert_eq!(parse_int("+7"), Some(7));
        assert_eq!(parse_int("1.0"), None);
        assert_eq!(parse_int("x"), None);
        assert_eq!(parse_int("99999999999999999999"), Some(i64::MAX));
        assert_eq!(parse_bool("YES"), Some(true));
        assert_eq!(parse_bool("maybe"), None);
    }
}
