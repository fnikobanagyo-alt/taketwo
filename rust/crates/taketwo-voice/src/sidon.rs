//! Studio restoration (step S18): backend/app/studio.py `_restore_segment` with Sidon's fp16 ONNX predictor and
//! vocoder through `ort` (CPU provider, errors-only logs, as Python's `log_severity_level = 3`), and the w2v-BERT
//! feature extractor ported by hand from transformers' `SeamlessM4TFeatureExtractor` and the `audio_utils`
//! functions it calls (`mel_filter_bank`, `window_function`, `spectrogram`).
//! Both sessions take and give float32 (`input_features` [1, T, 160] -> `features` [1, T, 1024] -> `audio` [1, M]);
//! the fp16 weights stay inside the graphs. Sessions load on the first segment and go when the `Sidon` is dropped.

use ort::logging::LogLevel;
use ort::session::Session;
use ort::value::Tensor;
use realfft::RealFftPlanner;
use std::path::{Path, PathBuf};
use taketwo_core::render::models::Restorer;

/// The folder under `TAKETWO_MODELS` and its two files (soniqo/Sidon-ONNX, `fp16/`).
pub const DIR: &str = "sidon";
pub const PREDICTOR: &str = "sidon-predictor.onnx";
pub const VOCODER: &str = "sidon-vocoder.onnx";

// facebook/w2v-bert-2.0 preprocessor_config.json: SeamlessM4TFeatureExtractor, 80 mel bins, stride 2,
// padding value 1, 16 kHz (no file is read for these).
const SR: usize = 16000;
const MEL_BINS: usize = 80;
const STRIDE: usize = 2;
const PADDING_VALUE: f32 = 1.0;
const FRAME: usize = 400;
const HOP: usize = 160;
const FFT: usize = 512;
const BINS: usize = FFT / 2 + 1;
const MEL_FLOOR: f64 = 1.192092955078125e-07;

/// `np.linspace(a, b, n)` (float64).
fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    let step = (b - a) / (n - 1) as f64;
    let mut v: Vec<f64> = (0..n).map(|i| i as f64 * step + a).collect();
    v[n - 1] = b;
    v
}

fn hz_to_mel_kaldi(f: f64) -> f64 {
    1127.0 * (1.0 + f / 700.0).ln()
}

/// `mel_filter_bank(257, 80, 20, 8000, 16000, norm=None, mel_scale="kaldi", triangularize_in_mel_space=True)`,
/// row k = FFT bin, column j = mel filter.
fn mel_filters() -> Vec<[f64; MEL_BINS]> {
    let filter_freqs = linspace(hz_to_mel_kaldi(20.0), hz_to_mel_kaldi((SR / 2) as f64), MEL_BINS + 2);
    let width = SR as f64 / ((BINS - 1) * 2) as f64;
    let diff: Vec<f64> = filter_freqs.windows(2).map(|w| w[1] - w[0]).collect();
    (0..BINS)
        .map(|k| {
            let fft = hz_to_mel_kaldi(width * k as f64);
            let mut row = [0.0; MEL_BINS];
            for (j, r) in row.iter_mut().enumerate() {
                let down = -(filter_freqs[j] - fft) / diff[j];
                let up = (filter_freqs[j + 2] - fft) / diff[j + 1];
                *r = 0f64.max(down.min(up));
            }
            row
        })
        .collect()
}

/// `window_function(400, "povey", periodic=False)`: `np.hanning(400) ** 0.85`.
fn povey() -> Vec<f64> {
    (0..FRAME).map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (FRAME - 1) as f64).cos()).powf(0.85)).collect()
}

