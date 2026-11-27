//! The speaker encoder: `ml.speaker_embedding(audio, sr)`, Resemble's GE2E voice encoder as mlx-audio runs it
//! (`tts/models/chatterbox/voice_encoder`). Stages, in order: resample to 16 kHz (mlx-audio's Kaiser sinc
//! polyphase, `resample.resample_audio_array`), energy trim (`embeds_from_wavs`, top_db 20), mel (40 bands,
//! power 2, no log), partial windows at rate 1.3, 3-layer LSTM + projection + ReLU + L2 per partial, mean + L2,
//! `utt_to_spk_embed`'s L2, then ml.py's `e / (norm + 1e-9)` in float32.
//! Plain CPU float32: the model is 5.7 MB, so no Candle device or GPU (Candle only reads the safetensors).
//! Weights: `TAKETWO_VOICE_ENCODER`, default `$TAKETWO_MODELS/voice-encoder/ve.safetensors` (never downloaded).

use realfft::RealFftPlanner;
use std::path::PathBuf;
use taketwo_core::audio::np::mean_f32;
use taketwo_core::audio::signal::firwin_kaiser;
use taketwo_core::render::models::SpeakerEncoder;

pub const SAMPLE_RATE: u32 = 16000;
pub const N_MELS: usize = 40;
const N_FFT: usize = 400;
const HOP: usize = 160;
const N_FREQS: usize = N_FFT / 2 + 1;
const FMAX: f64 = 8000.0;
const PARTIAL_FRAMES: usize = 160;
const HIDDEN: usize = 256;
const EMBED: usize = 256;
const LAYERS: usize = 3;

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `_polyphase_filter`: the kaiser_best-equivalent FIR (resampy defaults) for the reduced ratio.
pub fn fir(up: usize, down: usize) -> Vec<f64> {
    let max_rate = up.max(down);
    firwin_kaiser(2 * 64 * max_rate + 1, 0.9475937167399596 / max_rate as f64, 14.769656459379492)
}

/// `resample_audio(audio, sr, 16000)`: `scipy.signal.resample_poly(x, up, down, window=fir, padtype="edge")`.
/// The FIR stays float64 (scipy keeps the given window's dtype), each output is summed in float64 and cast
/// to float32; "edge" holds the first and last input sample beyond the ends.
pub fn resample(x: &[f32], sr: u32) -> Vec<f32> {
    let g = gcd(SAMPLE_RATE as usize, sr as usize);
    let (up, down) = (SAMPLE_RATE as usize / g, sr as usize / g);
    if up == down || x.is_empty() {
        return x.to_vec();
    }
    let h: Vec<f64> = fir(up, down).into_iter().map(|v| v * up as f64).collect();
    let half_len = (h.len() - 1) / 2;
    let n_pre_pad = down - half_len % down;
    let n_pre_remove = (half_len + n_pre_pad) / down;
    let n_out = (x.len() * up).div_ceil(down);
    let (up, last) = (up as i64, x.len() as i64 - 1);
    (0..n_out)
        .map(|i| {
            // upfirdn: y[k] = sum over t of hp[k*down - t*up] * x[t], hp = n_pre_pad zeros then h
            let p = ((n_pre_remove + i) * down) as i64 - n_pre_pad as i64; // index into h when t = 0
            let lo = (p - (h.len() as i64 - 1)).div_euclid(up) + i64::from((p - (h.len() as i64 - 1)).rem_euclid(up) != 0);
            let hi = p.div_euclid(up);
            let mut acc = 0.0f64;
            for t in lo..=hi {
                acc += h[(p - t * up) as usize] * x[t.clamp(0, last) as usize] as f64;
            }
            acc as f32
        })
        .collect()
}

/// The energy trim of `embeds_from_wavs` (frame 2048, hop 512, top_db 20): the kept sample range.
pub fn trim_bounds(w: &[f32]) -> (usize, usize) {
    const FRAME: usize = 2048;
    const HOP: usize = 512;
    if w.len() < FRAME {
        return (0, w.len()); // n_frames <= 0: nothing trimmed
    }
    let n_frames = 1 + (w.len() - FRAME) / HOP;
    let db: Vec<f64> = (0..n_frames)
        .map(|i| {
            let sq: Vec<f32> = w[i * HOP..(i * HOP + FRAME).min(w.len())].iter().map(|v| v * v).collect();
            let rms = (mean_f32(&sq) as f64).sqrt();
            20.0 * rms.max(1e-10).log10()
        })
        .collect();
    let threshold = db.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - 20.0;
    let first = db.iter().position(|&d| d >= threshold);
    let last = db.iter().rposition(|&d| d >= threshold);
    match (first, last) {
        (Some(s), Some(e)) => (s * HOP, ((e + 1) * HOP + FRAME).min(w.len())),
        _ => (0, w.len()),
    }
}

