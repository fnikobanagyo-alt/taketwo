//! Finding the engine: the port of `_engine` and `_is_taketwo` in mcp/taketwo_mcp.py.
//!
//! Isolated mode (the only one the program runs): a pinned `TAKETWO_URL`/`VOICEFORGE_URL` (never a port of
//! 8765-8785), else the remembered address if it still answers, else `<TAKETWO_DATA>/engine.json`, else start
//! `taketwo-engine serve --idle-exit 900` from this program's folder and wait for that file. No app-data
//! file is read and no port is scanned. The live search (`live_find`) is here for the shell step, tested on
//! stand-ins, and not reachable from the command line.

use crate::http::Transport;
use crate::tools::NOT_RUNNING;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use taketwo_core::paths::{ADDRESS_FILE, APP_NAMES, LIVE_RESERVED_PORTS};

const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// "the engine takes a few seconds to start": Python waits up to 60 s.
const START_WAIT: Duration = Duration::from_secs(60);

fn get_json(http: &dyn Transport, url: &str) -> Option<Value> {
    let mut r = http.send("GET", url, None, PROBE_TIMEOUT).ok()?;
    if r.status >= 400 {
        return None;
    }
    let mut buf = Vec::new();
    r.body.read_to_end(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// `_is_taketwo`: other local servers answer on these ports too, so check two TakeTwo-only endpoints.
pub fn is_taketwo(http: &dyn Transport, base: &str) -> bool {
    get_json(http, &format!("{base}/api/health")).is_some_and(|h| h.get("ok") == Some(&Value::Bool(true)))
        && get_json(http, &format!("{base}/api/pipeline-version")).is_some_and(|v| v.get("version").is_some())
}

/// `os.environ.get("TAKETWO_URL") or os.environ.get("VOICEFORGE_URL")`, trailing `/` stripped.
pub fn pinned(taketwo_url: Option<String>, voiceforge_url: Option<String>) -> Option<String> {
    taketwo_url.filter(|u| !u.is_empty()).or(voiceforge_url.filter(|u| !u.is_empty())).map(|u| u.trim_end_matches('/').to_string())
}

/// The port of a URL (`http://host:port/...`), 80 or 443 when none is written.
fn url_port(url: &str) -> Option<u16> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    match host_port.rsplit_once(':') {
        Some((_, p)) if !p.contains(']') => p.parse().ok(),
        _ => Some(if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 }),
    }
}

/// Isolated mode never talks to Franz's app: a pinned URL on 8765-8785 is refused before anything is sent.
pub fn check_pinned(url: &str) -> Result<(), String> {
    match url_port(url) {
        Some(p) if LIVE_RESERVED_PORTS.contains(&p) => Err(format!(
            "refusing to start: the pinned engine address {url} uses port {p}, which belongs to the live TakeTwo app \
             (8765-8785); isolated runs use 8790-8795, 8798, 8799"
        )),
        Some(_) => Ok(()),
        None => Err(format!("refusing to start: the pinned engine address {url} is not a URL")),
    }
}

/// The `url` of an address file, if it holds one.
pub fn read_address(file: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    v.get("url")?.as_str().map(str::to_string)
}

