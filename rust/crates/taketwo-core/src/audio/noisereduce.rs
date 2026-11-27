//! `noisereduce.reduce_noise(y, sr, stationary=True, prop_decrease=0.75)` with its other defaults (noisereduce
//! 3.x, `SpectralGateStationary`): n_fft = win = 1024, hop 256, chunks of 600000 samples padded by 30000,
//! 80 dB floor per frequency row, threshold mean + 1.5 std (dB) of the first 600000 samples, mask
//! smoothed over 500 Hz and 50 ms, scipy's `stft`/`istft` (periodic Hann, `boundary='zeros'`, `padded=False`).
//! Precision as Python: the noise statistics' STFT runs in float32 (the float32 input), each chunk in float64.
//! Parity vector: rust/golden/record_audio.py (`noisereduce` cases).

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

const N_FFT: usize = 1024;
const HOP: usize = N_FFT / 4;
const CHUNK: usize = 600_000;
const PADDING: usize = 30_000;
const PROP_DECREASE: f64 = 0.75;
const N_STD: f64 = 1.5;
const TOP_DB: f64 = 80.0;

/// scipy `get_window('hann', n)`: periodic Hann through `general_cosine` (linspace over [-pi, pi], last dropped).
fn hann_periodic(n: usize) -> Vec<f64> {
    let m = n + 1;
    let step = (2.0 * std::f64::consts::PI) / (m - 1) as f64;
    (0..n).map(|k| 0.5 + 0.5 * (k as f64 * step - std::f64::consts::PI).cos()).collect()
}

/// `_amp_to_db` on one frequency row over time: 20 log10(|x| + eps), floored at the row's max - 80 dB.
fn amp_to_db_rows(mag: &mut [Vec<f64>]) {
    for row in mag.iter_mut() {
        for x in row.iter_mut() {
            *x = 20.0 * (*x + f64::EPSILON).log10();
        }
        let top = row.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - TOP_DB;
        for x in row.iter_mut() {
            *x = x.max(top);
        }
    }
}

/// Frames of scipy's `stft(x, nperseg=1024, noverlap=768, padded=False)` with 512 zeros on each side.
fn frame_count(n: usize) -> usize {
    (n + N_FFT - (N_FFT - HOP)) / HOP
}

/// The noise threshold per frequency bin (dB), from a float32 STFT as scipy computes it for float32 input.
fn noise_thresh(noise: &[f32]) -> Vec<f64> {
    let win = hann_periodic(N_FFT);
    let win32: Vec<f32> = win.iter().map(|&w| w as f32).collect();
    let scale = (1.0 / win.iter().sum::<f64>().powi(2)).sqrt() as f32;
    let n = noise.len();
    let frames = frame_count(n);
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let (mut inp, mut out) = (fft.make_input_vec(), fft.make_output_vec());
    let bins = N_FFT / 2 + 1;
    let mut mag = vec![vec![0.0f64; frames]; bins];
    for t in 0..frames {
        for (k, v) in inp.iter_mut().enumerate() {
            // padded index t*HOP + k; the signal starts at 512
            let i = (t * HOP + k) as i64 - (N_FFT / 2) as i64;
            let x = if i >= 0 && (i as usize) < n { noise[i as usize] } else { 0.0 };
            *v = win32[k] * x;
        }
        fft.process(&mut inp, &mut out).expect("fft sizes match");
        for (b, c) in out.iter().enumerate() {
            let c = *c * scale;
            mag[b][t] = c.re.hypot(c.im) as f64;
        }
    }
    amp_to_db_rows(&mut mag);
    mag.iter()
        .map(|row| {
            let mean = super::np::mean_f64(row);
            let sq: Vec<f64> = row.iter().map(|x| (x - mean) * (x - mean)).collect();
            mean + super::np::mean_f64(&sq).sqrt() * N_STD
        })
        .collect()
}

