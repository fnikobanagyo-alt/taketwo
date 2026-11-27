//! The real renderer (step S16): a thin wrapper that gives taketwo-core's narration steps the three real models
//! (Qwen voice, whisper.cpp, the speaker encoder) behind one mutex, the wall clock, a random nonce and the job's
//! phase hook. Models load on the first job (never at start), each from `TAKETWO_MODELS`; a missing file is a job
//! error naming it. They are released after `IDLE_UNLOAD_SECONDS` without a job, as main.py `_release_models`.
//! `TAKETWO_MAX_TAKES` caps the synthesis calls of one engine run.

use crate::render::{bridged, job_error, preview_with, Cancel, Renderer};
use taketwo_core::reference;
use taketwo_core::render::preview::Preview;
use crate::worker::{self, Ids, JobError, Worker};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use taketwo_core::library::{Doc, Library};
use taketwo_core::render::models::{Take, Voice};
use taketwo_core::render::narration::{self, Ctx, Models};
use taketwo_core::render::RenderError;
use taketwo_core::transcript::{Transcriber, Transcript};
use taketwo_voice::encoder::VoiceEncoder;
use taketwo_voice::qwen::{self, QwenVoice};
use taketwo_voice::whisper::{Heads, WhisperCpp};

/// The Whisper model file under `TAKETWO_MODELS` (step S13's conversion).
pub const WHISPER_FILE: &str = "ggml-large-v3-turbo.bin";

/// whisper.cpp, opened on its first transcription (opening loads the model).
struct Whisper {
    path: PathBuf,
    open: Option<WhisperCpp>,
}

impl Transcriber for Whisper {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>, words: bool, cancel: &AtomicBool) -> Result<Transcript, String> {
        if self.open.is_none() {
            // as the whisper_parity test opens it: lead decision 13's heads; flash attention is off in the shim
            self.open = Some(WhisperCpp::open(&self.path, cfg!(any(feature = "metal", feature = "cuda")), Heads::TopTwoLayers)?);
        }
        self.open.as_mut().expect("opened above").transcribe(audio, language, words, cancel)
    }
}

/// `Voice` with the take cap: over `cap` synthesis calls in this engine run, the take fails with a clear message.
pub struct Capped<'a> {
    pub inner: &'a mut dyn Voice,
    pub used: &'a AtomicU32,
    pub cap: Option<u32>,
}

impl Voice for Capped<'_> {
    fn synthesize(&mut self, engine: &str, text: &str, ref_wav: &Path, ref_text: &str, language: &str, params: &Doc, seed: u32,
                  expected_seconds: f64, cancel: &AtomicBool) -> Result<Take, String> {
        if let Some(cap) = self.cap {
            if self.used.load(Ordering::SeqCst) >= cap {
                return Err(format!("TAKETWO_MAX_TAKES is {cap}: this engine run has recorded all {cap} takes it may, so the narration stops here."));
            }
        }
        let n = self.used.fetch_add(1, Ordering::SeqCst) + 1;
        // one stderr line per synthesis call (lead decision 21); never part of a reply
        match self.cap {
            Some(cap) => eprintln!("take {n} of {cap}"),
            None => eprintln!("take {n}"),
        }
        self.inner.synthesize(engine, text, ref_wav, ref_text, language, params, seed, expected_seconds, cancel)
    }
}

struct Loaded {
    qwen: QwenVoice,
    whisper: Whisper,
    encoder: VoiceEncoder,
    pitch_cache: HashMap<String, f64>,
}

impl Loaded {
    fn any(&self) -> bool {
        self.qwen.is_loaded() || self.whisper.open.is_some() || self.encoder.is_loaded()
    }
    fn release(&mut self) {
        self.qwen.unload();
        self.whisper.open = None;
        self.encoder.unload();
    }
}

pub struct RealRenderer {
    files: [PathBuf; 3],
    models: Mutex<Loaded>,
    /// The engine's worker, for the phase hook (set once the app state exists).
    pub worker: OnceLock<Worker>,
    used: AtomicU32,
    cap: Option<u32>,
    last_job: Mutex<Instant>,
    ids: Ids,
}

/// `IDLE_UNLOAD_SECONDS` (config.py: `TAKETWO_IDLE_UNLOAD_S`, then `VOICEFORGE_IDLE_UNLOAD_S`, else 300).
pub fn idle_unload() -> Duration {
    let v = ["TAKETWO_IDLE_UNLOAD_S", "VOICEFORGE_IDLE_UNLOAD_S"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.is_empty());
    Duration::from_secs_f64(v.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| s.is_finite() && *s >= 0.0).unwrap_or(300.0))
}