/// `SeamlessM4TFeatureExtractor(...)(x, sampling_rate=16000)["input_features"][0]`: (frames / 2) rows of 160,
/// returned flat with the row count. Kaldi-style log-mel (x 2^15, DC removed per frame, pre-emphasis 0.97, povey
/// window, power spectrum, mel floor, natural log), normalised per mel bin (ddof 1), padded to an even frame count
/// with 1.0, then two frames per row.
pub fn features(x: &[f32]) -> (Vec<f32>, usize) {
    let mel = mel_filters();
    let win = povey();
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(FFT);
    let wave: Vec<f64> = x.iter().map(|v| (v * 32768.0) as f64).collect();
    // spectrogram(center=False): int(1 + floor((n - 400) / 160)) frames
    let frames = if wave.len() < FRAME { 0 } else { 1 + (wave.len() - FRAME) / HOP };
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut feats: Vec<[f32; MEL_BINS]> = Vec::with_capacity(frames + 1);
    for f in 0..frames {
        let src = &wave[f * HOP..f * HOP + FRAME];
        let mean = src.iter().sum::<f64>() / FRAME as f64;
        buf.iter_mut().for_each(|b| *b = 0.0);
        for i in 0..FRAME {
            buf[i] = src[i] - mean;
        }
        // numpy evaluates `buffer[1:] -= 0.97 * buffer[:-1]` on the old values: walk backwards
        for i in (1..FRAME).rev() {
            buf[i] -= 0.97 * buf[i - 1];
        }
        buf[0] *= 1.0 - 0.97;
        for i in 0..FRAME {
            buf[i] *= win[i];
        }
        fft.process(&mut buf, &mut spec).expect("fixed FFT sizes");
        // the spectrum is stored as complex64, then |.| ** 2 in float64
        let power: Vec<f64> = spec.iter().map(|c| ((c.re as f32 as f64).powi(2) + (c.im as f32 as f64).powi(2)).sqrt().powi(2)).collect();
        let mut row = [0f32; MEL_BINS];
        for (j, r) in row.iter_mut().enumerate() {
            let e: f64 = (0..BINS).map(|k| mel[k][j] * power[k]).sum();
            *r = e.max(MEL_FLOOR).ln() as f32;
        }
        feats.push(row);
    }
    // per mel bin: (x - mean) / sqrt(var(ddof=1) + 1e-7)
    if frames > 0 {
        for j in 0..MEL_BINS {
            let mean = feats.iter().map(|r| r[j] as f64).sum::<f64>() / frames as f64;
            let var = feats.iter().map(|r| (r[j] as f64 - mean).powi(2)).sum::<f64>() / (frames as f64 - 1.0);
            let sd = (var + 1e-7).sqrt();
            for r in feats.iter_mut() {
                r[j] = ((r[j] as f64 - mean) / sd) as f32;
            }
        }
    }
    // pad(pad_to_multiple_of=2) after the normalisation, with padding_value
    if frames % STRIDE != 0 {
        feats.push([PADDING_VALUE; MEL_BINS]);
    }
    let rows = feats.len() / STRIDE;
    (feats.into_iter().flatten().collect(), rows)
}

/// The two Sidon sessions, opened on the first segment.
pub struct Sidon {
    dir: PathBuf,
    sessions: Option<(Session, Session)>,
}

fn err(what: &str, e: impl std::fmt::Display) -> String {
    format!("Studio restoration (Sidon) failed at {what}: {e}")
}

impl Sidon {
    /// Nothing is loaded here.
    pub fn new(dir: &Path) -> Sidon {
        Sidon { dir: dir.to_path_buf(), sessions: None }
    }

    pub fn is_loaded(&self) -> bool {
        self.sessions.is_some()
    }

    /// Open both sessions (the CPU provider, errors-only logs).
    pub fn load(&mut self) -> Result<(), String> {
        if self.sessions.is_some() {
            return Ok(());
        }
        let env = ort::init().with_name("taketwo-sidon").build().map_err(|e| err("the ONNX Runtime start", e))?;
        env.set_log_level(LogLevel::Error);
        let open = |name: &str| -> Result<Session, String> {
            let path = self.dir.join(name);
            Session::builder(&env)
                .map_err(|e| err(name, e))?
                .with_log_level(LogLevel::Error)
                .map_err(|e| err(name, e))?
                .commit_from_file(&path)
                .map_err(|e| err(name, e))
        };
        self.sessions = Some((open(PREDICTOR)?, open(VOCODER)?));
        Ok(())
    }

