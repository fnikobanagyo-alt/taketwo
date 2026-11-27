//! `taketwo-engine serve [--port N] [--ui DIR] [--stub-renderer] [--idle-exit SECONDS] [--exit-with-stdin]`:
//! isolated mode only (`--live` is refused until the shell step).
//! `--stub-renderer` wires the stub renderer (its gate is the folder `TAKETWO_STUB_GATE`); without it every
//! path that would start a job answers 501.
//! One engine per library (the lock file). `--idle-exit` stops after that long with no job and no request
//! (MCP passes 900); `--exit-with-stdin` stops when standard input closes (the parent went away).
//! On those, Ctrl-C and SIGTERM: the running job's cancel flag is set, the worker gets a few seconds,
//! the lock is released.

use crate::api::{self, AppState};
use crate::render::{Renderer, StubRenderer};
use std::ffi::OsStr;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use taketwo_core::library::{self, Library};
use taketwo_core::paths::{self, EngineLock, Mode};

/// How long shutdown waits for the running job to stop after its cancel flag is set.
const STOP_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq)]
pub struct ServeArgs {
    pub data: PathBuf,
    pub port: Option<u16>,
    pub ui: Option<PathBuf>,
    pub stub_renderer: bool,
    pub idle_exit: Option<Duration>,
    pub exit_with_stdin: bool,
}

/// Check the arguments before anything is opened or bound. `taketwo_data` is the value of `TAKETWO_DATA`.
pub fn parse(args: &[String], taketwo_data: Option<&OsStr>) -> Result<ServeArgs, String> {
    let mode = Mode::resolve(taketwo_data, args.iter().any(|a| a == "--live"))?;
    parse_for(args, &mode)
}

/// The checks that depend on the mode, given the mode (a unit test passes a live value here).
pub fn parse_for(args: &[String], mode: &Mode) -> Result<ServeArgs, String> {
    if mode.is_live() && args.iter().any(|a| a == "--stub-renderer") {
        return Err("--stub-renderer is only allowed in isolated mode (TAKETWO_DATA set, no --live)".into());
    }
    let Mode::Isolated { data } = mode.clone() else {
        return Err("serve --live is not built yet; set TAKETWO_DATA to a temporary folder".into());
    };
    let (mut port, mut ui, mut stub_renderer, mut idle_exit, mut exit_with_stdin) = (None, None, false, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => {
                let v = it.next().ok_or("--port needs a number")?;
                let p: u16 = v.parse().map_err(|_| format!("--port: not a port number: {v}"))?;
                mode.check_port(p)?;
                port = Some(p);
            }
            "--ui" => ui = Some(PathBuf::from(it.next().ok_or("--ui needs a folder")?)),
            "--stub-renderer" => stub_renderer = true,
            "--exit-with-stdin" => exit_with_stdin = true,
            "--idle-exit" => {
                let v = it.next().ok_or("--idle-exit needs a number of seconds")?;
                let n: u64 = v.parse().ok().filter(|n| *n >= 1).ok_or_else(|| format!("--idle-exit: not a whole number of seconds of at least 1: {v}"))?;
                idle_exit = Some(Duration::from_secs(n));
            }
            other => return Err(format!("serve: unknown argument {other}")),
        }
    }
    Ok(ServeArgs { data, port, ui, stub_renderer, idle_exit, exit_with_stdin })
}

fn bind(a: &ServeArgs) -> Result<TcpListener, String> {
    let ports = match a.port {
        Some(p) => vec![p],
        None => Mode::Isolated { data: a.data.clone() }.default_ports(),
    };
    for p in &ports {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", *p)) {
            return Ok(l);
        }
    }
    Err(format!("no free port among {ports:?}"))
}

async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

