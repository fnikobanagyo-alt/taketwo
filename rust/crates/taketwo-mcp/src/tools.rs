//! The 12 tools of mcp/taketwo_mcp.py, ported line by line over the `Transport` seam.
//! Each tool returns `Out` or the text of the Python exception it would raise.

use crate::http::{Reply, Transport};
use crate::py;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Map, Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use taketwo_core::text::py::{split, strip, truthy};

/// `urllib.parse.quote(s, safe="")`: everything but letters, digits and `_.-~` is quoted.
const QUOTE: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'.').remove(b'-').remove(b'~');
const TIMEOUT: Duration = Duration::from_secs(30);
const AUDIO_TIMEOUT: Duration = Duration::from_secs(120);
pub const NOT_RUNNING: &str = "TakeTwo is not running and could not be opened. Open the TakeTwo app, then try again.";

/// What a tool gives back: an object or list (sent as JSON text), or plain text (`get_subtitles`).
#[derive(Debug, PartialEq)]
pub enum Out {
    Json(Value),
    Text(String),
}

/// `_q`: quote one URL path segment.
pub fn q(segment: &str) -> String {
    utf8_percent_encode(segment, QUOTE).to_string()
}

/// `urllib.parse.urlencode` of text values (`quote_plus`: the `_q` set, space as `+`).
pub fn urlencode(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", q(k).replace("%20", "+"), q(v).replace("%20", "+")))
        .collect::<Vec<_>>()
        .join("&")
}

/// `" ".join(s.split())`.
fn collapse(s: &str) -> String {
    split(s).join(" ")
}

pub type EngineFn = dyn Fn() -> Result<String, String> + Send + Sync;

pub struct Tools {
    pub http: Arc<dyn Transport>,
    /// `_engine()`: the engine's base URL, or the "not running" text.
    pub engine: Arc<EngineFn>,
    /// `DOWNLOADS`, `~/Downloads/TakeTwo`.
    pub downloads: PathBuf,
}

fn get<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&Value::Null)
}

impl Tools {
    fn base(&self) -> Result<String, String> {
        (self.engine)()
    }