fn hz_to_mel(f: f64) -> f64 {
    let (f_sp, min_log_hz) = (200.0 / 3.0, 1000.0);
    if f >= min_log_hz { min_log_hz / f_sp + (f / min_log_hz).ln() / (6.4f64.ln() / 27.0) } else { f / f_sp }
}

fn mel_to_hz(m: f64) -> f64 {
    let (f_sp, min_log_hz) = (200.0 / 3.0, 1000.0);
    let min_log_mel = min_log_hz / f_sp;
    if m >= min_log_mel { min_log_hz * ((6.4f64.ln() / 27.0) * (m - min_log_mel)).exp() } else { f_sp * m }
}

/// `mel_filters(16000, 400, 40, 0, 8000, norm="slaney", mel_scale="slaney")`, row-major (40, 201).
/// Built in float64 and cast (mlx-audio builds it in float32; the difference is in the mel tolerance).
pub fn mel_filters() -> Vec<f32> {
    let lin = |a: f64, b: f64, n: usize| -> Vec<f64> { (0..n).map(|i| a + (b - a) * i as f64 / (n - 1) as f64).collect() };
    let freqs = lin(0.0, (SAMPLE_RATE / 2) as f64, N_FREQS);
    let f_pts: Vec<f64> = lin(hz_to_mel(0.0), hz_to_mel(FMAX), N_MELS + 2).into_iter().map(mel_to_hz).collect();
    let mut out = vec![0f32; N_MELS * N_FREQS];
    for m in 0..N_MELS {
        let enorm = 2.0 / (f_pts[m + 2] - f_pts[m]);
        for (k, f) in freqs.iter().enumerate() {
            let down = (f - f_pts[m]) / (f_pts[m + 1] - f_pts[m]);
            let up = (f_pts[m + 2] - f) / (f_pts[m + 2] - f_pts[m + 1]);
            out[m * N_FREQS + k] = (0f64.max(down.min(up)) * enorm) as f32;
        }
    }
    out
}

/// `melspectrogram(wav, hp)` transposed to (frames, 40): centred STFT (reflect pad 200, symmetric Hann 400,
/// hop 160), |X|^2, times the filterbank. The FFT and the band sums run in float64.
pub fn mel(w: &[f32], filters: &[f32]) -> Result<Vec<[f32; N_MELS]>, String> {
    let pad = N_FFT / 2;
    if w.len() <= pad {
        return Err(format!("Input is too short (length={}) for the voice encoder", w.len()));
    }
    let mut x = Vec::with_capacity(w.len() + 2 * pad);
    x.extend(w[1..=pad].iter().rev());
    x.extend_from_slice(w);
    x.extend(w[w.len() - pad - 1..w.len() - 1].iter().rev());
    let window: Vec<f32> =
        (0..N_FFT).map(|n| (0.5 * (1.0 - (2.0 * std::f64::consts::PI * n as f64 / (N_FFT - 1) as f64).cos())) as f32).collect();
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N_FFT);
    let (mut inp, mut spec) = (fft.make_input_vec(), fft.make_output_vec());
    let n_frames = 1 + (x.len() - N_FFT) / HOP;
    let mut out = Vec::with_capacity(n_frames);
    for f in 0..n_frames {
        for (i, v) in inp.iter_mut().enumerate() {
            *v = (x[f * HOP + i] * window[i]) as f64;
        }
        fft.process(&mut inp, &mut spec).map_err(|e| e.to_string())?;
        let power: Vec<f64> = spec.iter().map(|c| c.norm_sqr()).collect();
        let mut row = [0f32; N_MELS];
        for (m, r) in row.iter_mut().enumerate() {
            *r = filters[m * N_FREQS..(m + 1) * N_FREQS].iter().zip(&power).map(|(a, p)| *a as f64 * p).sum::<f64>() as f32;
        }
        out.push(row);
    }
    Ok(out)
}

