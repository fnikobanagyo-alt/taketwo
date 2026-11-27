//! The built `taketwo-engine serve` as a program: lock file, idle exit, exit when standard input closes.
//! Each test has its own temporary library and lets the engine pick a free dev port (8790-8795, 8798, 8799),
//! read from its ready line, so the tests can run in parallel. Every engine started here is killed and
//! waited for by its own pid when the test ends, also when an assert fails.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use taketwo_core::library::Library;
use taketwo_core::paths::EngineLock;

struct Engine {
    child: Child,
    port: u16,
    stderr: mpsc::Receiver<String>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Engine {
    /// Wait up to `limit` for the program to exit by itself.
    fn exits_within(&mut self, limit: Duration) -> Option<ExitStatus> {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            if let Some(s) = self.child.try_wait().unwrap() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn stderr_text(&self) -> String {
        self.stderr.try_iter().collect::<Vec<_>>().join("\n")
    }
}

fn wt() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn command(home: &Path, data: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_taketwo-engine"));
    c.arg("serve").args(args).env_remove("VOICEFORGE_DATA").env_remove("TAKETWO_STUB_GATE")
        .env("TAKETWO_DATA", data).env("HOME", home).current_dir(home)
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

fn lines(r: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for l in BufReader::new(r).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    rx
}

/// Start the program and wait for its ready line.
fn start(mut c: Command) -> Engine {
    let mut child = c.spawn().unwrap();
    let out = lines(child.stdout.take().unwrap());
    let stderr = lines(child.stderr.take().unwrap());
    let mut e = Engine { child, port: 0, stderr };
    let line = out.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| panic!("no ready line; stderr: {}", e.stderr_text()));
    let port = line.split("\"port\":").nth(1).and_then(|r| r.split(',').next()).and_then(|p| p.parse().ok());
    e.port = port.unwrap_or_else(|| panic!("not a ready line: {line}"));
    assert!((8790..=8799).contains(&e.port) && ![8796, 8797].contains(&e.port), "dev port only: {}", e.port);
    e
}

fn temp() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("library");
    (tmp, data)
}

#[test]
fn a_second_engine_on_the_same_library_is_refused() {
    let (tmp, data) = temp();
    let first = start(command(tmp.path(), &data, &[]));
    let out = command(tmp.path(), &data, &[]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("another TakeTwo engine is running for this library"), "{err}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("ready"), "it bound nothing");
    drop(first);
}

#[test]
fn idle_exit_stops_by_itself_and_frees_port_and_lock() {
    let (tmp, data) = temp();
    let mut e = start(command(tmp.path(), &data, &["--idle-exit", "2"]));
    let t0 = Instant::now();
    let status = e.exits_within(Duration::from_secs(15)).unwrap_or_else(|| panic!("still running; stderr: {}", e.stderr_text()));
    assert!(status.success(), "{status:?}");
    assert!(t0.elapsed() >= Duration::from_millis(1500), "not before the idle time");
    drop(TcpListener::bind(("127.0.0.1", e.port)).expect("the port is free again"));
    drop(EngineLock::acquire(&data.join("engine.lock")).expect("the lock is free again"));
}

fn post(port: u16, path: &str, body: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn idle_exit_waits_while_a_stub_job_waits_at_the_gate() {
    let (tmp, data) = temp();
    let gate = tmp.path().join("gate");
    std::fs::create_dir(&gate).unwrap();
    taketwo_engine::seed::seed_into(&Library::open(&data).unwrap(), &wt().join("contract/fixture.json")).unwrap();
    let mut c = command(tmp.path(), &data, &["--stub-renderer", "--idle-exit", "2"]);
    c.env("TAKETWO_STUB_GATE", &gate);
    let mut e = start(c);
    let answer = post(e.port, "/api/generations", r#"{"profile_id": "ctvoice01", "text": "A job that waits at the gate."}"#);
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(e.exits_within(Duration::from_secs(4)).is_none(), "it exited while a job waited");
    std::fs::write(gate.join("go"), "").unwrap();
    let status = e.exits_within(Duration::from_secs(15)).unwrap_or_else(|| panic!("still running; stderr: {}", e.stderr_text()));
    assert!(status.success(), "{status:?}");
    let lib = Library::open(&data).unwrap();
    let new: Vec<_> = lib.list_generations().unwrap().into_iter().filter(|g| !g["id"].as_str().unwrap().starts_with("ct")).collect();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0]["status"], "done", "the stub job finished before the engine stopped");
}

#[test]
fn exit_with_stdin_stops_when_the_pipe_closes() {
    let (tmp, data) = temp();
    let mut c = command(tmp.path(), &data, &["--exit-with-stdin"]);
    c.stdin(Stdio::piped());
    let mut e = start(c);
    drop(e.child.stdin.take());
    let status = e.exits_within(Duration::from_secs(5)).unwrap_or_else(|| panic!("still running 5 s after stdin closed; stderr: {}", e.stderr_text()));
    assert!(status.success(), "{status:?}");
}