    fn send(&self, method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Reply, String> {
        let url = self.base()? + path;
        let data = body.map(|b| serde_json::to_vec(b).expect("JSON body"));
        self.http.send(method, &url, data, timeout).map_err(|e| format!("<urlopen error {e}>"))
    }

    /// `_detail`: the "detail" of an error body, or None when the body is not a JSON object.
    fn detail(reply: &mut Reply) -> Value {
        let mut buf = Vec::new();
        let _ = reply.body.read_to_end(&mut buf);
        match serde_json::from_slice::<Value>(&buf) {
            Ok(Value::Object(o)) => o.get("detail").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    /// `_error`: "TakeTwo returned <code>: <message or reason or 'no details'>".
    fn error(reply: &Reply, detail: &Value) -> String {
        let msg = if detail.is_object() { get(detail, "message").clone() } else { detail.clone() };
        let text = if truthy(&msg) {
            py::str_(&msg)
        } else if !reply.reason.is_empty() {
            reply.reason.clone()
        } else {
            "no details".into()
        };
        format!("TakeTwo returned {}: {text}", reply.status)
    }

    /// `_call`: a JSON request. HTTP errors become the `_error` text, except a duplicate narration's 409.
    pub fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let mut reply = self.send(method, path, body, TIMEOUT)?;
        if reply.status >= 400 {
            let detail = Self::detail(&mut reply);
            if detail.is_object() && truthy(get(&detail, "duplicate_of")) {
                let mut out = Map::new();
                out.insert("error".into(), json!(reply.status));
                out.extend(detail.as_object().unwrap().clone());
                return Ok(Value::Object(out));
            }
            return Err(Self::error(&reply, &detail));
        }
        let mut buf = Vec::new();
        reply.body.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        serde_json::from_slice(&buf).map_err(|e| format!("JSONDecodeError: {e}"))
    }

    /// `_open`: a GET of a non-JSON body (subtitles, audio); HTTP errors as in `call`.
    fn open(&self, path: &str, timeout: Duration) -> Result<Box<dyn Read + Send>, String> {
        let mut reply = self.send("GET", path, None, timeout)?;
        if reply.status >= 400 {
            let detail = Self::detail(&mut reply);
            return Err(Self::error(&reply, &detail));
        }
        Ok(reply.body)
    }

    fn voice_id(&self, voice: &str) -> Result<String, String> {
        let all = self.call("GET", "/api/profiles", None)?;
        let voices: Vec<&Value> = all.as_array().into_iter().flatten().filter(|v| get(v, "status") == "ready").collect();
        let want = collapse(&voice.to_lowercase());
        let name = |v: &Value| get(v, "name").as_str().unwrap_or("").to_string();
        for v in &voices {
            if get(v, "id") == voice || collapse(&name(v).to_lowercase()) == want {
                return Ok(py::str_(get(v, "id")));
            }
        }
        let matches: Vec<&&Value> = voices.iter().filter(|v| collapse(&name(v).to_lowercase()).contains(&want)).collect();
        if matches.len() == 1 {
            return Ok(py::str_(get(matches[0], "id")));
        }
        let pool: Vec<&Value> = if matches.is_empty() { voices.clone() } else { matches.iter().map(|v| **v).collect() };
        let names = pool.iter().map(|v| collapse(&name(v))).collect::<Vec<_>>().join(", ");
        let lead = if matches.is_empty() { "No voice matches" } else { "Several voices match" };
        Err(format!("{lead} {}. Choose one of: {names}", py::repr_str(voice)))
    }

    pub fn list_voices(&self) -> Result<Out, String> {
        let all = self.call("GET", "/api/profiles", None)?;
        let out: Vec<Value> = all
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| get(v, "status") == "ready")
            .map(|v| {
                json!({"id": get(v, "id"), "name": collapse(get(v, "name").as_str().unwrap_or("")),
                       "language": v.get("language").cloned().unwrap_or(json!("en")),
                       "description": get(v, "description"), "speech_rate_wps": get(v, "speech_rate_wps"),
                       "studio": get(v, "studio")})
            })
            .collect();
        Ok(Out::Json(Value::Array(out)))
    }

    pub fn generate_narration(&self, text: &str, voice: &str, speed: f64, style: Option<f64>, stability: Option<f64>) -> Result<Out, String> {
        let mut body = json!({"profile_id": self.voice_id(voice)?, "text": text, "speed": speed});
        let mut vs = Map::new();
        for (k, v) in [("style", style), ("stability", stability)] {
            if let Some(v) = v {
                vs.insert(k.into(), json!(v));
            }
        }
        if !vs.is_empty() {
            body["voice_settings"] = Value::Object(vs);
        }
        let r = self.call("POST", "/api/generations", Some(&body))?;
        if truthy(get(&r, "duplicate_of")) {
            return Ok(Out::Json(json!({"narration_id": get(&r, "duplicate_of"), "note": get(&r, "message")})));
        }
        let parts = get(&r, "chunks").as_array().map_or(0, Vec::len);
        Ok(Out::Json(json!({"narration_id": get(&r, "id"), "parts": parts, "status": "queued"})))
    }

    pub fn get_status(&self, narration_id: &str) -> Result<Out, String> {
        let g = self.call("GET", &format!("/api/generations/{}", q(narration_id)), None)?;
        let chunks = get(&g, "chunks").as_array().cloned().unwrap_or_default();
        let done = chunks.iter().filter(|c| matches!(get(c, "status").as_str(), Some("ok" | "flagged"))).count();
        let job = get(&g, "job");
        let status = if truthy(job) && get(job, "status") == "running" {
            json!("running")
        } else if truthy(job) {
            json!("queued")
        } else {
            get(&g, "status").clone()
        };
        let queue = get(&g, "queue");
        let mut out = json!({"status": status, "review_status": status, "parts_done": done, "parts_total": chunks.len(),
            "message": g.get("message").cloned().unwrap_or(json!("")), "eta_seconds": get(&g, "eta"),
            "queue_ahead": if truthy(queue) { get(queue, "ahead").clone() } else { Value::Null },
            "open_in_app": format!("{}/#/history/{}", self.base()?, q(narration_id))});
        if status == "done" {
            let doubtful = get(&g, "outcome") == "needs_listen";
            out["status"] = json!(if doubtful { "done_needs_listen" } else { "done" });
            out["review_status"] = json!(if doubtful { "done_needs_review" } else { "done" });
            out["duration_seconds"] = get(&g, "duration").clone();
            out["parts_needing_review"] = review(&g);
        }
        Ok(Out::Json(out))
    }

    pub fn get_timings(&self, narration_id: &str) -> Result<Out, String> {
        Ok(Out::Json(self.call("GET", &format!("/api/generations/{}/timings", q(narration_id)), None)?))
    }

    pub fn get_audio(&self, narration_id: &str, format: &str, output_path: Option<&str>, overwrite: bool) -> Result<Out, String> {
        let fmt = if format.to_lowercase() == "wav" { "wav" } else { "mp3" };
        let mut dest = None;
        if let Some(op) = output_path.filter(|p| !p.is_empty()) {
            let d = expanduser(op);
            let suffix = d.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase()));
            if suffix.as_deref() != Some(&format!(".{fmt}")) {
                return Err(format!(
                    "output_path must end in .{fmt} to match format={} (got {}).",
                    py::repr_str(fmt),
                    py::repr_str(op)
                ));
            }
            if d.exists() && !overwrite {
                return Err(format!("{} already exists. Choose another output_path or pass overwrite=True.", d.display()));
            }
            dest = Some(d);
        }
        let g = self.call("GET", &format!("/api/generations/{}", q(narration_id)), None)?;
        if get(&g, "status") != "done" || !truthy(get(&g, "file")) {
            return Err(format!(
                "The narration is not finished yet (status: {}). Check get_status first.",
                py::str_(get(&g, "status"))
            ));
        }
        let dest = match dest {
            Some(d) => d,
            None => {
                let raw = match get(&g, "title") {
                    t if truthy(t) => py::str_(t),
                    _ => "Narration".into(),
                };
                // shortcut: Rust's is_alphanumeric stands in for Python's str.isalnum (same for letters and digits;
                // a few rare numeric symbols differ); upgrade if a title with those ever matters.
                let kept: String =
                    raw.replace('…', "").chars().filter(|c| c.is_alphanumeric() || " -_,.'".contains(*c)).collect();
                let mut title: String = strip(&kept).chars().take(60).collect();
                if title.is_empty() {
                    title = "Narration".into();
                }
                let voice = collapse(get(&g, "profile_name").as_str().unwrap_or(""));
                self.downloads.join(format!("{voice} - {title} ({narration_id}).{fmt}"))
            }
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut part_name = dest.file_name().unwrap_or_default().to_os_string();
        part_name.push(".part");
        let part = dest.with_file_name(part_name); // a failed download never leaves a half file at dest
        let saved = (|| {
            let mut r = self.open(&format!("/api/generations/{}/audio?fmt={fmt}", q(narration_id)), AUDIO_TIMEOUT)?;
            let mut f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
            std::io::copy(&mut r, &mut f).map_err(|e| e.to_string())?;
            drop(f);
            std::fs::rename(&part, &dest).map_err(|e| e.to_string())
        })();
        let _ = std::fs::remove_file(&part);
        saved?;
        let mut out = json!({"path": dest.to_string_lossy(), "duration_seconds": get(&g, "duration"), "format": fmt,
                             "outcome": get(&g, "outcome")});
        if get(&g, "outcome") == "needs_listen" {
            out["warning"] = json!("Some parts failed a quality check. Ask the user to listen to parts_needing_review before using this audio.");
            out["parts_needing_review"] = review(&g);
        }
        Ok(Out::Json(out))
    }

