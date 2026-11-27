//! The built `taketwo-mcp` as a program, driven by an rmcp client over stdio (rust/PLAN.md S08, lead decision 9).
//!
//! Each test has its own temporary library (seeded with contract/fixture.json by `taketwo-engine seed`) and,
//! where it needs one, its own `taketwo-engine serve --stub-renderer` on a free dev port read from the ready
//! line. At most two engines are up at once (only 8 dev ports are usable). Every child gets `HOME` = a temp
//! folder, `TAKETWO_DATA` = the test's library, `TAKETWO_URL` set or removed and `VOICEFORGE_URL` removed.
//! Every engine is killed and waited for by its pid when the test ends, also when an assert fails.
//!
//! The engine program: `taketwo-mcp` starts `taketwo-engine` from its own folder (`target/debug`), so the
//! helper runs `cargo build -p taketwo-engine` once per test run (always, so it is never stale; cargo does
//! not hold the build lock while tests run, checked by hand).

use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use taketwo_core::paths::EngineLock;

type Client = RunningService<RoleClient, ()>;

fn wt() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn mcp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_taketwo-mcp"))
}

/// `taketwo-engine` beside `taketwo-mcp`, built once per run.
fn engine_bin() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let out = Command::new(env!("CARGO"))
                .args(["build", "-p", "taketwo-engine"])
                .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
                .output()
                .expect("run cargo");
            assert!(out.status.success(), "cargo build -p taketwo-engine: {}", String::from_utf8_lossy(&out.stderr));
            let bin = mcp_bin().with_file_name(format!("taketwo-engine{}", std::env::consts::EXE_SUFFIX));
            assert!(bin.exists(), "{} after the build", bin.display());
            bin
        })
        .clone()
}

/// A temporary home folder and a library seeded with the contract fixture.
struct Lib {
    tmp: tempfile::TempDir,
    data: PathBuf,
}

impl Lib {
    fn seeded() -> Lib {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("library");
        std::fs::create_dir(&data).unwrap();
        let out = Command::new(engine_bin())
            .args(["seed", "--fixture"])
            .arg(wt().join("contract/fixture.json"))
            .env("TAKETWO_DATA", &data)
            .env("HOME", tmp.path())
            .env_remove("TAKETWO_URL")
            .env_remove("VOICEFORGE_URL")
            .output()
            .unwrap();
        assert!(out.status.success(), "seed: {}", String::from_utf8_lossy(&out.stderr));
        Lib { tmp, data }
    }
    fn home(&self) -> &Path {
        self.tmp.path()
    }
}

/// A stub engine started by the test.
struct Engine {
    child: Child,
    port: u16,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_stub_engine(lib: &Lib, gate: &Path) -> Engine {
    let mut child = Command::new(engine_bin())
        .args(["serve", "--stub-renderer"])
        .env("TAKETWO_DATA", &lib.data)
        .env("TAKETWO_STUB_GATE", gate)
        .env("HOME", lib.home())
        .env_remove("TAKETWO_URL")
        .env_remove("VOICEFORGE_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut e = Engine { child, port: 0 };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    let line = rx.recv_timeout(Duration::from_secs(30)).expect("engine ready line");
    e.port = line.split("\"port\":").nth(1).and_then(|r| r.split(',').next()).and_then(|p| p.parse().ok()).expect("port");
    assert!((8790..=8799).contains(&e.port) && ![8796, 8797].contains(&e.port), "dev port only: {}", e.port);
    e
}

/// The MCP program as a child of an rmcp client. `url` pins TAKETWO_URL (None: removed).
fn mcp_command(lib: &Lib, url: Option<&str>, args: &[&str]) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(mcp_bin());
    c.args(args).env("TAKETWO_DATA", &lib.data).env("HOME", lib.home()).env_remove("VOICEFORGE_URL").env_remove("TAKETWO_STUB_GATE");
    match url {
        Some(u) => c.env("TAKETWO_URL", u),
        None => c.env_remove("TAKETWO_URL"),
    };
    c.stderr(Stdio::null());
    c
}

async fn connect(lib: &Lib, url: Option<&str>, args: &[&str]) -> Client {
    ().serve(TokioChildProcess::new(mcp_command(lib, url, args)).unwrap()).await.expect("MCP handshake")
}

async fn call(c: &Client, name: &'static str, args: Value) -> CallToolResult {
    let p = CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone());
    tokio::time::timeout(Duration::from_secs(90), c.call_tool(p)).await.expect("tool answered in time").expect("tool call")
}