/// The address file, written the way `_publish_address` does (live mode and ports 8766-8785 only; an
/// isolated engine writes none). Returns how many files were written.
pub fn publish(mode: &Mode, app_data_root: &Path, port: u16, started: f64) -> usize {
    paths::publish_address(mode, &mode.address_files(app_data_root), "127.0.0.1", port, std::process::id(), started)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let a = parse(args, std::env::var_os("TAKETWO_DATA").as_deref())?;
    let mode = Mode::Isolated { data: a.data.clone() };
    // First, before the library is repaired or a port is bound: one engine per library.
    let lock = EngineLock::acquire(&mode.lock_file(Path::new("")))?;
    let lib = Library::open(&a.data)?;
    for gid in library::startup_repairs(&lib)? {
        eprintln!("removed narration {gid} (deleted more than 7 days ago)");
    }
    let listener = bind(&a)?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let ui = a.ui.filter(|d| d.is_dir());
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let renderer: Option<Arc<dyn Renderer>> = a.stub_renderer.then(|| {
        let gate = std::env::var_os("TAKETWO_STUB_GATE").filter(|g| !g.is_empty()).map(PathBuf::from);
        Arc::new(StubRenderer { gate }) as Arc<dyn Renderer>
    });
    let state = Arc::new(AppState::new(lib, ui, started, Box::new(api::reveal_in_file_manager), renderer, crate::worker::random_ids()));
    let app = api::router(state.clone());
    publish(&mode, Path::new(""), port, started);
    let stop = Arc::new(tokio::sync::Notify::new());
    if a.exit_with_stdin {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink()); // returns when stdin closes
            eprintln!("taketwo-engine: standard input closed; stopping");
            stop.notify_one();
        });
    }
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().map_err(|e| e.to_string())?;
    let served = rt.block_on(async {
        if let Some(after) = a.idle_exit {
            let (st, stop) = (state.clone(), stop.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let quiet = st.last_request.lock().unwrap_or_else(|p| p.into_inner()).elapsed();
                    if quiet >= after && st.worker.idle() {
                        eprintln!("taketwo-engine: idle for {} s; stopping", after.as_secs());
                        stop.notify_one();
                        return;
                    }
                }
            });
        }
        let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
        println!("{{\"ready\":true,\"port\":{port},\"pid\":{}}}", std::process::id());
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let until = async move {
            tokio::select! { _ = stop_signal() => {}, _ = stop.notified() => {} }
        };
        axum::serve(listener, app).with_graceful_shutdown(until).await.map_err(|e| e.to_string())
    });
    state.worker.cancel_running();
    if !state.worker.stop(STOP_WAIT) {
        eprintln!("taketwo-engine: the running job did not stop within {} s", STOP_WAIT.as_secs());
    }
    drop(lock);
    served
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn serve_refuses_without_taketwo_data_and_franz_ports() {
        assert!(parse(&args(&[]), None).unwrap_err().contains("refusing to start"));
        assert!(parse(&args(&["--live"]), None).unwrap_err().contains("not built yet"));
        let d = OsStr::new("/tmp/x");
        assert!(parse(&args(&["--live"]), Some(d)).is_err());
        for p in ["8765", "8766", "8785"] {
            assert!(parse(&args(&["--port", p]), Some(d)).unwrap_err().contains("reserved"), "{p}");
        }
        let ok = parse(&args(&["--port", "8790", "--ui", "ui"]), Some(d)).unwrap();
        assert_eq!(ok, ServeArgs { data: "/tmp/x".into(), port: Some(8790), ui: Some("ui".into()), stub_renderer: false, idle_exit: None, exit_with_stdin: false });
        assert!(parse(&args(&["--stub-renderer"]), Some(d)).unwrap().stub_renderer);
        assert!(parse(&args(&["--port", "x"]), Some(d)).is_err());
        assert!(parse(&args(&["--bogus"]), Some(d)).is_err());
    }

    #[test]
    fn idle_exit_and_stdin_flags() {
        let d = OsStr::new("/tmp/x");
        let a = parse(&args(&["--idle-exit", "900", "--exit-with-stdin"]), Some(d)).unwrap();
        assert_eq!((a.idle_exit, a.exit_with_stdin), (Some(Duration::from_secs(900)), true));
        for bad in [&["--idle-exit"][..], &["--idle-exit", "0"], &["--idle-exit", "x"], &["--idle-exit", "-3"]] {
            assert!(parse(&args(bad), Some(d)).is_err(), "{bad:?}");
        }
    }

    /// The address file through `publish`, on a temporary app-data folder: both names when live, none when isolated.
    #[test]
    fn address_file_is_written_only_in_live_mode() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(publish(&Mode::Live, tmp.path(), 8766, 1.5), 2);
        for name in ["TakeTwo", "VoiceForge"] {
            let text = std::fs::read_to_string(tmp.path().join(name).join("engine.json")).unwrap();
            assert_eq!(text, format!("{{\"url\": \"http://127.0.0.1:8766\", \"pid\": {}, \"started\": 1.5}}", std::process::id()));
        }
        assert_eq!(publish(&Mode::Live, tmp.path(), 8790, 1.5), 0, "a dev port is never published");
        let data = tmp.path().join("iso");
        assert_eq!(publish(&Mode::Isolated { data: data.clone() }, tmp.path(), 8790, 1.5), 0);
        assert!(!data.exists());
    }

    #[test]
    fn stub_renderer_is_refused_in_live_mode() {
        let e = parse_for(&args(&["--stub-renderer"]), &Mode::Live).unwrap_err();
        assert!(e.contains("only allowed in isolated mode"), "{e}");
        assert!(parse_for(&args(&[]), &Mode::Live).unwrap_err().contains("not built yet"));
    }
}
