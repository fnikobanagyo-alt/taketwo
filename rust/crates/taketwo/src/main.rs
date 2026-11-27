//! `taketwo [--port N] [--ui DIR] | --version`: the TakeTwo window (rust/PLAN.md S09, lead decision 10).
//! The port of desktop/main.js minus updates. Isolated mode only (TAKETWO_DATA): the shell starts its own
//! `taketwo-engine serve --exit-with-stdin` as a child, waits for it, and shows the engine's page in one window.
//! It never scans ports, never reads an address file and never attaches to an engine it did not start.
//! Environment: TAKETWO_DATA (required), TAKETWO_UI (instead of --ui), TAKETWO_SHELL_HIDDEN=1 (window not
//! visible and not focused), TAKETWO_SHELL_CLOSE_AFTER_MS=<n> (close the window n ms after the page loaded).

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use taketwo_core::paths::Mode;
use tauri::webview::PageLoadEvent;
use tauri::{RunEvent, WebviewUrl, WebviewWindowBuilder};

const READY_WAIT: Duration = Duration::from_secs(60);
const HEALTH_WAIT: Duration = Duration::from_secs(15);
const STOP_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq)]
struct Args {
    version: bool,
    port: Option<u16>,
    ui: Option<PathBuf>,
}

fn parse_args(args: &[String], mode: &Mode) -> Result<Args, String> {
    let mut a = Args { version: false, port: None, ui: None };
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--version" => a.version = true,
            "--port" => {
                let v = it.next().ok_or("--port needs a number")?;
                let p: u16 = v.parse().map_err(|_| format!("--port: not a port number: {v}"))?;
                mode.check_port(p)?;
                a.port = Some(p);
            }
            "--ui" => a.ui = Some(PathBuf::from(it.next().ok_or("--ui needs a folder")?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

/// The UI folder: `--ui`, else TAKETWO_UI. Never guessed.
fn ui_folder(arg: Option<PathBuf>, env: Option<&OsStr>) -> Result<PathBuf, String> {
    let dir = arg.or_else(|| env.filter(|v| !v.is_empty()).map(PathBuf::from)).ok_or(
        "Interface not built: pass --ui <folder> or set TAKETWO_UI (build it with `npm run build` in frontend/)",
    )?;
    if !dir.join("index.html").is_file() {
        return Err(format!("Interface not built: no index.html in {}", dir.display()));
    }
    std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// The port from the engine's ready line `{"ready":true,"port":..,"pid":..}`.
fn ready_port(line: &str) -> Option<u16> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("ready")? != true {
        return None;
    }
    v.get("port")?.as_u64()?.try_into().ok()
}

/// The same values as Electron's `process.platform`, which the UI reads from `window.vfDesktop`.
fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

/// A plain HTTP GET on 127.0.0.1 (no client library, no system HTTP cache). Returns the body of a 200 answer.
fn http_get(port: u16, path: &str) -> Option<String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let (head, body) = out.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 200").then(|| body.to_string())
}

fn health_is_taketwo(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|v| v["name"] == "TakeTwo")
}

struct Engine {
    child: Child,
    /// Kept open for the engine's life: when it closes (we stop, or this process dies), the engine stops.
    stdin: Option<ChildStdin>,
    log: PathBuf,
    log_from: u64,
}

impl Engine {
    /// What the engine wrote to engine.log since we started it (its refusal, or why it stopped).
    fn said(&self) -> String {
        let mut text = String::new();
        if let Ok(mut f) = std::fs::File::open(&self.log) {
            let _ = f.seek(SeekFrom::Start(self.log_from));
            let _ = f.read_to_string(&mut text);
        }
        text.trim().to_string()
    }

    /// Close standard input, give it STOP_WAIT to exit, then kill it.
    fn stop(&mut self) {
        drop(self.stdin.take());
        let until = Instant::now() + STOP_WAIT;
        while Instant::now() < until {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("taketwo: the engine did not stop within {} s; killing it", STOP_WAIT.as_secs());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `taketwo-engine serve --exit-with-stdin` and wait for its ready line. Returns the engine and its port.
fn start_engine(program: &Path, data: &Path, port: Option<u16>, ui: &Path) -> Result<(Engine, u16), String> {
    std::fs::create_dir_all(data).map_err(|e| format!("cannot create {}: {e}", data.display()))?;
    let log = data.join("engine.log");
    let log_file = OpenOptions::new().create(true).append(true).open(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let log_from = log_file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut c = Command::new(program);
    c.args(["serve", "--exit-with-stdin"]);
    if let Some(p) = port {
        c.args(["--port", &p.to_string()]);
    }
    c.arg("--ui").arg(ui).env("TAKETWO_DATA", data).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(log_file);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = c.spawn().map_err(|e| format!("cannot start {}: {e}", program.display()))?;
    let stdout = child.stdout.take().expect("piped");
    let stdin = child.stdin.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Reads to the end, so the engine never blocks on a full pipe.
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(p) = ready_port(&line) {
                let _ = tx.send(p);
            }
        }
    });
    let mut engine = Engine { child, stdin, log, log_from };
    match rx.recv_timeout(READY_WAIT) {
        Ok(p) => Ok((engine, p)),
        Err(e) => {
            if matches!(e, mpsc::RecvTimeoutError::Timeout) {
                engine.stop();
                return Err(format!("the engine was not ready after {} s: {}", READY_WAIT.as_secs(), engine.said()));
            }
            let status = engine.child.wait().map(|s| s.to_string()).unwrap_or_default();
            Err(format!("the engine did not start ({status}): {}", engine.said()))
        }
    }
}

fn fail(code: i32, msg: &str) -> ! {
    eprintln!("taketwo: {msg}");
    std::process::exit(code)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("taketwo {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // Live mode stays off (lead decision 10): the shell never asks for it.
    let mode = Mode::resolve(std::env::var_os("TAKETWO_DATA").as_deref(), false).unwrap_or_else(|e| fail(2, &e));
    let Mode::Isolated { data } = mode.clone() else { fail(2, "isolated mode only") };
    let a = parse_args(&args, &mode).unwrap_or_else(|e| fail(2, &e));
    let ui = ui_folder(a.ui, std::env::var_os("TAKETWO_UI").as_deref()).unwrap_or_else(|e| fail(2, &e));
    let hidden = std::env::var("TAKETWO_SHELL_HIDDEN").is_ok_and(|v| v == "1");
    let close_after = match std::env::var("TAKETWO_SHELL_CLOSE_AFTER_MS") {
        Ok(v) if !v.is_empty() => {
            Some(Duration::from_millis(v.parse().unwrap_or_else(|_| fail(2, &format!("TAKETWO_SHELL_CLOSE_AFTER_MS: not a whole number: {v}")))))
        }
        _ => None,
    };
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let program = exe_dir.join(format!("taketwo-engine{}", std::env::consts::EXE_SUFFIX));
    if !program.is_file() {
        fail(2, &format!("taketwo-engine not found beside this program ({})", program.display()));
    }

    // shortcut: one shell per library comes from the engine's lock file (a second shell's engine is refused and
    // it exits); live mode will need the single-instance plugin to focus the open window instead.
    let (engine, port) = start_engine(&program, &data, a.port, &ui).unwrap_or_else(|e| fail(1, &e));
    let engine = Arc::new(Mutex::new(engine));
    let stopping = Arc::new(AtomicBool::new(false));
    let stop = {
        let (engine, stopping) = (engine.clone(), stopping.clone());
        move || {
            stopping.store(true, Ordering::SeqCst);
            engine.lock().unwrap_or_else(|p| p.into_inner()).stop();
        }
    };

    let until = Instant::now() + HEALTH_WAIT;
    while !http_get(port, "/api/health").is_some_and(|b| health_is_taketwo(&b)) {
        if Instant::now() > until {
            stop();
            fail(1, &format!("no TakeTwo health answer on port {port} after {} s", HEALTH_WAIT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // The engine stopped by itself while the window is open: say why and exit 1 (the error page is deferred).
    {
        let (engine, stopping) = (engine.clone(), stopping.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
            if stopping.load(Ordering::SeqCst) {
                return;
            }
            if let Ok(Some(status)) = e.child.try_wait() {
                fail(1, &format!("the engine stopped ({status}): {}", e.said()));
            }
        });
    }

    let url: tauri::Url = format!("http://127.0.0.1:{port}/#/home").parse().expect("engine address");
    let origin = url.origin();
    let script = format!("window.vfDesktop = {{ isDesktop: true, platform: {:?} }};", platform());
    let closing = Arc::new(AtomicBool::new(false));
    let app = tauri::Builder::default()
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            if hidden {
                // No Dock icon and no activation while Franz works on this Mac.
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("TakeTwo")
                .inner_size(1440.0, 920.0)
                .min_inner_size(1100.0, 700.0)
                .visible(!hidden)
                .focused(!hidden)
                .incognito(true) // isolated mode: no site data on disk
                .initialization_script(&script)
                // Stay on the engine's page; other addresses are refused (opening them in the browser is deferred).
                .on_navigation(move |u| u.origin() == origin)
                .on_page_load(move |w, p| {
                    if p.event() != PageLoadEvent::Finished {
                        return;
                    }
                    eprintln!("{}", serde_json::json!({"page": "loaded", "url": p.url().as_str()}));
                    if let Some(after) = close_after
                        && !closing.swap(true, Ordering::SeqCst)
                    {
                        let w = w.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(after);
                            let _ = w.close(); // the same path as a user closing the window
                        });
                    }
                })
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            stop();
            fail(1, &format!("cannot open the window: {e}"));
        }
    };
    // The last window closed, or the app quit: stop the engine, then exit 0.
    app.run(move |_, event| {
        if let RunEvent::Exit = event {
            stop();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arguments_and_franz_ports() {
        let mode = Mode::Isolated { data: "/tmp/x".into() };
        let a = parse_args(&args(&["--port", "8790", "--ui", "dist"]), &mode).unwrap();
        assert_eq!(a, Args { version: false, port: Some(8790), ui: Some("dist".into()) });
        for p in ["8765", "8766", "8785"] {
            assert!(parse_args(&args(&["--port", p]), &mode).unwrap_err().contains("reserved"), "{p}");
        }
        assert!(parse_args(&args(&["--port", "x"]), &mode).is_err());
        assert!(parse_args(&args(&["--bogus"]), &mode).is_err());
        assert!(parse_args(&args(&["--ui"]), &mode).is_err());
    }

    #[test]
    fn the_interface_is_never_guessed() {
        assert!(ui_folder(None, None).unwrap_err().starts_with("Interface not built"));
        assert!(ui_folder(None, Some(OsStr::new(""))).unwrap_err().starts_with("Interface not built"));
        let tmp = std::env::temp_dir().join(format!("taketwo-shell-ui-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(ui_folder(Some(tmp.clone()), None).unwrap_err().starts_with("Interface not built"));
        std::fs::write(tmp.join("index.html"), "<div id=\"root\"></div>").unwrap();
        assert_eq!(ui_folder(None, Some(tmp.as_os_str())).unwrap(), std::fs::canonicalize(&tmp).unwrap());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn ready_line_and_health() {
        assert_eq!(ready_port(r#"{"ready":true,"port":8790,"pid":12}"#), Some(8790));
        assert_eq!(ready_port(r#"{"ready":false,"port":8790}"#), None);
        assert_eq!(ready_port("removed narration x"), None);
        assert!(health_is_taketwo(r#"{"ok":true,"name":"TakeTwo","formerly":"VoiceForge"}"#));
        assert!(!health_is_taketwo(r#"{"ok":true,"name":"VoiceForge"}"#));
        assert!(["darwin", "win32", "linux"].contains(&platform()));
    }
}