    /// Name, shape and element type of every input and output of both sessions (loads them).
    pub fn describe(&mut self) -> Result<String, String> {
        self.load()?;
        let (p, v) = self.sessions.as_ref().expect("loaded above");
        let mut s = String::new();
        for (name, sess) in [(PREDICTOR, p), (VOCODER, v)] {
            for i in sess.inputs() {
                s += &format!("{name} input {}: {}\n", i.name(), i.dtype());
            }
            for o in sess.outputs() {
                s += &format!("{name} output {}: {}\n", o.name(), o.dtype());
            }
        }
        Ok(s)
    }
}

impl Restorer for Sidon {
    fn restore_segment(&mut self, x16: &[f32]) -> Result<Vec<f32>, String> {
        self.load()?;
        let (pred, voc) = self.sessions.as_mut().expect("loaded above");
        // a little tail room so the last word decodes fully
        let mut x = x16.to_vec();
        x.extend(std::iter::repeat_n(0.0f32, SR / 2));
        let (feats, rows) = features(&x);
        let input = Tensor::from_array(([1usize, rows, MEL_BINS * STRIDE], feats)).map_err(|e| err("the features", e))?;
        let out = pred.run(ort::inputs!["input_features" => input]).map_err(|e| err("the predictor", e))?;
        let (shape, h) = out[0].try_extract_tensor::<f32>().map_err(|e| err("the predictor output", e))?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        let h = Tensor::from_array((dims, h.to_vec())).map_err(|e| err("the predictor output", e))?;
        drop(out);
        let out = voc.run(ort::inputs!["features" => h]).map_err(|e| err("the vocoder", e))?;
        let (_, y) = out[0].try_extract_tensor::<f32>().map_err(|e| err("the vocoder output", e))?;
        Ok(y[..y.len().min(x16.len() * 3)].to_vec())
    }

    fn available(&self) -> bool {
        self.dir.join(PREDICTOR).is_file() && self.dir.join(VOCODER).is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mel_filters_are_triangles_inside_20_to_8000_hz() {
        let m = mel_filters();
        assert_eq!(m.len(), 257);
        // bin 0 (0 Hz) is under 20 Hz: no filter; every filter has a positive peak of at most 1
        assert!(m[0].iter().all(|v| *v == 0.0));
        for j in 0..MEL_BINS {
            let peak = m.iter().map(|r| r[j]).fold(0.0, f64::max);
            assert!(peak > 0.0 && peak <= 1.0, "filter {j}: {peak}");
        }
    }

    #[test]
    fn features_have_two_frames_per_row_and_pad_odd_counts_with_one() {
        // 400 + 160 * 4 samples: 5 frames, padded to 6, 3 rows of 160
        let x: Vec<f32> = (0..1040).map(|i| ((i * 7919) % 200) as f32 / 1000.0 - 0.1).collect();
        let (f, rows) = features(&x);
        assert_eq!((rows, f.len()), (3, 480));
        assert!(f[400..].iter().all(|v| *v == 1.0), "the padded frame is 1.0");
        // 6 frames: no pad, and every bin is normalised to mean 0
        let x: Vec<f32> = (0..1200).map(|i| ((i * 7919) % 200) as f32 / 1000.0 - 0.1).collect();
        let (f, rows) = features(&x);
        assert_eq!(rows, 3);
        let bin0: f32 = (0..6).map(|fr| f[fr * 80]).sum();
        assert!(bin0.abs() < 1e-4, "{bin0}");
    }

    #[test]
    fn a_missing_folder_is_not_available() {
        let s = Sidon::new(Path::new("/nonexistent/sidon"));
        assert!(!s.available() && !s.is_loaded());
    }
}
