//! Speech to text: whisper.cpp with DTW token times, through the flat C shim `whisper_shim.c`,
//! grouped into words the way mlx-audio does it (`words.rs`). The result has `ml.transcribe`'s shape.

pub mod words;

use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use words::{SegmentTimes, Token};

/// `ml.transcribe`'s return value, key for key and in the same order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub segments: Vec<Segment>,
    pub language: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
    pub words: Vec<Word>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub word: String,
    pub start: f64,
    pub end: f64,
    pub p: f64,
}

pub trait Transcriber {
    /// `audio` is 16 kHz mono f32 (resampling is the caller's job). `language` None = detect.
    /// `words` = word timestamps. Setting `cancel` stops the run with an error.
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>, words: bool, cancel: &AtomicBool) -> Result<Transcript, String>;
}

/// Which cross-attention heads the DTW uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heads {
    /// Every head of the top 2 decoder layers: mlx-audio's default, what the Python app uses.
    TopTwoLayers,
    /// whisper.cpp's large-v3-turbo preset (OpenAI's six heads).
    LargeV3Turbo,
}

#[repr(C)]
struct Raw {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn tt_whisper_open(path: *const c_char, use_gpu: c_int, heads: c_int, quiet: c_int) -> *mut Raw;
    fn tt_whisper_close(w: *mut Raw);
    fn tt_whisper_run(w: *mut Raw, pcm: *const f32, n: c_int, language: *const c_char, abort: *const AtomicBool, n_threads: c_int) -> c_int;
    fn tt_whisper_n_windows(w: *const Raw) -> c_int;
    fn tt_whisper_window_first_segment(w: *const Raw, i: c_int) -> c_int;
    fn tt_whisper_n_segments(w: *const Raw) -> c_int;
    fn tt_whisper_segment_t0(w: *const Raw, s: c_int) -> i64;
    fn tt_whisper_segment_t1(w: *const Raw, s: c_int) -> i64;
    fn tt_whisper_segment_no_speech(w: *const Raw, s: c_int) -> f32;
    fn tt_whisper_n_tokens(w: *const Raw, s: c_int) -> c_int;
    fn tt_whisper_token_id(w: *const Raw, s: c_int, t: c_int) -> c_int;
    fn tt_whisper_token_p(w: *const Raw, s: c_int, t: c_int) -> f32;
    fn tt_whisper_token_plog(w: *const Raw, s: c_int, t: c_int) -> f32;
    fn tt_whisper_token_dtw_t0(w: *const Raw, s: c_int, t: c_int) -> i64;
    fn tt_whisper_token_dtw_t1(w: *const Raw, s: c_int, t: c_int) -> i64;
    fn tt_whisper_token_text(w: *const Raw, id: c_int) -> *const c_char;
    fn tt_whisper_token_eot(w: *const Raw) -> c_int;
    fn tt_whisper_language(w: *const Raw) -> *const c_char;
}

/// whisper.cpp with a loaded ggml model.
pub struct WhisperCpp {
    raw: *mut Raw,
    pub n_threads: i32,
}

// The context is only used through &mut self.
unsafe impl Send for WhisperCpp {}

impl Drop for WhisperCpp {
    fn drop(&mut self) {
        unsafe { tt_whisper_close(self.raw) }
    }
}

struct Tok {
    id: i32,
    p: f32,
    plog: f32,
    t0: i64,
    t1: i64,
}

