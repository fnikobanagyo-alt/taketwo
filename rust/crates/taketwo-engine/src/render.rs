//! The one seam between the engine and the voice: `Renderer`, the four pipeline calls main.py's jobs make
//! (pipeline.run_generation, regenerate_chunk, fix_remaining, select_take). The real one arrives in S15/S16.
//! `StubRenderer` is always compiled, so in-process tests and `serve --stub-renderer` (isolated mode only) reach it.

use crate::seed::tone;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use taketwo_core::reference;
use taketwo_core::render::models::{SpeakerEncoder, Take, Transcriber, Voice};
use taketwo_core::render::preview::{self, Preview};
use taketwo_core::render::{Render, RenderError};
use taketwo_core::transcript::{Segment, Transcript, Word};
use crate::worker::JobError;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use taketwo_core::library::{Doc, Library, is_plain_name};

pub type Cancel<'a> = &'a (dyn Fn() -> bool + Sync);

pub trait Renderer: Send + Sync {
    /// pipeline.run_generation: render the parts that are not `ok` (or only `only`).
    fn run(&self, lib: &Library, gid: &str, only: Option<&[i64]>, cancel: Cancel) -> Result<(), JobError>;
    /// pipeline.regenerate_chunk: re-speak one part, with new text when given.
    fn respeak(&self, lib: &Library, gid: &str, ci: i64, text: Option<&str>, cancel: Cancel) -> Result<(), JobError>;
    /// pipeline.fix_remaining: re-record the parts that still need it.
    fn fix_remaining(&self, lib: &Library, gid: &str, cancel: Cancel) -> Result<(), JobError>;
    /// pipeline.select_take: keep an earlier take and re-mix. Python passes no cancel check here.
    fn select_take(&self, lib: &Library, gid: &str, ci: i64, take: i64, cancel: Cancel) -> Result<(), JobError>;
    /// Whisper and the speaker encoder, for reference.analyze and refresh_clips (the voice model is not loaded).
    fn voice_tools(&self, cancel: Cancel, f: &mut dyn FnMut(&mut reference::Models) -> Result<(), RenderError>) -> Result<(), JobError>;
    /// pipeline.make_preview, with `is_downloaded` answered by `can_preview`.
    fn preview(&self, lib: &Library, pid: &str, cancel: Cancel) -> Result<Preview, JobError>;
    /// The voice model files are there (`engines.is_downloaded`), so a preview can be made.
    fn can_preview(&self) -> bool;
}

/// A job's result from taketwo-core: a stop is not an error (the caller looks at the cancel flag).
pub fn job_error(e: RenderError) -> Result<(), JobError> {
    match e {
        RenderError::Cancelled => Ok(()),
        RenderError::Value(m) => Err(JobError::new("ValueError", m)),
        RenderError::Other(m) => Err(JobError::new("RuntimeError", m)),
    }
}

/// Run `f` with an `AtomicBool` that follows the job's cancel check (the models read a flag; only another
/// thread can flip it during a long call). The watcher ends on every exit of `f`, a panic included.
pub fn bridged<T>(cancel: Cancel, f: impl FnOnce(&AtomicBool) -> T) -> T {
    struct SetOnDrop<'a>(&'a AtomicBool);
    impl Drop for SetOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (flag, done) = (AtomicBool::new(false), AtomicBool::new(false));
    std::thread::scope(|sc| {
        let _done = SetOnDrop(&done);
        sc.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                if cancel() {
                    flag.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        f(&flag)
    })
}

/// Stand-in models for `--stub-renderer` and the engine tests: a transcript made from the sound regions (one
/// sentence " word." per region, four to a segment), one fixed embedding, and a tone with a natural decay as the
/// voice. No model is loaded.
pub struct StubModels;

pub const STUB_EMBEDDING_DIM: usize = 256;

