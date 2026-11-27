//! Speech-to-text result shared by the checks and Whisper (`ml.transcribe`'s shape).

use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;

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

/// Speech to text (whisper.cpp in taketwo-voice; a scripted fake in tests). Moved here from taketwo-voice in S15.
pub trait Transcriber {
    /// `audio` is 16 kHz mono f32 (resampling is the caller's job). `language` None = detect.
    /// `words` = word timestamps. Setting `cancel` stops the run with an error.
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>, words: bool, cancel: &AtomicBool) -> Result<Transcript, String>;
}
