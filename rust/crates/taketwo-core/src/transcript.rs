//! Speech-to-text result shared by the checks and Whisper (`ml.transcribe`'s shape).

use serde::{Deserialize, Serialize};

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
