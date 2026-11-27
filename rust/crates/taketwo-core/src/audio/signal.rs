//! The scipy.signal pieces audio.py uses (scipy 1.17). `butter` (as SOS) and `sosfiltfilt` come from the
//! `sci-rs` crate, which meets the lead's tolerance against scipy (PROGRESS.md, step S10); this module adds
//! scipy's ValueError for too-short input, where sci-rs would panic. `resample_poly` is a hand port
//! (Kaiser 5.0 `firwin`, zero-phase polyphase filter, scipy's padding and trimming): `rubato` is a
//! different filter and cannot give scipy's length and samples.

use super::PyError;
use sci_rs::signal::filter::design::{DigitalFilter, FilterBandType, FilterOutputType, Sos as SciSos, butter_dyn};
use std::f64::consts::PI;

/// Second-order sections as scipy prints them: [b0, b1, b2, a0, a1, a2].
pub type Sos = Vec<[f64; 6]>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BType {
    Lowpass,
    Highpass,
    Bandpass,
}

/// `scipy.signal.butter(order, wn, btype, fs=fs, output="sos")`.
pub fn butter(order: usize, wn: &[f64], btype: BType, fs: f64) -> Result<Sos, PyError> {
    if wn.iter().any(|&w| w <= 0.0 || w >= fs / 2.0) {
        return Err(PyError::value("Digital filter critical frequencies must be 0 < Wn < fs/2"));
    }
    let bt = match btype {
        BType::Lowpass => FilterBandType::Lowpass,
        BType::Highpass => FilterBandType::Highpass,
        BType::Bandpass => FilterBandType::Bandpass,
    };
    match butter_dyn(order, wn.to_vec(), Some(bt), Some(false), Some(FilterOutputType::Sos), Some(fs)) {
        DigitalFilter::Sos(f) => Ok(f.sos.iter().map(|s| [s.b[0], s.b[1], s.b[2], s.a[0], s.a[1], s.a[2]]).collect()),
        _ => unreachable!("asked for SOS"),
    }
}

/// `scipy.signal.sosfiltfilt(sos, x)` (padtype "odd", default padlen); float64 output.
pub fn sosfiltfilt(sos: &Sos, x: &[f32]) -> Result<Vec<f64>, PyError> {
    let zeros = |col: usize| sos.iter().filter(|s| s[col] == 0.0).count();
    let edge = 3 * (2 * sos.len() + 1 - zeros(2).min(zeros(5)));
    if x.len() <= edge {
        return Err(PyError::value(&format!(
            "The length of the input vector x must be greater than padlen, which is {edge}."
        )));
    }
    let sections: Vec<SciSos<f64>> = sos.iter().map(|s| SciSos::new([s[0], s[1], s[2]], [s[3], s[4], s[5]])).collect();
    Ok(sci_rs::signal::filter::sosfiltfilt_dyn(x.iter().map(|&v| v as f64), &sections))
}

/// scipy.special.i0 by its power series (beta = 5 converges in about 25 terms).
fn i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut term, mut sum, mut k) = (1.0f64, 1.0f64, 1.0f64);
    while term > sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// `firwin(numtaps, cutoff, window=("kaiser", beta))`, a low-pass with cutoff relative to Nyquist.
pub fn firwin_kaiser(numtaps: usize, cutoff: f64, beta: f64) -> Vec<f64> {
    let alpha = 0.5 * (numtaps as f64 - 1.0);
    let sinc = |x: f64| {
        let y = PI * if x == 0.0 { 1.0e-20 } else { x };
        y.sin() / y
    };
    let walpha = (numtaps as f64 - 1.0) / 2.0;
    let mut h: Vec<f64> = (0..numtaps)
        .map(|i| {
            let m = i as f64 - alpha;
            let win = i0(beta * (1.0 - ((i as f64 - walpha) / walpha).powf(2.0)).sqrt()) / i0(beta);
            (cutoff * sinc(cutoff * m) - 0.0 * sinc(0.0 * m)) * win
        })
        .collect();
    let s = super::np::sum_f64(&h);
    h.iter_mut().for_each(|v| *v /= s);
    h
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `scipy.signal.resample_poly(x, up, down)` on float32 input (window ("kaiser", 5.0), padtype "constant").
/// The filter is cast to float32 as scipy does; each output is summed in float64 and cast back.
pub fn resample_poly(x: &[f32], up: u64, down: u64) -> Vec<f32> {
    let g = gcd(up, down);
    let (up, down) = (up / g, down / g);
    if up == 1 && down == 1 {
        return x.to_vec();
    }
    let n_in = x.len() as u64;
    let n_out = (n_in * up).div_ceil(down);
    let max_rate = up.max(down);
    let half_len = 10 * max_rate;
    let h: Vec<f32> = firwin_kaiser((2 * half_len + 1) as usize, 1.0 / max_rate as f64, 5.0)
        .into_iter()
        .map(|v| v as f32 * up as f32)
        .collect();
    let n_pre_pad = down - half_len % down;
    let n_pre_remove = (half_len + n_pre_pad) / down;
    // upfirdn output k is sum over t of hp[k*down - t*up] * x[t], hp = n_pre_pad zeros then h
    (0..n_out)
        .map(|i| {
            let pos = (n_pre_remove + i) * down; // index into the upsampled, convolved signal
            let mut acc = 0.0f64;
            // hp index j = pos - t*up must lie in [n_pre_pad, n_pre_pad + len(h))
            let lo = pos.saturating_sub(n_pre_pad + h.len() as u64 - 1).div_ceil(up);
            let hi = if pos >= n_pre_pad { ((pos - n_pre_pad) / up).min(n_in.saturating_sub(1)) } else { return 0.0 };
            if n_in == 0 {
                return 0.0;
            }
            let mut t = lo;
            while t <= hi {
                let j = pos - t * up - n_pre_pad;
                acc += h[j as usize] as f64 * x[t as usize] as f64;
                t += 1;
            }
            acc as f32
        })
        .collect()
}