/// Where the models are: `TAKETWO_MODELS`, else `<app data>/TakeTwo/models`.
pub fn models_dir() -> PathBuf {
    std::env::var_os("TAKETWO_MODELS")
        .filter(|m| !m.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_default().join("TakeTwo").join("models"))
}

impl RealRenderer {
    /// Nothing is loaded here. A thread releases the models after `idle` without a job.
    pub fn new(models: &Path, cap: Option<u32>, idle: Duration) -> Arc<RealRenderer> {
        let encoder = std::env::var_os("TAKETWO_VOICE_ENCODER").map(PathBuf::from).unwrap_or_else(|| models.join("voice-encoder/ve.safetensors"));
        let files = [models.join(qwen::MODEL_DIR), models.join(WHISPER_FILE), encoder];
        let r = Arc::new(RealRenderer {
            models: Mutex::new(Loaded {
                qwen: QwenVoice::new(&files[0]),
                whisper: Whisper { path: files[1].clone(), open: None },
                encoder: VoiceEncoder::new(&files[2]),
                pitch_cache: HashMap::new(),
            }),
            files,
            worker: OnceLock::new(),
            used: AtomicU32::new(0),
            cap,
            last_job: Mutex::new(Instant::now()),
            ids: worker::random_ids(),
        });
        let weak: Weak<RealRenderer> = Arc::downgrade(&r);
        std::thread::Builder::new()
            .name("taketwo-idle-unload".into())
            .spawn(move || {
                while let Some(r) = weak.upgrade() {
                    let quiet = r.last_job.lock().unwrap_or_else(|p| p.into_inner()).elapsed();
                    // never while a job holds the models
                    if quiet >= idle && let Ok(mut m) = r.models.try_lock() && m.any() {
                        m.release();
                        eprintln!("Idle for {:.0} s: released the voice and speech models.", idle.as_secs_f64());
                    }
                    drop(r);
                    std::thread::sleep(Duration::from_secs(1));
                }
            })
            .expect("idle unload thread");
        r
    }

    /// `engines.load(engine)` without the download: every model file must be there. The voice itself loads on its
    /// first take (inside the job, so a stop or a failure is reported as for any take).
    fn check_files(files: &[PathBuf; 3]) -> Result<(), String> {
        for f in files {
            if !f.exists() {
                return Err(format!("A model file TakeTwo needs is missing: {}. Put it in the models folder (TAKETWO_MODELS) and try again.", f.display()));
            }
        }
        Ok(())
    }

    fn phase(&self) -> impl Fn(&str, &mut dyn FnMut()) + '_ {
        move |name: &str, f: &mut dyn FnMut()| match self.worker.get() {
            Some(w) => w.phase(name, || f()),
            None => f(),
        }
    }

    /// Run one narration step with the models, the cancel flag kept in step with the job's cancel check.
    fn with_models(&self, lib: &Library, cancel: Cancel, f: impl FnOnce(&mut Ctx, &mut Models) -> Result<(), RenderError>) -> Result<(), JobError> {
        let mut guard = self.models.lock().unwrap_or_else(|p| p.into_inner());
        let m = &mut *guard;
        let out = bridged(cancel, |flag| {
            let phase = self.phase();
            let mut clock = worker::wall_clock;
            let mut nonce = || u32::from_str_radix(&(self.ids)()[..8], 16).unwrap_or(0);
            let files = &self.files;
            let mut voice = Capped { inner: &mut m.qwen, used: &self.used, cap: self.cap };
            let mut load = || Self::check_files(files);
            let mut cx = Ctx { lib, clock: &mut clock, phase: &phase };
            let mut models = Models {
                voice: &mut voice,
                transcriber: &mut m.whisper,
                encoder: &mut m.encoder,
                cancel: flag,
                nonce: &mut nonce,
                load: &mut load,
                pitch_cache: &mut m.pitch_cache,
            };
            f(&mut cx, &mut models)
        });
        *self.last_job.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
        out.or_else(job_error)
    }
}