/// `_smoothing_filter`'s two factors (it is their outer product, normalised): the frequency and time ramps.
fn ramp(n_grad: usize) -> Vec<f64> {
    let step = 1.0 / (n_grad + 1) as f64;
    let mut v: Vec<f64> = (0..=n_grad).map(|k| k as f64 * step + 0.0).collect(); // linspace(0, 1, n+1, endpoint=False)
    let m = n_grad + 2;
    let down = -1.0 / (m - 1) as f64;
    v.extend((0..m).map(|k| if k == m - 1 { 0.0 } else { k as f64 * down + 1.0 })); // linspace(1, 0, n+2)
    v[1..v.len() - 1].to_vec()
}

/// 'same' convolution along one axis with a symmetric odd kernel, zeros outside (fftconvolve's centring).
fn conv_same(x: &[f64], k: &[f64]) -> Vec<f64> {
    let (n, half) = (x.len() as i64, (k.len() / 2) as i64);
    (0..n)
        .map(|i| {
            let mut s = 0.0;
            for (p, &kv) in k.iter().enumerate() {
                let j = i + half - p as i64;
                if j >= 0 && j < n {
                    s += x[j as usize] * kv;
                }
            }
            s
        })
        .collect()
}

/// `spectral_gating_stationary` on one float64 chunk: the denoised chunk, zeros past the istft's length.
fn gate_chunk(chunk: &[f64], thresh: &[f64], f_kernel: &[f64], t_kernel: &[f64]) -> Vec<f64> {
    let win = hann_periodic(N_FFT);
    let wsum: f64 = win.iter().sum();
    let scale = (1.0 / wsum.powi(2)).sqrt();
    let n = chunk.len();
    let frames = frame_count(n);
    let bins = N_FFT / 2 + 1;
    let mut planner = RealFftPlanner::<f64>::new();
    let (fwd, inv) = (planner.plan_fft_forward(N_FFT), planner.plan_fft_inverse(N_FFT));
    let (mut inp, mut spec) = (fwd.make_input_vec(), fwd.make_output_vec());
    let mut stft = vec![vec![Complex::new(0.0, 0.0); frames]; bins];
    for t in 0..frames {
        for (k, v) in inp.iter_mut().enumerate() {
            let i = (t * HOP + k) as i64 - (N_FFT / 2) as i64;
            *v = win[k] * if i >= 0 && (i as usize) < n { chunk[i as usize] } else { 0.0 };
        }
        fwd.process(&mut inp, &mut spec).expect("fft sizes match");
        for b in 0..bins {
            stft[b][t] = spec[b] * scale;
        }
    }
    let mut db: Vec<Vec<f64>> = stft.iter().map(|r| r.iter().map(|c| c.norm()).collect()).collect();
    amp_to_db_rows(&mut db);
    // mask, smoothed over time then over frequency (the 2-D kernel is an outer product)
    let mut mask: Vec<Vec<f64>> = db
        .iter()
        .zip(thresh)
        .map(|(row, &th)| conv_same(&row.iter().map(|&d| if d > th { PROP_DECREASE } else { 0.0 } + (1.0 - PROP_DECREASE)).collect::<Vec<_>>(), t_kernel))
        .collect();
    for t in 0..frames {
        let col: Vec<f64> = mask.iter().map(|r| r[t]).collect();
        for (b, v) in conv_same(&col, f_kernel).into_iter().enumerate() {
            mask[b][t] = v;
        }
    }
    // istft: irfft, times win.sum() (undoes the 'spectrum' scaling), overlap-add with the window, divide by sum(win^2)
    let out_len = N_FFT + (frames - 1) * HOP;
    let (mut x, mut norm) = (vec![0.0f64; out_len], vec![0.0f64; out_len]);
    let (mut sp, mut frame) = (inv.make_input_vec(), inv.make_output_vec());
    for t in 0..frames {
        for b in 0..bins {
            sp[b] = stft[b][t] * mask[b][t];
        }
        // irfft ignores the imaginary parts of the DC and Nyquist bins, as numpy/scipy do
        sp[0].im = 0.0;
        sp[bins - 1].im = 0.0;
        inv.process(&mut sp, &mut frame).expect("fft sizes match");
        for k in 0..N_FFT {
            x[t * HOP + k] += frame[k] / N_FFT as f64 * wsum * win[k];
            norm[t * HOP + k] += win[k] * win[k];
        }
    }
    let mut out = vec![0.0f64; n];
    let body = &x[N_FFT / 2..out_len - N_FFT / 2];
    let nb = &norm[N_FFT / 2..out_len - N_FFT / 2];
    for (i, (v, w)) in body.iter().zip(nb).enumerate().take(n) {
        out[i] = v / if *w > 1e-10 { *w } else { 1.0 };
    }
    out
}