fn texts(r: &CallToolResult) -> Vec<String> {
    r.content.iter().map(|c| match c {
        ContentBlock::Text(t) => t.text.clone(),
        other => panic!("not text: {other:?}"),
    }).collect()
}

/// A successful object result: one indented JSON text, the same object as structured content.
async fn ok(c: &Client, name: &'static str, args: Value) -> Value {
    let r = call(c, name, args).await;
    assert_ne!(r.is_error, Some(true), "{name}: {:?}", texts(&r));
    let v = r.structured_content.clone().expect("structured content");
    assert_eq!(texts(&r), vec![serde_json::to_string_pretty(&v).unwrap()], "{name}: text is the indented JSON");
    v
}

/// A successful list result: one text per item, `{"result": [...]}` as structured content.
async fn ok_list(c: &Client, name: &'static str, args: Value) -> Vec<Value> {
    let r = call(c, name, args).await;
    assert_ne!(r.is_error, Some(true), "{name}: {:?}", texts(&r));
    let items = r.structured_content.clone().expect("structured")["result"].as_array().unwrap().clone();
    let want: Vec<String> = items.iter().map(|i| serde_json::to_string_pretty(i).unwrap()).collect();
    assert_eq!(texts(&r), want);
    items
}

/// A failing tool: a tool result with isError true (never a protocol error) whose text ends with `tail`.
async fn fails(c: &Client, name: &'static str, args: Value, tail: &str) -> String {
    let r = call(c, name, args).await;
    assert_eq!(r.is_error, Some(true), "{name} should fail: {:?}", texts(&r));
    let t = texts(&r).join("");
    assert!(t.starts_with(&format!("Error executing tool {name}: ")), "{t}");
    assert!(t.ends_with(tail), "{name}: {t:?} should end with {tail:?}");
    t
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(wt().join("contract/golden/narrations.json")).unwrap()).unwrap()
}

/// `fn.__doc__` of each tool in mcp/taketwo_mcp.py, as Python 3.13+ stores it (common indent stripped).
fn python_docstrings() -> Vec<(String, String)> {
    let src = std::fs::read_to_string(wt().join("mcp/taketwo_mcp.py")).unwrap();
    let mut out = vec![];
    for part in src.split("@mcp.tool()\ndef ").skip(1) {
        let name = part.split('(').next().unwrap().to_string();
        let body = part.split("\"\"\"").nth(1).unwrap();
        let lines: Vec<&str> = body.split('\n').collect();
        let indent = lines[1..].iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
        let mut doc = vec![lines[0].to_string()];
        doc.extend(lines[1..].iter().map(|l| if l.trim().is_empty() { String::new() } else { l[indent..].to_string() }));
        out.push((name, doc.join("\n")));
    }
    out
}

/// Parameter name, JSON type, required, default: written from the Python signatures.
fn python_signatures() -> Vec<(&'static str, Vec<(&'static str, &'static str, bool, Value)>)> {
    let id = || ("narration_id", "string", true, Value::Null);
    let opt = |n: &'static str, t: &'static str| (n, t, false, Value::Null);
    vec![
        ("list_voices", vec![]),
        ("generate_narration", vec![("text", "string", true, Value::Null), ("voice", "string", true, Value::Null),
            ("speed", "number", false, json!(1.0)), opt("style", "number"), opt("stability", "number")]),
        ("get_status", vec![id()]),
        ("get_timings", vec![id()]),
        ("get_audio", vec![id(), ("format", "string", false, json!("mp3")), opt("output_path", "string"),
            ("overwrite", "boolean", false, json!(false))]),
        ("get_subtitles", vec![id(), ("format", "string", false, json!("srt"))]),
        ("find_narration", vec![opt("text", "string"), opt("voice", "string"), opt("status", "string")]),
        ("respeak_part", vec![id(), ("part", "integer", true, Value::Null), opt("text", "string")]),
        ("fix_remaining", vec![id()]),
        ("stop_narration", vec![id()]),
        ("resume_narration", vec![id()]),
        ("mark_sounds_fine", vec![id(), ("part", "integer", true, Value::Null), ("undo", "boolean", false, json!(false))]),
    ]
}