impl Renderer for RealRenderer {
    fn run(&self, lib: &Library, gid: &str, only: Option<&[i64]>, cancel: Cancel) -> Result<(), JobError> {
        self.with_models(lib, cancel, |cx, m| narration::run_generation(cx, m, gid, only))
    }
    fn respeak(&self, lib: &Library, gid: &str, ci: i64, text: Option<&str>, cancel: Cancel) -> Result<(), JobError> {
        self.with_models(lib, cancel, |cx, m| narration::regenerate_chunk(cx, m, gid, ci, text))
    }
    fn fix_remaining(&self, lib: &Library, gid: &str, cancel: Cancel) -> Result<(), JobError> {
        self.with_models(lib, cancel, |cx, m| narration::fix_remaining(cx, m, gid))
    }
    fn select_take(&self, lib: &Library, gid: &str, ci: i64, take: i64, _cancel: Cancel) -> Result<(), JobError> {
        // no model: a take switch only re-mixes
        let phase = self.phase();
        let mut clock = worker::wall_clock;
        let mut cx = Ctx { lib, clock: &mut clock, phase: &phase };
        let r = narration::select_take(&mut cx, gid, ci, take);
        *self.last_job.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
        r.or_else(job_error)
    }
    fn voice_tools(&self, cancel: Cancel, f: &mut dyn FnMut(&mut reference::Models) -> Result<(), RenderError>) -> Result<(), JobError> {
        let mut guard = self.models.lock().unwrap_or_else(|p| p.into_inner());
        let m = &mut *guard;
        // the analysis needs Whisper and the encoder only; a missing file is named before anything loads
        if let Some(f) = self.files[1..].iter().find(|f| !f.exists()) {
            return Err(JobError::new("RuntimeError", format!("A model file TakeTwo needs is missing: {}. Put it in the models folder (TAKETWO_MODELS) and try again.", f.display())));
        }
        let out = bridged(cancel, |flag| f(&mut reference::Models { transcriber: &mut m.whisper, encoder: &mut m.encoder, cancel: flag }));
        *self.last_job.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
        out.or_else(job_error)
    }
    fn preview(&self, lib: &Library, pid: &str, cancel: Cancel) -> Result<Preview, JobError> {
        let downloaded = self.can_preview();
        let mut guard = self.models.lock().unwrap_or_else(|p| p.into_inner());
        let m = &mut *guard;
        let phase = self.phase();
        // the preview's takes go through the take cap, as a narration's do
        let out = bridged(cancel, |flag| {
            let mut voice = Capped { inner: &mut m.qwen, used: &self.used, cap: self.cap };
            preview_with(lib, pid, &mut voice, &mut m.whisper, &mut m.encoder, flag, &mut m.pitch_cache, &phase, downloaded)
        });
        *self.last_job.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
        match out {
            Ok(p) => Ok(p),
            Err(e) => job_error(e).map(|_| Preview::Nothing),
        }
    }
    fn can_preview(&self) -> bool {
        Self::check_files(&self.files).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts its calls and returns a short silent take.
    struct Counting(u32);

    impl Voice for Counting {
        fn synthesize(&mut self, _: &str, _: &str, _: &Path, _: &str, _: &str, _: &Doc, _: u32, _: f64, _: &AtomicBool) -> Result<Take, String> {
            self.0 += 1;
            Ok(Take { audio: vec![0.0; 1920], sr: 24000, report: Doc::new() })
        }
    }

    #[test]
    fn the_take_cap_stops_synthesis_over_the_limit() {
        let (used, never) = (AtomicU32::new(0), AtomicBool::new(false));
        let mut inner = Counting(0);
        let mut v = Capped { inner: &mut inner, used: &used, cap: Some(2) };
        let say = |v: &mut Capped| v.synthesize("qwen3-tts-1.7b", "Hi.", Path::new("r.wav"), "r", "en", &Doc::new(), 1, 1.0, &never);
        assert!(say(&mut v).is_ok() && say(&mut v).is_ok());
        let e = say(&mut v).unwrap_err();
        assert!(e.contains("TAKETWO_MAX_TAKES is 2"), "{e}");
        assert!(say(&mut v).is_err(), "it stays over the cap for the engine run");
        assert_eq!((inner.0, used.load(Ordering::SeqCst)), (2, 2), "the model was not called over the cap");
        let mut free = Counting(0);
        let mut v = Capped { inner: &mut free, used: &used, cap: None };
        for _ in 0..30 {
            say(&mut v).unwrap();
        }
    }

    #[test]
    fn a_panic_in_the_step_ends_the_watcher_and_reaches_the_caller() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::open(tmp.path()).unwrap();
        let r = RealRenderer::new(&tmp.path().join("models"), None, Duration::from_secs(300));
        let never = || false;
        let t = Instant::now();
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.with_models(&lib, &never, |_, _| panic!("boom"))));
        assert!(out.is_err(), "the panic is passed on");
        assert!(t.elapsed() < Duration::from_secs(5), "the watcher ended without a cancel");
    }

    #[test]
    fn a_missing_model_file_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let r = RealRenderer::new(tmp.path(), None, Duration::from_secs(300));
        let e = RealRenderer::check_files(&r.files).unwrap_err();
        assert!(e.contains(qwen::MODEL_DIR), "{e}");
        std::fs::create_dir(tmp.path().join(qwen::MODEL_DIR)).unwrap();
        let e = RealRenderer::check_files(&r.files).unwrap_err();
        assert!(e.contains(WHISPER_FILE), "{e}");
    }
}