    pub fn get_subtitles(&self, narration_id: &str, format: &str) -> Result<Out, String> {
        let fmt = format.to_lowercase();
        if !["srt", "vtt", "txt"].contains(&fmt.as_str()) {
            return Err(format!("format must be \"srt\", \"vtt\" or \"txt\" (got {}).", py::repr_str(format)));
        }
        let mut r = self.open(&format!("/api/generations/{}/subtitles?fmt={fmt}", q(narration_id)), TIMEOUT)?;
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        String::from_utf8(buf).map(Out::Text).map_err(|e| format!("UnicodeDecodeError: {e}"))
    }

    pub fn find_narration(&self, text: Option<&str>, voice: Option<&str>, status: Option<&str>) -> Result<Out, String> {
        let params: Vec<(&str, &str)> = [("q", text), ("voice", voice), ("status", status)]
            .into_iter()
            .filter_map(|(k, v)| v.filter(|v| !v.is_empty()).map(|v| (k, v)))
            .collect();
        Ok(Out::Json(self.call("GET", &format!("/api/generations/search?{}", urlencode(&params)), None)?))
    }

    pub fn respeak_part(&self, narration_id: &str, part: &Value, text: Option<&str>) -> Result<Out, String> {
        let ci = part_index(part)?;
        let body = match text.filter(|t| !t.is_empty()) {
            Some(t) => json!({"text": t}),
            None => json!({}),
        };
        self.call("POST", &format!("/api/generations/{}/chunks/{ci}/regenerate", q(narration_id)), Some(&body))?;
        Ok(Out::Json(json!({"narration_id": narration_id, "part": part, "status": "queued"})))
    }