/// `get_frame_step(0.5, 1.3)` and `get_num_wins(n_frames, step, 0.8)`: the partial start frames and the
/// padded length (the mel is zero-padded to it; frames past it are dropped).
pub fn partials(n_frames: usize) -> (Vec<usize>, usize) {
    let step = ((SAMPLE_RATE as f64 / 1.3) / PARTIAL_FRAMES as f64).round() as usize; // 77
    let (mut n_wins, rem) = {
        let v = (n_frames + step).saturating_sub(PARTIAL_FRAMES);
        (v / step, v % step)
    };
    if n_wins == 0 || (rem + PARTIAL_FRAMES - step) as f64 / PARTIAL_FRAMES as f64 >= 0.8 {
        n_wins += 1;
    }
    ((0..n_wins).map(|i| i * step).collect(), PARTIAL_FRAMES + step * (n_wins - 1))
}

fn l2(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter_mut().for_each(|x| *x /= n);
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// y[r] = b[r] + W[r, :] . x  (W row-major, rows x x.len())
fn affine(w: &[f32], b: &[f32], x: &[f32], y: &mut [f32]) {
    for (r, out) in y.iter_mut().enumerate() {
        *out = b[r] + w[r * x.len()..(r + 1) * x.len()].iter().zip(x).map(|(a, v)| a * v).sum::<f32>();
    }
}

struct Layer {
    wx: Vec<f32>, // (4H, in), gates i, f, g, o as PyTorch and MLX
    wh: Vec<f32>, // (4H, H)
    b: Vec<f32>,  // bias_ih + bias_hh, as `sanitize` combines them
}

struct Model {
    layers: Vec<Layer>,
    proj_w: Vec<f32>,
    proj_b: Vec<f32>,
}

impl Model {
    fn load(path: &std::path::Path) -> Result<Model, String> {
        let t = candle_core::safetensors::load(path, &candle_core::Device::Cpu).map_err(|e| format!("{}: {e}", path.display()))?;
        let get = |k: &str| -> Result<Vec<f32>, String> {
            let v = t.get(k).ok_or_else(|| format!("{}: no tensor {k}", path.display()))?;
            v.flatten_all().and_then(|v| v.to_vec1::<f32>()).map_err(|e| format!("{k}: {e}"))
        };
        let mut layers = Vec::new();
        for l in 0..LAYERS {
            let (bi, bh) = (get(&format!("lstm.bias_ih_l{l}"))?, get(&format!("lstm.bias_hh_l{l}"))?);
            let layer = Layer {
                wx: get(&format!("lstm.weight_ih_l{l}"))?,
                wh: get(&format!("lstm.weight_hh_l{l}"))?,
                b: bi.iter().zip(&bh).map(|(a, b)| a + b).collect(),
            };
            let input = if l == 0 { N_MELS } else { HIDDEN };
            if layer.wx.len() != 4 * HIDDEN * input || layer.wh.len() != 4 * HIDDEN * HIDDEN || layer.b.len() != 4 * HIDDEN {
                return Err(format!("{}: lstm layer {l} has the wrong shape", path.display()));
            }
            layers.push(layer);
        }
        let (proj_w, proj_b) = (get("proj.weight")?, get("proj.bias")?);
        if proj_w.len() != EMBED * HIDDEN || proj_b.len() != EMBED {
            return Err(format!("{}: proj has the wrong shape", path.display()));
        }
        Ok(Model { layers, proj_w, proj_b })
    }

    /// `VoiceEncoder.__call__` on one partial (frames x 40): last hidden state of the top layer, projection,
    /// ReLU, L2.
    fn partial(&self, frames: &[[f32; N_MELS]]) -> Vec<f32> {
        let mut seq: Vec<Vec<f32>> = frames.iter().map(|f| f.to_vec()).collect();
        let mut gates = vec![0f32; 4 * HIDDEN];
        let mut hg = vec![0f32; 4 * HIDDEN];
        for layer in &self.layers {
            let (mut h, mut c) = (vec![0f32; HIDDEN], vec![0f32; HIDDEN]);
            for x in seq.iter_mut() {
                affine(&layer.wx, &layer.b, x, &mut gates);
                affine(&layer.wh, &[0f32; 4 * HIDDEN], &h, &mut hg);
                for j in 0..HIDDEN {
                    let i = sigmoid(gates[j] + hg[j]);
                    let f = sigmoid(gates[HIDDEN + j] + hg[HIDDEN + j]);
                    let g = (gates[2 * HIDDEN + j] + hg[2 * HIDDEN + j]).tanh();
                    let o = sigmoid(gates[3 * HIDDEN + j] + hg[3 * HIDDEN + j]);
                    c[j] = f * c[j] + i * g;
                    h[j] = o * c[j].tanh();
                }
                *x = h.clone();
            }
        }
        let mut e = vec![0f32; EMBED];
        affine(&self.proj_w, &self.proj_b, seq.last().expect("a partial has frames"), &mut e);
        e.iter_mut().for_each(|v| *v = v.max(0.0));
        l2(&mut e);
        e
    }

    /// `embeds_from_mels([mel], as_spk=True, rate=1.3)` then ml.py's `e / (norm + 1e-9)`.
    fn embed_mel(&self, mel: &[[f32; N_MELS]]) -> Vec<f32> {
        let (starts, target) = partials(mel.len());
        let mut padded = mel.to_vec();
        padded.resize(target.max(mel.len()), [0f32; N_MELS]);
        let mut e = vec![0f32; EMBED];
        for s in &starts {
            for (a, p) in e.iter_mut().zip(self.partial(&padded[*s..*s + PARTIAL_FRAMES])) {
                *a += p;
            }
        }
        e.iter_mut().for_each(|v| *v /= starts.len() as f32);
        l2(&mut e); // inference()
        l2(&mut e); // utt_to_spk_embed
        let n = e.iter().map(|x| x * x).sum::<f32>().sqrt();
        e.iter_mut().for_each(|v| *v /= n + 1e-9f32);
        e
    }
}

/// `ml.speaker_embedding` behind `SpeakerEncoder`: loads the weights once, on first use; `unload()` frees them.
pub struct VoiceEncoder {
    path: PathBuf,
    model: Option<Model>,
    filters: Vec<f32>,
}

impl VoiceEncoder {
    pub fn new(path: impl Into<PathBuf>) -> VoiceEncoder {
        VoiceEncoder { path: path.into(), model: None, filters: mel_filters() }
    }

    /// `TAKETWO_VOICE_ENCODER`, else `$TAKETWO_MODELS/voice-encoder/ve.safetensors`.
    pub fn from_env() -> Result<VoiceEncoder, String> {
        if let Some(p) = std::env::var_os("TAKETWO_VOICE_ENCODER") {
            return Ok(VoiceEncoder::new(p));
        }
        let models = std::env::var_os("TAKETWO_MODELS").ok_or("set TAKETWO_VOICE_ENCODER or TAKETWO_MODELS")?;
        Ok(VoiceEncoder::new(PathBuf::from(models).join("voice-encoder/ve.safetensors")))
    }

    pub fn is_loaded(&self) -> bool {
        self.model.is_some()
    }

    pub fn load(&mut self) -> Result<(), String> {
        if self.model.is_none() {
            self.model = Some(Model::load(&self.path)?);
        }
        Ok(())
    }

    pub fn unload(&mut self) {
        self.model = None;
    }
}

impl SpeakerEncoder for VoiceEncoder {
    fn embedding(&mut self, audio: &[f32], sr: u32) -> Result<Vec<f32>, String> {
        self.load()?;
        let r = resample(audio, sr);
        let (s, e) = trim_bounds(&r);
        let m = mel(&r[s..e], &self.filters)?;
        Ok(self.model.as_ref().expect("loaded above").embed_mel(&m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use taketwo_core::render::models::cosine;

    fn golden() -> (PathBuf, Value) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden");
        let doc = serde_json::from_str(&std::fs::read_to_string(dir.join("encoder.json")).unwrap()).unwrap();
        (dir, doc)
    }

    fn f32s(p: &Path) -> Vec<f32> {
        std::fs::read(p).unwrap().chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
    }

    fn clips(doc: &Value) -> &Vec<Value> {
        let c = doc["clips"].as_array().unwrap();
        assert_eq!(c.len(), 4, "golden clips read");
        c
    }

    fn n(v: &Value) -> usize {
        v.as_u64().unwrap() as usize
    }

    fn wav(dir: &Path, c: &Value) -> Vec<f32> {
        let (a, sr) = taketwo_core::audio::wav::load(&dir.join(c["wav"].as_str().unwrap()), None).unwrap();
        assert_eq!(sr as u64, c["sr"].as_u64().unwrap());
        assert_eq!(a.len(), n(&c["n_in"]));
        a
    }

    #[test]
    fn encoder_fir_matches_mlx_audio() {
        let (dir, doc) = golden();
        let want: Vec<f64> = std::fs::read(dir.join(doc["fir"].as_str().unwrap()))
            .unwrap()
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let got = fir(2, 3);
        assert_eq!(got.len(), want.len());
        let max = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        println!("fir: {} taps, max abs difference {max:e}", got.len());
        assert!(max < 1e-15, "{max}");
    }

    #[test]
    fn encoder_resample_golden() {
        let (dir, doc) = golden();
        let mut read = 0;
        for c in clips(&doc) {
            let name = c["name"].as_str().unwrap();
            let want = f32s(&dir.join(format!("encoder/{name}.resampled.f32")));
            let got = resample(&wav(&dir, c), c["sr"].as_u64().unwrap() as u32);
            assert_eq!(got.len(), n(&c["resampled_len"]), "{name}");
            assert_eq!(want.len(), got.len(), "{name}");
            let max = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            let edge = got.iter().zip(&want).take(70).chain(got.iter().zip(&want).rev().take(70)).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            println!("resample {name}: max abs difference {max:e} (first/last 70: {edge:e})");
            assert!(max <= 1e-6, "{name}: {max}");
            // chained: the Rust resample gives the recorded trim bounds too
            assert_eq!(trim_bounds(&got), (n(&c["trim_start"]), n(&c["trim_end"])), "{name}");
            read += 1;
        }
        assert_eq!(read, 4);
    }

    #[test]
    fn encoder_trim_golden() {
        let (dir, doc) = golden();
        let mut read = 0;
        for c in clips(&doc) {
            let name = c["name"].as_str().unwrap();
            let r = f32s(&dir.join(format!("encoder/{name}.resampled.f32")));
            assert_eq!(trim_bounds(&r), (n(&c["trim_start"]), n(&c["trim_end"])), "{name}");
            read += 1;
        }
        assert_eq!(read, 4);
    }

    #[test]
    fn encoder_mel_filters_golden() {
        let (dir, doc) = golden();
        let want = f32s(&dir.join(doc["mel_filters"].as_str().unwrap()));
        let got = mel_filters();
        assert_eq!(got.len(), want.len());
        let max = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        println!("mel filters: max abs difference {max:e}");
        assert!(max < 1e-6, "{max}");
    }

    /// Tolerance: |rust - python| <= 1e-4 * max(python value in the clip) + 1e-3 * |python value|.
    /// Python's STFT, power and band sums run in float32 (MLX), ours in float64.
    #[test]
    fn encoder_mel_golden() {
        let (dir, doc) = golden();
        let filters = mel_filters();
        let mut read = 0;
        for c in clips(&doc) {
            let name = c["name"].as_str().unwrap();
            let r = f32s(&dir.join(format!("encoder/{name}.resampled.f32")));
            let want = f32s(&dir.join(format!("encoder/{name}.mel.f32")));
            let got = mel(&r[n(&c["trim_start"])..n(&c["trim_end"])], &filters).unwrap();
            assert_eq!(got.len(), n(&c["mel_frames"]), "{name}");
            assert_eq!(want.len(), got.len() * N_MELS, "{name}");
            let peak = want.iter().cloned().fold(0f32, f32::max);
            let (mut worst, mut max_abs) = (0f32, 0f32);
            for (g, w) in got.iter().flatten().zip(&want) {
                let d = (g - w).abs();
                max_abs = max_abs.max(d);
                worst = worst.max(d / (1e-4 * peak + 1e-3 * w.abs()));
            }
            println!("mel {name}: peak {peak:e}, max abs difference {max_abs:e} ({:e} of peak), worst / tolerance {worst:.3}", max_abs / peak);
            assert!(worst <= 1.0, "{name}: {worst}");
            read += 1;
        }
        assert_eq!(read, 4);
    }

    #[test]
    fn encoder_partials_golden() {
        let (_, doc) = golden();
        let mut read = 0;
        for c in clips(&doc) {
            let (starts, target) = partials(n(&c["mel_frames"]));
            let want: Vec<usize> = c["partial_starts"].as_array().unwrap().iter().map(n).collect();
            assert_eq!(starts, want, "{}", c["name"]);
            assert_eq!(starts.len(), n(&c["n_wins"]));
            assert_eq!(target, n(&c["target_n"]));
            assert_eq!(n(&c["frame_step"]), 77);
            read += 1;
        }
        assert_eq!(read, 4);
        // edges of get_num_wins: one frame, exactly one window, one frame over it
        assert_eq!(partials(1), (vec![0], 160));
        assert_eq!(partials(160), (vec![0], 160));
        assert_eq!(partials(161), (vec![0], 160));
    }

    #[test]
    fn encoder_short_input_is_an_error_not_a_panic() {
        assert!(mel(&[0.0; 200], &mel_filters()).is_err());
        assert_eq!(trim_bounds(&[0.0; 100]), (0, 100));
    }

    #[test]
    #[ignore = "needs ve.safetensors (TAKETWO_VOICE_ENCODER or TAKETWO_MODELS)"]
    fn encoder_parity() {
        let (dir, doc) = golden();
        let mut enc = VoiceEncoder::from_env().unwrap();
        let t0 = std::time::Instant::now();
        enc.load().unwrap();
        let load_s = t0.elapsed().as_secs_f64();
        let mut rust = Vec::new();
        let mut py = Vec::new();
        let mut out_clips = Vec::new();
        for c in clips(&doc) {
            let name = c["name"].as_str().unwrap();
            let a = wav(&dir, c);
            let t = std::time::Instant::now();
            let e = enc.embedding(&a, c["sr"].as_u64().unwrap() as u32).unwrap();
            let secs = t.elapsed().as_secs_f64();
            let p: Vec<f32> = c["embedding"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
            assert_eq!((e.len(), p.len()), (256, 256));
            let cos = cosine(&e, &p).unwrap();
            let maxd = e.iter().zip(&p).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            println!("clip {name}: cosine rust/python {cos:.7}, largest element difference {maxd:e}, {secs:.2} s");
            out_clips.push(serde_json::json!({"name": name, "cosine_rust_python": cos, "max_element_difference": maxd, "seconds": secs}));
            rust.push(e);
            py.push(p);
        }
        let names: Vec<&str> = clips(&doc).iter().map(|c| c["name"].as_str().unwrap()).collect();
        let pairs = doc["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 6);
        let (mut max_sim, mut max_py_cos) = (0f64, 0f64);
        let mut out_pairs = Vec::new();
        for p in pairs {
            let i = names.iter().position(|n| *n == p["a"].as_str().unwrap()).unwrap();
            let j = names.iter().position(|n| *n == p["b"].as_str().unwrap()).unwrap();
            let want = p["cosine"].as_f64().unwrap();
            let rr = cosine(&rust[i], &rust[j]).unwrap();
            let rp = cosine(&rust[i], &py[j]).unwrap();
            let pr = cosine(&py[i], &rust[j]).unwrap();
            let pp = cosine(&py[i], &py[j]).unwrap();
            let d = [(rr - want).abs(), (rp - want).abs(), (pr - want).abs()];
            println!(
                "pair {}-{}: python {want:.7}; rust/rust {rr:.7} (diff {:.2e}); rust {} vs python {} {rp:.7} (diff {:.2e}); python {} vs rust {} {pr:.7} (diff {:.2e}); render::cosine on python {pp:.9} (diff {:.1e})",
                names[i], names[j], d[0], names[i], names[j], d[1], names[i], names[j], d[2], (pp - want).abs()
            );
            for v in d {
                assert!(v < 0.005, "{}-{}: {v}", names[i], names[j]);
                max_sim = max_sim.max(v);
            }
            max_py_cos = max_py_cos.max((pp - want).abs());
            out_pairs.push(serde_json::json!({"a": names[i], "b": names[j], "python": want, "rust_rust": rr,
                "rust_vs_python_other": rp, "python_vs_rust_other": pr, "render_cosine_on_python": pp}));
        }
        println!("largest similarity difference {max_sim:.2e}; render::cosine vs ml.cosine on python embeddings: largest difference {max_py_cos:e}; load {load_s:.3} s");
        let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out");
        std::fs::create_dir_all(&out).unwrap();
        let doc = serde_json::json!({"clips": out_clips, "pairs": out_pairs, "max_sim_difference": max_sim,
            "max_render_cosine_vs_ml_cosine": max_py_cos, "load_seconds": load_s,
            "build": if cfg!(debug_assertions) { "debug" } else { "release" }});
        std::fs::write(out.join("encoder_parity.json"), serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        for c in &out_clips {
            assert!(c["cosine_rust_python"].as_f64().unwrap() >= 0.999, "{c}");
        }
        assert!(max_py_cos < 1e-6, "{max_py_cos}");
        enc.unload();
        assert!(!enc.is_loaded());
    }
}
