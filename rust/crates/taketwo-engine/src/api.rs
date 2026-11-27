//! The HTTP API, ported from backend/app/main.py (read side, step S06): the same routes, status codes,
//! `{"detail": ...}` texts and JSON key order. Write routes answer 501 until S07.

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
use taketwo_core::text::{self, Pronunciation, py};
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

fn pronunciations(s: &Doc) -> Vec<Pronunciation> {
    let field = |p: &Value, k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    s.get("pronunciations")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|p| Pronunciation { from: field(p, "from"), to: field(p, "to") }).collect())
        .unwrap_or_default()
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
            let strip = s.get("strip_citations").is_none_or(py::truthy);
            py::strip(&text::clean_text(&same, &pronunciations(&s), strip)).to_string()
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
    let mut g = match live_gen(&st.lib, &gid) {
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

// ------------------------------------------------------------------ not in 1.0 yet

fn method_not_allowed() -> Response {
    detail(405, "Method Not Allowed")
}

/// POST on `/api/profiles/{pid}`: only `upgrade-all` exists there, as in Python.
async fn profile_post(Path(pid): Path<String>) -> Response {
    if pid == "upgrade-all" { not_yet() } else { method_not_allowed() }
}

/// POST on `/api/generations/{gid}`: only `reveal` exists there.
async fn generation_post(Path(gid): Path<String>) -> Response {
    if gid == "reveal" { not_yet() /* S07 */ } else { method_not_allowed() }
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
        .route("/api/generations", get(list_generations).post(|| async { not_yet() /* S07 */ }))
        .route(
            "/api/generations/{gid}",
            get(get_generation).post(generation_post).patch(|| async { not_yet() /* S07 */ }).delete(delete_checked /* S07 */),
        )
        .route("/api/generations/{gid}/cancel", post(cancel_generation))
        .route("/api/generations/{gid}/resume", post(|| async { not_yet() /* S07 */ }))
        .route("/api/generations/{gid}/reveal", post(|| async { not_yet() /* S07 */ }))
        .route("/api/generations/{gid}/restore", post(|| async { not_yet() /* S07 */ }))
        .route("/api/generations/{gid}/fix-remaining", post(|| async { not_yet() /* S07 */ }))
        .route("/api/generations/{gid}/subtitles", get(subtitles))
        .route("/api/generations/{gid}/timings", get(generation_timings))
        .route("/api/generations/{gid}/audio", get(generation_audio))
        .route("/api/generations/{gid}/chunks/{ci}/audio", get(chunk_audio))
        .route("/api/generations/{gid}/chunks/{ci}/regenerate", post(|| async { not_yet() /* S07 */ }))
        .route("/api/generations/{gid}/chunks/{ci}/accept", post(|| async { not_yet() /* S07 */ }).delete(|| async { not_yet() /* S07 */ }))
        .route("/api/generations/{gid}/chunks/{ci}/select-take", post(|| async { not_yet() /* S07 */ }))
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