    pub fn fix_remaining(&self, narration_id: &str) -> Result<Out, String> {
        self.call("POST", &format!("/api/generations/{}/fix-remaining", q(narration_id)), None)?;
        Ok(Out::Json(json!({"narration_id": narration_id, "status": "queued"})))
    }

    pub fn stop_narration(&self, narration_id: &str) -> Result<Out, String> {
        self.call("POST", &format!("/api/generations/{}/cancel", q(narration_id)), None)?;
        Ok(Out::Json(json!({"narration_id": narration_id, "status": "stopping"})))
    }

    pub fn resume_narration(&self, narration_id: &str) -> Result<Out, String> {
        self.call("POST", &format!("/api/generations/{}/resume", q(narration_id)), None)?;
        Ok(Out::Json(json!({"narration_id": narration_id, "status": "queued"})))
    }

    pub fn mark_sounds_fine(&self, narration_id: &str, part: &Value, undo: bool) -> Result<Out, String> {
        let ci = part_index(part)?;
        let method = if undo { "DELETE" } else { "POST" };
        self.call(method, &format!("/api/generations/{}/chunks/{ci}/accept", q(narration_id)), None)?;
        Ok(Out::Json(json!({"narration_id": narration_id, "part": part, "sounds_fine": !undo})))
    }

    /// Run one tool by name on the arguments of a `tools/call` (missing optional ones take Python's defaults).
    pub fn run(&self, name: &str, args: &Map<String, Value>) -> Result<Out, String> {
        let a = Args { name, args };
        match name {
            "list_voices" => self.list_voices(),
            "generate_narration" => self.generate_narration(
                &a.text("text")?,
                &a.text("voice")?,
                a.number("speed")?.unwrap_or(1.0),
                a.number("style")?,
                a.number("stability")?,
            ),
            "get_status" => self.get_status(&a.text("narration_id")?),
            "get_timings" => self.get_timings(&a.text("narration_id")?),
            "get_audio" => self.get_audio(
                &a.text("narration_id")?,
                &a.opt_text("format")?.unwrap_or_else(|| "mp3".into()),
                a.opt_text("output_path")?.as_deref(),
                a.flag("overwrite")?,
            ),
            "get_subtitles" => self.get_subtitles(&a.text("narration_id")?, &a.opt_text("format")?.unwrap_or_else(|| "srt".into())),
            "find_narration" => {
                self.find_narration(a.opt_text("text")?.as_deref(), a.opt_text("voice")?.as_deref(), a.opt_text("status")?.as_deref())
            }
            "respeak_part" => self.respeak_part(&a.text("narration_id")?, &a.whole("part")?, a.opt_text("text")?.as_deref()),
            "fix_remaining" => self.fix_remaining(&a.text("narration_id")?),
            "stop_narration" => self.stop_narration(&a.text("narration_id")?),
            "resume_narration" => self.resume_narration(&a.text("narration_id")?),
            "mark_sounds_fine" => self.mark_sounds_fine(&a.text("narration_id")?, &a.whole("part")?, a.flag("undo")?),
            other => return Err(format!("Unknown tool: {other}")),
        }
        .map_err(|e| format!("Error executing tool {name}: {e}"))
    }
}

