//! `taketwo-engine serve [--port N] [--ui DIR]`: isolated mode only (no lock file, idle exit,
//! parent watch or address file yet; those are S07b).

use crate::api::{self, AppState};
use std::ffi::OsStr;
use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use taketwo_core::library::{self, Library};
use taketwo_core::paths::Mode;

#[derive(Debug, PartialEq)]
pub struct ServeArgs {
    pub data: PathBuf,
    pub port: Option<u16>,
    pub ui: Option<PathBuf>,
}

/// Check the arguments before anything is opened or bound. `taketwo_data` is the value of `TAKETWO_DATA`.
pub fn parse(args: &[String], taketwo_data: Option<&OsStr>) -> Result<ServeArgs, String> {
    let mode = Mode::resolve(taketwo_data, args.iter().any(|a| a == "--live"))?;
    let Mode::Isolated { data } = mode.clone() else {
        return Err("serve --live is not built yet; set TAKETWO_DATA to a temporary folder".into());
    };
    let (mut port, mut ui) = (None, None);
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
            other => return Err(format!("serve: unknown argument {other}")),
        }
    }
    Ok(ServeArgs { data, port, ui })
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

pub fn run(args: &[String]) -> Result<(), String> {
    let a = parse(args, std::env::var_os("TAKETWO_DATA").as_deref())?;
    let lib = Library::open(&a.data)?;
    for gid in library::startup_repairs(&lib)? {
        eprintln!("removed narration {gid} (deleted more than 7 days ago)");
    }
    let listener = bind(&a)?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let ui = a.ui.filter(|d| d.is_dir());
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let app = api::router(Arc::new(AppState { lib, ui, started, reveal: Box::new(api::reveal_in_file_manager) }));
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
        println!("{{\"ready\":true,\"port\":{port},\"pid\":{}}}", std::process::id());
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        axum::serve(listener, app).with_graceful_shutdown(stop_signal()).await.map_err(|e| e.to_string())
    })
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
        assert_eq!(ok, ServeArgs { data: "/tmp/x".into(), port: Some(8790), ui: Some("ui".into()) });
        assert!(parse(&args(&["--port", "x"]), Some(d)).is_err());
        assert!(parse(&args(&["--bogus"]), Some(d)).is_err());
    }
}