/// `reduce_noise(y, sr, stationary=True, prop_decrease=0.75)` -> float32, as analyze's `.astype(np.float32)`.
pub fn reduce_noise(y: &[f32], sr: u32) -> Result<Vec<f32>, String> {
    reduce_noise_chunked(y, sr, CHUNK, PADDING)
}

/// The same with another chunk size and padding (the parity vector checks the chunked path on a short signal).
pub fn reduce_noise_chunked(y: &[f32], sr: u32, chunk: usize, padding: usize) -> Result<Vec<f32>, String> {
    let n_grad_freq = (500.0 / (sr as f64 / (N_FFT as f64 / 2.0))) as i64;
    let n_grad_time = (50.0 / ((HOP as f64 / sr as f64) * 1000.0)) as i64;
    if n_grad_freq < 1 {
        return Err(format!("freq_mask_smooth_hz needs to be at least {}Hz", (sr as f64 / (N_FFT as f64 / 2.0)) as i64));
    }
    if n_grad_time < 1 {
        return Err(format!("time_mask_smooth_ms needs to be at least {}ms", ((HOP as f64 / sr as f64) * 1000.0) as i64));
    }
    let norm = |v: Vec<f64>| {
        let s: f64 = v.iter().sum();
        v.into_iter().map(|x| x / s).collect::<Vec<f64>>()
    };
    // shortcut: with n_grad 1 on both axes Python skips smoothing; at 24 kHz both are above 1, so it always smooths here
    let (fk, tk) = (norm(ramp(n_grad_freq as usize)), norm(ramp(n_grad_time as usize)));
    let thresh = noise_thresh(&y[..y.len().min(chunk)]);
    let n = y.len();
    let filter = |start: usize, end: usize| -> Vec<f64> {
        // `filter_chunk`: pad with the neighbouring samples (zeros outside the signal), gate, cut the middle
        let (i1, i2) = (start as i64 - padding as i64, (end + padding) as i64);
        let padded: Vec<f64> = (i1..i2).map(|i| if i >= 0 && (i as usize) < n { y[i as usize] as f64 } else { 0.0 }).collect();
        gate_chunk(&padded, &thresh, &fk, &tk)[padding..padding + end - start].to_vec()
    };
    let out: Vec<f64> = if n > chunk {
        let last = (n - 1) / chunk;
        let mut out = Vec::with_capacity(n);
        for ich in 0..=last {
            let full = filter(ich * chunk, (ich + 1) * chunk);
            let end0 = if ich == last { n - ich * chunk } else { chunk };
            out.extend_from_slice(&full[..end0]);
        }
        out
    } else {
        filter(0, n)
    };
    Ok(out.into_iter().map(|v| v as f32).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_ramps_match_numpy_at_24k() {
        // int(500 / (24000 / 512)) = 10 and int(50 / (256 / 24000 * 1000)) = 4: a 21 x 9 kernel
        let (f, t) = (ramp(10), ramp(4));
        assert_eq!((f.len(), t.len()), (21, 9));
        // numpy's values, printed by backend/.venv python
        assert_eq!(t, vec![0.2, 0.4, 0.6000000000000001, 0.8, 1.0, 0.8, 0.6, 0.3999999999999999, 0.19999999999999996]);
        assert_eq!((f[9], f[11], f[19], f[20]), (0.9090909090909092, 0.9090909090909091, 0.18181818181818177, 0.09090909090909083));
    }
}