impl Transcriber for StubModels {
    fn transcribe(&mut self, audio: &[f32], _language: Option<&str>, _words: bool, _cancel: &AtomicBool) -> Result<Transcript, String> {
        let (regions, _, _) = taketwo_core::audio::sound_regions(audio, 16000, 130);
        let mut segments: Vec<Segment> = Vec::new();
        for (k, &(a, b)) in regions.iter().enumerate() {
            let w = Word { word: " word.".into(), start: a as f64 * 0.01, end: b as f64 * 0.01, p: 0.9 };
            if k % 4 == 0 {
                segments.push(Segment { start: w.start, end: w.end, text: String::new(), avg_logprob: -0.2, no_speech_prob: 0.01, words: Vec::new() });
            }
            let s = segments.last_mut().expect("pushed above");
            s.end = w.end;
            s.text.push_str(&w.word);
            s.words.push(w);
        }
        let text = segments.iter().map(|s| s.text.as_str()).collect();
        Ok(Transcript { text, segments, language: "en".into() })
    }
}

impl SpeakerEncoder for StubModels {
    fn embedding(&mut self, _audio: &[f32], _sr: u32) -> Result<Vec<f32>, String> {
        let mut e = vec![0.0f32; STUB_EMBEDDING_DIM];
        e[0] = 1.0;
        Ok(e)
    }
}

impl Voice for StubModels {
    fn synthesize(&mut self, _engine: &str, _text: &str, _ref_wav: &Path, _ref_text: &str, _language: &str, _params: &Doc, seed: u32,
                  expected_seconds: f64, _cancel: &AtomicBool) -> Result<Take, String> {
        let sr = 24000u32;
        let n = (expected_seconds.clamp(0.5, 8.0) * sr as f64) as usize;
        let hz = 180.0 + (seed % 7) as f64 * 10.0;
        let tail = (0.2 * sr as f64) as usize; // the last 0.2 s fade out (np.geomspace(1, 1e-5)), so no take ends cut off
        let audio = (0..n)
            .map(|i| {
                let fade = if i + tail >= n { 10f64.powf(-5.0 * (i + tail - n) as f64 / (tail - 1) as f64) } else { 1.0 };
                (0.3 * fade * (2.0 * std::f64::consts::PI * hz * i as f64 / sr as f64).sin()) as f32
            })
            .collect();
        let report = json!({"stop_reason": "eos", "sample_rate": sr, "generation_route": "stub", "decoder": "stub"});
        Ok(Take { audio, sr, report: report.as_object().expect("an object").clone() })
    }
}

/// `make_preview` over the given models, with the library's profile writes and the wall clock.
pub fn preview_with(lib: &Library, pid: &str, voice: &mut dyn Voice, transcriber: &mut dyn Transcriber, encoder: &mut dyn SpeakerEncoder,
                    flag: &AtomicBool, pitch_cache: &mut std::collections::HashMap<String, f64>, phase: taketwo_core::render::Phase,
                    downloaded: bool) -> Result<Preview, RenderError> {
    let (pd, gd) = (lib.profiles_dir(), lib.generations_dir());
    let mut put = |p: &mut Doc| lib.put_profile(p);
    let mut clock = crate::worker::wall_clock;
    let mut r = Render {
        profiles_dir: &pd, generations_dir: &gd, voice, transcriber, encoder, put_profile: &mut put, clock: &mut clock,
        cancel: flag, pitch_cache, phase,
    };
    preview::make_preview(&mut r, lib, pid, downloaded)
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
    fn voice_tools(&self, cancel: Cancel, f: &mut dyn FnMut(&mut reference::Models) -> Result<(), RenderError>) -> Result<(), JobError> {
        let (mut t, mut e) = (StubModels, StubModels);
        bridged(cancel, |flag| f(&mut reference::Models { transcriber: &mut t, encoder: &mut e, cancel: flag })).or_else(job_error)
    }
    fn preview(&self, lib: &Library, pid: &str, cancel: Cancel) -> Result<Preview, JobError> {
        let downloaded = self.can_preview();
        let (mut v, mut t, mut e) = (StubModels, StubModels, StubModels);
        let mut cache = std::collections::HashMap::new();
        match bridged(cancel, |flag| preview_with(lib, pid, &mut v, &mut t, &mut e, flag, &mut cache, &taketwo_core::render::no_phase, downloaded)) {
            Ok(p) => Ok(p),
            Err(e) => job_error(e).map(|_| Preview::Nothing),
        }
    }
    /// As the real renderer: the Qwen model folder under `TAKETWO_MODELS` (no model is read).
    fn can_preview(&self) -> bool {
        crate::real::models_dir().join(taketwo_voice::qwen::MODEL_DIR).is_dir()
    }
}
