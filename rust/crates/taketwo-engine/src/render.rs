//! The one seam between the engine and the voice: `Renderer`, the four pipeline calls main.py's jobs make
//! (pipeline.run_generation, regenerate_chunk, fix_remaining, select_take). The real one arrives in S15/S16.
//! `StubRenderer` is always compiled, so in-process tests and `serve --stub-renderer` (isolated mode only) reach it.

use crate::seed::tone;
use crate::worker::JobError;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use taketwo_core::library::{Doc, Library, is_plain_name};

pub type Cancel<'a> = &'a dyn Fn() -> bool;

pub trait Renderer: Send + Sync {
    /// pipeline.run_generation: render the parts that are not `ok` (or only `only`).
    fn run(&self, lib: &Library, gid: &str, only: Option<&[i64]>, cancel: Cancel) -> Result<(), JobError>;
    /// pipeline.regenerate_chunk: re-speak one part, with new text when given.
    fn respeak(&self, lib: &Library, gid: &str, ci: i64, text: Option<&str>, cancel: Cancel) -> Result<(), JobError>;
    /// pipeline.fix_remaining: re-record the parts that still need it.
    fn fix_remaining(&self, lib: &Library, gid: &str, cancel: Cancel) -> Result<(), JobError>;
    /// pipeline.select_take: keep an earlier take and re-mix. Python passes no cancel check here.
    fn select_take(&self, lib: &Library, gid: &str, ci: i64, take: i64, cancel: Cancel) -> Result<(), JobError>;
}

pub const STUB_RUNNING: &str = "Stub render";
pub const STUB_FAILED: &str = "Stub render failed.";
/// pipeline.py's message when a render is cancelled.
pub const STOPPED: &str = "Stopped. Finished parts are kept; press Resume to finish the rest.";
const PART_SECONDS: f64 = 0.5;
const PAUSE_SECONDS: f64 = 0.1;

/// A deterministic renderer for tests (rust/golden/record_http.py fakes the same steps in Python).
/// With a gate folder it waits until `<gate>/go` exists, fails when `<gate>/fail` exists, and obeys cancel.
/// shortcut: stub document, replaced by the real renderer in S16
pub struct StubRenderer {
    pub gate: Option<PathBuf>,
}

fn other(e: String) -> JobError {
    JobError::new("RuntimeError", e)
}

fn chunks_mut(g: &mut Doc) -> impl Iterator<Item = &mut Value> {
    g.get_mut("chunks").and_then(Value::as_array_mut).into_iter().flatten()
}

impl StubRenderer {
    fn file(&self, name: &str) -> bool {
        self.gate.as_ref().is_some_and(|d| d.join(name).exists())
    }

    fn render(&self, lib: &Library, gid: &str, first: Option<i64>, cancel: Cancel) -> Result<(), JobError> {
        let Some(mut g) = lib.get_generation(gid).map_err(other)? else { return Err(other(format!("no narration {gid}"))) };
        // Mark it running, with the first part to do running, in one save.
        g.insert("status".into(), "running".into());
        g.insert("message".into(), STUB_RUNNING.into());
        let first = first.or_else(|| {
            g.get("chunks")?.as_array()?.iter().find(|c| c.get("status").and_then(Value::as_str) != Some("ok"))?.get("i")?.as_i64()
        });
        for c in chunks_mut(&mut g) {
            if first.is_some() && c.get("i").and_then(Value::as_i64) == first {
                c["status"] = "running".into();
            }
        }
        lib.put_generation_live(&mut g).map_err(other)?;
        loop {
            if self.file("fail") {
                return Err(JobError::new("RuntimeError", STUB_FAILED));
            }
            if cancel() {
                g.insert("status".into(), "cancelled".into());
                g.insert("message".into(), STOPPED.into());
                for c in chunks_mut(&mut g) {
                    if c.get("status").and_then(Value::as_str) == Some("running") {
                        c["status"] = "pending".into();
                    }
                }
                lib.put_generation_live(&mut g).map_err(other)?;
                return Ok(());
            }
            if self.gate.is_none() || self.file("go") {
                return finish(lib, gid, g);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Finish as contract/seed_python.py writes a finished fixture narration: a tone per part, final.wav,
/// the timeline and duration, every part `ok`.
fn finish(lib: &Library, gid: &str, mut g: Doc) -> Result<(), JobError> {
    if !is_plain_name(gid) {
        return Err(other(format!("bad narration id {gid}")));
    }
    let dir = lib.generations_dir().join(gid);
    std::fs::create_dir_all(&dir).map_err(|e| other(e.to_string()))?;
    let mut timeline = Vec::new();
    let mut t = 0.0;
    for c in chunks_mut(&mut g) {
        let i = c.get("i").and_then(Value::as_i64).unwrap_or(0);
        let name = format!("chunk_{i:03}.wav");
        tone(&dir.join(&name), PART_SECONDS, 200.0 + 20.0 * i as f64).map_err(other)?;
        c["status"] = "ok".into();
        c["file"] = name.into();
        c["issues"] = json!([]);
        c["dur"] = PART_SECONDS.into();
        timeline.push(json!({"i": i, "start": t, "end": t + PART_SECONDS}));
        t += PART_SECONDS + PAUSE_SECONDS;
    }
    let duration = (t - PAUSE_SECONDS).max(PART_SECONDS);
    tone(&dir.join("final.wav"), duration, 220.0).map_err(other)?;
    g.insert("status".into(), "done".into());
    g.insert("message".into(), "Done. All parts passed the automated checks.".into());
    g.insert("file".into(), "final.wav".into());
    g.insert("duration".into(), duration.into());
    g.insert("timeline".into(), timeline.into());
    g.insert("progress".into(), 1.0.into());
    lib.put_generation_live(&mut g).map_err(other)?;
    Ok(())
}

impl Renderer for StubRenderer {
    fn run(&self, lib: &Library, gid: &str, only: Option<&[i64]>, cancel: Cancel) -> Result<(), JobError> {
        self.render(lib, gid, only.and_then(|o| o.first().copied()), cancel)
    }
    fn respeak(&self, lib: &Library, gid: &str, ci: i64, _text: Option<&str>, cancel: Cancel) -> Result<(), JobError> {
        self.render(lib, gid, Some(ci), cancel)
    }
    fn fix_remaining(&self, lib: &Library, gid: &str, cancel: Cancel) -> Result<(), JobError> {
        self.render(lib, gid, None, cancel)
    }
    fn select_take(&self, lib: &Library, gid: &str, ci: i64, _take: i64, cancel: Cancel) -> Result<(), JobError> {
        self.render(lib, gid, Some(ci), cancel)
    }
}