/// `_review`: the seven keys of each review item, missing ones null.
fn review(g: &Value) -> Value {
    let keys = ["part", "text", "reasons", "issues", "start", "end", "checked_by"];
    let items = get(g, "review").as_array().cloned().unwrap_or_default();
    Value::Array(items.iter().map(|r| Value::Object(keys.iter().map(|k| (k.to_string(), get(r, k).clone())).collect())).collect())
}

/// `_part_index`: MCP parts are numbered from 1, like the app shows them; the engine counts from 0.
pub fn part_index(part: &Value) -> Result<i64, String> {
    match part.as_i64() {
        Some(p) if p >= 1 && !part.is_boolean() => Ok(p - 1),
        _ => Err(format!(
            "part must be a whole number from 1 (got {}). See parts_needing_review in get_status.",
            py::repr(part)
        )),
    }
}

/// `Path(p).expanduser()`: a leading `~` or `~/` is the home folder.
fn expanduser(p: &str) -> PathBuf {
    let home = std::env::home_dir();
    match (p.strip_prefix('~'), home) {
        (Some(""), Some(h)) => h,
        (Some(rest), Some(h)) if rest.starts_with('/') || rest.starts_with(std::path::MAIN_SEPARATOR) => h.join(&rest[1..]),
        _ => Path::new(p).to_path_buf(),
    }
}

/// The arguments of one call, checked the way FastMCP's pydantic model would refuse them.
/// shortcut: the refusal texts are short ("<name>: Field required"), not pydantic's full report, and only JSON
/// types are accepted (no "1" for a number, no "true" for a flag); upgrade if a client depends on lax coercion.
struct Args<'a> {
    name: &'a str,
    args: &'a Map<String, Value>,
}

impl Args<'_> {
    fn bad(&self, key: &str, what: &str) -> String {
        format!("1 validation error for {}Arguments\n{key}\n  {what}", self.name)
    }
    fn required(&self, key: &str) -> Result<Value, String> {
        self.args.get(key).cloned().ok_or_else(|| self.bad(key, "Field required"))
    }
    /// An `int` parameter as pydantic passes it on: a whole float (2.0) becomes 2; anything else goes to
    /// `_part_index` unchanged, which refuses it with its own message.
    fn whole(&self, key: &str) -> Result<Value, String> {
        let v = self.required(key)?;
        Ok(match v.as_f64() {
            Some(f) if v.is_f64() && f.fract() == 0.0 && f.abs() < 1e15 => json!(f as i64),
            _ => v,
        })
    }
    fn text(&self, key: &str) -> Result<String, String> {
        match self.required(key)? {
            Value::String(s) => Ok(s),
            _ => Err(self.bad(key, "Input should be a valid string")),
        }
    }
    fn opt_text(&self, key: &str) -> Result<Option<String>, String> {
        match self.args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(self.bad(key, "Input should be a valid string")),
        }
    }
    fn number(&self, key: &str) -> Result<Option<f64>, String> {
        match self.args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_f64()),
            Some(_) => Err(self.bad(key, "Input should be a valid number")),
        }
    }
    fn flag(&self, key: &str) -> Result<bool, String> {
        match self.args.get(key) {
            None => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(self.bad(key, "Input should be a valid boolean")),
        }
    }
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
