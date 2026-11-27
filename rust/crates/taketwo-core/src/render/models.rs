//! The model seams the render pipeline calls (step S15; S12, S13 and S14 implement them): `Voice`
//! (engines.synthesize plus that call's engines.synthesis_report), `Transcriber` (ml.transcribe, moved here from
//! taketwo-voice) and `SpeakerEncoder` (ml.speaker_embedding), with `cosine` as in ml.py.
//! `fake` replays recorded outputs in call order and records every call, for tests without a model.

use crate::library::Doc;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub use crate::transcript::Transcriber;

/// One synthesis: mono audio, its rate, and `engines.synthesis_report()` for this call (`tokens`, `token_limit`,
/// `raw_samples`, `sample_rate`, `stop_reason`, `sampler`, `generation_route`, `decoder`). There is no global last report.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub audio: Vec<f32>,
    pub sr: u32,
    pub report: Doc,
}

pub trait Voice {
    /// `engines.synthesize(engine, text, ref_wav, ref_text, language, params, seed, expected_seconds=...)`.
    #[allow(clippy::too_many_arguments)]
    fn synthesize(
        &mut self,
        engine: &str,
        text: &str,
        ref_wav: &Path,
        ref_text: &str,
        language: &str,
        params: &Doc,
        seed: u32,
        expected_seconds: f64,
        cancel: &AtomicBool,
    ) -> Result<Take, String>;
}

pub trait SpeakerEncoder {
    /// `ml.speaker_embedding(audio, sr)`.
    fn embedding(&mut self, audio: &[f32], sr: u32) -> Result<Vec<f32>, String>;
}

/// `ml.cosine(a, b)` on float32 vectors: numpy keeps the dot product, the norms, their product and the `+ 1e-9`
/// in float32 (numpy 2 treats the Python float as weak), then `float()`.
/// shortcut: sums run in order; BLAS may sum a long vector in another order (last-bit differences), S14 measures it.
pub fn cosine(a: &[f32], b: &[f32]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!("ValueError: shapes ({},) and ({},) not aligned", a.len(), b.len()));
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    Ok((dot / (na * nb + 1e-9f32)) as f64)
}

/// A scripted stand-in for all three models and the render clock.
pub mod fake {
    use super::*;
    use crate::transcript::Transcript;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    /// What a call was; `cancel_after` counts these.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Kind {
        Synthesize,
        Transcribe,
        Embedding,
        Clock,
    }

    /// One recorded call with the arguments the golden vectors compare.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Call {
        Synthesize { engine: String, text: String, ref_file: String, ref_text: String, language: String, params: Doc, seed: u32, expected_seconds: f64 },
        /// `len` is the length handed to the trait (16 kHz).
        Transcribe { len: usize, language: Option<String>, words: bool },
        Embedding { len: usize, sr: u32 },
    }

    struct State {
        takes: VecDeque<Take>,
        transcripts: VecDeque<Transcript>,
        embeddings: VecDeque<Vec<f32>>,
        calls: Vec<Call>,
        counts: [usize; 4],
        flip: Option<(Kind, usize)>,
        flag: Arc<AtomicBool>,
        t0: f64,
        step: f64,
    }

    impl State {
        fn counted(&mut self, k: Kind) {
            self.counts[k as usize] += 1;
            if self.flip == Some((k, self.counts[k as usize])) {
                self.flag.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Clone it freely: every handle shares one script, one call list and one cancel flag.
    #[derive(Clone)]
    pub struct Fake(Rc<RefCell<State>>);

    impl Fake {
        /// The clock reads `t0 + n * step` on its n-th reading (n from 0), as the recorder's clock does.
        pub fn new(takes: Vec<Take>, transcripts: Vec<Transcript>, embeddings: Vec<Vec<f32>>, t0: f64, step: f64) -> Fake {
            Fake(Rc::new(RefCell::new(State {
                takes: takes.into(),
                transcripts: transcripts.into(),
                embeddings: embeddings.into(),
                calls: Vec::new(),
                counts: [0; 4],
                flip: None,
                flag: Arc::new(AtomicBool::new(false)),
                t0,
                step,
            })))
        }

        /// Set the cancel flag when the `n`-th call of `kind` (from 1) has run; n = 0 sets it now.
        pub fn cancel_after(&self, kind: Kind, n: usize) {
            let mut s = self.0.borrow_mut();
            if n == 0 {
                s.flag.store(true, Ordering::SeqCst);
            } else {
                s.flip = Some((kind, n));
            }
        }

        pub fn flag(&self) -> Arc<AtomicBool> {
            self.0.borrow().flag.clone()
        }

        pub fn calls(&self) -> Vec<Call> {
            self.0.borrow().calls.clone()
        }

        /// Scripted outputs not handed out yet: (takes, transcripts, embeddings).
        pub fn left(&self) -> (usize, usize, usize) {
            let s = self.0.borrow();
            (s.takes.len(), s.transcripts.len(), s.embeddings.len())
        }

        /// One clock reading.
        pub fn now(&self) -> f64 {
            let mut s = self.0.borrow_mut();
            let t = s.t0 + s.counts[Kind::Clock as usize] as f64 * s.step;
            s.counted(Kind::Clock);
            t
        }
    }

    impl Voice for Fake {
        fn synthesize(
            &mut self,
            engine: &str,
            text: &str,
            ref_wav: &Path,
            ref_text: &str,
            language: &str,
            params: &Doc,
            seed: u32,
            expected_seconds: f64,
            _cancel: &AtomicBool,
        ) -> Result<Take, String> {
            let mut s = self.0.borrow_mut();
            let ref_file = ref_wav.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            s.calls.push(Call::Synthesize {
                engine: engine.into(), text: text.into(), ref_file, ref_text: ref_text.into(), language: language.into(),
                params: params.clone(), seed, expected_seconds,
            });
            let t = s.takes.pop_front().ok_or("scripted fake: no take left")?;
            s.counted(Kind::Synthesize);
            Ok(t)
        }
    }

    impl Transcriber for Fake {
        fn transcribe(&mut self, audio: &[f32], language: Option<&str>, words: bool, _cancel: &AtomicBool) -> Result<Transcript, String> {
            let mut s = self.0.borrow_mut();
            s.calls.push(Call::Transcribe { len: audio.len(), language: language.map(str::to_string), words });
            let t = s.transcripts.pop_front().ok_or("scripted fake: no transcript left")?;
            s.counted(Kind::Transcribe);
            Ok(t)
        }
    }

    impl SpeakerEncoder for Fake {
        fn embedding(&mut self, audio: &[f32], sr: u32) -> Result<Vec<f32>, String> {
            let mut s = self.0.borrow_mut();
            s.calls.push(Call::Embedding { len: audio.len(), sr });
            let e = s.embeddings.pop_front().ok_or("scripted fake: no embedding left")?;
            s.counted(Kind::Embedding);
            Ok(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_small_integer_vectors_is_exact() {
        // float32(4/5) and float32(12/13): every sum and norm is exact for these
        assert_eq!(cosine(&[4.0, 3.0], &[1.0, 0.0]).unwrap(), 0.8f32 as f64);
        assert_eq!(cosine(&[12.0, 5.0], &[1.0, 0.0]).unwrap(), (12.0f32 / 13.0f32) as f64);
        assert!(cosine(&[1.0], &[1.0, 0.0]).is_err());
    }
}
