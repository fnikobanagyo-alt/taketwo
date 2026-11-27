//! The 15 cases of backend/tests/test_mcp_tools.py, by name. No network: the `Transport` seam answers with
//! canned replies (the Python tests stub `_req` and `urlopen`), and `_engine()` is "http://engine".

use super::*;
use crate::http::Reply;
use std::io::Cursor;
use std::sync::Mutex;

type Answer = Box<dyn Fn(&str, &str) -> std::io::Result<Reply> + Send + Sync>;

struct Fake {
    calls: Mutex<Vec<(String, String, Option<Value>)>>,
    answer: Mutex<Answer>,
}

impl Transport for Fake {
    fn send(&self, method: &str, url: &str, body: Option<Vec<u8>>, _t: Duration) -> std::io::Result<Reply> {
        let path = url.strip_prefix("http://engine").expect("the stubbed engine").to_string();
        let body = body.map(|b| serde_json::from_slice(&b).unwrap());
        self.calls.lock().unwrap().push((method.into(), path, body));
        (self.answer.lock().unwrap())(method, url)
    }
}

fn reply(status: u16, body: Vec<u8>) -> Reply {
    Reply { status, reason: "Reason".into(), body: Box::new(Cursor::new(body)) }
}

struct T {
    fake: Arc<Fake>,
    m: Tools,
}

impl T {
    fn new() -> T {
        let fake = Arc::new(Fake { calls: Mutex::new(vec![]), answer: Mutex::new(Box::new(|_, _| Ok(reply(200, b"{}".to_vec())))) });
        let m = Tools { http: fake.clone(), engine: Arc::new(|| Ok("http://engine".into())), downloads: PathBuf::from("/nonexistent-downloads") };
        T { fake, m }
    }
    /// `self.reply = ...`: every request answers 200 with this JSON.
    fn reply(&self, v: Value) {
        *self.fake.answer.lock().unwrap() = Box::new(move |_, _| Ok(reply(200, serde_json::to_vec(&v).unwrap())));
    }
    fn answer(&self, f: impl Fn(&str, &str) -> std::io::Result<Reply> + Send + Sync + 'static) {
        *self.fake.answer.lock().unwrap() = Box::new(f);
    }
    fn calls(&self) -> Vec<(String, String, Option<Value>)> {
        self.fake.calls.lock().unwrap().clone()
    }
    fn paths(&self) -> Vec<String> {
        self.calls().into_iter().map(|c| c.1).collect()
    }
}

fn json_of(out: Out) -> Value {
    match out {
        Out::Json(v) => v,
        Out::Text(t) => panic!("expected JSON, got text {t:?}"),
    }
}

fn done() -> Value {
    json!({"status": "done", "file": "a.mp3", "outcome": "needs_listen", "duration": 12.5, "title": "T", "profile_name": "Ann",
        "chunks": [{"status": "ok"}, {"status": "flagged"}],
        "review": [{"part": 2, "text": "Hi.", "issues": ["repeat"], "reasons": ["Repeated words"], "start": 1.0, "end": 2.0,
                    "heard": "hi hi", "pitch_hz": 200, "voice_pitch_hz": 190, "checked_by": "whisper", "take": 1}]})
}

fn with(mut v: Value, k: &str, x: Value) -> Value {
    v[k] = x;
    v
}