/// An address that nothing listens on: tests that make no tool call pin it, so discovery can never run.
const UNUSED: &str = "http://127.0.0.1:9";

#[tokio::test]
async fn tool_list_matches_python() {
    let lib = Lib::seeded();
    let c = connect(&lib, Some(UNUSED), &[]).await;
    assert_eq!(c.peer_info().unwrap().server_info.clone().unwrap().name, "taketwo");
    let tools = c.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    let want = python_signatures();
    assert_eq!(names, want.iter().map(|w| w.0).collect::<Vec<_>>(), "exactly the 12 tools, in Python's order");
    let docs = python_docstrings();
    assert_eq!(docs.len(), 12);
    for ((tool, params), (doc_name, doc)) in tools.iter().zip(&want).map(|(t, w)| (t, &w.1)).zip(&docs) {
        assert_eq!(tool.name, *doc_name);
        assert_eq!(tool.description.as_deref(), Some(doc.as_str()), "{}: the docstring word for word", tool.name);
        let s = Value::Object((*tool.input_schema).clone());
        assert_eq!(s["type"], "object");
        let props = s["properties"].as_object().unwrap();
        assert_eq!(props.keys().map(String::as_str).collect::<Vec<_>>(), params.iter().map(|p| p.0).collect::<Vec<_>>(), "{}", tool.name);
        let required: Vec<&str> = params.iter().filter(|p| p.2).map(|p| p.0).collect();
        let got_required: Vec<&str> = s.get("required").and_then(Value::as_array).map(|a| a.iter().map(|v| v.as_str().unwrap()).collect()).unwrap_or_default();
        assert_eq!(got_required, required, "{}", tool.name);
        for (name, ty, req, default) in params {
            let p = &props[*name];
            if *req {
                assert_eq!(p["type"], *ty, "{}.{name}", tool.name);
                assert!(p.get("default").is_none(), "{}.{name}", tool.name);
            } else if default.is_null() {
                assert_eq!(p["anyOf"], json!([{"type": ty}, {"type": "null"}]), "{}.{name}", tool.name);
                assert_eq!(p.get("default"), Some(&Value::Null), "{}.{name}", tool.name);
            } else {
                assert_eq!((&p["type"], &p["default"]), (&json!(ty), default), "{}.{name}", tool.name);
            }
        }
    }
    c.cancel().await.unwrap();
    let c = connect(&lib, Some(UNUSED), &["--name", "voiceforge"]).await;
    assert_eq!(c.peer_info().unwrap().server_info.clone().unwrap().name, "voiceforge");
    assert_eq!(c.list_all_tools().await.unwrap().len(), 12);
    c.cancel().await.unwrap();
    assert!(!lib.data.join("engine.json").exists(), "no engine was started");
}