/// Start the engine for this library, detached: its own process group (so a signal to the MCP's group does
/// not reach it), standard input, output and error all closed (output carries the MCP protocol). It stops by
/// itself after 15 idle minutes (Franz's decision #12), not with the MCP. A thread reaps it when it exits.
pub fn start_engine(program: &Path, data: &Path) -> std::io::Result<u32> {
    let mut c = Command::new(program);
    c.args(["serve", "--idle-exit", "900"])
        .env("TAKETWO_DATA", data)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    let mut child = c.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// `taketwo-engine` next to this program (`target/debug` in development, the app folder when installed).
pub fn engine_program() -> PathBuf {
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    dir.join(format!("taketwo-engine{}", std::env::consts::EXE_SUFFIX))
}

/// The isolated-mode `_engine()`.
pub struct Finder {
    pub http: Arc<dyn Transport>,
    pub data: PathBuf,
    pub pinned: Option<String>,
    pub program: PathBuf,
    /// `_base`. The lock also keeps two tool calls from starting two engines at once.
    remembered: Mutex<Option<String>>,
}

impl Finder {
    pub fn new(http: Arc<dyn Transport>, data: PathBuf, pinned: Option<String>, program: PathBuf) -> Finder {
        Finder { http, data, pinned, program, remembered: Mutex::new(None) }
    }

    pub fn base(&self) -> Result<String, String> {
        if let Some(p) = &self.pinned {
            return Ok(p.clone()); // pin a specific engine (tests, remote setups); used without a probe
        }
        let mut remembered = self.remembered.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(b) = remembered.as_ref().filter(|b| is_taketwo(&*self.http, b)) {
            return Ok(b.clone());
        }
        let file = self.data.join(ADDRESS_FILE);
        let answering = || read_address(&file).filter(|u| is_taketwo(&*self.http, u));
        if let Some(u) = answering() {
            *remembered = Some(u.clone());
            return Ok(u);
        }
        if start_engine(&self.program, &self.data).is_ok() {
            // A stale file of a killed engine may still be there: wait for an address that answers.
            let until = Instant::now() + START_WAIT;
            while Instant::now() < until {
                std::thread::sleep(Duration::from_millis(250));
                if let Some(u) = answering() {
                    *remembered = Some(u.clone());
                    return Ok(u);
                }
            }
        }
        Err(NOT_RUNNING.into())
    }
}

/// The live-mode `_engine()` after the pinned URL and the remembered address: the address file under both
/// app-data names, then a scan of `ports`, then `start()` (opening the app) and up to 60 checks a `wait()`
/// apart, then one more scan. Base folder, ports, probe, start and wait are parameters so it is tested on
/// stand-ins. Not reachable from the command line: `--live` is refused until the shell step.
pub fn live_find(
    app_data_root: &Path,
    ports: &[u16],
    probe: &dyn Fn(&str) -> bool,
    start: &mut dyn FnMut(),
    wait: &mut dyn FnMut(),
) -> Result<String, String> {
    for name in APP_NAMES {
        if let Some(u) = read_address(&app_data_root.join(name).join(ADDRESS_FILE)).filter(|u| probe(u)) {
            return Ok(u);
        }
    }
    let scan = || ports.iter().map(|p| format!("http://127.0.0.1:{p}")).find(|u| probe(u));
    for attempt in 0..2 {
        if let Some(u) = scan() {
            return Ok(u);
        }
        if attempt == 0 {
            start();
            for _ in 0..60 {
                wait();
                if scan().is_some() {
                    break;
                }
            }
        }
    }
    Err(NOT_RUNNING.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn pinned_url_rules() {
        assert_eq!(pinned(Some("http://127.0.0.1:8791/".into()), Some("http://x".into())).as_deref(), Some("http://127.0.0.1:8791"));
        assert_eq!(pinned(Some(String::new()), Some("http://127.0.0.1:8792".into())).as_deref(), Some("http://127.0.0.1:8792"));
        assert_eq!(pinned(None, None), None);
        for bad in ["http://127.0.0.1:8766", "http://localhost:8765/", "http://127.0.0.1:8785/api", "http://[::1]:8770"] {
            assert!(check_pinned(bad).unwrap_err().contains("belongs to the live TakeTwo app"), "{bad}");
        }
        for ok in ["http://127.0.0.1:8790", "http://127.0.0.1:8786", "http://engine", "http://[::1]:8791/"] {
            assert!(check_pinned(ok).is_ok(), "{ok}");
        }
        assert!(check_pinned("not a url").is_err());
    }

    #[test]
    fn address_file_url() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("engine.json");
        assert_eq!(read_address(&f), None);
        std::fs::write(&f, r#"{"url": "http://127.0.0.1:8793", "pid": 1, "started": 1.0}"#).unwrap();
        assert_eq!(read_address(&f).as_deref(), Some("http://127.0.0.1:8793"));
        std::fs::write(&f, "{broken").unwrap();
        assert_eq!(read_address(&f), None);
    }

    /// The live search on a temporary app-data folder and a stand-in probe: nothing real is read or probed.
    #[test]
    fn live_find_on_stand_ins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ports = [8766, 8767, 8768];
        let (mut start, mut wait) = (|| panic!("no start needed"), || {});
        // Nothing answers, so it starts the app once, waits 60 times, scans again and gives up.
        let (starts, waits) = (Cell::new(0), Cell::new(0));
        let err = live_find(root, &ports, &|_| false, &mut || starts.set(starts.get() + 1), &mut || waits.set(waits.get() + 1));
        assert_eq!((err.unwrap_err().as_str(), starts.get(), waits.get()), (NOT_RUNNING, 1, 60));
        // A scan finds the first port that answers.
        assert_eq!(live_find(root, &ports, &|u| u.ends_with(":8767"), &mut start, &mut wait).unwrap(), "http://127.0.0.1:8767");
        // The VoiceForge address file is read when the TakeTwo one is missing, and only if it answers.
        std::fs::create_dir_all(root.join("VoiceForge")).unwrap();
        std::fs::write(root.join("VoiceForge/engine.json"), r#"{"url": "http://127.0.0.1:8770"}"#).unwrap();
        assert_eq!(live_find(root, &ports, &|u| u.ends_with(":8770"), &mut start, &mut wait).unwrap(), "http://127.0.0.1:8770");
        // TakeTwo's file comes first.
        std::fs::create_dir_all(root.join("TakeTwo")).unwrap();
        std::fs::write(root.join("TakeTwo/engine.json"), r#"{"url": "http://127.0.0.1:8771"}"#).unwrap();
        assert_eq!(live_find(root, &ports, &|u| !u.ends_with(":8766"), &mut start, &mut wait).unwrap(), "http://127.0.0.1:8771");
        // Started by `start`, found after a few waits.
        let up = RefCell::new(false);
        let mut waited = 0;
        let found = live_find(tmp.path().join("none").as_path(), &ports, &|u| *up.borrow() && u.ends_with(":8768"),
            &mut || {}, &mut || { waited += 1; if waited == 3 { *up.borrow_mut() = true } });
        assert_eq!(found.unwrap(), "http://127.0.0.1:8768");
        assert_eq!(waited, 3);
    }
}
