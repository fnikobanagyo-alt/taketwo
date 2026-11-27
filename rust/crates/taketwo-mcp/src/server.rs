//! The rmcp server: the tool list (names, descriptions, input schemas as FastMCP gives them) and the calls.

use crate::tools::{Out, Tools};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// A parameter: name, JSON type, and its default (`None` = required; `Some(Null)` = optional, `X | None`).
type Param = (&'static str, &'static str, Option<Value>);

/// The docstrings of mcp/taketwo_mcp.py as `fn.__doc__` holds them (Python 3.13+ strips the common indent).
/// `tests/tools.rs` compares them with the Python file.
pub fn tool_table() -> Vec<(&'static str, &'static str, Vec<Param>)> {
    let id = || ("narration_id", "string", None);
    vec![
        ("list_voices", "List the cloned voices in TakeTwo that are ready to use: id, name, language, description,
speech_rate_wps (typical words per second, for estimating length) and studio.", vec![]),
        ("generate_narration", "Start a narration of `text` in a TakeTwo voice (name or id from list_voices).

Returns at once with a narration id; rendering takes about one to two seconds per second
of audio. Poll get_status with the id, then call get_audio when the status is \"done\".
Send a whole script as one narration and cut video with get_timings, rather than joining several.
speed: 0.7 to 1.3 (1.0 = the voice's natural pace; faster also shortens pauses).
style: 0 to 1, how expressive (default 0 = the voice's chosen clip). Leave unset for the voice's default.
stability: 0 to 1, more varied <-> steadier delivery (default 0.5). Leave unset for the voice's default.
", vec![("text", "string", None), ("voice", "string", None), ("speed", "number", Some(json!(1.0))),
        ("style", "number", Some(Value::Null)), ("stability", "number", Some(Value::Null))]),
        ("get_status", "Progress of a narration.

status: queued, running, done, done_needs_listen, error or cancelled (stopped; resume_narration continues it).
review_status: the same, except a finished narration with doubtful parts is \"done_needs_review\".
done_needs_listen is the legacy name of done_needs_review; prefer review_status.
Also: parts_done/parts_total, eta_seconds (while rendering), queue_ahead (while queued), open_in_app
(a link to the narration in the TakeTwo app) and, once done, parts_needing_review: each with its
1-based part number, text, reasons, issues, start/end seconds in the finished audio and checked_by (which automated checks found it: \"take check\", \"final check\", \"script check\").
Doubtful parts are still playable; ask the user to listen to them, or call respeak_part / fix_remaining.
", vec![id()]),
        ("get_timings", "Start and end (seconds, in the finished file after speed) of every sentence and script word of a finished
narration, for cutting video to the words. \"estimated\": true means some parts predate word timing and were
shared out by character count.", vec![id()]),
        ("get_audio", "Save a finished narration and return the file path. format: \"mp3\" or \"wav\".

By default the file goes to ~/Downloads/TakeTwo. output_path (optional) saves it there instead: it must end
in .mp3 or .wav matching format; ~ is expanded and missing folders are created. An existing file is never
replaced unless overwrite=True. The result includes outcome (\"clean\" or \"needs_listen\"); when it is
\"needs_listen\" it also has a warning and parts_needing_review, which the user should hear before using it.
", vec![id(), ("format", "string", Some(json!("mp3"))), ("output_path", "string", Some(Value::Null)),
        ("overwrite", "boolean", Some(json!(false)))]),
        ("get_subtitles", "Subtitles of a finished narration as text. format: \"srt\", \"vtt\" or \"txt\" (the spoken script without timings).",
         vec![id(), ("format", "string", Some(json!("srt")))]),
        ("find_narration", "Search past narrations, newest first. All filters are optional.

text: words from the title or script. voice: a voice name or id.
status: needs_review, done, running, queued, error or cancelled.
Each result has id (use it as narration_id), title, voice, status, outcome, created, duration and source.
", vec![("text", "string", Some(Value::Null)), ("voice", "string", Some(Value::Null)), ("status", "string", Some(Value::Null))]),
        ("respeak_part", "Record one part of a narration again, keeping every other part as it is.

part: the 1-based number from parts_needing_review in get_status. text (optional): new wording for that
part; leave unset to re-record the same words. Fails while the narration is busy. Poll get_status afterwards.
", vec![id(), ("part", "integer", None), ("text", "string", Some(Value::Null))]),
        ("fix_remaining", "Re-record every part of a narration that still needs review (the app's Fix button). Poll get_status afterwards.
Fails with \"Nothing left to fix.\" when no part needs it.", vec![id()]),
        ("stop_narration", "Stop a queued or running narration. Finished parts are kept; resume_narration continues it later.", vec![id()]),
        ("resume_narration", "Continue a stopped or failed narration from its unfinished parts. Fails if it is already running.", vec![id()]),
        ("mark_sounds_fine", "Mark a part as checked by a person, so it no longer needs review. undo=True removes the mark.

Use this ONLY when the user has listened to that part and says it sounds fine. Never call it on your own
judgement or because a measurement looks good: you cannot hear the audio.
part: the 1-based number from parts_needing_review in get_status.
", vec![id(), ("part", "integer", None), ("undo", "boolean", Some(json!(false)))]),
    ]
}

/// The input schema pydantic writes for the arguments model (keys sorted, as pydantic does).
fn schema(tool: &str, params: &[Param]) -> Map<String, Value> {
    let mut props = Map::new();
    let mut required = Vec::new();
    for (name, ty, default) in params {
        let title: String = name.split('_').map(|w| w[..1].to_uppercase() + &w[1..]).collect::<Vec<_>>().join(" ");
        let p = match default {
            None => {
                required.push(json!(name));
                json!({"title": title, "type": ty})
            }
            Some(Value::Null) => json!({"anyOf": [{"type": ty}, {"type": "null"}], "default": null, "title": title}),
            Some(d) => json!({"default": d, "title": title, "type": ty}),
        };
        props.insert(name.to_string(), p);
    }
    let mut s = Map::new();
    s.insert("properties".into(), Value::Object(props));
    if !required.is_empty() {
        s.insert("required".into(), Value::Array(required));
    }
    s.insert("title".into(), json!(format!("{tool}Arguments")));
    s.insert("type".into(), json!("object"));
    s
}

pub fn tool_list() -> Vec<Tool> {
    tool_table().into_iter().map(|(name, desc, params)| Tool::new(name, desc, Arc::new(schema(name, &params)))).collect()
}

/// A tool's result as FastMCP sends it: an object as one indented JSON text, a list as one text per item,
/// plain text as it is; the structured copy is the object itself or `{"result": ...}` for a list or text.
/// shortcut: no outputSchema is declared (FastMCP declares one); upgrade if a client needs it.
pub fn to_result(out: Result<Out, String>) -> CallToolResult {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).expect("JSON");
    match out {
        Ok(Out::Json(Value::Array(items))) => {
            let mut r = CallToolResult::success(items.iter().map(|i| ContentBlock::text(pretty(i))).collect());
            r.structured_content = Some(json!({"result": items}));
            r
        }
        Ok(Out::Json(v)) => {
            let mut r = CallToolResult::success(vec![ContentBlock::text(pretty(&v))]);
            r.structured_content = Some(v);
            r
        }
        Ok(Out::Text(t)) => {
            let mut r = CallToolResult::success(vec![ContentBlock::text(t.clone())]);
            r.structured_content = Some(json!({"result": t}));
            r
        }
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
    }
}

pub struct Server {
    pub name: String,
    pub tools: Arc<Tools>,
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(self.name.clone(), env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tool_list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tools = self.tools.clone();
        let args = request.arguments.unwrap_or_default();
        let name = request.name.to_string();
        // The tools block on HTTP (and on starting the engine), so they run off the protocol thread.
        let out = tokio::task::spawn_blocking(move || tools.run(&name, &args))
            .await
            .unwrap_or_else(|e| Err(format!("the tool stopped: {e}")));
        Ok(to_result(out).into())
    }
}