impl WhisperCpp {
    /// `use_gpu` only matters in a build with a GPU backend (the `metal` or `cuda` feature).
    pub fn open(model: &Path, use_gpu: bool, heads: Heads) -> Result<Self, String> {
        let path = CString::new(model.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let quiet = std::env::var_os("TAKETWO_WHISPER_LOG").is_none() as c_int;
        let h = match heads {
            Heads::TopTwoLayers => 0,
            Heads::LargeV3Turbo => 1,
        };
        let raw = unsafe { tt_whisper_open(path.as_ptr(), use_gpu as c_int, h, quiet) };
        if raw.is_null() {
            return Err(format!("whisper.cpp could not load {}", model.display()));
        }
        let n_threads = std::thread::available_parallelism().map(|n| n.get().min(8) as i32).unwrap_or(4);
        Ok(Self { raw, n_threads })
    }

    fn token(&self, id: i32) -> Token {
        let p = unsafe { tt_whisper_token_text(self.raw, id) };
        let bytes = if p.is_null() { Vec::new() } else { unsafe { CStr::from_ptr(p) }.to_bytes().to_vec() };
        Token { id, bytes }
    }
}

fn c_string(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

impl Transcriber for WhisperCpp {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>, words: bool, cancel: &AtomicBool) -> Result<Transcript, String> {
        let lang = language.map(|l| CString::new(l).map_err(|e| e.to_string())).transpose()?;
        let lang_ptr = lang.as_ref().map_or(std::ptr::null(), |l| l.as_ptr());
        let rc = unsafe {
            tt_whisper_run(self.raw, audio.as_ptr(), audio.len() as c_int, lang_ptr, cancel as *const AtomicBool, self.n_threads)
        };
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if rc != 0 {
            return Err(format!("whisper.cpp failed (code {rc})"));
        }
        let w = self.raw as *const Raw;
        let eot = unsafe { tt_whisper_token_eot(w) };
        let language = c_string(unsafe { tt_whisper_language(w) });
        let n_seg = unsafe { tt_whisper_n_segments(w) } as usize;

        // Window boundaries: the segment count at each encoder start (auto-detect adds a leading 0).
        let mut starts: Vec<usize> = (0..unsafe { tt_whisper_n_windows(w) })
            .map(|i| unsafe { tt_whisper_window_first_segment(w, i) } as usize)
            .collect();
        starts.push(n_seg);
        starts.dedup();

        let mut segments = Vec::with_capacity(n_seg);
        let mut all_text: Vec<Token> = Vec::new();
        let mut last_speech = 0.0f64;
        for win in starts.windows(2) {
            let (a, b) = (win[0], win[1]);
            let mut toks: Vec<Vec<Tok>> = Vec::new();
            for s in a..b {
                let si = s as c_int;
                let n = unsafe { tt_whisper_n_tokens(w, si) };
                toks.push(
                    (0..n)
                        .map(|t| unsafe {
                            Tok {
                                id: tt_whisper_token_id(w, si, t),
                                p: tt_whisper_token_p(w, si, t),
                                plog: tt_whisper_token_plog(w, si, t),
                                t0: tt_whisper_token_dtw_t0(w, si, t),
                                t1: tt_whisper_token_dtw_t1(w, si, t),
                            }
                        })
                        .collect(),
                );
            }
            // mlx-audio: one avg_logprob per window, sum of sampled-token log probs / (tokens + 1).
            // shortcut: whisper.cpp's segments hold no eot, so its log prob is missing from the sum.
            let n_win: usize = toks.iter().map(|t| t.len()).sum();
            let sum: f64 = toks.iter().flatten().map(|t| t.plog as f64).sum();
            let avg_logprob = if n_win == 0 { 0.0 } else { sum / (n_win as f64 + 1.0) };

            let mut times: Vec<SegmentTimes> = Vec::new();
            for (k, s) in (a..b).enumerate() {
                let si = s as c_int;
                let text: Vec<Token> = toks[k].iter().filter(|t| t.id < eot).map(|t| self.token(t.id)).collect();
                times.push(SegmentTimes {
                    start: unsafe { tt_whisper_segment_t0(w, si) } as f64 / 100.0,
                    end: unsafe { tt_whisper_segment_t1(w, si) } as f64 / 100.0,
                    n_text_tokens: text.len(),
                });
                let bytes: Vec<u8> = text.iter().flat_map(|t| t.bytes.iter().copied()).collect();
                segments.push(Segment {
                    start: 0.0,
                    end: 0.0,
                    text: taketwo_core::text::py::strip(&String::from_utf8_lossy(&bytes)).to_string(),
                    avg_logprob,
                    no_speech_prob: unsafe { tt_whisper_segment_no_speech(w, si) } as f64,
                    words: Vec::new(),
                });
            }

            let mut word_lists = vec![Vec::new(); b - a];
            if words {
                let text: Vec<&Tok> = toks.iter().flatten().filter(|t| t.id < eot).collect();
                let tokens: Vec<Token> = text.iter().map(|t| self.token(t.id)).collect();
                // t0 of a token is the end of the one before it (the window start for the first);
                // t1 of the last text token is where eot starts. Centiseconds, absolute.
                let mut jumps: Vec<f64> = text.iter().map(|t| t.t0 as f64 / 100.0).collect();
                if let Some(t) = text.last() {
                    jumps.push(t.t1 as f64 / 100.0);
                }
                let probs: Vec<f64> = text.iter().map(|t| t.p as f64).collect();
                let al = words::alignment(&tokens, eot, &language, &jumps, &probs);
                let (lists, last) = words::add_word_timestamps(&mut times, al, 0.0, last_speech);
                last_speech = last;
                word_lists = lists;
            }
            let base = segments.len() - (b - a);
            for (k, (t, ws)) in times.iter().zip(word_lists).enumerate() {
                let seg = &mut segments[base + k];
                seg.start = t.start;
                seg.end = t.end;
                seg.words = ws.into_iter().map(|w| Word { word: w.word, start: w.start, end: w.end, p: w.probability }).collect();
                // mlx-audio clears an instantaneous or empty segment
                if seg.start == seg.end || seg.text.is_empty() {
                    seg.text.clear();
                    seg.words.clear();
                } else {
                    all_text.extend(toks[k].iter().filter(|t| t.id < eot).map(|t| self.token(t.id)));
                }
            }
        }
        let all: Vec<u8> = all_text.iter().flat_map(|t| t.bytes.iter().copied()).collect();
        let text = taketwo_core::text::py::strip(&String::from_utf8_lossy(&all)).to_string();
        Ok(Transcript { text, segments, language })
    }
}