/// Poll get_status until the narration is neither queued nor running.
async fn settled(c: &Client, id: &str) -> Value {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let s = ok(c, "get_status", json!({"narration_id": id})).await;
        if !["queued", "running"].contains(&s["status"].as_str().unwrap()) || Instant::now() > until {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn all_twelve_tools() {
    let lib = Lib::seeded();
    let gate = lib.home().join("gate");
    std::fs::create_dir(&gate).unwrap(); // closed until `go` is written
    let engine = start_stub_engine(&lib, &gate);
    let url = format!("http://127.0.0.1:{}", engine.port);
    // McpTools.setUpClass: the pinned URL is used as given (trailing "/" stripped), nothing else is looked up.
    let c = connect(&lib, Some(&format!("{url}/")), &[]).await;
    let g = golden();
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(wt().join("contract/fixture.json")).unwrap()).unwrap();
    let voice = &fixture["profiles"][0];

    // list_voices (McpTools.test_list_voices)
    let voices = ok_list(&c, "list_voices", json!({})).await;
    assert_eq!(voices, vec![json!({"id": "ctvoice01", "name": voice["name"], "language": "en", "description": voice["description"],
                                   "speech_rate_wps": voice["speech_rate_wps"], "studio": voice["studio"]})]);
    assert_eq!(keys(&voices[0]), ["id", "name", "language", "description", "speech_rate_wps", "studio"]);

    // get_status (McpTools.test_get_status_reports_review)
    let s = ok(&c, "get_status", json!({"narration_id": "ctreview01"})).await;
    assert_eq!(keys(&s), ["status", "review_status", "parts_done", "parts_total", "message", "eta_seconds", "queue_ahead", "open_in_app",
                          "duration_seconds", "parts_needing_review"]);
    assert_eq!((&s["status"], &s["review_status"], &s["parts_done"], &s["parts_total"]),
               (&json!("done_needs_listen"), &json!("done_needs_review"), &json!(3), &json!(3)));
    assert_eq!(s["parts_needing_review"], g["ctreview01"]["review"]);
    assert_eq!(s["open_in_app"], format!("{url}/#/history/ctreview01"));
    assert_eq!((&s["eta_seconds"], &s["queue_ahead"]), (&Value::Null, &Value::Null));
    let s = ok(&c, "get_status", json!({"narration_id": "ctclean01"})).await;
    assert_eq!((&s["status"], &s["review_status"], &s["parts_needing_review"]), (&json!("done"), &json!("done"), &json!([])));
    fails(&c, "get_status", json!({"narration_id": "nosuch"}), "TakeTwo returned 404: Not found").await;

    // get_timings, get_subtitles, find_narration (McpTools.test_timings_subtitles_and_search)
    let t = ok(&c, "get_timings", json!({"narration_id": "ctclean01"})).await;
    assert_eq!(t, g["ctclean01"]["timings"]);
    for fmt in ["srt", "vtt", "txt"] {
        let r = call(&c, "get_subtitles", json!({"narration_id": "ctclean01", "format": fmt.to_uppercase()})).await;
        assert_ne!(r.is_error, Some(true));
        assert_eq!(texts(&r), vec![g["ctclean01"][fmt].as_str().unwrap().to_string()], "{fmt}");
        assert_eq!(r.structured_content, Some(json!({"result": g["ctclean01"][fmt]})));
    }
    let r = call(&c, "get_subtitles", json!({"narration_id": "ctclean01"})).await;
    assert_eq!(texts(&r), vec![g["ctclean01"]["srt"].as_str().unwrap().to_string()], "srt is the default");
    fails(&c, "get_subtitles", json!({"narration_id": "ctclean01", "format": "docx"}), r#"format must be "srt", "vtt" or "txt" (got 'docx')."#).await;
    fails(&c, "get_subtitles", json!({"narration_id": "ctpending01"}), "TakeTwo returned 404: Subtitles are available once the audio is rendered").await;
    let all = ok_list(&c, "find_narration", json!({})).await;
    let ids: Vec<&str> = all.iter().map(|r| r["id"].as_str().unwrap()).collect();
    for id in ["ctclean01", "ctreview01", "ctaccept01", "ctpending01", "cttrash01"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    let review = ok_list(&c, "find_narration", json!({"status": "needs_review"})).await;
    let review_ids: Vec<&str> = review.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(review_ids.contains(&"ctreview01") && !review_ids.contains(&"ctclean01"), "{review_ids:?}");
    let found = ok_list(&c, "find_narration", json!({"text": "Rivers and towns", "voice": "ctvoice01"})).await;
    assert_eq!(found.iter().map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>(), ["ctclean01"]);

    // get_audio (McpTools.test_get_audio_saves_the_wav_with_a_review_warning)
    let out_dir = lib.home().join("out");
    let wav = out_dir.join("sub").join("n.wav");
    let a = ok(&c, "get_audio", json!({"narration_id": "ctreview01", "format": "wav", "output_path": wav})).await;
    assert_eq!(keys(&a), ["path", "duration_seconds", "format", "outcome", "warning", "parts_needing_review"]);
    assert_eq!((&a["path"], &a["format"], &a["outcome"]), (&json!(wav), &json!("wav"), &json!("needs_listen")));
    assert_eq!(a["parts_needing_review"], g["ctreview01"]["review"]);
    assert_eq!(&std::fs::read(&wav).unwrap()[..4], b"RIFF");
    let first = std::fs::read(&wav).unwrap();
    fails(&c, "get_audio", json!({"narration_id": "ctreview01", "format": "wav", "output_path": wav}),
          &format!("{} already exists. Choose another output_path or pass overwrite=True.", wav.display())).await;
    let a = ok(&c, "get_audio", json!({"narration_id": "ctclean01", "format": "wav", "output_path": wav, "overwrite": true})).await;
    assert_eq!(keys(&a), ["path", "duration_seconds", "format", "outcome"], "a clean narration has no warning");
    assert_eq!(a["outcome"], "clean");
    assert_ne!(std::fs::read(&wav).unwrap(), first, "overwritten with the other narration");
    let bad = out_dir.join("n.mp3");
    fails(&c, "get_audio", json!({"narration_id": "ctclean01", "format": "wav", "output_path": bad}),
          &format!("output_path must end in .wav to match format='wav' (got '{}').", bad.display())).await;
    fails(&c, "get_audio", json!({"narration_id": "ctpending01", "format": "wav", "output_path": out_dir.join("p.wav")}),
          "The narration is not finished yet (status: cancelled). Check get_status first.").await;
    // MP3 is made in S16: until then the engine answers 404, a clean error, and no .part file is left.
    let mp3 = out_dir.join("n.mp3");
    let e = fails(&c, "get_audio", json!({"narration_id": "ctclean01", "output_path": mp3}), "").await;
    assert!(e.contains("TakeTwo returned 404: "), "{e}");
    assert!(!mp3.exists() && !out_dir.join("n.mp3.part").exists());
    let mut left: Vec<String> = std::fs::read_dir(&out_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
    left.sort();
    assert_eq!(left, ["sub"]);
    assert!(!lib.home().join("Downloads").exists(), "tests always pass output_path");

    // mark_sounds_fine and undo (McpTools.test_mark_sounds_fine_round_trip on ctaccept01; ctreview01 too)
    let m = ok(&c, "mark_sounds_fine", json!({"narration_id": "ctaccept01", "part": 2})).await;
    assert_eq!(m, json!({"narration_id": "ctaccept01", "part": 2, "sounds_fine": true}));
    assert_eq!(ok(&c, "get_status", json!({"narration_id": "ctaccept01"})).await["status"], "done");
    let m = ok(&c, "mark_sounds_fine", json!({"narration_id": "ctaccept01", "part": 2, "undo": true})).await;
    assert_eq!(m["sounds_fine"], false);
    assert_eq!(ok(&c, "get_status", json!({"narration_id": "ctaccept01"})).await["status"], "done_needs_listen");
    let before = ok(&c, "get_status", json!({"narration_id": "ctreview01"})).await["parts_needing_review"].as_array().unwrap().len();
    ok(&c, "mark_sounds_fine", json!({"narration_id": "ctreview01", "part": 2})).await;
    let mid = ok(&c, "get_status", json!({"narration_id": "ctreview01"})).await["parts_needing_review"].as_array().unwrap().len();
    assert_eq!(mid, before - 1);
    ok(&c, "mark_sounds_fine", json!({"narration_id": "ctreview01", "part": 2, "undo": true})).await;
    assert_eq!(ok(&c, "get_status", json!({"narration_id": "ctreview01"})).await["parts_needing_review"], g["ctreview01"]["review"]);
    fails(&c, "mark_sounds_fine", json!({"narration_id": "ctreview01", "part": 0}),
          "part must be a whole number from 1 (got 0). See parts_needing_review in get_status.").await;

    // generate_narration with the stub gate closed: the first runs at once, a second waits behind it.
    let first_text = "The first stub narration waits at the gate.";
    let n1 = ok(&c, "generate_narration", json!({"text": first_text, "voice": "Contract  voice"})).await;
    assert_eq!(keys(&n1), ["narration_id", "parts", "status"]);
    assert_eq!((&n1["status"], n1["parts"].as_u64().unwrap() >= 1), (&json!("queued"), true));
    let id1 = n1["narration_id"].as_str().unwrap().to_string();
    let until = Instant::now() + Duration::from_secs(10);
    let s1 = loop {
        let s = ok(&c, "get_status", json!({"narration_id": id1})).await;
        if s["status"] != "queued" || Instant::now() > until {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!((&s1["status"], &s1["review_status"]), (&json!("running"), &json!("running")));
    let n2 = ok(&c, "generate_narration", json!({"text": "A second, different script waits its turn.", "voice": "ctvoice01", "speed": 1.1, "style": 0.3})).await;
    let id2 = n2["narration_id"].as_str().unwrap().to_string();
    let s2 = ok(&c, "get_status", json!({"narration_id": id2})).await;
    assert_eq!((&s2["status"], &s2["queue_ahead"]), (&json!("queued"), &json!(1)));
    let dup = ok(&c, "generate_narration", json!({"text": first_text, "voice": "ctvoice01"})).await;
    assert_eq!(keys(&dup), ["narration_id", "note"]);
    assert_eq!(dup["narration_id"], json!(id1));
    assert_eq!(dup["note"], "Already in the queue: this narration is rendering now. It wasn't added twice.");
    fails(&c, "generate_narration", json!({"text": "x", "voice": "Nobody"}), "No voice matches 'Nobody'. Choose one of: Contract Voice").await;
    // stop_narration, resume_narration
    assert_eq!(ok(&c, "stop_narration", json!({"narration_id": id2})).await, json!({"narration_id": id2, "status": "stopping"}));
    assert_eq!(settled(&c, &id2).await["status"], "cancelled");
    fails(&c, "resume_narration", json!({"narration_id": id1}), "TakeTwo returned 409: Already running").await;
    assert_eq!(ok(&c, "resume_narration", json!({"narration_id": id2})).await, json!({"narration_id": id2, "status": "queued"}));
    std::fs::write(gate.join("go"), "").unwrap();
    for id in [&id1, &id2] {
        let s = settled(&c, id).await;
        assert_eq!((&s["status"], &s["review_status"], &s["parts_done"]), (&json!("done"), &json!("done"), &s["parts_total"]), "{id}: {s}");
    }

    // fix_remaining and respeak_part are accepted where a part needs it; nothing left to fix is an error.
    assert_eq!(ok(&c, "fix_remaining", json!({"narration_id": "ctreview01"})).await, json!({"narration_id": "ctreview01", "status": "queued"}));
    let s = settled(&c, "ctreview01").await;
    // Part 2 (take check) was re-recorded; part 3 keeps its script-check flag, since its script still holds "&39;".
    assert_eq!(s["status"], "done_needs_listen");
    assert_eq!(s["parts_needing_review"], json!([g["ctreview01"]["review"][1].as_object().unwrap().iter()
        .map(|(k, v)| (k.clone(), if k == "start" || k == "end" { s["parts_needing_review"][0][k].clone() } else { v.clone() }))
        .collect::<serde_json::Map<_, _>>()]));
    let r = ok(&c, "respeak_part", json!({"narration_id": "ctaccept01", "part": 2, "text": "Part two again."})).await;
    assert_eq!(r, json!({"narration_id": "ctaccept01", "part": 2, "status": "queued"}));
    assert_eq!(settled(&c, "ctaccept01").await["status"], "done");
    fails(&c, "fix_remaining", json!({"narration_id": "ctclean01"}), "TakeTwo returned 400: Nothing left to fix.").await;

    c.cancel().await.unwrap();
    drop(engine);
}

/// Kills the engine named in `<library>/engine.json` and waits until it is gone, also when a tool call or an
/// assert fails. Made before the first call; the pid is read from the file when first needed (at the latest on drop).
struct EngineOf {
    data: PathBuf,
    pid: std::cell::Cell<Option<u32>>,
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

impl EngineOf {
    fn pid(&self) -> Option<u32> {
        if self.pid.get().is_none() {
            let v: Option<Value> = std::fs::read_to_string(self.data.join("engine.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
            self.pid.set(v.and_then(|v| v["pid"].as_u64()).map(|p| p as u32));
        }
        self.pid.get()
    }

    fn stop(&self, signal: &str) -> bool {
        let Some(pid) = self.pid() else { return true };
        let _ = Command::new("kill").args([signal, &pid.to_string()]).status();
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for EngineOf {
    fn drop(&mut self) {
        if self.pid().is_some_and(alive) && !self.stop("-TERM") {
            self.stop("-KILL");
        }
    }
}

fn address(lib: &Lib) -> Value {
    serde_json::from_str(&std::fs::read_to_string(lib.data.join("engine.json")).expect("engine.json")).unwrap()
}

#[tokio::test]
async fn starts_an_engine_when_none_answers() {
    let lib = Lib::seeded();
    assert!(!lib.data.join("engine.json").exists());
    // The guard comes first, so a failed call or assertion below still stops the engine.
    let guard = EngineOf { data: lib.data.clone(), pid: std::cell::Cell::new(None) };
    let c = connect(&lib, None, &[]).await;
    let r = call(&c, "list_voices", json!({})).await;
    let addr = address(&lib);
    let pid = guard.pid().expect("engine pid");
    assert_eq!(Some(u64::from(pid)), addr["pid"].as_u64());
    assert_ne!(r.is_error, Some(true), "{:?}", texts(&r));
    assert_eq!(r.structured_content.as_ref().unwrap()["result"][0]["id"], "ctvoice01");
    let port: u16 = addr["url"].as_str().unwrap().rsplit(':').next().unwrap().parse().unwrap();
    assert!((8790..=8799).contains(&port) && ![8796, 8797].contains(&port), "dev port only: {port}");
    let cmd = Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().unwrap();
    let cmd = String::from_utf8_lossy(&cmd.stdout);
    assert!(cmd.contains("taketwo-engine serve --idle-exit 900"), "{cmd}");
    assert!(!cmd.contains("--exit-with-stdin"), "{cmd}");
    assert!(lib.data.join("engine.log").is_file(), "the started engine's standard error goes to engine.log");
    c.cancel().await.unwrap();
    // The engine outlives the MCP that started it (it stops after 15 idle minutes, decision #12).
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid), "the engine stopped with the MCP");

    // A second MCP on the same library finds it through the address file and starts none.
    let c = connect(&lib, None, &["--name", "voiceforge"]).await;
    let r = call(&c, "list_voices", json!({})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", texts(&r));
    assert_eq!(address(&lib)["pid"], addr["pid"], "the same engine");
    c.cancel().await.unwrap();

    assert!(guard.stop("-TERM"), "the engine stopped");
    drop(TcpListener::bind(("127.0.0.1", port)).expect("the port is free again"));
    drop(EngineLock::acquire(&lib.data.join("engine.lock")).expect("the lock is free again"));
    assert!(!lib.data.join("engine.json").exists(), "a clean exit removes the address file");
}

/// The program refuses before it speaks MCP or starts anything.
fn refused(lib: &Lib, data: bool, envs: &[(&str, &str)], args: &[&str]) -> String {
    let mut c = Command::new(mcp_bin());
    c.args(args).env("HOME", lib.home()).env_remove("TAKETWO_URL").env_remove("VOICEFORGE_URL");
    if data { c.env("TAKETWO_DATA", &lib.data) } else { c.env_remove("TAKETWO_DATA") };
    for (k, v) in envs {
        c.env(k, v);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "nothing on the protocol stream");
    assert!(!lib.data.join("engine.json").exists(), "no engine was started");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn refuses_without_taketwo_data() {
    let lib = Lib::seeded();
    let e = refused(&lib, false, &[], &[]);
    assert!(e.contains("refusing to start: set TAKETWO_DATA"), "{e}");
}

#[test]
fn refuses_live() {
    let lib = Lib::seeded();
    let e = refused(&lib, false, &[], &["--live"]);
    assert!(e.contains("--live is not built yet; set TAKETWO_DATA to a temporary folder"), "{e}");
    let e = refused(&lib, true, &[], &["--live"]);
    assert!(e.contains("TAKETWO_DATA and --live were both given"), "{e}");
}

#[test]
fn refuses_a_pinned_url_on_franz_ports() {
    let lib = Lib::seeded();
    for (var, url) in [("TAKETWO_URL", "http://127.0.0.1:8766"), ("VOICEFORGE_URL", "http://127.0.0.1:8766/"), ("TAKETWO_URL", "http://localhost:8785")] {
        let e = refused(&lib, true, &[(var, url)], &[]);
        assert!(e.contains("which belongs to the live TakeTwo app (8765-8785); isolated runs use 8790-8795, 8798, 8799"), "{e}");
    }
}