#[test]
fn test_error_shapes_become_runtime_errors() {
    let t = T::new();
    for (body, want) in [
        (&br#"{"detail": "Already running"}"#[..], "409: Already running"),
        (br#"{"detail": {"message": "Busy now", "x": 1}}"#, "409: Busy now"),
        (b"<html>oops</html>", "409: Reason"),
        (br#"["a list"]"#, "409: Reason"),
        (b"", "409: Reason"),
    ] {
        let body = body.to_vec();
        t.answer(move |_, _| Ok(reply(409, body.clone())));
        let e = t.m.call("POST", "/api/x", None).unwrap_err();
        assert!(e.contains(want), "{e}");
        assert_eq!(e, format!("TakeTwo returned {want}"));
    }
    // An empty reason phrase falls back to "no details".
    t.answer(|_, _| Ok(Reply { status: 500, reason: String::new(), body: Box::new(Cursor::new(vec![])) }));
    assert_eq!(t.m.call("GET", "/api/x", None).unwrap_err(), "TakeTwo returned 500: no details");
}

#[test]
fn test_duplicate_narration_is_still_returned() {
    let t = T::new();
    t.answer(|_, _| Ok(reply(409, br#"{"detail": {"message": "Already in the queue", "duplicate_of": "g9"}}"#.to_vec())));
    let r = t.m.call("POST", "/api/generations", None).unwrap();
    assert_eq!(r["duplicate_of"], "g9");
    assert_eq!(r, json!({"error": 409, "message": "Already in the queue", "duplicate_of": "g9"}));
}

#[test]
fn test_respeak_part_two_is_chunk_one() {
    let t = T::new();
    t.m.respeak_part("g1", &json!(2), Some("New words.")).unwrap();
    assert_eq!(t.calls()[0], ("POST".into(), "/api/generations/g1/chunks/1/regenerate".into(), Some(json!({"text": "New words."}))));
    t.m.respeak_part("g1", &json!(1), None).unwrap();
    assert_eq!((t.calls()[1].1.as_str(), t.calls()[1].2.clone()), ("/api/generations/g1/chunks/0/regenerate", Some(json!({}))));
    for (bad, shown) in [(json!(0), "0"), (json!(-1), "-1"), (json!(1.5), "1.5"), (json!(true), "True")] {
        let e = t.m.respeak_part("g1", &bad, None).unwrap_err();
        assert_eq!(e, format!("part must be a whole number from 1 (got {shown}). See parts_needing_review in get_status."));
    }
    assert_eq!(t.calls().len(), 2, "a refused part sends nothing");
}

#[test]
fn test_mark_sounds_fine_and_undo() {
    let t = T::new();
    assert_eq!(json_of(t.m.mark_sounds_fine("g1", &json!(3), false).unwrap()), json!({"narration_id": "g1", "part": 3, "sounds_fine": true}));
    assert_eq!(json_of(t.m.mark_sounds_fine("g1", &json!(3), true).unwrap())["sounds_fine"], false);
    let got: Vec<(String, String)> = t.calls().into_iter().map(|c| (c.0, c.1)).collect();
    assert_eq!(got, vec![("POST".into(), "/api/generations/g1/chunks/2/accept".into()), ("DELETE".into(), "/api/generations/g1/chunks/2/accept".into())]);
}

#[test]
fn test_other_actions_hit_their_endpoints() {
    let t = T::new();
    t.m.fix_remaining("g1").unwrap();
    t.m.stop_narration("g1").unwrap();
    t.m.resume_narration("g1").unwrap();
    assert_eq!(t.paths(), vec!["/api/generations/g1/fix-remaining", "/api/generations/g1/cancel", "/api/generations/g1/resume"]);
}

#[test]
fn test_find_narration_passes_only_given_filters() {
    let t = T::new();
    t.reply(json!([{"id": "g1"}]));
    assert_eq!(json_of(t.m.find_narration(Some("big cats"), None, Some("needs_review")).unwrap()), json!([{"id": "g1"}]));
    assert_eq!(t.paths()[0], "/api/generations/search?q=big+cats&status=needs_review");
    t.m.find_narration(None, Some("Ann"), None).unwrap();
    assert_eq!(t.paths()[1], "/api/generations/search?voice=Ann");
    // quote_plus keeps `~` and quotes `*`, unlike form encoding.
    t.m.find_narration(Some("a~b*c&d"), None, Some("")).unwrap();
    assert_eq!(t.paths()[2], "/api/generations/search?q=a~b%2Ac%26d");
}

#[test]
fn test_ids_are_quoted() {
    let t = T::new();
    t.reply(json!({"status": "queued", "chunks": [], "job": {"status": "queued"}}));
    let out = json_of(t.m.get_status("../a b/?").unwrap());
    assert_eq!(t.paths()[0], "/api/generations/..%2Fa%20b%2F%3F");
    assert_eq!(out["open_in_app"], "http://engine/#/history/..%2Fa%20b%2F%3F");
    t.m.stop_narration("x/y").unwrap();
    assert_eq!(t.paths()[1], "/api/generations/x%2Fy/cancel");
}

#[test]
fn test_status_while_queued_and_running() {
    let t = T::new();
    t.reply(json!({"status": "running", "chunks": [{"status": "ok"}, {"status": "pending"}], "eta": 40,
                   "job": {"status": "queued"}, "queue": {"ahead": 2, "waiting_for": ["A"]}}));
    let out = json_of(t.m.get_status("g1").unwrap());
    assert_eq!((&out["status"], &out["review_status"], &out["queue_ahead"], &out["eta_seconds"], &out["parts_done"]),
               (&json!("queued"), &json!("queued"), &json!(2), &json!(40), &json!(1)));
    assert!(out.get("parts_needing_review").is_none());
    let keys: Vec<&String> = out.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["status", "review_status", "parts_done", "parts_total", "message", "eta_seconds", "queue_ahead", "open_in_app"]);
    t.reply(json!({"status": "running", "chunks": [], "job": {"status": "running"}}));
    assert_eq!(json_of(t.m.get_status("g1").unwrap())["status"], "running");
}

#[test]
fn test_status_when_done_needs_review() {
    let t = T::new();
    t.reply(done());
    let out = json_of(t.m.get_status("g1").unwrap());
    assert_eq!((&out["status"], &out["review_status"]), (&json!("done_needs_listen"), &json!("done_needs_review")));
    assert_eq!(out["open_in_app"], "http://engine/#/history/g1");
    assert_eq!(out["parts_needing_review"], json!([{"part": 2, "text": "Hi.", "reasons": ["Repeated words"], "issues": ["repeat"],
                                                   "start": 1.0, "end": 2.0, "checked_by": "whisper"}]));
    let keys: Vec<&String> = out["parts_needing_review"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["part", "text", "reasons", "issues", "start", "end", "checked_by"]);
    t.reply(with(with(done(), "outcome", json!("clean")), "review", json!([])));
    let out = json_of(t.m.get_status("g1").unwrap());
    assert_eq!((&out["status"], &out["review_status"], &out["parts_needing_review"]), (&json!("done"), &json!("done"), &json!([])));
}

#[test]
fn test_list_voices_includes_new_fields_for_ready_voices() {
    let t = T::new();
    t.reply(json!([{"id": "v1", "name": " Ann  B ", "status": "ready", "description": "warm", "speech_rate_wps": 2.6, "studio": true},
                   {"id": "v2", "name": "Cal", "status": "processing"}]));
    assert_eq!(json_of(t.m.list_voices().unwrap()), json!([{"id": "v1", "name": "Ann B", "language": "en", "description": "warm",
                                                            "speech_rate_wps": 2.6, "studio": true}]));
}

#[test]
fn test_subtitles_validate_format_and_return_text() {
    let t = T::new();
    assert_eq!(t.m.get_subtitles("g1", "docx").unwrap_err(), r#"format must be "srt", "vtt" or "txt" (got 'docx')."#);
    t.answer(|_, _| Ok(reply(200, b"1\n00:00 --> 00:01\nHi\n".to_vec())));
    assert_eq!(t.m.get_subtitles("g1", "VTT").unwrap(), Out::Text("1\n00:00 --> 00:01\nHi\n".into()));
    assert_eq!(t.paths(), vec!["/api/generations/g1/subtitles?fmt=vtt"]);
}

#[test]
fn test_subtitles_error_before_audio_is_clean() {
    let t = T::new();
    t.answer(|_, _| Ok(reply(404, br#"{"detail": "Subtitles are available once the audio is rendered"}"#.to_vec())));
    let e = t.m.get_subtitles("g1", "srt").unwrap_err();
    assert!(e.contains("404: Subtitles are available"), "{e}");
}

/// The narration document for `/api/generations/g1`, audio bytes for `/audio`.
fn audio_answers(t: &T, doc: Value, audio: &'static [u8]) {
    t.answer(move |_, url| {
        Ok(if url.contains("/audio?") { reply(200, audio.to_vec()) } else { reply(200, serde_json::to_vec(&doc).unwrap()) })
    });
}

#[test]
fn test_get_audio_warns_and_saves_to_output_path() {
    let t = T::new();
    audio_answers(&t, done(), b"ID3audio");
    let d = tempfile::tempdir().unwrap();
    let dest = d.path().join("new").join("out.mp3");
    let ds = dest.to_str().unwrap();
    let out = json_of(t.m.get_audio("g1", "mp3", Some(ds), false).unwrap());
    assert_eq!(std::fs::read(&dest).unwrap(), b"ID3audio");
    assert_eq!((&out["path"], &out["outcome"]), (&json!(ds), &json!("needs_listen")));
    assert!(out["warning"].as_str().unwrap().contains("listen"));
    assert_eq!(out["parts_needing_review"][0]["part"], 2);
    let keys: Vec<&String> = out.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["path", "duration_seconds", "format", "outcome", "warning", "parts_needing_review"]);
    let e = t.m.get_audio("g1", "mp3", Some(ds), false).unwrap_err();
    assert_eq!(e, format!("{ds} already exists. Choose another output_path or pass overwrite=True."));
    assert_eq!(std::fs::read(&dest).unwrap(), b"ID3audio");
    t.m.get_audio("g1", "mp3", Some(ds), true).unwrap();
    let wrong = d.path().join("out.mp3");
    let e = t.m.get_audio("g1", "wav", Some(wrong.to_str().unwrap()), false).unwrap_err();
    assert_eq!(e, format!("output_path must end in .wav to match format='wav' (got '{}').", wrong.display()));
    // Not finished: refused before anything is downloaded.
    audio_answers(&t, json!({"status": "cancelled", "chunks": []}), b"");
    let e = t.m.get_audio("g1", "wav", Some(d.path().join("x.wav").to_str().unwrap()), false).unwrap_err();
    assert_eq!(e, "The narration is not finished yet (status: cancelled). Check get_status first.");
}

#[test]
fn test_get_audio_clean_has_no_warning() {
    let t = T::new();
    audio_answers(&t, with(with(done(), "outcome", json!("clean")), "review", json!([])), b"RIFF");
    let d = tempfile::tempdir().unwrap();
    let out = json_of(t.m.get_audio("g1", "wav", Some(d.path().join("a.WAV").to_str().unwrap()), false).unwrap());
    assert_eq!(out["outcome"], "clean");
    assert!(out.get("warning").is_none());
    // The default name under the downloads folder, as Python builds it.
    let t = T { m: Tools { downloads: d.path().join("Downloads/TakeTwo"), ..t.m }, ..t };
    audio_answers(&t, with(with(done(), "title", json!("Big…cats: a/b?")), "profile_name", json!(" Ann  B ")), b"ID3");
    let out = json_of(t.m.get_audio("g1", "MP3", None, false).unwrap());
    let want = d.path().join("Downloads/TakeTwo/Ann B - Bigcats ab (g1).mp3");
    assert_eq!(out["path"], json!(want.to_str().unwrap()));
    assert_eq!(std::fs::read(&want).unwrap(), b"ID3");
}

struct Broken;
impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "dropped"))
    }
}

#[test]
fn test_a_failed_download_leaves_no_partial_file() {
    let t = T::new();
    t.answer(|_, url| {
        Ok(if url.contains("/audio?") {
            Reply { status: 200, reason: "OK".into(), body: Box::new(Broken) }
        } else {
            reply(200, serde_json::to_vec(&done()).unwrap())
        })
    });
    let d = tempfile::tempdir().unwrap();
    let dest = d.path().join("out.mp3");
    let e = t.m.get_audio("g1", "mp3", Some(dest.to_str().unwrap()), false).unwrap_err();
    assert!(e.contains("dropped"), "{e}");
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "neither out.mp3 nor out.mp3.part");
}

/// Not a Python case: `run` wraps failures as FastMCP does and applies the defaults.
#[test]
fn run_wraps_errors_and_applies_defaults() {
    let t = T::new();
    let args = |v: Value| v.as_object().unwrap().clone();
    let e = t.m.run("respeak_part", &args(json!({"narration_id": "g1", "part": 0}))).unwrap_err();
    assert!(e.starts_with("Error executing tool respeak_part: part must be a whole number from 1 (got 0)."), "{e}");
    t.m.run("respeak_part", &args(json!({"narration_id": "g1", "part": 2.0}))).unwrap();
    assert_eq!(t.paths().last().unwrap(), "/api/generations/g1/chunks/1/regenerate");
    assert!(t.m.run("get_status", &args(json!({}))).unwrap_err().contains("narration_id\n  Field required"));
    assert_eq!(t.m.run("nope", &args(json!({}))).unwrap_err(), "Unknown tool: nope", "FastMCP does not wrap this one");
    t.answer(|_, _| Ok(reply(200, b"1\n".to_vec())));
    t.m.run("get_subtitles", &args(json!({"narration_id": "g1"}))).unwrap();
    assert_eq!(t.paths().last().unwrap(), "/api/generations/g1/subtitles?fmt=srt");
    t.answer(|_, _| Ok(reply(200, br#"[{"id": "v1", "name": "Ann", "status": "ready"}]"#.to_vec())));
    let _ = t.m.run("generate_narration", &args(json!({"text": "Hi.", "voice": "ann"})));
    assert_eq!(t.calls().last().unwrap().2, Some(json!({"profile_id": "v1", "text": "Hi.", "speed": 1.0})));
    let _ = t.m.run("generate_narration", &args(json!({"text": "Hi.", "voice": "v1", "style": 0.4})));
    assert_eq!(t.calls().last().unwrap().2, Some(json!({"profile_id": "v1", "text": "Hi.", "speed": 1.0, "voice_settings": {"style": 0.4}})));
}

/// Not a Python case: `_voice_id`'s two refusals.
#[test]
fn voice_id_errors() {
    let t = T::new();
    t.reply(json!([{"id": "v1", "name": "Ann  Lee", "status": "ready"}, {"id": "v2", "name": "Anna", "status": "ready"},
                   {"id": "v3", "name": "Bob", "status": "processing"}]));
    assert_eq!(t.m.voice_id("ann lee").unwrap(), "v1");
    assert_eq!(t.m.voice_id("v2").unwrap(), "v2");
    assert_eq!(t.m.voice_id("lee").unwrap(), "v1");
    assert_eq!(t.m.voice_id("an").unwrap_err(), "Several voices match 'an'. Choose one of: Ann Lee, Anna");
    assert_eq!(t.m.voice_id("Bob").unwrap_err(), "No voice matches 'Bob'. Choose one of: Ann Lee, Anna");
}
